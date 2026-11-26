//! Fetch, Pull and Push as small dialogs with options, as Sourcegit does: the toolbar buttons
//! open them instead of acting at once.
//!
//! Kept in a file of its own so the rest of the tree only carries small hooks.

use egui::{Align, Layout, RichText, Ui, vec2};

use super::dialogs::{Outcome, buttons, title, valid_ref_name};
use super::{Action, Dialog, Refresh, RepoTab, Undo};
use crate::theme::Tokens;

/// Width of the labels left of each control.
const LABEL_W: f32 = 108.0;

#[derive(Clone, Copy)]
pub enum SyncKind {
    Fetch,
    Pull,
    Push,
}

/// How a pull brings the remote's commits in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Integrate {
    FastForward,
    Merge,
    Rebase,
}

impl Integrate {
    const ALL: [Integrate; 3] = [Integrate::FastForward, Integrate::Merge, Integrate::Rebase];

    fn label(self) -> &'static str {
        match self {
            Integrate::FastForward => "Fast-forward only",
            Integrate::Merge => "Merge",
            Integrate::Rebase => "Rebase",
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Integrate::FastForward => "--ff-only",
            Integrate::Merge => "--no-rebase",
            Integrate::Rebase => "--rebase",
        }
    }
}

pub enum SyncDialog {
    Fetch(Fetch),
    Pull(Pull),
    Push(Push),
}

pub struct Fetch {
    remotes: Vec<String>,
    /// `None`: every remote.
    remote: Option<usize>,
    prune: bool,
    no_tags: bool,
    /// Overwrite local refs that diverged from the remote's (`--force`).
    force: bool,
}

pub struct Pull {
    remotes: Vec<String>,
    /// Each remote's branches, without the remote's name.
    branches: Vec<Vec<String>>,
    /// The checked-out branch; empty when HEAD is detached.
    current: String,
    remote: usize,
    branch: String,
    how: Integrate,
    autostash: bool,
    no_tags: bool,
    /// Update the submodules too (`--recurse-submodules`); only offered when there are some.
    submodules: bool,
    has_submodules: bool,
    /// Some submodule has commits on no branch, which updating them would leave behind.
    unsaved_submodules: bool,
    /// There are uncommitted changes.
    dirty: bool,
}

pub struct Push {
    remotes: Vec<String>,
    /// Local branches and their upstream (`origin/x`, empty when none).
    locals: Vec<(String, String)>,
    local: usize,
    remote: usize,
    remote_branch: String,
    upstream: bool,
    tags: bool,
    /// Refuse when a submodule has commits that aren't pushed (`--recurse-submodules=check`);
    /// only offered when there are submodules.
    check_submodules: bool,
    has_submodules: bool,
    force: bool,
    /// With `force`: don't check that the remote has nothing new.
    no_lease: bool,
}

impl RepoTab {
    /// The dialog of a toolbar button.
    pub(super) fn sync_dialog(&self, kind: SyncKind) -> Dialog {
        let remotes: Vec<String> = self.remotes.iter().map(|r| r.name.clone()).collect();
        let preferred = remotes.iter().position(|r| r == "origin").unwrap_or(0);
        Dialog::Sync(match kind {
            SyncKind::Fetch => SyncDialog::Fetch(Fetch {
                remotes,
                remote: None,
                prune: true,
                no_tags: false,
                force: false,
            }),
            SyncKind::Pull => {
                let branches: Vec<Vec<String>> = remotes
                    .iter()
                    .map(|remote| {
                        let prefix = format!("{remote}/");
                        self.refs
                            .remotes
                            .iter()
                            .filter(|(r, _)| r == remote)
                            .flat_map(|(_, list)| list)
                            .filter_map(|b| b.name.strip_prefix(&prefix))
                            .filter(|n| *n != "HEAD")
                            .map(str::to_owned)
                            .collect()
                    })
                    .collect();
                let current = self.refs.current.clone().unwrap_or_default();
                let upstream = self
                    .refs
                    .local
                    .iter()
                    .find(|b| b.name == current && !b.upstream.is_empty() && !b.upstream_gone())
                    .and_then(|b| b.upstream.split_once('/'))
                    .and_then(|(r, n)| Some((remotes.iter().position(|x| x == r)?, n.to_owned())));
                let (remote, branch) = upstream.unwrap_or_else(|| {
                    let branch = if branches
                        .get(preferred)
                        .is_some_and(|l| l.contains(&current))
                    {
                        current.clone()
                    } else {
                        branches
                            .get(preferred)
                            .and_then(|l| l.first().cloned())
                            .unwrap_or_default()
                    };
                    (preferred, branch)
                });
                SyncDialog::Pull(Pull {
                    remotes,
                    branches,
                    current,
                    remote,
                    branch,
                    how: Integrate::FastForward,
                    autostash: false,
                    no_tags: false,
                    submodules: false,
                    has_submodules: !self.submodules.is_empty(),
                    unsaved_submodules: self.submodules.iter().any(|s| s.unsaved),
                    dirty: !self.work.is_clean(),
                })
            }
            SyncKind::Push => {
                let locals: Vec<(String, String)> = self
                    .refs
                    .local
                    .iter()
                    .map(|b| {
                        let upstream = if b.upstream_gone() {
                            String::new()
                        } else {
                            b.upstream.clone()
                        };
                        (b.name.clone(), upstream)
                    })
                    .collect();
                let local = self
                    .refs
                    .current
                    .as_ref()
                    .and_then(|c| locals.iter().position(|(n, _)| n == c))
                    .unwrap_or(0);
                let mut push = Push {
                    remotes,
                    locals,
                    local,
                    remote: preferred,
                    remote_branch: String::new(),
                    upstream: false,
                    tags: false,
                    check_submodules: !self.submodules.is_empty(),
                    has_submodules: !self.submodules.is_empty(),
                    force: false,
                    no_lease: false,
                };
                push.use_defaults_of_branch();
                SyncDialog::Push(push)
            }
        })
    }
}

impl SyncDialog {
    pub(super) fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        match self {
            SyncDialog::Fetch(d) => d.ui(ui, t, enter),
            SyncDialog::Pull(d) => d.ui(ui, t, enter),
            SyncDialog::Push(d) => d.ui(ui, t, enter),
        }
    }
}

/// A label, then what `add` puts in the rest of the line.
pub(super) fn row<R>(ui: &mut Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            vec2(LABEL_W, 26.0),
            Layout::left_to_right(Align::Center),
            |ui| ui.label(RichText::new(label).color(t.text_dim)),
        );
        add(ui)
    })
    .inner
}

#[derive(Clone, Default)]
struct SearchPickState {
    query: String,
    cursor: usize,
    focused: bool,
}

/// Case-insensitive multi-token substring match for branch filtering.
pub(super) fn matches_branch_filter(name: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let lower = name.to_lowercase();
    q.split_ascii_whitespace()
        .all(|tok| lower.contains(&tok.to_lowercase()))
}

/// A drop-down filling the line: `show` fills the list and says which entry was picked.
pub(super) fn pick(
    ui: &mut Ui,
    id: &str,
    selected: &str,
    show: impl FnOnce(&mut Ui) -> bool,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .width(ui.available_width().max(80.0))
        .selected_text(selected)
        .show_ui(ui, |ui| changed = show(ui));
    changed
}

/// A searchable drop-down for potentially large branch lists: pins a live search field at the top,
/// supports arrow/Enter selection and virtualized scrolling, and resets its filter when closed.
#[allow(clippy::too_many_arguments)]
pub(super) fn pick_searchable<T: AsRef<str>>(
    ui: &mut Ui,
    id: &str,
    selected: &str,
    items: &[T],
    selected_idx: Option<usize>,
    empty_label: &str,
    hint: &str,
    mut on_pick: impl FnMut(usize, &str),
) -> bool {
    let w = ui.available_width().max(80.0);
    let avail = (ui.ctx().content_rect().width() - 24.0).max(160.0);
    let popup_w = w.clamp(240.0, 420.0).min(avail);
    let state_id = egui::Id::new(("pick-search-state", ui.make_persistent_id(id)));
    let scroll_id = state_id.with("scroll");
    let t = Tokens::get(ui.ctx());
    let mut changed = false;
    let ir = egui::ComboBox::from_id_salt(id)
        .width(w)
        .height(320.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.set_min_width(popup_w);
            ui.set_max_width(popup_w);
            let mut state = ui
                .ctx()
                .data(|d| d.get_temp::<SearchPickState>(state_id))
                .unwrap_or_else(|| SearchPickState {
                    cursor: selected_idx.unwrap_or(0),
                    ..Default::default()
                });

            ui.horizontal(|ui| {
                let clear_w = if state.query.is_empty() { 0.0 } else { 26.0 };
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut state.query)
                        .hint_text(hint)
                        .desired_width((ui.available_width() - clear_w).max(80.0))
                        .margin(egui::Margin::symmetric(6, 4)),
                );
                if !state.focused {
                    if edit.has_focus() {
                        state.focused = true;
                    } else {
                        edit.request_focus();
                    }
                }
                if edit.changed() {
                    state.cursor = 0;
                }
                if !state.query.is_empty() && ui.small_button("×").clicked() {
                    state.query.clear();
                    state.cursor = 0;
                    edit.request_focus();
                }
            });

            let (down, up, enter_key) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::Enter),
                )
            });

            let filtered: Vec<(usize, &str)> = items
                .iter()
                .enumerate()
                .map(|(i, item)| (i, item.as_ref()))
                .filter(|(_, name)| matches_branch_filter(name, &state.query))
                .collect();

            if !filtered.is_empty() {
                state.cursor = state.cursor.min(filtered.len() - 1);
                if down {
                    state.cursor = (state.cursor + 1).min(filtered.len() - 1);
                }
                if up {
                    state.cursor = state.cursor.saturating_sub(1);
                }
                if enter_key {
                    let (orig_idx, name) = filtered[state.cursor];
                    on_pick(orig_idx, name);
                    changed = true;
                    ui.close();
                }
            }

            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            if items.is_empty() {
                ui.label(RichText::new(empty_label).color(t.text_faint));
            } else if filtered.is_empty() {
                ui.label(RichText::new("No matching branches").color(t.text_faint));
            } else {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                egui::ScrollArea::vertical()
                    .id_salt(scroll_id)
                    .max_height(220.0)
                    .auto_shrink([false, true])
                    .show_rows(ui, 24.0, filtered.len(), |ui, range| {
                        for row_idx in range {
                            let Some(&(orig_idx, name)) = filtered.get(row_idx) else {
                                continue;
                            };
                            let is_cur = row_idx == state.cursor;
                            let resp = ui
                                .add_sized(
                                    [ui.available_width(), 24.0],
                                    egui::Button::selectable(is_cur, name),
                                )
                                .on_hover_text(name);
                            if (down || up) && is_cur {
                                resp.scroll_to_me(None);
                            }
                            if resp.clicked() {
                                on_pick(orig_idx, name);
                                changed = true;
                                ui.close();
                            }
                        }
                    });
            }

            if !changed {
                ui.ctx().data_mut(|d| d.insert_temp(state_id, state));
            }
        });
    if ir.inner.is_none() || changed {
        ui.ctx()
            .data_mut(|d| d.remove::<SearchPickState>(state_id));
    }
    changed
}

impl Fetch {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        title(
            ui,
            "Fetch",
            "Download new commits, branches and tags. Your files don't change.",
            t,
        );
        row(ui, t, "Remote", |ui| {
            let text = match self.remote {
                Some(i) => self.remotes.get(i).map_or("", String::as_str),
                None => "All remotes",
            };
            pick(ui, "fetch-remote", text, |ui| {
                let mut changed = false;
                if ui
                    .selectable_label(self.remote.is_none(), "All remotes")
                    .clicked()
                {
                    self.remote = None;
                    changed = true;
                }
                for (i, name) in self.remotes.iter().enumerate() {
                    if ui.selectable_label(self.remote == Some(i), name).clicked() {
                        self.remote = Some(i);
                        changed = true;
                    }
                }
                changed
            })
        });
        ui.add_space(6.0);
        ui.checkbox(&mut self.prune, "Prune branches deleted on the remote");
        ui.checkbox(&mut self.no_tags, "Fetch without tags");
        ui.checkbox(
            &mut self.force,
            "Enable --force (overwrite diverged local refs)",
        );
        ui.add_space(8.0);
        let valid = self
            .remote
            .and_then(|i| self.remotes.get(i))
            .is_none_or(|r| valid_ref_name(r));
        buttons(ui, "Fetch", valid, enter, || self.action())
    }

    fn action(&self) -> Action {
        let mut args = vec!["fetch".to_owned()];
        // Unticked means no pruning even when `fetch.prune` is set in the user's config.
        args.push(if self.prune { "--prune" } else { "--no-prune" }.into());
        if self.no_tags {
            args.push("--no-tags".into());
        }
        if self.force {
            args.push("--force".into());
        }
        match self.remote.and_then(|i| self.remotes.get(i)) {
            Some(remote) => args.push(remote.clone()),
            None => args.push("--all".into()),
        }
        Action::FetchWith(args)
    }
}

impl Pull {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        let sub = if self.current.is_empty() {
            "Pulling needs a checked-out branch.".to_owned()
        } else {
            format!("Bring a remote branch into {}.", self.current)
        };
        title(ui, "Pull", &sub, t);
        let remote_changed = row(ui, t, "Remote", |ui| {
            let text = self.remotes.get(self.remote).map_or("", String::as_str);
            pick(ui, "pull-remote", text, |ui| {
                let mut changed = false;
                for (i, name) in self.remotes.iter().enumerate() {
                    if ui.selectable_label(self.remote == i, name).clicked() {
                        self.remote = i;
                        changed = true;
                    }
                }
                changed
            })
        });
        let branches = self.branches.get(self.remote).cloned().unwrap_or_default();
        if remote_changed {
            self.branch = if branches.contains(&self.current) {
                self.current.clone()
            } else {
                branches.first().cloned().unwrap_or_default()
            };
        }
        row(ui, t, "Branch", |ui| {
            let text = if self.branch.is_empty() {
                "No branch on this remote".to_owned()
            } else {
                self.branch.clone()
            };
            let selected_idx = branches.iter().position(|name| name == &self.branch);
            pick_searchable(
                ui,
                "pull-branch",
                &text,
                &branches,
                selected_idx,
                "No branch on this remote",
                "Search branches…",
                |_, name| self.branch = name.to_owned(),
            )
        });
        row(ui, t, "Into", |ui| {
            ui.label(RichText::new(&self.current).font(crate::theme::semibold(13.0)));
        });
        row(ui, t, "Method", |ui| {
            pick(ui, "pull-how", self.how.label(), |ui| {
                let mut changed = false;
                for how in Integrate::ALL {
                    if ui.selectable_label(self.how == how, how.label()).clicked() {
                        self.how = how;
                        changed = true;
                    }
                }
                changed
            })
        });
        ui.add_space(6.0);
        ui.checkbox(
            &mut self.autostash,
            "Stash local changes first and reapply them",
        );
        ui.checkbox(&mut self.no_tags, "Fetch without tags");
        if self.has_submodules {
            ui.checkbox(&mut self.submodules, "Update submodules too");
            if self.submodules && self.unsaved_submodules {
                ui.label(
                    RichText::new(
                        "A submodule has commits on no branch: updating it leaves them behind.",
                    )
                    .color(t.warning)
                    .small(),
                );
            }
        }
        if self.dirty && !self.autostash {
            ui.label(
                RichText::new("You have uncommitted changes; git may refuse to pull over them.")
                    .color(t.warning)
                    .small(),
            );
        }
        ui.add_space(8.0);
        let remote_ok = self
            .remotes
            .get(self.remote)
            .is_some_and(|r| valid_ref_name(r));
        let valid = remote_ok && !self.current.is_empty() && valid_ref_name(&self.branch);
        buttons(ui, "Pull", valid, enter, || self.action())
    }

    fn action(&self) -> Action {
        let remote = self.remotes.get(self.remote).cloned().unwrap_or_default();
        let mut args = vec!["pull".to_owned(), self.how.flag().to_owned()];
        if self.autostash {
            args.push("--autostash".into());
        }
        if self.no_tags {
            args.push("--no-tags".into());
        }
        if self.submodules {
            args.push("--recurse-submodules".into());
        }
        args.push(remote.clone());
        args.push(self.branch.clone());
        Action::UndoableOps {
            label: format!("Pull {remote}/{}", self.branch),
            ops: vec![args],
            refresh: Refresh::All,
            undo: Undo::HeadMove { mode: "--keep" },
        }
    }
}

impl Push {
    /// The remote, remote branch and "set upstream" the selected local branch starts with.
    fn use_defaults_of_branch(&mut self) {
        let Some((name, upstream)) = self.locals.get(self.local) else {
            return;
        };
        let tracked = upstream
            .split_once('/')
            .and_then(|(r, n)| Some((self.remotes.iter().position(|x| x == r)?, n.to_owned())));
        match tracked {
            Some((remote, branch)) => {
                self.remote = remote;
                self.remote_branch = branch;
                self.upstream = false;
            }
            None => {
                self.remote_branch = name.clone();
                self.upstream = true;
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        title(ui, "Push", "Upload a local branch to a remote branch.", t);
        let branch_changed = row(ui, t, "Branch", |ui| {
            let text = self.locals.get(self.local).map_or("", |(n, _)| n.as_str());
            let names: Vec<&str> = self.locals.iter().map(|(n, _)| n.as_str()).collect();
            pick_searchable(
                ui,
                "push-local",
                text,
                &names,
                Some(self.local),
                "No local branches",
                "Search branches…",
                |i, _| self.local = i,
            )
        });
        if branch_changed {
            self.use_defaults_of_branch();
        }
        row(ui, t, "Remote", |ui| {
            let text = self.remotes.get(self.remote).map_or("", String::as_str);
            pick(ui, "push-remote", text, |ui| {
                let mut changed = false;
                for (i, name) in self.remotes.iter().enumerate() {
                    if ui.selectable_label(self.remote == i, name).clicked() {
                        self.remote = i;
                        changed = true;
                    }
                }
                changed
            })
        });
        row(ui, t, "Remote branch", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.remote_branch)
                    .desired_width(ui.available_width().max(80.0))
                    .margin(egui::Margin::symmetric(8, 4)),
            );
            super::widgets::text_edit_menu(&edit, &mut self.remote_branch);
            edit
        });
        ui.add_space(6.0);
        ui.checkbox(&mut self.upstream, "Track the remote branch (set upstream)");
        ui.checkbox(&mut self.tags, "Push all tags");
        if self.has_submodules {
            ui.checkbox(
                &mut self.check_submodules,
                "Check that submodule commits are pushed",
            );
        }
        ui.checkbox(&mut self.force, "Force push");
        if self.force {
            ui.indent("force-options", |ui| {
                ui.checkbox(
                    &mut self.no_lease,
                    "Even if the remote has commits I haven't fetched",
                );
            });
            ui.label(
                RichText::new(
                    "Replaces the remote branch: commits it has that yours doesn't are removed for everyone.",
                )
                .color(t.warning)
                .small(),
            );
        }
        ui.add_space(8.0);
        let valid = self
            .locals
            .get(self.local)
            .is_some_and(|(n, _)| valid_ref_name(n))
            && self
                .remotes
                .get(self.remote)
                .is_some_and(|r| valid_ref_name(r))
            && valid_ref_name(&self.remote_branch);
        let label = if self.force { "Force push" } else { "Push" };
        buttons(ui, label, valid, enter, || self.action())
    }

    fn action(&self) -> Action {
        let local = self
            .locals
            .get(self.local)
            .map(|(n, _)| n.clone())
            .unwrap_or_default();
        let remote = self.remotes.get(self.remote).cloned().unwrap_or_default();
        let remote_branch = self.remote_branch.trim().to_owned();
        let mut args = vec!["push".to_owned()];
        if self.force {
            if self.no_lease {
                args.push("--force".into());
            } else {
                // The lease alone is void once a fetch (gitcrafted fetches in the background) has
                // updated the remote-tracking branch; `--force-if-includes` also asks that what
                // was fetched is part of the branch being pushed.
                args.push("--force-with-lease".into());
                args.push("--force-if-includes".into());
            }
        }
        if self.upstream {
            args.push("--set-upstream".into());
        }
        if self.tags {
            args.push("--tags".into());
        }
        if self.has_submodules && self.check_submodules {
            args.push("--recurse-submodules=check".into());
        }
        args.push(remote.clone());
        args.push(format!("{local}:{remote_branch}"));
        Action::Ops {
            label: format!("Push {local} to {remote}/{remote_branch}"),
            ops: vec![args],
            refresh: Refresh::All,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(locals: &[(&str, &str)], remotes: &[&str]) -> Push {
        Push {
            remotes: remotes.iter().map(|r| (*r).to_owned()).collect(),
            locals: locals
                .iter()
                .map(|(n, u)| ((*n).to_owned(), (*u).to_owned()))
                .collect(),
            local: 0,
            remote: 0,
            remote_branch: String::new(),
            upstream: false,
            tags: false,
            check_submodules: false,
            has_submodules: false,
            force: false,
            no_lease: false,
        }
    }

    #[test]
    fn a_tracked_branch_pushes_to_its_upstream() {
        let mut p = push(&[("feat", "up/topic")], &["origin", "up"]);
        p.use_defaults_of_branch();
        assert_eq!(
            (p.remote, p.remote_branch.as_str(), p.upstream),
            (1, "topic", false)
        );
    }

    #[test]
    fn an_untracked_branch_gets_its_own_name_and_sets_upstream() {
        let mut p = push(&[("feat/x", "")], &["origin"]);
        p.use_defaults_of_branch();
        assert_eq!((p.remote_branch.as_str(), p.upstream), ("feat/x", true));
    }

    #[test]
    fn pull_methods_map_to_git_flags() {
        assert_eq!(Integrate::FastForward.flag(), "--ff-only");
        assert_eq!(Integrate::Merge.flag(), "--no-rebase");
        assert_eq!(Integrate::Rebase.flag(), "--rebase");
    }

    /// Against real git: each pull method brings in a remote commit, and a diverged branch is
    /// refused by fast-forward only but accepted by merge and rebase.
    #[test]
    fn pull_methods_against_real_git() {
        use std::path::Path;
        use std::process::Command;
        let git = |dir: &Path, args: &[&str]| -> bool {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .env("GIT_EDITOR", "true")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.autocrlf=false"])
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        let base = std::env::temp_dir().join(format!("gitcrafted-pull-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).ok();
        let (a, b) = (base.join("a"), base.join("b"));
        let url = base.join("remote.git").display().to_string();
        assert!(git(
            &base,
            &["init", "-q", "--bare", "-b", "main", "remote.git"]
        ));
        assert!(git(&base, &["clone", "-q", &url, "a"]));
        assert!(git(&a, &["commit", "-q", "--allow-empty", "-m", "one"]));
        assert!(git(&a, &["push", "-q", "-u", "origin", "main"]));
        assert!(git(&base, &["clone", "-q", &url, "b"]));
        // b commits and pushes; a commits too, so a has diverged.
        assert!(git(&b, &["commit", "-q", "--allow-empty", "-m", "remote"]));
        assert!(git(&b, &["push", "-q"]));
        assert!(git(&a, &["commit", "-q", "--allow-empty", "-m", "local"]));
        assert!(!git(
            &a,
            &["pull", Integrate::FastForward.flag(), "origin", "main"]
        ));
        assert!(git(
            &a,
            &[
                "pull",
                Integrate::Rebase.flag(),
                "--autostash",
                "origin",
                "main"
            ]
        ));
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::test_support::*;

    fn fetch_dialog(f: &Fixture) -> Fetch {
        let Dialog::Sync(SyncDialog::Fetch(d)) = f.tab.sync_dialog(SyncKind::Fetch) else {
            unreachable!("a fetch dialog")
        };
        d
    }

    fn pull_dialog(f: &Fixture) -> Pull {
        let Dialog::Sync(SyncDialog::Pull(d)) = f.tab.sync_dialog(SyncKind::Pull) else {
            unreachable!("a pull dialog")
        };
        d
    }

    fn push_dialog(f: &Fixture) -> Push {
        let Dialog::Sync(SyncDialog::Push(d)) = f.tab.sync_dialog(SyncKind::Push) else {
            unreachable!("a push dialog")
        };
        d
    }

    fn commit_file(dir: &std::path::Path, name: &str, text: &str, message: &str) {
        write(dir, name, text);
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    #[test]
    fn fetch_updates_prunes_and_can_skip_tags() {
        let (work, bare) = repo_with_remote("sync-fetch");
        let other = clone_of(&bare, "sync-fetch-other");
        commit_file(&other, "b.txt", "b\n", "from other");
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        git(&other, &["push", "-q", "origin", "HEAD:refs/heads/temp"]);
        let mut f = Fixture::open(&work);

        let mut d = fetch_dialog(&f);
        assert!(d.remote.is_none() && d.prune && !d.no_tags && !d.force);
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&work, &["rev-parse", "origin/main"]),
            git(&other, &["rev-parse", "HEAD"])
        );
        assert!(git_ok(
            &work,
            &["rev-parse", "--verify", "-q", "origin/temp"]
        ));

        // A branch deleted on the remote stays until a fetch that prunes.
        git(&other, &["push", "-q", "origin", "--delete", "temp"]);
        d.prune = false;
        let Action::FetchWith(args) = d.action() else {
            unreachable!("a fetch")
        };
        assert!(args.contains(&"--no-prune".to_owned()));
        f.run(d.action());
        assert!(git_ok(
            &work,
            &["rev-parse", "--verify", "-q", "origin/temp"]
        ));
        d.prune = true;
        f.run(d.action());
        assert!(!git_ok(
            &work,
            &["rev-parse", "--verify", "-q", "origin/temp"]
        ));

        // One remote by name instead of --all.
        d.remote = Some(0);
        let Action::FetchWith(args) = d.action() else {
            unreachable!("a fetch")
        };
        assert_eq!(args.last().map(String::as_str), Some("origin"));

        // Tags come along with new commits unless told not to.
        commit_file(&other, "c.txt", "c\n", "tagged");
        git(&other, &["tag", "v1"]);
        git(&other, &["push", "-q", "origin", "HEAD:main", "v1"]);
        d.no_tags = true;
        d.remote = None;
        f.run(d.action());
        assert!(git(&work, &["tag", "-l", "v1"]).is_empty());
        d.no_tags = false;
        f.run(d.action());
        assert_eq!(git(&work, &["tag", "-l", "v1"]), "v1");
    }

    #[test]
    fn pull_defaults_follow_the_upstream() {
        let (work, _bare) = repo_with_remote("sync-pull-defaults");
        let f = Fixture::open(&work);
        let d = pull_dialog(&f);
        assert_eq!(
            (d.remote, d.branch.as_str(), d.current.as_str()),
            (0, "main", "main")
        );
        assert!(d.how == Integrate::FastForward && !d.autostash && !d.dirty);
        assert!(d.branches[0].contains(&"main".to_owned()));
        assert!(!d.has_submodules);
    }

    #[test]
    fn pull_methods_on_a_diverged_branch() {
        let (work, bare) = repo_with_remote("sync-pull");
        let other = clone_of(&bare, "sync-pull-other");
        commit_file(&other, "r.txt", "remote\n", "remote work");
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        commit_file(&work, "l.txt", "local\n", "local work");
        let mut f = Fixture::open(&work);
        let before = git(&work, &["rev-parse", "HEAD"]);

        // Fast-forward only refuses.
        let mut d = pull_dialog(&f);
        f.run(d.action());
        assert!(f.failed());
        assert_eq!(git(&work, &["rev-parse", "HEAD"]), before);

        // Merge makes a merge commit, and Undo takes it back.
        d.how = Integrate::Merge;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&work, &["rev-list", "--parents", "-1", "HEAD"])
                .split(' ')
                .count(),
            3
        );
        assert!(work.join("r.txt").exists() && work.join("l.txt").exists());
        f.run(Action::Undo);
        assert_eq!(git(&work, &["rev-parse", "HEAD"]), before);

        // Rebase keeps history linear with the local commit on top.
        d.how = Integrate::Rebase;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&work, &["rev-list", "--parents", "-1", "HEAD"])
                .split(' ')
                .count(),
            2
        );
        assert_eq!(git(&work, &["log", "-1", "--format=%s"]), "local work");
        assert_eq!(
            git(&work, &["log", "-1", "--format=%s", "HEAD^"]),
            "remote work"
        );
    }

    #[test]
    fn pull_fast_forwards_when_only_behind_and_autostashes_a_dirty_tree() {
        let (work, bare) = repo_with_remote("sync-autostash");
        commit_file(
            &work,
            "a.txt",
            "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n",
            "lines",
        );
        git(&work, &["push", "-q"]);
        let other = clone_of(&bare, "sync-autostash-other");
        commit_file(
            &other,
            "a.txt",
            "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nremote\n",
            "edit bottom",
        );
        git(&other, &["push", "-q"]);
        // Local, uncommitted: the top line.
        write(&work, "a.txt", "local\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n");
        let mut f = Fixture::open(&work);
        let mut d = pull_dialog(&f);
        assert!(d.dirty);

        f.run(d.action());
        assert!(f.failed(), "git refuses to pull over local edits");
        assert!(read(&work, "a.txt").starts_with("local"));

        d.autostash = true;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        let text = read(&work, "a.txt");
        assert!(
            text.starts_with("local") && text.contains("remote"),
            "{text}"
        );
        assert!(git(&work, &["stash", "list"]).is_empty());
    }

    #[test]
    fn pull_flags_reach_git() {
        let (work, _bare) = repo_with_remote("sync-pull-flags");
        let f = Fixture::open(&work);
        let mut d = pull_dialog(&f);
        d.autostash = true;
        d.no_tags = true;
        d.submodules = true;
        d.how = Integrate::Rebase;
        let Action::UndoableOps { ops, .. } = d.action() else {
            unreachable!("pull is undoable")
        };
        assert_eq!(
            ops[0],
            [
                "pull",
                "--rebase",
                "--autostash",
                "--no-tags",
                "--recurse-submodules",
                "origin",
                "main"
            ]
        );
    }

    #[test]
    fn push_defaults_publish_and_track() {
        let (work, bare) = repo_with_remote("sync-push");
        let mut f = Fixture::open(&work);
        let d = push_dialog(&f);
        assert_eq!(
            (d.remote, d.remote_branch.as_str(), d.upstream),
            (0, "main", false)
        );

        // A new commit goes to the tracked branch.
        commit_file(&work, "b.txt", "b\n", "second");
        f.reload();
        f.run(push_dialog(&f).action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&bare, &["rev-parse", "main"]),
            git(&work, &["rev-parse", "HEAD"])
        );

        // A branch with no upstream is published and starts tracking.
        git(&work, &["checkout", "-q", "-b", "feat/x"]);
        commit_file(&work, "c.txt", "c\n", "on feat");
        f.reload();
        let d = push_dialog(&f);
        assert_eq!((d.remote_branch.as_str(), d.upstream), ("feat/x", true));
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&bare, &["rev-parse", "feat/x"]),
            git(&work, &["rev-parse", "HEAD"])
        );
        assert_eq!(git(&work, &["config", "branch.feat/x.remote"]), "origin");

        // Another local branch, another remote name, and tags.
        let mut d = push_dialog(&f);
        let main = d.locals.iter().position(|(n, _)| n == "main").unwrap_or(0);
        d.local = main;
        d.use_defaults_of_branch();
        assert_eq!((d.remote_branch.as_str(), d.upstream), ("main", false));
        d.remote_branch = "release".to_owned();
        git(&work, &["tag", "t1"]);
        d.tags = true;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&bare, &["rev-parse", "release"]),
            git(&work, &["rev-parse", "main"])
        );
        assert_eq!(git(&bare, &["tag", "-l", "t1"]), "t1");
    }

    #[test]
    fn force_push_rewrites_the_remote_only_when_asked() {
        let (work, bare) = repo_with_remote("sync-force");
        commit_file(&work, "b.txt", "b\n", "second");
        git(&work, &["push", "-q"]);
        let other = clone_of(&bare, "sync-force-other");
        let mut f = Fixture::open(&work);

        // After an amend a plain push is refused; with force it goes through.
        git(&work, &["commit", "-q", "--amend", "-m", "second, amended"]);
        f.reload();
        let mut d = push_dialog(&f);
        f.run(d.action());
        assert!(f.failed());
        d.force = true;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&bare, &["rev-parse", "main"]),
            git(&work, &["rev-parse", "HEAD"])
        );

        // Someone else pushes; we don't fetch and amend again: the lease refuses, --force wins.
        git(&other, &["fetch", "-q"]);
        git(&other, &["reset", "-q", "--hard", "origin/main"]);
        commit_file(&other, "o.txt", "o\n", "theirs");
        git(&other, &["push", "-q"]);
        git(&work, &["commit", "-q", "--amend", "-m", "second, again"]);
        f.reload();
        let mut d = push_dialog(&f);
        d.force = true;
        f.run(d.action());
        assert!(f.failed(), "the lease must refuse commits we haven't seen");
        d.no_lease = true;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&bare, &["log", "-1", "--format=%s", "main"]),
            "second, again"
        );
    }
}

/// Data-safety tests: pushing never drops someone else's commits unless explicitly told to;
/// failed pulls leave everything recoverable.
#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::repo::test_support::*;
    use crate::repo::{InProgress, abort_action};

    fn push_dialog(f: &Fixture) -> Push {
        let Dialog::Sync(SyncDialog::Push(d)) = f.tab.sync_dialog(SyncKind::Push) else {
            unreachable!("a push dialog")
        };
        d
    }

    fn pull_dialog(f: &Fixture) -> Pull {
        let Dialog::Sync(SyncDialog::Pull(d)) = f.tab.sync_dialog(SyncKind::Pull) else {
            unreachable!("a pull dialog")
        };
        d
    }

    /// We pushed "mine"; someone else then pushed "theirs" on top.
    fn raced(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let (work, bare) = repo_with_remote(name);
        commit_file(&work, "mine.txt", "mine\n", "mine");
        git(&work, &["push", "-q"]);
        let other = clone_of(&bare, &format!("{name}-other"));
        commit_file(&other, "theirs.txt", "theirs\n", "theirs");
        git(&other, &["push", "-q"]);
        (work, bare)
    }

    #[test]
    fn a_background_fetch_does_not_turn_the_lease_into_a_plain_force() {
        let (work, bare) = raced("sf-lease-fetch");
        git(&work, &["fetch", "-q"]); // what gitcrafted's background fetch does
        git(&work, &["commit", "-q", "--amend", "-m", "mine, amended"]);
        let mut f = Fixture::open(&work);
        let mut d = push_dialog(&f);
        d.force = true;
        f.run(d.action());
        assert!(
            f.failed(),
            "their commit was fetched but never looked at: refuse"
        );
        assert_eq!(git(&bare, &["log", "-1", "--format=%s", "main"]), "theirs");
    }

    #[test]
    fn after_integrating_their_commit_the_lease_push_keeps_it() {
        let (work, bare) = raced("sf-lease-integrated");
        git(&work, &["fetch", "-q"]);
        git(&work, &["rebase", "-q", "origin/main"]);
        git(
            &work,
            &["commit", "-q", "--amend", "-m", "mine, on top of theirs"],
        );
        let mut f = Fixture::open(&work);
        let mut d = push_dialog(&f);
        d.force = true;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        let log = git(&bare, &["log", "--format=%s", "main"]);
        assert!(
            log.contains("theirs") && log.contains("mine, on top of theirs"),
            "{log}"
        );
    }

    #[test]
    fn without_force_a_rejected_push_changes_nothing_anywhere() {
        let (work, bare) = raced("sf-plain-push");
        commit_file(&work, "more.txt", "more\n", "more of mine");
        let mut f = Fixture::open(&work);
        let remote_before = git(&bare, &["rev-parse", "main"]);
        let before = Snapshot::take(&work);
        f.run(push_dialog(&f).action());
        assert!(f.failed());
        assert_eq!(git(&bare, &["rev-parse", "main"]), remote_before);
        assert_same(&before, &Snapshot::take(&work), "rejected push");
    }

    #[test]
    fn a_conflicting_pull_rebase_aborts_back_to_the_exact_state() {
        let (work, bare) = repo_with_remote("sf-pull-abort");
        let other = clone_of(&bare, "sf-pull-abort-other");
        commit_file(&other, "a.txt", "theirs\n", "theirs");
        git(&other, &["push", "-q"]);
        commit_file(&work, "a.txt", "mine\n", "mine");
        write(&work, "u.txt", "untracked\n");
        let mut f = Fixture::open(&work);
        let before = Snapshot::take(&work);
        let mut d = pull_dialog(&f);
        d.how = Integrate::Rebase;
        f.run(d.action());
        assert!(f.failed());
        assert_eq!(f.tab.work.in_progress, Some(InProgress::Rebase));
        f.run(abort_action(InProgress::Rebase));
        assert_same_locally(&before, &Snapshot::take(&work), "pull --rebase, then abort");
    }

    #[test]
    fn an_autostash_that_conflicts_keeps_the_edit_safe() {
        let (work, bare) = repo_with_remote("sf-autostash-conflict");
        let other = clone_of(&bare, "sf-autostash-conflict-other");
        commit_file(&other, "a.txt", "theirs\n", "theirs");
        git(&other, &["push", "-q"]);
        write(&work, "a.txt", "mine, uncommitted\n");
        let mut f = Fixture::open(&work);
        let mut d = pull_dialog(&f);
        d.autostash = true;
        f.run(d.action());
        // Whatever git reports, the edit survives: in the file, or in the stash git kept.
        let in_file = read(&work, "a.txt").contains("mine, uncommitted");
        let in_stash = git_ok(&work, &["rev-parse", "--verify", "-q", "stash@{0}"])
            && git(&work, &["show", "stash@{0}:a.txt"]).contains("mine, uncommitted");
        assert!(in_file || in_stash, "the uncommitted edit must not be lost");
    }

    #[test]
    fn undoing_a_pull_refuses_to_overwrite_later_edits() {
        let (work, bare) = repo_with_remote("sf-pull-undo");
        let other = clone_of(&bare, "sf-pull-undo-other");
        commit_file(&other, "t.txt", "theirs\n", "theirs");
        git(&other, &["push", "-q"]);
        let mut f = Fixture::open(&work);
        f.run(pull_dialog(&f).action());
        assert!(!f.failed(), "{:?}", f.status());
        write(&work, "t.txt", "edited after pulling\n");
        f.run(Action::Undo);
        assert!(f.failed(), "undo refuses rather than overwrite");
        assert_eq!(read(&work, "t.txt"), "edited after pulling\n");
    }

    #[test]
    fn branch_filter_matches_case_insensitive_multi_token() {
        assert!(matches_branch_filter("origin/32420-add-ai-images-modal", ""));
        assert!(matches_branch_filter(
            "origin/32420-add-ai-images-modal",
            "   "
        ));
        assert!(matches_branch_filter(
            "32420-add-ai-images-modal",
            "32420 MODAL"
        ));
        assert!(matches_branch_filter(
            "release-5.20",
            "RELEASE 5.20"
        ));
        assert!(!matches_branch_filter(
            "32420-add-ai-images-modal",
            "32420 other"
        ));
    }

    #[test]
    fn pull_branch_dropdown_filters_and_selects_branch() {
        use egui_kittest::kittest::Queryable;

        let d = std::rc::Rc::new(std::cell::RefCell::new(Pull {
            remotes: vec!["origin".to_owned()],
            branches: vec![vec![
                "main".to_owned(),
                "32160-alpha-client".to_owned(),
                "32420-add-images-modal".to_owned(),
                "32798-dropdown-width".to_owned(),
            ]],
            current: "main".to_owned(),
            remote: 0,
            branch: "main".to_owned(),
            how: Integrate::FastForward,
            autostash: false,
            no_tags: false,
            submodules: false,
            has_submodules: false,
            unsaved_submodules: false,
            dirty: false,
        }));
        let state = d.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size([640.0, 480.0])
            .build_ui(move |ui| {
                if ui.ctx().cumulative_frame_nr() == 0 {
                    crate::theme::install_fonts(ui.ctx());
                    ui.ctx().request_repaint();
                    return;
                }
                let t = Tokens::get(ui.ctx());
                let mut pull = state.borrow_mut();
                let _ = pull.ui(ui, &t, false);
            });
        harness.run();
        // Click the Pull branch ComboBox (`Into` label also reads `main`, and Remote is also a ComboBox).
        let branch_box = harness
            .get_all_by_role(egui::accesskit::Role::ComboBox)
            .nth(1);
        assert!(branch_box.is_some());
        if let Some(node) = branch_box {
            node.click();
        }
        harness.run();
        harness.step();
        // Type a filter query into the auto-focused search box and confirm by pressing Enter.
        harness.event(egui::Event::Text("32420 modal".to_owned()));
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        drop(harness);
        assert_eq!(d.borrow().branch, "32420-add-images-modal");
    }
}
