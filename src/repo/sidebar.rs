//! Left pane: local branches, remotes and their branches, worktrees, stashes, pull requests,
//! tags.

use egui::{Align2, Rect, Response, Sense, Ui, pos2, vec2};

use super::branch_filter::{Change, local_ref, remote_ref};
use super::widgets;
use super::{
    Action, Dialog, Refresh, RepoTab, Undo, compare_action, fast_forward_action, merge_action, ops,
    rebase_action,
};
use crate::git::{self, Branch};
use crate::icons;
use crate::settings::{BranchSort, Settings};
use crate::theme::{self, Tokens};

/// The pane's rows: a little taller than the file lists', 2 px more above and below.
const ROW_H: f32 = widgets::ROW_H + 4.0;

/// The button in the pane's header that picks the order of the branches and tags.
fn sort_button(ui: &mut Ui) {
    let mut settings = Settings::get(ui.ctx());
    let resp = icons::small_button(ui, "arrow-up-down", "Sort branches and tags");
    egui::Popup::menu(&resp)
        .style(crate::theme::menu_style)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            let mut changed = false;
            for sort in BranchSort::ALL {
                changed |= ui
                    .radio_value(&mut settings.branch_sort, sort, sort.label())
                    .changed();
            }
            ui.separator();
            changed |= ui
                .checkbox(&mut settings.current_branch_first, "Checked-out branch first")
                .changed();
            if changed {
                settings.set(ui.ctx());
            }
        });
}

/// `branches` in the order `sort` asks for, the checked-out one first with `head_first`.
fn sort_branches(branches: &mut [&Branch], sort: BranchSort, head_first: bool) {
    branches.sort_by(|a, b| {
        let by_name = || natural_cmp(&a.name, &b.name);
        let order = match sort {
            BranchSort::NameAsc => by_name(),
            BranchSort::NameDesc => by_name().reverse(),
            BranchSort::Newest => b.time.cmp(&a.time).then_with(by_name),
            BranchSort::Oldest => a.time.cmp(&b.time).then_with(by_name),
        };
        if head_first {
            b.is_head.cmp(&a.is_head).then(order)
        } else {
            order
        }
    });
}

/// Compares names the way people read them: letters without regard to case, runs of digits
/// by their value (`v2` before `v10`); names equal that way by their bytes.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let run = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        digits.push(c);
                    }
                    digits
                };
                let (m, n) = (run(&mut x), run(&mut y));
                let (m, n) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let order = m.len().cmp(&n.len()).then_with(|| m.cmp(n));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(c), Some(d)) => {
                let order = c.to_lowercase().cmp(d.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                x.next();
                y.next();
            }
        }
    }
}

impl RepoTab {
    pub(super) fn sidebar(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let t = Tokens::get(ui.ctx());
        widgets::panel_header(ui, "Branches", sort_button);
        // Sized like the commit search bar (Ctrl+F).
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            icons::inline(ui, "search", 15.0, t.text_faint);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.branch_filter)
                    .hint_text("Filter branches, tags and submodules")
                    .desired_width((ui.available_width() - 10.0).max(80.0))
                    .margin(egui::Margin::symmetric(8, 4)),
            );
            widgets::text_edit_menu(&resp, &mut self.branch_filter);
        });
        ui.add_space(8.0);

        let filter = self.branch_filter.to_lowercase();
        let matches = |b: &&Branch| filter.is_empty() || b.name.to_lowercase().contains(&filter);
        let idle = self.busy_op.is_none();
        let settings = Settings::get(ui.ctx());
        let (sort, head_first) = (settings.branch_sort, settings.current_branch_first);

        egui::ScrollArea::vertical()
            .id_salt("branches")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;

                self.local_changes_row(ui, acts, idle);
                ui.add_space(4.0);
                let (sep, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter().rect_filled(sep, 0.0, t.separator);
                ui.add_space(4.0);

                let mut local: Vec<&Branch> = self.refs.local.iter().filter(matches).collect();
                sort_branches(&mut local, sort, head_first);
                let count = self.visible_count(local.iter().map(|b| local_ref(&b.name)));
                let (open, add) = widgets::section_with_button(
                    ui,
                    "local",
                    "laptop",
                    "LOCAL",
                    count,
                    true,
                    Some(("plus", "New branch at HEAD…")),
                );
                if add && idle && !self.refs.head.is_empty() {
                    acts.push(Action::Dialog(Dialog::create_branch(
                        &self.refs.head,
                        "HEAD",
                    )));
                }
                if open {
                    for b in local {
                        ui.push_id(("local", &b.name), |ui| {
                            self.local_branch_row(ui, acts, b, idle);
                        });
                    }
                }

                self.remotes_section(ui, acts, idle, &filter, sort);
                self.worktree_section(ui, acts, idle);
                self.submodule_section(ui, acts, idle, &filter);
                self.stash_section(ui, acts, idle);
                self.pr_section(ui, acts, idle);

                let mut tags: Vec<&Branch> = self.refs.tags.iter().filter(matches).collect();
                sort_branches(&mut tags, sort, false);
                if !tags.is_empty()
                    && widgets::section(ui, "tags", "tag", "TAGS", tags.len(), false)
                {
                    for tag in tags {
                        ui.push_id(("tag", &tag.name), |ui| {
                            let r = plain_row(ui, "tag", &tag.name, "");
                            if r.clicked() && !tag.target.is_empty() {
                                acts.push(Action::SelectHash(tag.target.clone()));
                            }
                            egui::Popup::context_menu(&r)
                                .style(crate::theme::menu_style)
                                .show(|ui| {
                                    if ui.button("Copy name").clicked() {
                                        ui.ctx().copy_text(tag.name.clone());
                                    }
                                    if let Some(remote) = self.default_remote() {
                                        if ui
                                            .add_enabled(
                                                idle,
                                                egui::Button::new(format!("Push to {remote}")),
                                            )
                                            .clicked()
                                        {
                                            acts.push(Action::Ops {
                                                label: format!("Push tag {}", tag.name),
                                                ops: ops(&[&[
                                                    "push",
                                                    &remote,
                                                    &format!("refs/tags/{}", tag.name),
                                                ]]),
                                                refresh: Refresh::All,
                                            });
                                        }
                                        if ui
                                            .add_enabled(
                                                idle,
                                                egui::Button::new(format!("Delete from {remote}…")),
                                            )
                                            .clicked()
                                        {
                                            acts.push(Action::Dialog(Dialog::confirm(
                                                "Delete remote tag",
                                                format!("Delete the tag “{}” from {remote}?", tag.name),
                                                "Delete",
                                                ops(&[&[
                                                    "push",
                                                    &remote,
                                                    "--delete",
                                                    &format!("refs/tags/{}", tag.name),
                                                ]]),
                                                None,
                                            )));
                                        }
                                    }
                                    ui.separator();
                                    if ui
                                        .add_enabled(idle, egui::Button::new("Delete tag…"))
                                        .clicked()
                                    {
                                        let mut dlg = Dialog::confirm(
                                            "Delete tag",
                                            format!("Delete the local tag “{}”?", tag.name),
                                            "Delete",
                                            ops(&[&["tag", "-d", &tag.name]]),
                                            None,
                                        );
                                        if !tag.target.is_empty() {
                                            dlg = dlg.undo(Undo::Custom {
                                                undo: ops(&[&["tag", &tag.name, &tag.target]]),
                                                redo: ops(&[&["tag", "-d", &tag.name]]),
                                            });
                                        }
                                        acts.push(Action::Dialog(dlg));
                                    }
                                });
                        });
                    }
                }
            });
    }

    fn local_changes_row(&self, ui: &mut Ui, acts: &mut Vec<Action>, idle: bool) {
        let t = Tokens::get(ui.ctx());
        let active = self.showing_local_changes();
        let count = self.work.conflicted.len() + self.work.unstaged.len() + self.work.staged.len();
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let painter = ui.painter();
        if active {
            painter.rect_filled(rect, 0.0, t.row_selected);
            painter.rect_filled(
                Rect::from_min_size(rect.min, vec2(3.0, ROW_H)),
                0.0,
                t.accent,
            );
        } else if resp.hovered() {
            painter.rect_filled(rect, 0.0, t.hover);
        }
        let y = rect.center().y;
        let icon_color = if !self.work.conflicted.is_empty() {
            t.danger
        } else if active || count > 0 {
            t.accent
        } else {
            t.text_faint
        };
        icons::paint(
            ui,
            Rect::from_center_size(pos2(rect.left() + 14.0, y), vec2(15.0, 15.0)),
            "file-diff",
            14.0,
            icon_color,
        );
        let badge = if !self.work.conflicted.is_empty() {
            Some((count.to_string(), t.danger))
        } else if count > 0 {
            Some((count.to_string(), t.accent))
        } else {
            Some(("clean".to_owned(), t.text_faint))
        };
        let mut right = rect.right() - 10.0;
        if let Some((text, color)) = badge {
            let r = painter.text(
                pos2(right, y),
                Align2::RIGHT_CENTER,
                text,
                theme::medium(11.0),
                color,
            );
            right = r.left() - 8.0;
        }
        let font = if active {
            theme::medium(12.5)
        } else {
            egui::FontId::proportional(12.5)
        };
        painter
            .with_clip_rect(Rect::from_min_max(rect.min, pos2(right, rect.bottom())))
            .text(
                pos2(rect.left() + 28.0, y),
                Align2::LEFT_CENTER,
                "Local changes",
                font,
                t.text,
            );
        let resp = resp.on_hover_text("View working tree changes and commit box");
        if resp.clicked() {
            acts.push(Action::ShowLocalChanges);
        }
        egui::Popup::context_menu(&resp)
            .style(crate::theme::menu_style)
            .show(|ui| {
                if ui.button("Show local changes").clicked() {
                    acts.push(Action::ShowLocalChanges);
                }
                if count > 0
                    && ui
                        .add_enabled(idle, egui::Button::new("Stash all changes…"))
                        .clicked()
                {
                    acts.push(Action::Dialog(self.stash_dialog()));
                }
            });
    }

    fn local_branch_row(&self, ui: &mut Ui, acts: &mut Vec<Action>, b: &Branch, idle: bool) {
        let (ahead, behind) = b.ahead_behind();
        // Checked out in another worktree: git won't check it out here too.
        let elsewhere = self
            .worktrees
            .iter()
            .find(|w| !w.current && w.branch.as_deref() == Some(b.name.as_str()));
        let has_pr = self.prs.iter().any(|p| p.branch == b.name);
        let full = local_ref(&b.name);
        // The checked-out branch is always drawn.
        let eye = (!b.is_head).then(|| self.graph_filter.shows(&full));
        let (r, eye_clicked) = branch_row(
            ui,
            &b.name,
            b.is_head,
            ahead,
            behind,
            b.upstream_gone(),
            elsewhere.is_some(),
            has_pr,
            eye,
        );
        if eye_clicked {
            acts.push(Action::BranchFilter(Change::Toggle(full)));
        }
        let tip = match (b.upstream.is_empty(), b.upstream_gone()) {
            (true, _) => "Not tracking a remote branch".to_owned(),
            (false, true) => format!("{} was deleted on the remote", b.upstream),
            (false, false) => format!("Tracks {}", b.upstream),
        };
        let r = match elsewhere {
            Some(wt) => r.on_hover_text(format!(
                "{tip}\nChecked out in worktree {}\nDouble-click to open it",
                wt.path.display()
            )),
            None => r.on_hover_text(format!("{tip}\nDouble-click to check out")),
        };
        if r.double_clicked() && elsewhere.is_some() {
            if let Some(wt) = elsewhere {
                acts.push(Action::OpenTab(wt.path.clone()));
            }
        } else if r.double_clicked() && !b.is_head && idle {
            acts.push(Action::Checkout(b.name.clone()));
        } else if r.clicked() {
            acts.push(Action::SelectHash(b.target.clone()));
        }
        egui::Popup::context_menu(&r)
            .style(crate::theme::menu_style)
            .show(|ui| self.branch_menu_items(ui, acts, b, idle));
    }

    /// Hide, show or solo a branch in the graph (`full`: its ref name), at the end of its menus.
    pub(super) fn filter_items(&self, ui: &mut Ui, acts: &mut Vec<Action>, full: &str, head: bool) {
        ui.separator();
        let f = &self.graph_filter;
        if f.shows(full) || head {
            if ui
                .add_enabled(!head, egui::Button::new("Hide in the graph"))
                .on_disabled_hover_text("The checked-out branch is always shown")
                .clicked()
            {
                acts.push(Action::BranchFilter(Change::Toggle(full.to_owned())));
            }
        } else if ui.button("Show in the graph").clicked() {
            acts.push(Action::BranchFilter(Change::Toggle(full.to_owned())));
        }
        if f.soloed(full) {
            if ui.button("Stop soloing").clicked() {
                acts.push(Action::BranchFilter(Change::Unsolo(full.to_owned())));
            }
        } else if ui
            .button("Solo")
            .on_hover_text("Show only this branch in the graph (and any others soloed)")
            .clicked()
        {
            acts.push(Action::BranchFilter(Change::Solo(full.to_owned())));
        }
        if f.is_active() && ui.button("Show all branches").clicked() {
            acts.push(Action::BranchFilter(Change::Clear));
        }
    }

    /// `shown/total` while the graph hides or solos branches, else the total.
    fn visible_count(&self, refs: impl Iterator<Item = String>) -> String {
        let refs: Vec<String> = refs.collect();
        if !self.graph_filter.is_active() {
            return refs.len().to_string();
        }
        let current = self.refs.current.as_deref().map(local_ref);
        let shown = refs
            .iter()
            .filter(|r| self.graph_filter.shows(r) || current.as_ref() == Some(*r))
            .count();
        format!("{shown}/{}", refs.len())
    }

    /// Confirm deleting local branch `b`, with optional force and deleting its live upstream.
    pub(super) fn delete_local_branch_dialog(b: &Branch) -> Dialog {
        let mut dlg = Dialog::confirm(
            "Delete branch",
            format!("Delete the local branch “{}”?", b.name),
            "Delete",
            ops(&[&["branch", "-d", &b.name]]),
            Some((
                "Force, even if it isn't merged (-D)".to_owned(),
                ops(&[&["branch", "-D", &b.name]]),
            )),
        );
        if !b.upstream_gone()
            && let Some((remote, short)) = b.upstream_remote_branch()
        {
            dlg = dlg.also(
                format!("Also delete “{short}” from {remote}"),
                ops(&[&["push", remote, "--delete", short]]),
            );
        }
        dlg.undo(Undo::Custom {
            undo: ops(&[&["branch", &b.name, &b.target]]),
            redo: ops(&[&["branch", "-D", &b.name]]),
        })
    }

    /// "Fast-forward to origin/x": the branch moved up to its remote-tracking branch as last
    /// fetched, and nothing else (see [`super::fast_forward_upstream_action`]). Disabled, saying
    /// why, when that isn't a fast-forward.
    fn fast_forward_upstream_item(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        b: &Branch,
        in_other_worktree: bool,
        idle: bool,
    ) {
        if b.upstream.is_empty() {
            return;
        }
        let (ahead, behind) = b.ahead_behind();
        let problem = if b.upstream_gone() {
            Some(format!("{} was deleted on the remote", b.upstream))
        } else if in_other_worktree {
            Some("It is checked out in another worktree: fast-forward it there".to_owned())
        } else if behind == 0 {
            Some(format!("Already up to date with {}", b.upstream))
        } else if ahead > 0 {
            Some(format!(
                "It has {ahead} commit{} {} doesn't: merge or rebase instead",
                if ahead == 1 { "" } else { "s" },
                b.upstream
            ))
        } else {
            None
        };
        let resp = ui
            .add_enabled(
                idle && problem.is_none(),
                egui::Button::new(format!("Fast-forward to {}", b.upstream)),
            )
            .on_hover_text(format!(
                "Move {} up to {} as last fetched; nothing is fetched, checked out or stashed",
                b.name, b.upstream
            ));
        let resp = match &problem {
            Some(problem) => resp.on_disabled_hover_text(problem),
            None => resp,
        };
        if resp.clicked()
            && let Some(action) = super::fast_forward_upstream_action(b)
        {
            acts.push(action);
        }
    }

    /// The menu of a local branch: in the sidebar and in the graph's commit menu.
    pub(super) fn branch_menu_items(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        b: &Branch,
        idle: bool,
    ) {
        let elsewhere = self
            .worktrees
            .iter()
            .find(|w| !w.current && w.branch.as_deref() == Some(b.name.as_str()));
        if let Some(wt) = elsewhere {
            if ui.button("Open its worktree in a tab").clicked() {
                acts.push(Action::OpenTab(wt.path.clone()));
            }
        } else {
            if ui
                .add_enabled(!b.is_head && idle, egui::Button::new("Check out"))
                .clicked()
            {
                acts.push(Action::Checkout(b.name.clone()));
            }
            if ui
                .add_enabled(
                    !b.is_head && idle && !self.refs.head.is_empty(),
                    egui::Button::new("Check out in a new worktree…"),
                )
                .clicked()
            {
                acts.push(Action::Dialog(Dialog::add_worktree(
                    &self.path,
                    Some(&b.name),
                    &self.refs.head,
                )));
            }
        }
        self.fast_forward_upstream_item(ui, acts, b, elsewhere.is_some(), idle);
        if let Some(current) = self.refs.current.as_deref()
            && !b.is_head
        {
            ui.separator();
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(format!("Fast-forward {current} to {}", b.name)),
                )
                .clicked()
            {
                acts.push(fast_forward_action(&b.name, current));
            }
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(format!("Merge {} into {current}", b.name)),
                )
                .clicked()
            {
                acts.push(merge_action(&b.name, current));
            }
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(format!("Rebase {current} onto {}", b.name)),
                )
                .clicked()
            {
                acts.push(rebase_action(&b.name, current));
            }
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(format!("Interactive rebase {current} onto {}…", b.name)),
                )
                .clicked()
            {
                acts.push(Action::InteractiveRebase {
                    onto: b.name.clone(),
                    label: b.name.clone(),
                });
            }
            ui.separator();
            super::graph_view::compare_items(ui, acts, &b.name, current);
        }
        // The checked-out branch against the one pull requests usually go to.
        if b.is_head
            && let Some(base) = self.base_ref()
            && base != b.name
        {
            ui.separator();
            if ui
                .button(format!("Changes {} adds to {base}", b.name))
                .on_hover_text("What it changed since the two diverged, as a pull request shows it")
                .clicked()
            {
                acts.push(compare_action(&base, &b.name, true));
            }
        }
        ui.separator();
        let push_dest = if !b.upstream_gone()
            && let Some((remote, rb)) = b.upstream_remote_branch()
        {
            Some((remote.to_owned(), Some(rb.to_owned())))
        } else {
            self.default_remote().map(|r| (r, None))
        };
        if let Some((remote, remote_branch)) = push_dest
            && ui
                .add_enabled(idle, egui::Button::new(format!("Push to {remote}")))
                .clicked()
        {
            let args = match remote_branch {
                Some(rb) => ops(&[&["push", &remote, &format!("{}:{rb}", b.name)]]),
                None => ops(&[&["push", "-u", &remote, &b.name]]),
            };
            acts.push(Action::Ops {
                label: format!("Push {}", b.name),
                ops: args,
                refresh: Refresh::All,
            });
        }
        self.force_push_item(ui, acts, b, idle);
        let has_remote_branches = self
            .refs
            .remotes
            .iter()
            .flat_map(|(_, l)| l.iter())
            .any(|r| !r.name.ends_with("/HEAD"));
        let track_label = if b.upstream.is_empty() {
            "Track remote branch…"
        } else {
            "Change tracked branch…"
        };
        let track_btn = ui.add_enabled(idle && has_remote_branches, egui::Button::new(track_label));
        let track_btn = if !has_remote_branches {
            track_btn.on_disabled_hover_text("No remote branches available")
        } else {
            track_btn
        };
        let track_btn = if !b.upstream.is_empty() {
            track_btn.on_hover_text(format!("Currently tracks {}", b.upstream))
        } else {
            track_btn
        };
        if track_btn.clicked() {
            acts.push(Action::Dialog(self.track_remote_dialog(b)));
        }
        if !b.upstream.is_empty()
            && ui
                .add_enabled(idle, egui::Button::new(format!("Untrack ({})", b.upstream)))
                .clicked()
        {
            acts.push(super::branch_ops::untrack_action(
                &b.name,
                &b.upstream,
                b.upstream_gone(),
            ));
        }
        if let Some(forge) = self.forge {
            let existing = self.prs.iter().find(|p| p.branch == b.name);
            match existing {
                Some(pr) => {
                    if ui
                        .button(format!("Open {} #{}", forge.noun(), pr.number))
                        .clicked()
                    {
                        acts.push(Action::OpenUrl(pr.url.clone()));
                    }
                }
                None => {
                    if ui
                        .add_enabled(idle, egui::Button::new(format!("Create {}…", forge.noun())))
                        .clicked()
                    {
                        let base = self.default_base();
                        let title = self
                            .history
                            .iter()
                            .find(|c| c.hash == b.target)
                            .map(|c| c.summary.clone())
                            .unwrap_or_default();
                        acts.push(Action::Dialog(Dialog::create_pr(
                            forge.noun(),
                            &b.name,
                            &base,
                            &title,
                        )));
                    }
                }
            }
        }
        ui.separator();
        if ui.button("Create branch here…").clicked() {
            acts.push(Action::Dialog(Dialog::create_branch(&b.target, &b.name)));
        }
        if ui.button("Create tag here…").clicked() {
            acts.push(Action::Dialog(Dialog::tag(
                &b.target,
                &b.name,
                self.default_remote(),
            )));
        }
        if ui.add_enabled(idle, egui::Button::new("Rename…")).clicked() {
            acts.push(Action::Dialog(Dialog::rename_branch(&b.name)));
        }
        if ui.button("Copy name").clicked() {
            ui.ctx().copy_text(b.name.clone());
        }
        self.filter_items(ui, acts, &local_ref(&b.name), b.is_head);
        ui.separator();
        if ui
            .add_enabled(!b.is_head && idle, egui::Button::new("Delete…"))
            .on_disabled_hover_text("Can't delete the checked-out branch")
            .clicked()
        {
            acts.push(Action::Dialog(Self::delete_local_branch_dialog(b)));
        }
    }

    /// The branch pull requests go into by default: the remote's HEAD, else main / master.
    /// The branch to compare the current one with: the local default base, or its remote one.
    fn base_ref(&self) -> Option<String> {
        let base = self.default_base();
        if self.refs.local.iter().any(|b| b.name == base) {
            return Some(base);
        }
        let remote = format!("{}/{base}", self.default_remote()?);
        self.refs
            .remotes
            .iter()
            .flat_map(|(_, list)| list)
            .any(|b| b.name == remote)
            .then_some(remote)
    }

    fn default_base(&self) -> String {
        let remote = self.default_remote().unwrap_or_else(|| "origin".to_owned());
        let names: Vec<&str> = self
            .refs
            .remotes
            .iter()
            .filter(|(r, _)| *r == remote)
            .flat_map(|(_, list)| list.iter().map(|b| b.name.as_str()))
            .collect();
        for candidate in ["main", "master", "develop"] {
            if names.contains(&format!("{remote}/{candidate}").as_str()) {
                return candidate.to_owned();
            }
        }
        "main".to_owned()
    }

    fn remotes_section(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        idle: bool,
        filter: &str,
        sort: BranchSort,
    ) {
        let t = Tokens::get(ui.ctx());
        let (open, add) = widgets::section_with_button(
            ui,
            "remotes",
            "cloud",
            "REMOTES",
            self.remotes.len(),
            true,
            Some(("plus", "Add a remote…")),
        );
        if add && idle {
            acts.push(Action::Dialog(Dialog::remote(None)));
        }
        if !open {
            return;
        }
        let current = self.refs.current.clone();
        for remote in &self.remotes {
            let mut branches: Vec<&Branch> = self
                .refs
                .remotes
                .iter()
                .filter(|(r, _)| *r == remote.name)
                .flat_map(|(_, list)| list.iter())
                .filter(|b| filter.is_empty() || b.name.to_lowercase().contains(filter))
                .collect();
            sort_branches(&mut branches, sort, false);
            ui.push_id(("remote", &remote.name), |ui| {
                // A collapsible row per remote.
                let id = ui.id().with(("remote-open", &remote.name));
                let mut expanded = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(true);
                let (rect, r) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                if r.hovered() {
                    ui.painter().rect_filled(rect, 0.0, t.hover);
                }
                if r.clicked() {
                    expanded = !expanded;
                    ui.data_mut(|d| d.insert_temp(id, expanded));
                }
                let y = rect.center().y;
                icons::paint(
                    ui,
                    Rect::from_center_size(pos2(rect.left() + 24.0, y), vec2(12.0, 12.0)),
                    if expanded {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    11.0,
                    t.text_faint,
                );
                icons::paint(
                    ui,
                    Rect::from_center_size(pos2(rect.left() + 40.0, y), vec2(14.0, 14.0)),
                    "cloud",
                    13.0,
                    t.icon,
                );
                let p = ui.painter_at(rect);
                p.text(
                    pos2(rect.left() + 54.0, y),
                    Align2::LEFT_CENTER,
                    &remote.name,
                    theme::medium(12.5),
                    t.text,
                );
                p.text(
                    pos2(rect.right() - 10.0, y),
                    Align2::RIGHT_CENTER,
                    self.visible_count(branches.iter().map(|b| remote_ref(&b.name))),
                    theme::medium(11.0),
                    t.text_faint,
                );
                let r = r.on_hover_text(&remote.url);
                egui::Popup::context_menu(&r).style(crate::theme::menu_style).show(|ui| {
                    if ui.add_enabled(idle, egui::Button::new(format!("Fetch {}", remote.name))).clicked() {
                        acts.push(Action::Ops {
                            label: format!("Fetch {}", remote.name),
                            ops: ops(&[&["fetch", "--prune", &remote.name]]),
                            refresh: Refresh::All,
                        });
                    }
                    if ui.add_enabled(idle, egui::Button::new("Edit…")).clicked() {
                        acts.push(Action::Dialog(Dialog::remote(Some((remote.name.clone(), remote.url.clone())))));
                    }
                    if ui.button("Copy URL").clicked() {
                        ui.ctx().copy_text(remote.url.clone());
                    }
                    ui.separator();
                    if ui.add_enabled(idle, egui::Button::new("Remove…")).clicked() {
                        acts.push(Action::Dialog(Dialog::confirm(
                            "Remove remote",
                            format!(
                                "Remove the remote “{}” ({})? Its branches disappear from this repository; nothing changes on the server.",
                                remote.name, remote.url
                            ),
                            "Remove",
                            ops(&[&["remote", "remove", &remote.name]]),
                            None,
                        )));
                    }
                });
                if !expanded {
                    return;
                }
                for b in branches {
                    let short = b
                        .name
                        .strip_prefix(&format!("{}/", remote.name))
                        .unwrap_or(&b.name);
                    let has_pr = self.prs.iter().any(|p| p.branch == short);
                    ui.push_id(("remote-branch", &b.name), |ui| {
                        let full = remote_ref(&b.name);
                        let (r, eye_clicked) =
                            branch_row_indented(ui, short, has_pr, self.graph_filter.shows(&full));
                        if eye_clicked {
                            acts.push(Action::BranchFilter(Change::Toggle(full.clone())));
                        }
                        let r = r.on_hover_text(format!("{}\nDouble-click to check out", b.name));
                        if r.double_clicked() && idle {
                            acts.push(Action::CheckoutRemote(b.name.clone()));
                        } else if r.clicked() {
                            acts.push(Action::SelectHash(b.target.clone()));
                        }
                        egui::Popup::context_menu(&r)
                            .style(crate::theme::menu_style)
                            .show(|ui| {
                                if ui
                                    .add_enabled(idle, egui::Button::new("Check out as local branch"))
                                    .clicked()
                                {
                                    acts.push(Action::CheckoutRemote(b.name.clone()));
                                }
                                if let Some(current) = current.as_deref() {
                                    if ui
                                        .add_enabled(
                                            idle,
                                            egui::Button::new(format!("Fast-forward {current} to it")),
                                        )
                                        .clicked()
                                    {
                                        acts.push(fast_forward_action(&b.name, current));
                                    }
                                    if ui
                                        .add_enabled(
                                            idle,
                                            egui::Button::new(format!("Merge into {current}")),
                                        )
                                        .clicked()
                                    {
                                        acts.push(merge_action(&b.name, current));
                                    }
                                    if ui
                                        .add_enabled(
                                            idle,
                                            egui::Button::new(format!("Rebase {current} onto it")),
                                        )
                                        .clicked()
                                    {
                                        acts.push(rebase_action(&b.name, current));
                                    }
                                    ui.separator();
                                    super::graph_view::compare_items(ui, acts, &b.name, current);
                                    ui.separator();
                                } else {
                                    ui.separator();
                                }
                                if !b.name.ends_with("/HEAD") {
                                    self.remote_branch_track_menu_items(
                                        ui,
                                        acts,
                                        &b.name,
                                        current.as_deref(),
                                        idle,
                                    );
                                    ui.separator();
                                }
                                if ui.button("Create branch here…").clicked() {
                                    acts.push(Action::Dialog(Dialog::create_branch(&b.target, &b.name)));
                                }
                                ui.separator();
                                if ui.button("Copy name").clicked() {
                                    ui.ctx().copy_text(b.name.clone());
                                }
                                self.filter_items(ui, acts, &full, false);
                                ui.separator();
                                if ui
                                    .add_enabled(idle, egui::Button::new("Delete from remote…"))
                                    .clicked()
                                {
                                    acts.push(Action::Dialog(Dialog::confirm(
                                    "Delete remote branch",
                                    format!(
                                        "Delete “{short}” from {}? This changes the remote for everyone.",
                                        remote.name
                                    ),
                                    "Delete",
                                    ops(&[&["push", &remote.name, "--delete", short]]),
                                    None,
                                )));
                                }
                            });
                    });
                }
            });
        }
    }

    fn stash_section(&self, ui: &mut Ui, acts: &mut Vec<Action>, idle: bool) {
        let t = Tokens::get(ui.ctx());
        let (open, add) = widgets::section_with_button(
            ui,
            "stashes",
            "archive",
            "STASHES",
            self.stashes.len(),
            true,
            Some(("plus", "Stash changes…")),
        );
        if add && idle && !self.work.is_clean() {
            acts.push(Action::Dialog(self.stash_dialog()));
        }
        if !open {
            return;
        }
        let now = git::now();
        for (i, stash) in self.stashes.iter().enumerate() {
            ui.push_id(("stash", i, &stash.name), |ui| {
                let (rect, r) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
                if self.stash_view == Some(i) {
                    ui.painter().rect_filled(rect, 0.0, t.row_selected);
                } else if r.hovered() {
                    ui.painter().rect_filled(rect, 0.0, t.hover);
                }
                let y = rect.center().y;
                icons::paint(
                    ui,
                    Rect::from_center_size(pos2(rect.left() + 34.0, y), vec2(16.0, 16.0)),
                    "archive",
                    13.0,
                    t.text_faint,
                );
                let p = ui.painter_at(rect);
                p.text(
                    pos2(rect.left() + 48.0, y - 7.0),
                    Align2::LEFT_CENTER,
                    &stash.message,
                    egui::FontId::proportional(12.5),
                    t.text,
                );
                p.text(
                    pos2(rect.left() + 48.0, y + 8.0),
                    Align2::LEFT_CENTER,
                    format!("{} · {}", stash.name, git::relative_time(stash.time, now)),
                    egui::FontId::proportional(11.0),
                    t.text_faint,
                );
                if r.clicked() {
                    acts.push(Action::SelectStash(i));
                }
                egui::Popup::context_menu(&r)
                    .style(crate::theme::menu_style)
                    .show(|ui| stash_menu_items(ui, acts, &stash.name, idle));
            });
        }
    }

    fn pr_section(&self, ui: &mut Ui, acts: &mut Vec<Action>, idle: bool) {
        let Some(forge) = self.forge else { return };
        let t = Tokens::get(ui.ctx());
        let title = match forge {
            crate::forge::Forge::GitHub => "PULL REQUESTS",
            crate::forge::Forge::GitLab => "MERGE REQUESTS",
        };
        if !widgets::section(ui, "prs", "git-pull-request", title, self.prs.len(), true) {
            return;
        }
        if let Some(err) = &self.pr_error {
            let (rect, r) =
                ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
            ui.painter_at(rect).text(
                pos2(rect.left() + 34.0, rect.center().y),
                Align2::LEFT_CENTER,
                err.lines().next().unwrap_or_default(),
                egui::FontId::proportional(11.0),
                t.text_faint,
            );
            r.on_hover_text(err.as_str());
            return;
        }
        for pr in &self.prs {
            ui.push_id(("pr", pr.number), |ui| {
                let (rect, r) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
                if r.hovered() {
                    ui.painter().rect_filled(rect, 0.0, t.hover);
                }
                let y = rect.center().y;
                icons::paint(
                    ui,
                    Rect::from_center_size(pos2(rect.left() + 34.0, y), vec2(16.0, 16.0)),
                    "git-pull-request",
                    13.0,
                    if pr.draft { t.text_faint } else { t.success },
                );
                let p = ui.painter_at(rect);
                p.text(
                    pos2(rect.left() + 48.0, y - 7.0),
                    Align2::LEFT_CENTER,
                    format!("#{} {}", pr.number, pr.title),
                    egui::FontId::proportional(12.5),
                    t.text,
                );
                let sub = format!(
                    "{} · {}{}",
                    pr.branch,
                    pr.author,
                    if pr.draft { " · draft" } else { "" }
                );
                p.text(
                    pos2(rect.left() + 48.0, y + 8.0),
                    Align2::LEFT_CENTER,
                    sub,
                    egui::FontId::proportional(11.0),
                    t.text_faint,
                );
                let r = r.on_hover_text(format!("{}\nDouble-click to open in the browser", pr.url));
                // The branch's tip, local or on a remote.
                let remote_match = self
                    .refs
                    .remotes
                    .iter()
                    .flat_map(|(_, l)| l.iter())
                    .find(|b| b.name.split_once('/').map(|(_, n)| n) == Some(pr.branch.as_str()));
                let target = self
                    .refs
                    .local
                    .iter()
                    .find(|b| b.name == pr.branch)
                    .or(remote_match)
                    .map(|b| b.target.clone());
                if r.double_clicked() {
                    acts.push(Action::OpenUrl(pr.url.clone()));
                } else if r.clicked()
                    && let Some(target) = &target
                {
                    acts.push(Action::SelectHash(target.clone()));
                }
                egui::Popup::context_menu(&r)
                    .style(crate::theme::menu_style)
                    .show(|ui| {
                        if ui.button("Open in the browser").clicked() {
                            acts.push(Action::OpenUrl(pr.url.clone()));
                        }
                        if ui
                            .add_enabled(idle, egui::Button::new(format!("Check out {}", pr.branch)))
                            .clicked()
                        {
                            if self.refs.local.iter().any(|b| b.name == pr.branch) {
                                acts.push(Action::Checkout(pr.branch.clone()));
                            } else if let Some(b) = remote_match {
                                acts.push(Action::CheckoutRemote(b.name.clone()));
                            } else {
                                let remote =
                                    self.default_remote().unwrap_or_else(|| "origin".to_owned());
                                acts.push(Action::CheckoutRemote(format!("{remote}/{}", pr.branch)));
                            }
                        }
                        if ui.button("Copy URL").clicked() {
                            ui.ctx().copy_text(pr.url.clone());
                        }
                    });
            });
        }
    }
}

/// Apply / pop / drop for a stash (sidebar and detail pane).
pub(super) fn stash_menu_items(ui: &mut Ui, acts: &mut Vec<Action>, name: &str, idle: bool) {
    if ui
        .add_enabled(idle, egui::Button::new("Apply"))
        .on_hover_text("Bring the changes back and keep the stash")
        .clicked()
    {
        acts.push(Action::Ops {
            label: format!("Apply {name}"),
            ops: ops(&[&["stash", "apply", "--index", name]]),
            refresh: Refresh::All,
        });
    }
    if ui
        .add_enabled(idle, egui::Button::new("Pop"))
        .on_hover_text("Bring the changes back and drop the stash")
        .clicked()
    {
        acts.push(Action::Ops {
            label: format!("Pop {name}"),
            ops: ops(&[&["stash", "pop", "--index", name]]),
            refresh: Refresh::All,
        });
    }
    ui.separator();
    super::patch::stash_patch_item(ui, acts, name);
    if ui.add_enabled(idle, egui::Button::new("Drop…")).clicked() {
        acts.push(Action::Dialog(Dialog::confirm(
            "Drop stash",
            format!("Delete {name}? Its changes are gone for good."),
            "Drop",
            ops(&[&["stash", "drop", name]]),
            None,
        )));
    }
}

impl RepoTab {
    fn worktree_section(&self, ui: &mut Ui, acts: &mut Vec<Action>, idle: bool) {
        let t = Tokens::get(ui.ctx());
        let (open, add) = widgets::section_with_button(
            ui,
            "worktrees",
            "folder-tree",
            "WORKTREES",
            self.worktrees.len(),
            true,
            (!self.refs.head.is_empty()).then_some(("plus", "Add a worktree…")),
        );
        if add && idle && !self.refs.head.is_empty() {
            acts.push(Action::Dialog(Dialog::add_worktree(
                &self.path,
                None,
                &self.refs.head,
            )));
        }
        if !open {
            return;
        }
        let any_prunable = self.worktrees.iter().any(|w| w.prunable.is_some());
        for wt in &self.worktrees {
            ui.push_id(("wt", &wt.path), |ui| {
                let (rect, r) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
                let painter = ui.painter_at(rect);
                if wt.current {
                    painter.rect_filled(rect, 0.0, t.accent.gamma_multiply(0.14));
                }
                if r.hovered() {
                    painter.rect_filled(rect, 0.0, t.hover);
                }
                let y = rect.center().y;
                let icon = if wt.current { "check" } else { "folder-git-2" };
                icons::paint(
                    ui,
                    Rect::from_center_size(pos2(rect.left() + 34.0, y), vec2(16.0, 16.0)),
                    icon,
                    14.0,
                    if wt.current { t.accent } else { t.text_faint },
                );
                let font = if wt.current {
                    theme::semibold(12.5)
                } else {
                    egui::FontId::proportional(12.5)
                };
                let mut right = rect.right() - 8.0;
                for (on, label, color) in [
                    (wt.prunable.is_some(), "missing", t.danger),
                    (wt.locked.is_some(), "locked", t.warning),
                    (wt.main, "main", t.text_faint),
                ] {
                    if on {
                        let r = painter.text(
                            pos2(right, y),
                            Align2::RIGHT_CENTER,
                            label,
                            theme::medium(10.5),
                            color,
                        );
                        right = r.left() - 6.0;
                    }
                }
                let clip =
                    painter.with_clip_rect(Rect::from_min_max(rect.min, pos2(right, rect.bottom())));
                clip.text(
                    pos2(rect.left() + 48.0, y - 7.0),
                    Align2::LEFT_CENTER,
                    wt.name(),
                    font,
                    t.text,
                );
                let sub = match (&wt.branch, wt.bare) {
                    (_, true) => "bare".to_owned(),
                    (Some(branch), _) => branch.clone(),
                    (None, _) => format!("detached at {}", wt.head.get(..8).unwrap_or(&wt.head)),
                };
                clip.text(
                    pos2(rect.left() + 48.0, y + 8.0),
                    Align2::LEFT_CENTER,
                    sub,
                    egui::FontId::proportional(11.0),
                    t.text_faint,
                );

                let mut tip = wt.path.display().to_string();
                if let Some(reason) = wt.locked.as_deref().filter(|r| !r.is_empty()) {
                    tip.push_str(&format!("\nLocked: {reason}"));
                }
                if let Some(reason) = &wt.prunable {
                    tip.push_str(&format!("\nMissing: {reason}"));
                }
                if !wt.current && !wt.bare {
                    tip.push_str("\nDouble-click to open in a tab");
                }
                let r = r.on_hover_text(tip);
                let openable = !wt.current && !wt.bare && wt.prunable.is_none();
                if r.double_clicked() && openable {
                    acts.push(Action::OpenTab(wt.path.clone()));
                } else if r.clicked() && !wt.head.is_empty() {
                    acts.push(Action::SelectHash(wt.head.clone()));
                }
                let path = wt.path.display().to_string();
                egui::Popup::context_menu(&r).style(crate::theme::menu_style).show(|ui| {
                    if ui
                        .add_enabled(openable, egui::Button::new("Open in a tab"))
                        .clicked()
                    {
                        acts.push(Action::OpenTab(wt.path.clone()));
                    }
                    if ui.button("Open terminal here").clicked() {
                        acts.push(Action::NewTerminal { shell: None, cwd: Some(wt.path.clone()) });
                    }
                    if ui.button(super::reveal_label()).clicked() {
                        let _ = open::that_detached(&wt.path);
                    }
                    if ui.button("Copy path").clicked() {
                        ui.ctx().copy_text(path.clone());
                    }
                    if !wt.main {
                        ui.separator();
                        let (label, args) = if wt.locked.is_some() {
                            ("Unlock", vec!["worktree", "unlock", path.as_str()])
                        } else {
                            ("Lock", vec!["worktree", "lock", path.as_str()])
                        };
                        if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                            acts.push(Action::Ops {
                                label: format!("{label} worktree {}", wt.name()),
                                ops: ops(&[&args]),
                                refresh: Refresh::All,
                            });
                        }
                        if ui
                            .add_enabled(idle && !wt.current && !wt.bare, egui::Button::new("Remove…"))
                            .on_disabled_hover_text("Open another worktree to remove this one")
                            .clicked()
                        {
                            acts.push(Action::Dialog(Dialog::confirm(
                                "Remove worktree",
                                format!(
                                    "Remove the worktree at {path}? Its folder is deleted; the branch stays."
                                ),
                                "Remove",
                                ops(&[&["worktree", "remove", &path]]),
                                Some((
                                    "Force, even with uncommitted changes or locked".to_owned(),
                                    ops(&[&["worktree", "remove", "--force", "--force", &path]]),
                                )),
                            )));
                        }
                    }
                    if any_prunable {
                        ui.separator();
                        if ui.add_enabled(idle, egui::Button::new("Prune missing worktrees")).clicked() {
                            acts.push(Action::Ops {
                                label: "Prune worktrees".into(),
                                ops: ops(&[&["worktree", "prune"]]),
                                refresh: Refresh::All,
                            });
                        }
                    }
                });
            });
        }
    }
}

/// A branch: icon (a check for the checked-out one), name, ahead/behind counts.
#[allow(clippy::too_many_arguments)]
fn branch_row(
    ui: &mut Ui,
    name: &str,
    is_head: bool,
    ahead: u32,
    behind: u32,
    gone: bool,
    in_worktree: bool,
    has_pr: bool,
    eye: Option<bool>,
) -> (Response, bool) {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let painter = ui.painter_at(rect);
    if is_head {
        painter.rect_filled(rect, 0.0, t.accent.gamma_multiply(0.14));
    }
    if resp.hovered() {
        painter.rect_filled(rect, 0.0, t.hover);
    }
    let y = rect.center().y;
    let (mut right, eye_clicked) = match eye {
        Some(shown) => eye_button(ui, &resp, rect, shown),
        None => (rect.right() - 8.0, false),
    };
    let shown = eye.unwrap_or(true);
    let icon_rect = Rect::from_center_size(pos2(rect.left() + 34.0, y), vec2(16.0, 16.0));
    if is_head {
        icons::paint(ui, icon_rect, "check", 14.0, t.accent);
    } else {
        icons::paint(ui, icon_rect, "git-branch", 13.0, t.text_faint);
    }

    let small = theme::mono(11.0);
    if has_pr {
        let ir = Rect::from_center_size(pos2(right - 6.0, y), vec2(12.0, 12.0));
        icons::paint(ui, ir, "git-pull-request", 12.0, t.success);
        right = ir.left() - 6.0;
    }
    if in_worktree {
        let ir = Rect::from_center_size(pos2(right - 6.0, y), vec2(12.0, 12.0));
        icons::paint(ui, ir, "folder-tree", 12.0, t.text_dim);
        right = ir.left() - 6.0;
    }
    if gone {
        let r = painter.text(
            pos2(right, y),
            Align2::RIGHT_CENTER,
            "gone",
            theme::medium(10.5),
            t.text_faint,
        );
        right = r.left() - 6.0;
    }
    for (n, icon) in [(behind, "arrow-down"), (ahead, "arrow-up")] {
        if n > 0 {
            let r = painter.text(
                pos2(right, y),
                Align2::RIGHT_CENTER,
                n.to_string(),
                small.clone(),
                t.text_dim,
            );
            let ir = Rect::from_center_size(pos2(r.left() - 7.0, y), vec2(12.0, 12.0));
            icons::paint(ui, ir, icon, 11.0, t.text_dim);
            right = ir.left() - 6.0;
        }
    }
    let font = if is_head {
        theme::semibold(12.5)
    } else {
        egui::FontId::proportional(12.5)
    };
    painter
        .with_clip_rect(Rect::from_min_max(
            pos2(rect.left(), rect.top()),
            pos2(right, rect.bottom()),
        ))
        .text(
            pos2(rect.left() + 48.0, y),
            Align2::LEFT_CENTER,
            name,
            font,
            if shown { t.text } else { t.text_faint },
        );
    (resp, eye_clicked)
}

/// The eye at a branch row's right end: on hover, or always while the branch is hidden.
/// Returns where the row's other things end, and whether it was clicked.
fn eye_button(ui: &mut Ui, row: &Response, rect: Rect, shown: bool) -> (f32, bool) {
    let t = Tokens::get(ui.ctx());
    let r = Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), vec2(20.0, 20.0));
    let b = ui
        .interact(r, row.id.with("eye"), Sense::click())
        .on_hover_text(if shown {
            "Hide in the graph"
        } else {
            "Show in the graph"
        });
    if row.hovered() || b.hovered() || !shown {
        if b.hovered() {
            ui.painter().rect_filled(r, t.radius_sm, t.pressed);
        }
        let color = if b.hovered() { t.text } else { t.text_faint };
        icons::paint(ui, r, if shown { "eye" } else { "eye-off" }, 13.0, color);
    }
    (r.left() - 4.0, b.clicked())
}

/// A remote's branch, one level in under its remote, with its eye (see [`eye_button`]).
fn branch_row_indented(ui: &mut Ui, name: &str, has_pr: bool, shown: bool) -> (Response, bool) {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, t.hover);
    }
    let y = rect.center().y;
    let (mut right, eye_clicked) = eye_button(ui, &resp, rect, shown);
    icons::paint(
        ui,
        Rect::from_center_size(pos2(rect.left() + 52.0, y), vec2(16.0, 16.0)),
        "git-branch",
        13.0,
        t.text_faint,
    );
    if has_pr {
        let ir = Rect::from_center_size(pos2(right - 6.0, y), vec2(12.0, 12.0));
        icons::paint(ui, ir, "git-pull-request", 12.0, t.success);
        right = ir.left() - 6.0;
    }
    ui.painter_at(rect.with_max_x(right)).text(
        pos2(rect.left() + 66.0, y),
        Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(12.5),
        if shown { t.text } else { t.text_faint },
    );
    (resp, eye_clicked)
}

/// An icon, a name and optional faint text after it.
fn plain_row(ui: &mut Ui, icon: &str, name: &str, sub: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, t.hover);
    }
    let y = rect.center().y;
    icons::paint(
        ui,
        Rect::from_center_size(pos2(rect.left() + 34.0, y), vec2(16.0, 16.0)),
        icon,
        13.0,
        t.text_faint,
    );
    let p = ui.painter_at(rect);
    let r = p.text(
        pos2(rect.left() + 48.0, y),
        Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(12.5),
        t.text,
    );
    if !sub.is_empty() {
        p.text(
            pos2(r.right() + 8.0, y),
            Align2::LEFT_CENTER,
            sub,
            egui::FontId::proportional(11.0),
            t.text_faint,
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::test_support::*;

    fn branch(name: &str, time: i64, is_head: bool) -> Branch {
        Branch {
            name: name.to_owned(),
            target: String::new(),
            is_head,
            upstream: String::new(),
            upstream_ref: String::new(),
            track: String::new(),
            time,
        }
    }

    fn names(list: &[&Branch]) -> Vec<String> {
        list.iter().map(|b| b.name.clone()).collect()
    }

    #[test]
    fn names_compare_without_case_and_numbers_by_value() {
        use std::cmp::Ordering::*;
        assert_eq!(natural_cmp("v2", "v10"), Less);
        assert_eq!(natural_cmp("release-1.10", "release-1.9"), Greater);
        assert_eq!(natural_cmp("Feature", "fix"), Less);
        assert_eq!(natural_cmp("a", "a/b"), Less);
        assert_eq!(natural_cmp("v02", "v2"), Less);
        assert_eq!(natural_cmp("main", "main"), Equal);
    }

    #[test]
    fn branches_sort_by_name_or_commit_time() {
        let (a, b, c) = (
            branch("beta", 300, false),
            branch("Alpha", 100, false),
            branch("gamma", 200, true),
        );
        let mut list = vec![&a, &b, &c];
        sort_branches(&mut list, BranchSort::NameAsc, false);
        assert_eq!(names(&list), ["Alpha", "beta", "gamma"]);
        sort_branches(&mut list, BranchSort::NameDesc, false);
        assert_eq!(names(&list), ["gamma", "beta", "Alpha"]);
        sort_branches(&mut list, BranchSort::Newest, false);
        assert_eq!(names(&list), ["beta", "gamma", "Alpha"]);
        sort_branches(&mut list, BranchSort::Oldest, false);
        assert_eq!(names(&list), ["Alpha", "gamma", "beta"]);
        sort_branches(&mut list, BranchSort::NameAsc, true);
        assert_eq!(names(&list), ["gamma", "Alpha", "beta"]);
    }

    #[test]
    fn refs_carry_their_commit_time() {
        let dir = repo("sb-ref-time");
        let refs = git::refs(&dir).unwrap_or_default();
        assert!(refs.local.iter().all(|b| b.time > 0), "{:?}", refs.local);
    }

    /// `work` with `feat` pushed and tracked, then moved on by someone else and fetched:
    /// `feat` is one commit behind `origin/feat`. Returns (work, the new origin/feat).
    fn behind(name: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let (work, bare) = repo_with_remote(name);
        git(&work, &["branch", "feat"]);
        git(&work, &["push", "-q", "-u", "origin", "feat"]);
        let other = clone_of(&bare, &format!("{name}-other"));
        configure(&other);
        git(&other, &["checkout", "-q", "feat"]);
        let new = commit_file(&other, "b.txt", "two\n", "from someone else");
        git(&other, &["push", "-q", "origin", "feat"]);
        git(&work, &["fetch", "-q"]);
        (work, other, new)
    }

    /// The local branch `name` as the tab lists it (a branch of that name with nothing in it
    /// when it isn't listed, which the assertions after then catch).
    fn local(f: &Fixture, name: &str) -> Branch {
        let found = f.tab.refs.local.iter().find(|b| b.name == name).cloned();
        assert!(found.is_some(), "{name} is not listed");
        found.unwrap_or_else(|| branch(name, 0, false))
    }

    /// Runs the fast-forward of `name` to its upstream.
    fn fast_forward(f: &mut Fixture, name: &str) {
        let action = super::super::fast_forward_upstream_action(&local(f, name));
        assert!(action.is_some(), "{name} tracks nothing");
        if let Some(action) = action {
            f.run(action);
        }
    }

    #[test]
    fn fast_forwards_another_branch_to_its_upstream_without_checking_it_out() {
        let (work, _, new) = behind("sb-ff-other");
        // A fetch from `.` with pruning on once deleted the branch instead of moving it.
        git(&work, &["config", "fetch.prune", "true"]);
        let mut f = Fixture::open(&work);
        let main = git(&work, &["rev-parse", "main"]);
        let was = git(&work, &["rev-parse", "feat"]);
        assert_eq!(local(&f, "feat").ahead_behind(), (0, 1));
        fast_forward(&mut f, "feat");
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&work, &["rev-parse", "feat"]), new);
        assert_eq!(git(&work, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(git(&work, &["rev-parse", "HEAD"]), main);
        f.run(Action::Undo);
        assert_eq!(git(&work, &["rev-parse", "feat"]), was);
        f.run(Action::Redo);
        assert_eq!(git(&work, &["rev-parse", "feat"]), new);
    }

    #[test]
    fn fast_forwards_the_checked_out_branch_to_its_upstream() {
        let (work, _, new) = behind("sb-ff-head");
        git(&work, &["checkout", "-q", "feat"]);
        let mut f = Fixture::open(&work);
        fast_forward(&mut f, "feat");
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&work, &["rev-parse", "HEAD"]), new);
        assert_eq!(read(&work, "b.txt"), "two\n");
    }

    #[test]
    fn a_diverged_branch_is_not_moved() {
        let (work, _, _) = behind("sb-ff-diverged");
        git(&work, &["checkout", "-q", "feat"]);
        let mine = commit_file(&work, "c.txt", "mine\n", "mine");
        git(&work, &["checkout", "-q", "main"]);
        let mut f = Fixture::open(&work);
        assert_eq!(local(&f, "feat").ahead_behind(), (1, 1));
        fast_forward(&mut f, "feat");
        assert!(f.failed(), "{:?}", f.status());
        assert_eq!(git(&work, &["rev-parse", "feat"]), mine);
    }

    #[test]
    fn no_fast_forward_without_an_upstream() {
        let b = branch("lonely", 1, false);
        assert!(super::super::fast_forward_upstream_action(&b).is_none());
    }

    #[test]
    fn delete_branch_dialog_includes_remote_only_when_upstream_is_live() {
        use super::Dialog;
        let mut tracked = branch("feat", 1, false);
        tracked.upstream = "origin/topic-a".to_owned();
        let Dialog::Confirm { also, .. } = RepoTab::delete_local_branch_dialog(&tracked) else {
            panic!("expected confirm dialog");
        };
        let (label, extra) = also.as_ref().expect("tracked branch should offer remote delete");
        assert!(label.contains("topic-a"));
        let step = extra.first().expect("remote delete step");
        assert_eq!(step, &["push", "origin", "--delete", "topic-a"]);

        let mut gone = tracked.clone();
        gone.track = "[gone]".to_owned();
        let Dialog::Confirm { also, .. } = RepoTab::delete_local_branch_dialog(&gone) else {
            panic!("expected confirm dialog");
        };
        assert!(also.is_none());

        let lonely = branch("lonely", 1, false);
        let Dialog::Confirm { also, .. } = RepoTab::delete_local_branch_dialog(&lonely) else {
            panic!("expected confirm dialog");
        };
        assert!(also.is_none());
    }
}
