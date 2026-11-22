//! Dialogs for merging (fast-forward, squash, no-commit, message), stashing (only the selected
//! files, keep the index) and for a checkout that local changes block (stash, switch, reapply).

use egui::{Align2, Key, Rect, RichText, Sense, Ui, pos2, vec2};

use super::dialogs::{Outcome, buttons, title, valid_ref_name};
use super::ext::{DialogBody, dialog};
use super::sync::{matches_branch_filter, pick, row};
use super::{Action, Dialog, Refresh, RepoTab, Undo, ops, widgets};
use crate::git::Branch;
use crate::theme::{self, Tokens};

// ---- merge ----------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum FastForward {
    /// Git's default: fast-forward when it can, else a merge commit.
    Default,
    /// Always a merge commit (`--no-ff`).
    Never,
    /// Refuse when a merge commit would be needed (`--ff-only`).
    Only,
}

impl FastForward {
    const ALL: [FastForward; 3] = [FastForward::Default, FastForward::Never, FastForward::Only];

    fn label(self) -> &'static str {
        match self {
            FastForward::Default => "Fast-forward when possible",
            FastForward::Never => "Always create a merge commit",
            FastForward::Only => "Fast-forward only",
        }
    }
}

struct MergeDialog {
    branch: String,
    current: String,
    ff: FastForward,
    squash: bool,
    no_commit: bool,
    message: String,
}

pub(super) fn merge_dialog(branch: &str, current: &str) -> Dialog {
    dialog(MergeDialog {
        branch: branch.to_owned(),
        current: current.to_owned(),
        ff: FastForward::Default,
        squash: false,
        no_commit: false,
        message: String::new(),
    })
}

impl MergeDialog {
    fn args(&self) -> Vec<String> {
        let mut args = vec!["merge".to_owned()];
        if self.squash {
            // `--squash` can't be combined with the fast-forward options.
            args.push("--squash".into());
        } else if self.no_commit {
            // git fast-forwards despite `--no-commit` when it can, moving the branch: only
            // `--no-ff` really leaves the merge for the user to commit.
            args.push("--no-ff".into());
        } else {
            match self.ff {
                FastForward::Default => {}
                FastForward::Never => args.push("--no-ff".into()),
                FastForward::Only => args.push("--ff-only".into()),
            }
        }
        if self.no_commit {
            args.push("--no-commit".into());
        }
        let message = self.message.trim();
        if message.is_empty() || self.squash || self.no_commit {
            args.push("--no-edit".into());
        } else {
            args.extend(["-m".to_owned(), message.to_owned()]);
        }
        args.push(self.branch.clone());
        args
    }
}

impl DialogBody for MergeDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        title(
            ui,
            "Merge",
            &format!("Merge {} into {}.", self.branch, self.current),
            t,
        );
        ui.add_enabled_ui(!self.squash && !self.no_commit, |ui| {
            let shown = if self.no_commit {
                FastForward::Never.label()
            } else {
                self.ff.label()
            };
            row(ui, t, "Fast-forward", |ui| {
                pick(ui, "merge-ff", shown, |ui| {
                    let mut changed = false;
                    for ff in FastForward::ALL {
                        if ui.selectable_label(self.ff == ff, ff.label()).clicked() {
                            self.ff = ff;
                            changed = true;
                        }
                    }
                    changed
                })
            });
        });
        ui.add_space(4.0);
        ui.add_enabled_ui(!self.squash && !self.no_commit, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.message)
                    .hint_text("Merge commit message (optional)")
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(8, 6)),
            );
        });
        ui.add_space(6.0);
        ui.checkbox(
            &mut self.squash,
            "Squash into one change, don't commit (--squash)",
        );
        ui.checkbox(
            &mut self.no_commit,
            "Merge but don't commit yet (--no-commit)",
        );
        ui.add_space(8.0);
        let valid = super::dialogs::valid_ref_name(&self.branch);
        buttons(ui, "Merge", valid, enter, || self.action())
    }
}

impl MergeDialog {
    fn action(&self) -> Action {
        let plain = !self.squash && !self.no_commit;
        Action::UndoableOps {
            label: format!("Merge {} into {}", self.branch, self.current),
            ops: vec![self.args()],
            refresh: Refresh::All,
            // A squash or a merge left uncommitted doesn't move HEAD.
            undo: if plain {
                Undo::HeadMove { mode: "--keep" }
            } else {
                Undo::None
            },
        }
    }
}

// ---- stash ----------------------------------------------------------------------------------

struct StashDialog {
    message: String,
    untracked: bool,
    keep_index: bool,
    /// The files picked with Ctrl+click in the Changes lists (and their old names).
    selected: Vec<String>,
    only_selected: bool,
    focus: bool,
}

impl StashDialog {
    fn args(&self) -> Vec<String> {
        let mut args = vec!["stash".to_owned(), "push".to_owned()];
        if self.untracked {
            args.push("--include-untracked".into());
        }
        if self.keep_index {
            args.push("--keep-index".into());
        }
        if !self.message.trim().is_empty() {
            args.extend(["-m".to_owned(), self.message.trim().to_owned()]);
        }
        if self.only_selected && !self.selected.is_empty() {
            args.push("--".into());
            args.extend(self.selected.iter().map(|p| super::ext::literal(p)));
        }
        args
    }
}

impl DialogBody for StashDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        title(
            ui,
            "Stash changes",
            "Put changes aside and clean the working tree.",
            t,
        );
        let edit = ui.add(
            egui::TextEdit::singleline(&mut self.message)
                .hint_text("Message (optional)")
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(8, 6)),
        );
        if self.focus {
            edit.request_focus();
            self.focus = false;
        }
        ui.add_space(6.0);
        ui.checkbox(&mut self.untracked, "Include untracked files");
        ui.checkbox(
            &mut self.keep_index,
            "Keep staged changes in the working tree (--keep-index)",
        );
        if !self.selected.is_empty() {
            ui.checkbox(
                &mut self.only_selected,
                format!("Only the {} selected file(s)", self.selected.len()),
            );
        } else {
            ui.label(
                RichText::new("Ctrl+click files in the Changes lists to stash only those.")
                    .color(t.text_faint)
                    .small(),
            );
        }
        ui.add_space(8.0);
        buttons(ui, "Stash", true, enter, || self.action())
    }
}

impl StashDialog {
    fn action(&self) -> Action {
        let steps = vec![self.args()];
        Action::UndoableOps {
            label: "Stash".to_owned(),
            ops: steps.clone(),
            refresh: Refresh::All,
            undo: Undo::Custom {
                undo: ops(&[&["stash", "pop", "--index"]]),
                redo: steps,
            },
        }
    }
}

impl StashDialog {
    /// A dialog for `tab`; files picked in the Changes lists can be stashed alone.
    fn for_tab(tab: &RepoTab) -> StashDialog {
        let mut selected: Vec<String> = Vec::new();
        for f in &tab.patch_sel {
            selected.push(f.path.clone());
            selected.extend(f.old_path.clone());
        }
        StashDialog {
            message: String::new(),
            untracked: true,
            keep_index: false,
            only_selected: !selected.is_empty(),
            selected,
            focus: true,
        }
    }
}

impl RepoTab {
    /// The Stash dialog.
    pub(super) fn stash_dialog(&self) -> Dialog {
        dialog(StashDialog::for_tab(self))
    }
}

// ---- a checkout that local changes block -----------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) enum CheckoutTarget {
    Branch(String),
    Pull(String),
    Remote { remote_branch: String, local: String },
}

impl CheckoutTarget {
    fn display_name(&self) -> &str {
        match self {
            CheckoutTarget::Branch(b) | CheckoutTarget::Pull(b) => b,
            CheckoutTarget::Remote { local, .. } => local,
        }
    }

    fn checkout_ops(&self) -> Vec<Vec<String>> {
        match self {
            CheckoutTarget::Branch(b) => vec![vec!["checkout".into(), b.clone()]],
            CheckoutTarget::Pull(b) => vec![
                vec!["checkout".into(), b.clone()],
                vec!["pull".into(), "--ff-only".into()],
            ],
            CheckoutTarget::Remote { remote_branch, .. } => vec![vec![
                "checkout".into(),
                "--track".into(),
                remote_branch.clone(),
            ]],
        }
    }

    fn force_checkout_ops(&self) -> Vec<Vec<String>> {
        match self {
            CheckoutTarget::Branch(b) => vec![vec!["checkout".into(), "--force".into(), b.clone()]],
            CheckoutTarget::Pull(b) => vec![
                vec!["checkout".into(), "--force".into(), b.clone()],
                vec!["pull".into(), "--ff-only".into()],
            ],
            CheckoutTarget::Remote { remote_branch, .. } => vec![vec![
                "checkout".into(),
                "--force".into(),
                "--track".into(),
                remote_branch.clone(),
            ]],
        }
    }
}

struct BlockedCheckout {
    target: CheckoutTarget,
    discard: bool,
}

fn is_blocked_checkout_error(err: &str) -> bool {
    err.contains("would be overwritten by checkout")
        || err.contains("commit your changes or stash them before you switch")
}

/// The dialog to offer when `label` (a checkout) failed with `err` because of local changes.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn blocked_checkout(label: &str, err: &str) -> Option<Dialog> {
    blocked_checkout_for(None, label, err)
}

/// Like [`blocked_checkout`], preserving structured checkout target info (branch, pull, remote).
pub(super) fn blocked_checkout_for(
    target: Option<CheckoutTarget>,
    label: &str,
    err: &str,
) -> Option<Dialog> {
    if !is_blocked_checkout_error(err) {
        return None;
    }
    let resolved = target.or_else(|| {
        if let Some(b) = label.strip_prefix("Checkout and pull ").map(str::trim)
            && !b.is_empty()
        {
            return Some(CheckoutTarget::Pull(b.to_owned()));
        }
        let b = label.strip_prefix("Checkout ")?.trim();
        (!b.is_empty()).then(|| CheckoutTarget::Branch(b.to_owned()))
    })?;
    Some(dialog(BlockedCheckout {
        target: resolved,
        discard: false,
    }))
}

/// Stash what is in the way (untracked files too), switch, bring the stash back.
#[cfg_attr(not(test), allow(dead_code))]
fn switch_with_changes(target: &str) -> Action {
    switch_with_target(&CheckoutTarget::Branch(target.to_owned()))
}

fn switch_with_target(target: &CheckoutTarget) -> Action {
    let name = target.display_name();
    let mut ops_list = vec![vec![
        "stash".into(),
        "push".into(),
        "--include-untracked".into(),
        "-m".into(),
        format!("gitcrafted: before switching to {name}"),
    ]];
    ops_list.extend(target.checkout_ops());
    ops_list.push(vec!["stash".into(), "pop".into(), "--index".into()]);
    Action::Ops {
        label: format!("Switch to {name} with your changes"),
        ops: ops_list,
        refresh: Refresh::All,
    }
}

/// Switch and lose the local changes.
#[cfg_attr(not(test), allow(dead_code))]
fn discard_and_switch(target: &str) -> Action {
    discard_and_switch_target(&CheckoutTarget::Branch(target.to_owned()))
}

fn discard_and_switch_target(target: &CheckoutTarget) -> Action {
    Action::Ops {
        label: format!("Checkout {}", target.display_name()),
        ops: target.force_checkout_ops(),
        refresh: Refresh::All,
    }
}

impl DialogBody for BlockedCheckout {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, _enter: bool) -> Outcome {
        title(
            ui,
            "Local changes are in the way",
            &format!(
                "Switching to {} would overwrite them.",
                self.target.display_name()
            ),
            t,
        );
        let mut outcome = Outcome::Open;
        if widgets::primary_button(ui, "Stash, switch and reapply", true, ui.available_width())
            .on_hover_text("Stashes your changes, checks out, then pops the stash back")
            .clicked()
        {
            outcome = Outcome::Done(switch_with_target(&self.target));
        }
        ui.add_space(8.0);
        ui.checkbox(
            &mut self.discard,
            RichText::new(
                "Throw my local changes away (untracked files in its way are replaced too)",
            )
            .color(t.danger),
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.discard,
                    egui::Button::new("Discard changes and switch"),
                )
                .clicked()
            {
                outcome = Outcome::Done(discard_and_switch_target(&self.target));
            }
            if ui.button("Cancel").clicked() {
                outcome = Outcome::Cancel;
            }
        });
        outcome
    }
}

// ---- track / untrack ------------------------------------------------------------------------

/// Sets `local`'s upstream to `upstream`, undoable back to its previous upstream (or untracked).
pub(super) fn track_action(
    local: &str,
    upstream: &str,
    prev_upstream: Option<&str>,
    prev_gone: bool,
) -> Action {
    let set_cmd = vec![
        "branch".to_owned(),
        format!("--set-upstream-to={upstream}"),
        local.to_owned(),
    ];
    let undo_cmd = match prev_upstream.filter(|p| !p.is_empty() && !prev_gone) {
        Some(prev) => vec![
            "branch".to_owned(),
            format!("--set-upstream-to={prev}"),
            local.to_owned(),
        ],
        None => vec![
            "branch".to_owned(),
            "--unset-upstream".to_owned(),
            local.to_owned(),
        ],
    };
    Action::UndoableOps {
        label: format!("Track {upstream} on {local}"),
        ops: vec![set_cmd.clone()],
        refresh: Refresh::All,
        undo: Undo::Custom {
            undo: vec![undo_cmd],
            redo: vec![set_cmd],
        },
    }
}

/// Removes upstream tracking from `local` (`--unset-upstream`), undoable unless upstream is gone.
pub(super) fn untrack_action(local: &str, prev_upstream: &str, prev_gone: bool) -> Action {
    let unset_cmd = vec![
        "branch".to_owned(),
        "--unset-upstream".to_owned(),
        local.to_owned(),
    ];
    if !prev_upstream.is_empty() && !prev_gone {
        Action::UndoableOps {
            label: format!("Untrack {local}"),
            ops: vec![unset_cmd.clone()],
            refresh: Refresh::All,
            undo: Undo::Custom {
                undo: vec![vec![
                    "branch".to_owned(),
                    format!("--set-upstream-to={prev_upstream}"),
                    local.to_owned(),
                ]],
                redo: vec![unset_cmd],
            },
        }
    } else {
        Action::Ops {
            label: format!("Untrack {local}"),
            ops: vec![unset_cmd],
            refresh: Refresh::All,
        }
    }
}

enum TrackMode {
    /// Picking a remote branch to track for `local`.
    ForLocal {
        local: String,
        current_upstream: String,
        current_gone: bool,
        remotes: Vec<String>,
    },
    /// Picking a local branch to track `remote_branch`.
    ForRemote {
        remote_branch: String,
        /// `(local_name, current_upstream, upstream_gone)`.
        locals: Vec<(String, String, bool)>,
    },
}

struct TrackBranchDialog {
    mode: TrackMode,
    query: String,
    selected: Option<usize>,
    focus: bool,
}

impl TrackBranchDialog {
    fn item_count(&self) -> usize {
        match &self.mode {
            TrackMode::ForLocal { remotes, .. } => remotes.len(),
            TrackMode::ForRemote { locals, .. } => locals.len(),
        }
    }

    fn item_name(&self, idx: usize) -> Option<&str> {
        match &self.mode {
            TrackMode::ForLocal { remotes, .. } => remotes.get(idx).map(String::as_str),
            TrackMode::ForRemote { locals, .. } => locals.get(idx).map(|(n, _, _)| n.as_str()),
        }
    }

    fn track_selected_action(&self) -> Option<Action> {
        let idx = self.selected?;
        match &self.mode {
            TrackMode::ForLocal {
                local,
                current_upstream,
                current_gone,
                remotes,
            } => {
                let remote = remotes.get(idx)?;
                if !valid_ref_name(local)
                    || !valid_ref_name(remote)
                    || (remote == current_upstream && !*current_gone)
                {
                    return None;
                }
                let prev = (!current_upstream.is_empty()).then_some(current_upstream.as_str());
                Some(track_action(local, remote, prev, *current_gone))
            }
            TrackMode::ForRemote {
                remote_branch,
                locals,
            } => {
                let (local, cur_up, cur_gone) = locals.get(idx)?;
                if !valid_ref_name(local)
                    || !valid_ref_name(remote_branch)
                    || (cur_up == remote_branch && !*cur_gone)
                {
                    return None;
                }
                let prev = (!cur_up.is_empty()).then_some(cur_up.as_str());
                Some(track_action(local, remote_branch, prev, *cur_gone))
            }
        }
    }

    fn untrack_selected_action(&self) -> Option<Action> {
        match &self.mode {
            TrackMode::ForLocal {
                local,
                current_upstream,
                current_gone,
                ..
            } => (!current_upstream.is_empty())
                .then(|| untrack_action(local, current_upstream, *current_gone)),
            TrackMode::ForRemote {
                remote_branch,
                locals,
            } => {
                let idx = self.selected?;
                let (local, cur_up, cur_gone) = locals.get(idx)?;
                (cur_up == remote_branch).then(|| untrack_action(local, cur_up, *cur_gone))
            }
        }
    }
}

impl DialogBody for TrackBranchDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        match &self.mode {
            TrackMode::ForLocal {
                local,
                current_upstream,
                current_gone,
                ..
            } => {
                let sub = if current_upstream.is_empty() {
                    format!("Choose a remote branch for “{local}” to track.")
                } else if *current_gone {
                    format!(
                        "Currently tracks {current_upstream} (deleted on remote). Pick a replacement or untrack."
                    )
                } else {
                    format!("Currently tracks {current_upstream}.")
                };
                title(ui, "Track remote branch", &sub, t);
            }
            TrackMode::ForRemote { remote_branch, .. } => {
                title(
                    ui,
                    "Track with local branch",
                    &format!("Choose a local branch to track “{remote_branch}”."),
                    t,
                );
            }
        }

        let hint = match &self.mode {
            TrackMode::ForLocal { .. } => "Search remote branches…",
            TrackMode::ForRemote { .. } => "Search local branches…",
        };
        ui.horizontal(|ui| {
            let clear_w = if self.query.is_empty() { 0.0 } else { 26.0 };
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(hint)
                    .desired_width((ui.available_width() - clear_w).max(80.0))
                    .margin(egui::Margin::symmetric(8, 5)),
            );
            if self.focus {
                edit.request_focus();
                self.focus = false;
            }
            if !self.query.is_empty() && ui.small_button("×").clicked() {
                self.query.clear();
                edit.request_focus();
            }
        });
        ui.add_space(6.0);

        let total = self.item_count();
        let filtered: Vec<usize> = (0..total)
            .filter(|&i| {
                self.item_name(i)
                    .is_some_and(|name| matches_branch_filter(name, &self.query))
            })
            .collect();

        let mut moved = false;
        if filtered.is_empty() {
            self.selected = None;
        } else {
            let pos = self
                .selected
                .and_then(|sel| filtered.iter().position(|&i| i == sel))
                .unwrap_or(0);
            let (down, up) = ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp)));
            moved = down || up;
            let next_pos = if down {
                (pos + 1).min(filtered.len() - 1)
            } else if up {
                pos.saturating_sub(1)
            } else {
                pos
            };
            self.selected = Some(filtered[next_pos]);
        }

        let mut outcome = Outcome::Open;
        if total == 0 {
            let msg = match &self.mode {
                TrackMode::ForLocal { .. } => "No remote branches available",
                TrackMode::ForRemote { .. } => "No local branches available",
            };
            ui.label(RichText::new(msg).color(t.text_faint));
        } else if filtered.is_empty() {
            ui.label(RichText::new("No matching branches").color(t.text_faint));
        } else {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("track-branch-rows")
                    .max_height(240.0)
                    .auto_shrink([false, true])
                    .show_rows(ui, 24.0, filtered.len(), |ui, range| {
                        for row_idx in range {
                            let Some(&orig_idx) = filtered.get(row_idx) else {
                                continue;
                            };
                            let Some(name) = self.item_name(orig_idx) else {
                                continue;
                            };
                            let name = name.to_owned();
                            let tag = match &self.mode {
                                TrackMode::ForLocal {
                                    current_upstream,
                                    current_gone,
                                    ..
                                } => (name == *current_upstream && !*current_gone)
                                    .then(|| ("current".to_owned(), t.accent)),
                                TrackMode::ForRemote {
                                    remote_branch,
                                    locals,
                                } => locals.get(orig_idx).and_then(|(_, cur_up, cur_gone)| {
                                    if cur_up == remote_branch && !*cur_gone {
                                        Some(("tracks this".to_owned(), t.accent))
                                    } else if !cur_up.is_empty() {
                                        let label = if *cur_gone {
                                            format!("{cur_up} (gone)")
                                        } else {
                                            cur_up.clone()
                                        };
                                        Some((label, t.text_faint))
                                    } else {
                                        None
                                    }
                                }),
                            };
                            let is_selected = self.selected == Some(orig_idx);
                            let (rect, resp) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 24.0),
                                Sense::click(),
                            );
                            resp.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::SelectableLabel,
                                    true,
                                    &name,
                                )
                            });
                            let p = ui.painter_at(rect);
                            if is_selected {
                                p.rect_filled(rect, 3.0, t.row_selected);
                            } else if resp.hovered() {
                                p.rect_filled(rect, 3.0, t.hover);
                            }
                            let y = rect.center().y;
                            let mut text_right = rect.right() - 8.0;
                            if let Some((tag_text, tag_color)) = &tag {
                                let r = p.text(
                                    pos2(text_right, y),
                                    Align2::RIGHT_CENTER,
                                    tag_text,
                                    theme::medium(11.0),
                                    *tag_color,
                                );
                                text_right = (r.left() - 8.0).max(rect.left() + 8.0);
                            }
                            p.with_clip_rect(Rect::from_min_max(
                                pos2(rect.left() + 8.0, rect.top()),
                                pos2(text_right, rect.bottom()),
                            ))
                            .text(
                                pos2(rect.left() + 8.0, y),
                                Align2::LEFT_CENTER,
                                &name,
                                theme::mono(12.0),
                                t.text,
                            );
                            let tip = match &tag {
                                Some((tag_text, _)) => format!("{name}\n{tag_text}"),
                                None => name.clone(),
                            };
                            let resp = resp.on_hover_text(tip);
                            if moved && is_selected {
                                resp.scroll_to_me(None);
                            }
                            if resp.clicked() {
                                self.selected = Some(orig_idx);
                            }
                            if resp.double_clicked() {
                                self.selected = Some(orig_idx);
                                if let Some(act) = self.track_selected_action() {
                                    outcome = Outcome::Done(act);
                                }
                            }
                        }
                    });
            });
        }

        ui.add_space(10.0);
        let track_act = self.track_selected_action();
        let untrack_act = self.untrack_selected_action();
        ui.horizontal_wrapped(|ui| {
            let clicked_track =
                widgets::primary_button(ui, "Track", track_act.is_some(), 90.0).clicked();
            if (clicked_track || enter)
                && let Some(act) = track_act
            {
                outcome = Outcome::Done(act);
            }
            if let Some(act) = untrack_act
                && ui
                    .add(egui::Button::new("Untrack").min_size(vec2(80.0, 30.0)))
                    .clicked()
            {
                outcome = Outcome::Done(act);
            }
            if ui
                .add(egui::Button::new("Cancel").min_size(vec2(80.0, 30.0)))
                .clicked()
            {
                outcome = Outcome::Cancel;
            }
        });
        outcome
    }
}

impl RepoTab {
    /// Opens a searchable dialog picking which remote branch `b` should track (or untrack).
    pub(super) fn track_remote_dialog(&self, b: &Branch) -> Dialog {
        let mut remotes: Vec<String> = self
            .refs
            .remotes
            .iter()
            .flat_map(|(_, list)| list.iter())
            .filter(|r| !r.name.ends_with("/HEAD"))
            .map(|r| r.name.clone())
            .collect();
        remotes.sort();
        remotes.dedup();
        let same_name = format!("/{}", b.name);
        let selected = (!b.upstream.is_empty() && !b.upstream_gone())
            .then(|| remotes.iter().position(|r| r == &b.upstream))
            .flatten()
            .or_else(|| remotes.iter().position(|r| r.ends_with(&same_name)))
            .or_else(|| (!remotes.is_empty()).then_some(0));
        dialog(TrackBranchDialog {
            mode: TrackMode::ForLocal {
                local: b.name.clone(),
                current_upstream: b.upstream.clone(),
                current_gone: b.upstream_gone(),
                remotes,
            },
            query: String::new(),
            selected,
            focus: true,
        })
    }

    /// Opens a searchable dialog picking which local branch should track `remote_branch`.
    pub(super) fn track_local_dialog(&self, remote_branch: &str) -> Dialog {
        let locals: Vec<(String, String, bool)> = self
            .refs
            .local
            .iter()
            .map(|b| (b.name.clone(), b.upstream.clone(), b.upstream_gone()))
            .collect();
        let short = remote_branch
            .split_once('/')
            .map_or(remote_branch, |(_, s)| s);
        let selected = self
            .refs
            .current
            .as_deref()
            .and_then(|cur| locals.iter().position(|(n, _, _)| n == cur))
            .or_else(|| locals.iter().position(|(n, _, _)| n == short))
            .or_else(|| (!locals.is_empty()).then_some(0));
        dialog(TrackBranchDialog {
            mode: TrackMode::ForRemote {
                remote_branch: remote_branch.to_owned(),
                locals,
            },
            query: String::new(),
            selected,
            focus: true,
        })
    }

    /// Context-menu items to track or untrack a remote branch from local branches.
    pub(super) fn remote_branch_track_menu_items(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        remote_branch: &str,
        current: Option<&str>,
        idle: bool,
    ) {
        if remote_branch.ends_with("/HEAD") {
            return;
        }
        if let Some(cur) = current
            && let Some(cur_branch) = self.refs.local.iter().find(|b| b.name == cur)
        {
            if cur_branch.upstream == remote_branch && !cur_branch.upstream_gone() {
                if ui
                    .add_enabled(idle, egui::Button::new(format!("Untrack from {cur}")))
                    .clicked()
                {
                    acts.push(untrack_action(cur, &cur_branch.upstream, false));
                }
            } else if ui
                .add_enabled(idle, egui::Button::new(format!("Track with {cur}")))
                .clicked()
            {
                let prev = (!cur_branch.upstream.is_empty()).then_some(cur_branch.upstream.as_str());
                acts.push(track_action(
                    cur,
                    remote_branch,
                    prev,
                    cur_branch.upstream_gone(),
                ));
            }
        }
        let track_local_btn = ui.add_enabled(
            idle && !self.refs.local.is_empty(),
            egui::Button::new("Track with local branch…"),
        );
        let track_local_btn = if self.refs.local.is_empty() {
            track_local_btn.on_disabled_hover_text("No local branches")
        } else {
            track_local_btn
        };
        if track_local_btn.clicked() {
            acts.push(Action::Dialog(self.track_local_dialog(remote_branch)));
        }
        let others: Vec<&Branch> = self
            .refs
            .local
            .iter()
            .filter(|b| {
                Some(b.name.as_str()) != current && b.upstream == remote_branch && !b.upstream_gone()
            })
            .collect();
        if others.len() == 1 {
            let b = others[0];
            if ui
                .add_enabled(idle, egui::Button::new(format!("Untrack from {}", b.name)))
                .clicked()
            {
                acts.push(untrack_action(&b.name, &b.upstream, false));
            }
        } else if others.len() > 1 {
            ui.add_enabled_ui(idle, |ui| {
                ui.menu_button("Untrack from", |ui| {
                    for b in &others {
                        if ui.button(&b.name).clicked() {
                            acts.push(untrack_action(&b.name, &b.upstream, false));
                        }
                    }
                });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_args_follow_the_options() {
        let mut m = MergeDialog {
            branch: "feat".into(),
            current: "main".into(),
            ff: FastForward::Never,
            squash: false,
            no_commit: false,
            message: String::new(),
        };
        assert_eq!(m.args(), ["merge", "--no-ff", "--no-edit", "feat"]);
        m.message = " Merge feat ".into();
        assert_eq!(m.args(), ["merge", "--no-ff", "-m", "Merge feat", "feat"]);
        m.squash = true;
        assert_eq!(m.args(), ["merge", "--squash", "--no-edit", "feat"]);
        m.squash = false;
        m.ff = FastForward::Only;
        m.no_commit = true;
        assert_eq!(
            m.args(),
            ["merge", "--no-ff", "--no-commit", "--no-edit", "feat"],
            "never fast-forwarded when nothing is to be committed"
        );
    }

    #[test]
    fn stash_args_pick_files_only_when_asked() {
        let mut s = StashDialog {
            message: "wip".into(),
            untracked: true,
            keep_index: true,
            selected: vec!["a.txt".into(), "old.txt".into()],
            only_selected: false,
            focus: false,
        };
        assert_eq!(
            s.args(),
            [
                "stash",
                "push",
                "--include-untracked",
                "--keep-index",
                "-m",
                "wip"
            ]
        );
        s.only_selected = true;
        assert_eq!(
            s.args().last().map(String::as_str),
            Some(":(literal)old.txt")
        );
        assert!(s.args().contains(&"--".to_owned()));
    }

    #[test]
    fn only_local_change_errors_offer_the_dialog() {
        let err = "error: Your local changes to the following files would be overwritten by checkout:\n\ta.txt";
        assert!(blocked_checkout("Checkout feat", err).is_some());
        assert!(blocked_checkout("Checkout feat", "fatal: unrelated").is_none());
        assert!(blocked_checkout("Merge x into y", err).is_none());
    }

    /// Against real git: every merge option combination is accepted, and squash leaves HEAD.
    #[test]
    fn merge_options_against_real_git() {
        use std::path::Path;
        use std::process::Command;
        let git = |dir: &Path, args: &[String]| -> bool {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .env("GIT_EDITOR", "true")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "core.autocrlf=false",
                ])
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        let g = |dir: &Path, args: &[&str]| {
            git(
                dir,
                &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            )
        };
        let base = std::env::temp_dir().join(format!("gitcrafted-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).ok();
        assert!(g(&base, &["init", "-q", "-b", "main"]));
        assert!(g(&base, &["commit", "-q", "--allow-empty", "-m", "base"]));
        for (name, file) in [("one", "1.txt"), ("two", "2.txt"), ("three", "3.txt")] {
            assert!(g(&base, &["checkout", "-q", "-b", name, "main"]));
            std::fs::write(base.join(file), name).ok();
            assert!(g(&base, &["add", "."]));
            assert!(g(&base, &["commit", "-q", "-m", name]));
        }
        assert!(g(&base, &["checkout", "-q", "main"]));
        let head = |dir: &Path| {
            String::from_utf8_lossy(
                &Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args(["rev-parse", "HEAD"])
                    .output()
                    .map(|o| o.stdout)
                    .unwrap_or_default(),
            )
            .trim()
            .to_owned()
        };
        let mk = |branch: &str, ff, squash, no_commit, msg: &str| MergeDialog {
            branch: branch.into(),
            current: "main".into(),
            ff,
            squash,
            no_commit,
            message: msg.into(),
        };
        // A merge commit with a message.
        assert!(git(
            &base,
            &mk("one", FastForward::Never, false, false, "joined one").args()
        ));
        assert_eq!(
            String::from_utf8_lossy(
                &Command::new("git")
                    .arg("-C")
                    .arg(&base)
                    .args(["log", "-1", "--format=%s"])
                    .output()
                    .map(|o| o.stdout)
                    .unwrap_or_default()
            )
            .trim(),
            "joined one"
        );
        // A squash changes the tree but not HEAD.
        let before = head(&base);
        assert!(git(
            &base,
            &mk("two", FastForward::Default, true, false, "").args()
        ));
        assert_eq!(head(&base), before);
        assert!(base.join("2.txt").exists());
        assert!(g(&base, &["reset", "-q", "--hard"]));
        // Fast-forward only refuses a diverged branch.
        assert!(!git(
            &base,
            &mk("three", FastForward::Only, false, false, "").args()
        ));
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::test_support::*;
    use crate::repo::{InProgress, abort_action};

    fn commit(dir: &std::path::Path, name: &str, text: &str, message: &str) {
        write(dir, name, text);
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    fn merge_dialog(branch: &str) -> MergeDialog {
        MergeDialog {
            branch: branch.to_owned(),
            current: "main".to_owned(),
            ff: FastForward::Default,
            squash: false,
            no_commit: false,
            message: String::new(),
        }
    }

    /// `main` and `feature` each with a commit of their own, on main.
    fn diverged(name: &str) -> std::path::PathBuf {
        let dir = repo(name);
        git(&dir, &["checkout", "-q", "-b", "feature"]);
        commit(&dir, "f.txt", "feature\n", "feature work");
        git(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "m.txt", "main\n", "main work");
        dir
    }

    fn parents(dir: &std::path::Path) -> usize {
        git(dir, &["rev-list", "--parents", "-1", "HEAD"])
            .split(' ')
            .count()
            - 1
    }

    #[test]
    fn a_merge_commit_with_a_message_is_undone_by_undo() {
        let dir = diverged("bo-merge");
        let mut f = Fixture::open(&dir);
        let before = git(&dir, &["rev-parse", "HEAD"]);
        let mut d = merge_dialog("feature");
        d.message = "joined feature".to_owned();
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "joined feature");
        assert_eq!(parents(&dir), 2);
        f.run(Action::Undo);
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn squash_stages_the_changes_and_leaves_head_alone() {
        let dir = diverged("bo-squash");
        let mut f = Fixture::open(&dir);
        let before = git(&dir, &["rev-parse", "HEAD"]);
        let mut d = merge_dialog("feature");
        d.squash = true;
        d.ff = FastForward::Only; // ignored with --squash
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), before);
        assert!(f.tab.work.staged.iter().any(|x| x.path == "f.txt"));
        assert!(
            f.tab.undo_stack.is_empty(),
            "nothing moved, nothing to undo"
        );
    }

    #[test]
    fn no_commit_leaves_a_merge_to_finish_or_abort() {
        let dir = diverged("bo-nocommit");
        let mut f = Fixture::open(&dir);
        let mut d = merge_dialog("feature");
        d.no_commit = true;
        d.ff = FastForward::Never;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(f.tab.work.in_progress, Some(InProgress::Merge));
        f.run(abort_action(InProgress::Merge));
        assert_eq!(f.tab.work.in_progress, None);
        assert!(!dir.join("f.txt").exists());
    }

    #[test]
    fn fast_forward_options() {
        let dir = repo("bo-ff");
        git(&dir, &["checkout", "-q", "-b", "feature"]);
        commit(&dir, "f.txt", "feature\n", "feature work");
        git(&dir, &["checkout", "-q", "main"]);
        let mut f = Fixture::open(&dir);

        // Always a merge commit, even though main could fast-forward.
        let mut d = merge_dialog("feature");
        d.ff = FastForward::Never;
        f.run(d.action());
        assert_eq!(parents(&dir), 2);
        f.run(Action::Undo);
        assert_eq!(parents(&dir), 0, "back on the root commit");

        // Fast-forward only works here, and the default does too.
        d.ff = FastForward::Only;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&dir, &["rev-parse", "HEAD"]),
            git(&dir, &["rev-parse", "feature"])
        );

        // On a diverged pair it refuses.
        let diverged = diverged("bo-ff-diverged");
        let mut g = Fixture::open(&diverged);
        let mut d = merge_dialog("feature");
        d.ff = FastForward::Only;
        g.run(d.action());
        assert!(g.failed());
        assert_eq!(parents(&diverged), 1);
    }

    /// a.txt (nine lines), b.txt and c.txt tracked; b staged, a edited, plus an untracked file.
    fn dirty(name: &str) -> std::path::PathBuf {
        let dir = repo(name);
        write(&dir, "a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\n");
        write(&dir, "b.txt", "b\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "files"]);
        write(&dir, "a.txt", "1\n2\n3\n4\n5\n6\n7\n8\nnine\n");
        write(&dir, "b.txt", "b changed\n");
        git(&dir, &["add", "b.txt"]);
        write(&dir, "u.txt", "untracked\n");
        dir
    }

    fn stash_names(dir: &std::path::Path) -> String {
        git(
            dir,
            &[
                "stash",
                "show",
                "--include-untracked",
                "--name-only",
                "stash@{0}",
            ],
        )
    }

    #[test]
    fn stashing_everything_takes_untracked_files_too() {
        let dir = dirty("bo-stash-all");
        let mut f = Fixture::open(&dir);
        let mut d = StashDialog::for_tab(&f.tab);
        assert!(d.selected.is_empty() && !d.only_selected && d.untracked);
        d.message = "everything".to_owned();
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert!(git(&dir, &["status", "--porcelain"]).is_empty());
        let names = stash_names(&dir);
        assert!(
            names.contains("a.txt") && names.contains("b.txt") && names.contains("u.txt"),
            "{names}"
        );
        assert!(git(&dir, &["stash", "list"]).contains("everything"));
        // Undo pops it back, staged state included.
        f.run(Action::Undo);
        assert!(git(&dir, &["stash", "list"]).is_empty());
        assert!(dir.join("u.txt").exists());
        assert_eq!(git(&dir, &["diff", "--cached", "--name-only"]), "b.txt");
    }

    #[test]
    fn stashing_only_the_selected_files_leaves_the_rest() {
        let dir = dirty("bo-stash-some");
        let mut f = Fixture::open(&dir);
        let a = f
            .tab
            .work
            .unstaged
            .iter()
            .find(|x| x.path == "a.txt")
            .cloned();
        assert!(a.is_some());
        f.tab.patch_action(
            crate::repo::patch::PatchAction::Toggle(
                a.unwrap_or_else(|| unreachable!("a.txt is unstaged")),
            ),
            &f.ctx,
        );
        let mut d = StashDialog::for_tab(&f.tab);
        assert_eq!(d.selected, ["a.txt"]);
        assert!(d.only_selected);
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert!(stash_names(&dir).contains("a.txt"));
        assert_eq!(read(&dir, "a.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n9\n");
        // The staged edit and the untracked file were not touched.
        assert_eq!(git(&dir, &["diff", "--cached", "--name-only"]), "b.txt");
        assert_eq!(read(&dir, "b.txt"), "b changed\n");
        assert!(dir.join("u.txt").exists());

        // Ticking "only selected" off stashes the rest too.
        d.only_selected = false;
        f.reload();
        write(&dir, "a.txt", "again\n");
        f.run(d.action());
        assert!(
            git(&dir, &["status", "--porcelain"]).is_empty(),
            "{}",
            git(&dir, &["status", "--porcelain"])
        );
    }

    #[test]
    fn keep_index_leaves_staged_changes_in_place() {
        let dir = dirty("bo-stash-index");
        let mut f = Fixture::open(&dir);
        let mut d = StashDialog::for_tab(&f.tab);
        d.keep_index = true;
        d.untracked = false;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["diff", "--cached", "--name-only"]), "b.txt");
        assert_eq!(read(&dir, "a.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n9\n");
        assert!(
            dir.join("u.txt").exists(),
            "untracked files stay when not included"
        );
    }

    #[test]
    fn a_blocked_checkout_offers_stash_switch_and_reapply() {
        let dir = repo("bo-blocked");
        write(&dir, "a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "nine"]);
        git(&dir, &["checkout", "-q", "-b", "other"]);
        commit(&dir, "a.txt", "ONE\n2\n3\n4\n5\n6\n7\n8\n9\n", "top line");
        git(&dir, &["checkout", "-q", "main"]);
        write(&dir, "a.txt", "1\n2\n3\n4\n5\n6\n7\n8\nnine\n");
        write(&dir, "u.txt", "untracked\n");
        let mut f = Fixture::open(&dir);

        // Git refuses, and the dialog opens.
        f.run(Action::Checkout("other".to_owned()));
        assert!(f.failed());
        assert!(f.tab.dialog.is_some(), "the prompt must open");
        assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");

        // Stash, switch, reapply: both edits meet on the other branch.
        f.tab.dialog = None;
        f.run(switch_with_changes("other"));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "other");
        assert_eq!(read(&dir, "a.txt"), "ONE\n2\n3\n4\n5\n6\n7\n8\nnine\n");
        assert!(dir.join("u.txt").exists());
        assert!(git(&dir, &["stash", "list"]).is_empty());
    }

    #[test]
    fn discarding_switches_and_loses_the_edit() {
        let dir = repo("bo-discard");
        git(&dir, &["checkout", "-q", "-b", "other"]);
        commit(&dir, "a.txt", "other version\n", "other");
        git(&dir, &["checkout", "-q", "main"]);
        write(&dir, "a.txt", "my edit\n");
        let mut f = Fixture::open(&dir);
        f.run(discard_and_switch("other"));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "other");
        assert_eq!(read(&dir, "a.txt"), "other version\n");
    }

    #[test]
    fn other_checkout_failures_do_not_open_the_prompt() {
        let dir = repo("bo-other-fail");
        let mut f = Fixture::open(&dir);
        f.run(Action::Checkout("no-such-branch".to_owned()));
        assert!(f.failed());
        assert!(f.tab.dialog.is_none());
    }
}

/// Data-safety tests: stashing, merging and switching must never lose work.
#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::repo::patch::PatchAction;
    use crate::repo::test_support::*;
    use crate::repo::{InProgress, abort_action};

    fn select(f: &mut Fixture, path: &str) {
        let file = f
            .tab
            .work
            .unstaged
            .iter()
            .chain(&f.tab.work.staged)
            .find(|x| x.path == path)
            .cloned();
        assert!(file.is_some(), "{path} is listed");
        if let Some(file) = file {
            f.tab.patch_action(PatchAction::Toggle(file), &f.ctx);
        }
    }

    // A file called `*.txt` can't exist on Windows.
    #[cfg(unix)]
    #[test]
    fn stashing_a_selected_file_named_like_a_pattern_takes_only_that_file() {
        let dir = repo("sf-stash-glob");
        for name in ["*.txt", "b.txt", "[ab].txt"] {
            commit_file(&dir, name, "committed\n", name);
        }
        for name in ["*.txt", "a.txt", "b.txt", "[ab].txt"] {
            write(&dir, name, "edited\n");
        }
        let mut f = Fixture::open(&dir);
        select(&mut f, "*.txt");
        select(&mut f, "[ab].txt");
        let before = Snapshot::take(&dir);
        f.run(StashDialog::for_tab(&f.tab).action());
        assert!(!f.failed(), "{:?}", f.status());
        let mut lost = before.lost_in(&Snapshot::take(&dir));
        lost.sort();
        assert_eq!(
            lost,
            ["*.txt", "[ab].txt"],
            "only the two selected files were stashed"
        );
        assert_eq!(read(&dir, "a.txt"), "edited\n");
        assert_eq!(read(&dir, "b.txt"), "edited\n");
        f.run(Action::Undo);
        assert_same(
            &before,
            &Snapshot::take(&dir),
            "undo brings back exactly what was there",
        );
    }

    /// a.txt edited, b.txt edited and staged, u.txt untracked, i.log ignored.
    fn busy(name: &str) -> std::path::PathBuf {
        let dir = repo(name);
        commit_file(&dir, ".gitignore", "*.log\n", "ignore");
        commit_file(&dir, "b.txt", "b\n", "b");
        write(&dir, "a.txt", "edited\n");
        write(&dir, "b.txt", "staged edit\n");
        git(&dir, &["add", "b.txt"]);
        write(&dir, "u.txt", "untracked\n");
        write(&dir, "i.log", "ignored\n");
        dir
    }

    #[test]
    fn every_stash_option_round_trips_exactly_through_undo() {
        let combos = [
            (true, false, false),
            (false, false, false),
            (true, true, false),
            (false, true, false),
            (true, false, true),
            (false, true, true),
        ];
        for (i, (untracked, keep_index, only_selected)) in combos.into_iter().enumerate() {
            let what = format!(
                "untracked {untracked}, keep index {keep_index}, only selected {only_selected}"
            );
            let dir = busy(&format!("sf-stash-trip-{i}"));
            let mut f = Fixture::open(&dir);
            if only_selected {
                select(&mut f, "a.txt");
            }
            let before = Snapshot::take(&dir);
            let mut d = StashDialog::for_tab(&f.tab);
            d.untracked = untracked;
            d.keep_index = keep_index;
            d.message = "round trip".to_owned();
            f.run(d.action());
            assert!(!f.failed(), "{what}: {:?}", f.status());
            assert_ne!(
                Snapshot::take(&dir),
                before,
                "{what}: something was stashed"
            );
            assert_eq!(
                read(&dir, "i.log"),
                "ignored\n",
                "{what}: ignored files are never stashed"
            );
            f.run(Action::Undo);
            assert!(!f.failed(), "{what}: undo {:?}", f.status());
            assert_same(&before, &Snapshot::take(&dir), &what);
        }
    }

    #[test]
    fn a_conflicting_reapply_keeps_the_stash_and_the_edit() {
        let dir = repo("sf-reapply-conflict");
        git(&dir, &["checkout", "-q", "-b", "other"]);
        commit_file(&dir, "a.txt", "theirs\n", "theirs");
        git(&dir, &["checkout", "-q", "main"]);
        write(&dir, "a.txt", "mine\n");
        write(&dir, "u.txt", "untracked\n");
        let mut f = Fixture::open(&dir);
        f.run(Action::Checkout("other".to_owned()));
        assert!(f.tab.dialog.is_some());

        f.run(switch_with_changes("other"));
        assert!(f.failed(), "the reapply conflicts");
        // Nothing is lost: the stash is still there with the edit, and the file shows both sides.
        assert_eq!(git(&dir, &["stash", "list"]).lines().count(), 1);
        assert_eq!(git(&dir, &["show", "stash@{0}:a.txt"]), "mine");
        let text = read(&dir, "a.txt");
        assert!(text.contains("mine") && text.contains("theirs"), "{text}");
        assert_eq!(read(&dir, "u.txt"), "untracked\n");
    }

    #[test]
    fn discarding_to_switch_keeps_untracked_files_that_are_not_in_the_way() {
        let dir = repo("sf-discard-untracked");
        git(&dir, &["checkout", "-q", "-b", "other"]);
        commit_file(&dir, "a.txt", "theirs\n", "theirs");
        git(&dir, &["checkout", "-q", "main"]);
        write(&dir, "a.txt", "mine\n");
        write(&dir, "notes.txt", "my notes\n");
        let mut f = Fixture::open(&dir);
        f.run(discard_and_switch("other"));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(read(&dir, "notes.txt"), "my notes\n");
        assert_eq!(
            read(&dir, "a.txt"),
            "theirs\n",
            "the tracked edit is what was thrown away"
        );
    }

    #[test]
    fn aborting_a_conflicted_merge_restores_everything_including_unrelated_edits() {
        let dir = repo("sf-merge-abort");
        commit_file(&dir, "b.txt", "b\n", "b");
        git(&dir, &["checkout", "-q", "-b", "feature"]);
        commit_file(&dir, "a.txt", "feature\n", "feature");
        git(&dir, &["checkout", "-q", "main"]);
        commit_file(&dir, "a.txt", "main\n", "main");
        write(&dir, "b.txt", "unrelated edit\n");
        write(&dir, "u.txt", "untracked\n");
        let mut f = Fixture::open(&dir);
        let before = Snapshot::take(&dir);
        let d = MergeDialog {
            branch: "feature".into(),
            current: "main".into(),
            ff: FastForward::Default,
            squash: false,
            no_commit: false,
            message: String::new(),
        };
        f.run(d.action());
        assert!(f.failed());
        assert_eq!(f.tab.work.in_progress, Some(InProgress::Merge));
        f.run(abort_action(InProgress::Merge));
        assert_same(&before, &Snapshot::take(&dir), "merge, then abort");
    }

    #[test]
    fn undoing_a_merge_never_overwrites_edits_made_after_it() {
        let dir = repo("sf-merge-undo");
        git(&dir, &["checkout", "-q", "-b", "feature"]);
        commit_file(&dir, "f.txt", "feature\n", "feature");
        git(&dir, &["checkout", "-q", "main"]);
        commit_file(&dir, "m.txt", "main\n", "main");
        let mut f = Fixture::open(&dir);
        let d = MergeDialog {
            branch: "feature".into(),
            current: "main".into(),
            ff: FastForward::Default,
            squash: false,
            no_commit: false,
            message: String::new(),
        };
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        write(&dir, "f.txt", "edited after the merge\n");
        f.run(Action::Undo);
        assert!(f.failed(), "undo refuses rather than overwrite the edit");
        assert_eq!(read(&dir, "f.txt"), "edited after the merge\n");
    }

    // A file called `*.txt` can't exist on Windows.
    #[cfg(unix)]
    #[test]
    fn per_file_discard_and_delete_touch_only_that_file() {
        let dir = repo("sf-discard-glob");
        commit_file(&dir, "*.txt", "committed\n", "star");
        write(&dir, "*.txt", "edited\n");
        write(&dir, "a.txt", "edited\n");
        write(&dir, "*.md", "new\n");
        write(&dir, "notes.md", "my notes, never committed\n");
        let mut f = Fixture::open(&dir);
        for name in ["*.txt", "*.md"] {
            let file = f.tab.work.unstaged.iter().find(|x| x.path == name).cloned();
            assert!(file.is_some(), "{name}");
            let Some(file) = file else { continue };
            f.run(confirmed(Action::Dialog(
                crate::repo::detail::discard_dialog(&file),
            )));
            assert!(!f.failed(), "{name}: {:?}", f.status());
        }
        assert_eq!(read(&dir, "*.txt"), "committed\n");
        assert!(!dir.join("*.md").exists());
        assert_eq!(
            read(&dir, "a.txt"),
            "edited\n",
            "a pattern-looking name is not a pattern"
        );
        assert_eq!(read(&dir, "notes.md"), "my notes, never committed\n");
    }
}

/// Regression tests (review round 2).
#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::repo::patch::PatchAction;
    use crate::repo::test_support::*;
    use crate::repo::{InProgress, abort_action};

    #[test]
    fn merge_without_committing_never_moves_the_branch_even_when_it_could_fast_forward() {
        let dir = repo("rv-nocommit-ff");
        git(&dir, &["checkout", "-q", "-b", "ahead"]);
        commit_file(&dir, "f.txt", "ahead\n", "ahead");
        git(&dir, &["checkout", "-q", "main"]);
        let before = git(&dir, &["rev-parse", "HEAD"]);
        let mut f = Fixture::open(&dir);
        let d = MergeDialog {
            branch: "ahead".into(),
            current: "main".into(),
            ff: FastForward::Only,
            squash: false,
            no_commit: true,
            message: String::new(),
        };
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&dir, &["rev-parse", "HEAD"]),
            before,
            "the branch didn't move"
        );
        assert_eq!(
            f.tab.work.in_progress,
            Some(InProgress::Merge),
            "a merge waits to be committed"
        );
        assert!(f.tab.work.staged.iter().any(|x| x.path == "f.txt"));
        f.run(abort_action(InProgress::Merge));
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), before);
        assert!(!dir.join("f.txt").exists());
    }

    #[test]
    fn a_committed_file_leaves_the_selection_and_the_stash_dialog() {
        let dir = repo("rv-stale-selection");
        write(&dir, "a.txt", "edited\n");
        write(&dir, "b.txt", "new\n");
        let mut f = Fixture::open(&dir);
        let a = f
            .tab
            .work
            .unstaged
            .iter()
            .find(|x| x.path == "a.txt")
            .cloned();
        if let Some(a) = a.clone() {
            f.tab.patch_action(PatchAction::Toggle(a), &f.ctx);
        }
        assert_eq!(StashDialog::for_tab(&f.tab).selected, ["a.txt"]);

        git(&dir, &["commit", "-q", "-am", "commit a"]);
        f.reload();
        assert!(f.tab.patch_sel.is_empty());
        let d = StashDialog::for_tab(&f.tab);
        assert!(
            d.selected.is_empty() && !d.only_selected,
            "no longer offered"
        );
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());

        // Edited again later: not selected again by itself.
        write(&dir, "a.txt", "edited again\n");
        f.reload();
        let again = f
            .tab
            .work
            .unstaged
            .iter()
            .find(|x| x.path == "a.txt")
            .cloned();
        assert!(again.is_some_and(|x| !f.tab.is_patch_selected(&x)));
    }

    #[test]
    fn a_status_refresh_alone_prunes_the_selection_too() {
        let dir = repo("rv-prune-status");
        write(&dir, "a.txt", "edited\n");
        let mut f = Fixture::open(&dir);
        let a = f.tab.work.unstaged.first().cloned();
        if let Some(a) = a {
            f.tab.patch_action(PatchAction::Toggle(a), &f.ctx);
        }
        git(&dir, &["checkout", "--", "a.txt"]);
        f.tab.reload_status(&f.ctx);
        f.pump_until(|f| f.tab.patch_sel.is_empty());
        assert!(f.tab.patch_sel.is_empty());
    }

    #[test]
    fn blocked_checkout_preserves_pull_and_remote_targets() {
        assert!(blocked_checkout_for(
            Some(CheckoutTarget::Pull("main".into())),
            "anything",
            "would be overwritten by checkout"
        )
        .is_some());
        let pull = CheckoutTarget::Pull("main".into());
        assert_eq!(pull.checkout_ops(), ops(&[&["checkout", "main"], &["pull", "--ff-only"]]));
        let rem = CheckoutTarget::Remote {
            local: "feat".into(),
            remote_branch: "origin/feat".into(),
        };
        assert_eq!(
            rem.checkout_ops(),
            ops(&[&["checkout", "--track", "origin/feat"]])
        );
        assert_eq!(
            rem.force_checkout_ops(),
            ops(&[&["checkout", "--force", "--track", "origin/feat"]])
        );
    }

    #[test]
    fn track_and_untrack_branch_round_trip_with_undo() {
        let (work, _bare) = repo_with_remote("rv-track-untrack");
        git(&work, &["push", "-q", "origin", "HEAD:refs/heads/topic-a"]);
        git(&work, &["push", "-q", "origin", "HEAD:refs/heads/topic-b"]);
        git(&work, &["fetch", "-q", "--prune"]);
        git(&work, &["branch", "feat"]);

        let mut f = Fixture::open(&work);
        let upstream_of = |f: &Fixture, name: &str| {
            f.tab
                .refs
                .local
                .iter()
                .find(|b| b.name == name)
                .map(|b| (b.upstream.clone(), b.upstream_gone()))
        };
        assert_eq!(upstream_of(&f, "feat"), Some((String::new(), false)));

        // Track an untracked branch and verify undo / redo.
        f.run(track_action("feat", "origin/topic-a", None, false));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            upstream_of(&f, "feat"),
            Some(("origin/topic-a".to_owned(), false))
        );
        f.run(Action::Undo);
        assert_eq!(upstream_of(&f, "feat"), Some((String::new(), false)));
        f.run(Action::Redo);
        assert_eq!(
            upstream_of(&f, "feat"),
            Some(("origin/topic-a".to_owned(), false))
        );

        // Switch tracked upstream and undo back to previous remote branch.
        f.run(track_action(
            "feat",
            "origin/topic-b",
            Some("origin/topic-a"),
            false,
        ));
        assert_eq!(
            upstream_of(&f, "feat"),
            Some(("origin/topic-b".to_owned(), false))
        );
        f.run(Action::Undo);
        assert_eq!(
            upstream_of(&f, "feat"),
            Some(("origin/topic-a".to_owned(), false))
        );

        // Untrack live upstream and undo back.
        f.run(untrack_action("feat", "origin/topic-a", false));
        assert_eq!(upstream_of(&f, "feat"), Some((String::new(), false)));
        f.run(Action::Undo);
        assert_eq!(
            upstream_of(&f, "feat"),
            Some(("origin/topic-a".to_owned(), false))
        );

        // Delete remote branch so upstream becomes [gone], then untrack cleanly.
        git(&work, &["push", "-q", "origin", "--delete", "topic-a"]);
        git(&work, &["fetch", "-q", "--prune"]);
        f.reload();
        assert_eq!(
            upstream_of(&f, "feat"),
            Some(("origin/topic-a".to_owned(), true))
        );
        f.run(untrack_action("feat", "origin/topic-a", true));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(upstream_of(&f, "feat"), Some((String::new(), false)));
    }

    #[test]
    fn track_dialog_excludes_head_and_filters_across_viewports() {
        use egui_kittest::kittest::Queryable;

        let (work, _bare) = repo_with_remote("rv-track-dialog");
        git(&work, &["push", "-q", "origin", "HEAD:refs/heads/alpha-feat"]);
        git(&work, &["push", "-q", "origin", "HEAD:refs/heads/beta-fix"]);
        git(&work, &["remote", "set-head", "origin", "main"]);
        git(&work, &["fetch", "-q", "--prune"]);
        let f = Fixture::open(&work);
        let main_branch = f.tab.refs.local.iter().find(|b| b.name == "main").cloned();
        assert!(main_branch.is_some());
        if let Some(b) = main_branch {
            let picked = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
            for size in [[640.0, 480.0], [280.0, 360.0]] {
                let Dialog::Ext(body) = f.tab.track_remote_dialog(&b) else {
                    unreachable!("track dialog should be Dialog::Ext")
                };
                let s = std::rc::Rc::new(std::cell::RefCell::new(body));
                let p = picked.clone();
                let mut harness = egui_kittest::Harness::builder()
                    .with_size(size)
                    .build_ui(move |ui| {
                        if ui.ctx().cumulative_frame_nr() == 0 {
                            crate::theme::install_fonts(ui.ctx());
                            ui.ctx().request_repaint();
                            return;
                        }
                        let t = Tokens::get(ui.ctx());
                        let enter = ui.input(|i| i.key_pressed(Key::Enter));
                        if let Outcome::Done(Action::UndoableOps { label, .. }) =
                            s.borrow_mut().ui(ui, &t, enter)
                        {
                            *p.borrow_mut() = Some(label);
                        }
                    });
                harness.run();
                harness.step();
                assert!(harness.query_by_label("origin/HEAD").is_none());
                assert!(harness.query_by_label("origin/alpha-feat").is_some());
                if size[0] > 400.0 {
                    harness.event(egui::Event::Text("beta".to_owned()));
                    harness.run();
                    assert!(harness.query_by_label("origin/alpha-feat").is_none());
                    assert!(harness.query_by_label("origin/beta-fix").is_some());
                    harness.key_press(Key::Enter);
                    harness.run();
                }
            }
            assert_eq!(
                picked.borrow().as_deref(),
                Some("Track origin/beta-fix on main")
            );
        }

        // Also render `ForRemote` mode across both viewports to verify zero panics.
        for size in [[280.0, 360.0], [640.0, 480.0]] {
            let Dialog::Ext(remote_body) = f.tab.track_local_dialog("origin/beta-fix") else {
                unreachable!("track local dialog should be Dialog::Ext")
            };
            let s = std::rc::Rc::new(std::cell::RefCell::new(remote_body));
            let mut harness = egui_kittest::Harness::builder()
                .with_size(size)
                .build_ui(move |ui| {
                    if ui.ctx().cumulative_frame_nr() == 0 {
                        crate::theme::install_fonts(ui.ctx());
                        ui.ctx().request_repaint();
                        return;
                    }
                    let t = Tokens::get(ui.ctx());
                    let _ = s.borrow_mut().ui(ui, &t, false);
                });
            harness.run();
        }
    }
}
