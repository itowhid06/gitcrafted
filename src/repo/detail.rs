//! Right pane: the selected commit, or the working-tree changes with staging and the commit box.

use std::borrow::Cow;

use egui::{Align2, Key, Rect, RichText, Sense, Ui, pos2, vec2};

use super::selection::{discard_files_dialog, stage_files, unstage_files};
use super::widgets::{self, RowExtra};
use super::{Action, Dialog, DiffKey, DiffView, Refresh, RepoTab, Reword, abort_action, ops};
use crate::git::{Area, ChangedFile, StatusFile};
use crate::icons;
use crate::settings::Settings;
use crate::theme::{self, Tokens};

impl RepoTab {
    pub(super) fn detail_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let wip = self.show_local_changes || self.selected.is_some_and(|r| self.is_wip(r));
        if self.compare.is_some() {
            self.compare_panel(ui, acts);
        } else if wip {
            self.changes_panel(ui, acts);
        } else {
            self.commit_panel(ui, acts);
        }
    }

    fn open_key(&self) -> Option<&DiffKey> {
        self.diff.as_ref().map(|d: &DiffView| &d.key)
    }

    fn commit_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let t = Tokens::get(ui.ctx());
        let idle = self.busy_op.is_none();
        let stash = self.stash_view.and_then(|i| self.stashes.get(i)).cloned();
        let title = if stash.is_some() { "Stash" } else { "Commit" };
        widgets::panel_header(ui, title, |ui| {
            if let Some(stash) = &stash {
                let more = icons::small_button(ui, "chevron-down", "Apply, pop or drop");
                egui::Popup::menu(&more)
                    .style(crate::theme::menu_style)
                    .show(|ui| {
                        super::sidebar::stash_menu_items(ui, acts, &stash.name, idle);
                    });
            }
        });
        if let Some(stash) = &stash {
            egui::Frame::new()
                .fill(t.accent.gamma_multiply(0.10))
                .inner_margin(egui::Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        icons::inline(ui, "archive", 15.0, t.accent);
                        ui.label(RichText::new(&stash.name).font(theme::medium(12.5)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for (label, args) in [
                                ("Pop", ["stash", "pop", "--index", stash.name.as_str()]),
                                ("Apply", ["stash", "apply", "--index", stash.name.as_str()]),
                            ] {
                                if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                                    acts.push(Action::Ops {
                                        label: format!("{label} {}", stash.name),
                                        ops: super::ops(&[&args]),
                                        refresh: super::Refresh::All,
                                    });
                                }
                            }
                        });
                    });
                });
        }
        if let Some(err) = &self.detail_error {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.colored_label(t.danger, err);
            });
            return;
        }
        let Some(detail) = &self.detail else {
            ui.centered_and_justified(|ui| {
                if self.selected.is_some() {
                    ui.spinner();
                } else {
                    ui.label(RichText::new("Select a commit").color(t.text_dim));
                }
            });
            return;
        };

        let allow_reword_keys = !self.keys_blocked
            && !self.has_dialog()
            && !egui::Popup::is_any_open(ui.ctx());
        // Taken out while the pane borrows `self`; put back after it.
        let mut reword = self.reword.take();
        let head = self.refs.head.clone();
        let issue_rules = self.issue_rules(ui.ctx());
        egui::ScrollArea::vertical()
            .id_salt("detail_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        let (subject, body) = detail
                            .message
                            .split_once('\n')
                            .map(|(s, b)| (s, b.trim()))
                            .unwrap_or((detail.message.as_str(), ""));
                        let mut close = false;
                        match reword.as_mut().filter(|r| r.hash == detail.hash) {
                            Some(r) => {
                                if let Some(save) = reword_editor(ui, r, allow_reword_keys) {
                                    let message = r.message();
                                    if save && message != detail.message.trim() {
                                        acts.push(Action::Reword {
                                            hash: detail.hash.clone(),
                                            message,
                                        });
                                    }
                                    close = true;
                                }
                            }
                            None => {
                                // A commit of the checked-out branch: double-click its message to
                                // edit it (the root commit has no parent to rebase from).
                                let editable = idle
                                    && detail.in_head
                                    && (detail.hash == head || !detail.merges_after)
                                    && (detail.hash == head || !detail.parents.is_empty());
                                let sense = if editable {
                                    Sense::click()
                                } else {
                                    Sense::hover()
                                };
                                let (summary, summary_link) = super::issues::linked_text_hit(
                                    ui,
                                    subject,
                                    theme::semibold(15.0),
                                    t.text,
                                    &issue_rules,
                                    sense,
                                );
                                let summary = if editable && !summary_link {
                                    summary.on_hover_text("Double-click to edit the message")
                                } else if detail.in_head && detail.merges_after {
                                    summary.on_hover_text(
                                        "Merges came after this commit: editing its message would flatten them",
                                    )
                                } else {
                                    summary
                                };
                                let mut clicked = summary.double_clicked() && !summary_link;
                                if !body.is_empty() {
                                    ui.add_space(2.0);
                                    let (body_resp, body_link) = super::issues::linked_text_hit(
                                        ui,
                                        body,
                                        egui::TextStyle::Body.resolve(ui.style()),
                                        t.text_dim,
                                        &issue_rules,
                                        sense,
                                    );
                                    clicked |= body_resp.double_clicked() && !body_link;
                                }
                                if clicked && editable {
                                    reword = Some(Reword {
                                        hash: detail.hash.clone(),
                                        subject: subject.to_owned(),
                                        body: body.to_owned(),
                                        focus: true,
                                    });
                                }
                            }
                        }
                        if close {
                            reword = None;
                        }
                        ui.add_space(12.0);

                        // Author line with an initials avatar.
                        ui.horizontal(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                            widgets::avatar(ui, rect.center(), 16.0, &detail.author, &detail.email);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(RichText::new(&detail.author).font(theme::medium(12.5)))
                                    .on_hover_text(&detail.email);
                                ui.label(
                                    RichText::new(&detail.author_date)
                                        .color(t.text_faint)
                                        .font(theme::mono(11.0)),
                                );
                            });
                        });
                        if detail.committer != detail.author {
                            ui.label(
                                RichText::new(format!(
                                    "Committed by {} · {}",
                                    detail.committer, detail.commit_date
                                ))
                                .color(t.text_faint)
                                .small(),
                            );
                        }
                        ui.add_space(10.0);

                        egui::Grid::new("commit_meta")
                            .num_columns(2)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("Commit").color(t.text_faint));
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(
                                            detail.hash.get(..12).unwrap_or(&detail.hash),
                                        )
                                        .font(theme::mono(12.0)),
                                    )
                                    .on_hover_text(&detail.hash);
                                    if icons::small_button(ui, "copy", "Copy full hash").clicked() {
                                        ui.ctx().copy_text(detail.hash.clone());
                                    }
                                });
                                ui.end_row();
                                if !detail.parents.is_empty() {
                                    ui.label(
                                        RichText::new(if detail.parents.len() > 1 {
                                            "Parents"
                                        } else {
                                            "Parent"
                                        })
                                        .color(t.text_faint),
                                    );
                                    ui.horizontal_wrapped(|ui| {
                                        for p in &detail.parents {
                                            if ui
                                                .link(
                                                    RichText::new(p.get(..8).unwrap_or(p))
                                                        .font(theme::mono(12.0)),
                                                )
                                                .clicked()
                                            {
                                                acts.push(Action::SelectHash(p.clone()));
                                            }
                                        }
                                    });
                                    ui.end_row();
                                }
                            });
                    });

                let note = (detail.parents.len() > 1).then_some("vs. first parent");
                files_header(ui, &detail.files, note);
                ui.spacing_mut().item_spacing.y = 0.0;
                self.file_list(ui, acts, &detail.files, Some(&detail.hash), |path| {
                    DiffKey::Commit {
                        hash: detail.hash.clone(),
                        path: path.to_owned(),
                    }
                });
            });
        self.reword = reword;
    }

    /// Changed files, each opening its diff (`key`) when clicked; `rev`: where the files are,
    /// for their menu (`None`: the working tree).
    fn file_list(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        files: &[ChangedFile],
        rev: Option<&str>,
        key: impl Fn(&str) -> DiffKey,
    ) {
        if Settings::get(ui.ctx()).files_as_tree {
            let tree = FileTree::build(files);
            return self.file_tree(ui, acts, &tree, "", 0, rev, &key);
        }
        for f in files {
            let key = key(&f.path);
            let selected = self.open_key() == Some(&key);
            let r = widgets::file_row(
                ui,
                f.status,
                &f.path,
                f.old_path.as_deref(),
                selected,
                RowExtra::Stats(f.added, f.removed),
            );
            widgets::file_menu(&r.row, &f.path, rev, false, acts);
            if r.row.clicked() {
                acts.push(Action::OpenDiff(key));
            }
        }
    }

    /// One folder of [`FileTree`]: its subfolders (collapsible), then its files, `depth` in.
    #[allow(clippy::too_many_arguments)]
    fn file_tree(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        folder: &FileTree<'_>,
        parent: &str,
        depth: usize,
        rev: Option<&str>,
        key: &dyn Fn(&str) -> DiffKey,
    ) {
        let t = Tokens::get(ui.ctx());
        let indent = depth as f32 * TREE_INDENT;
        for (name, sub) in &folder.folders {
            // A folder holding only one folder reads as one row: `src/repo`.
            let (mut name, mut sub) = (name.clone(), sub);
            while sub.files.is_empty() && sub.folders.len() == 1 {
                let Some((next, inner)) = sub.folders.iter().next() else {
                    break;
                };
                name = format!("{name}/{next}");
                sub = inner;
            }
            let path = if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent}/{name}")
            };
            let id = ui.id().with(("files-folder", &path));
            let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(true);
            let (rect, r) =
                ui.allocate_exact_size(vec2(ui.available_width(), widgets::ROW_H), Sense::click());
            if r.hovered() {
                ui.painter().rect_filled(rect, 0.0, t.hover);
            }
            if r.clicked() {
                open = !open;
                ui.data_mut(|d| d.insert_temp(id, open));
            }
            let y = rect.center().y;
            let x = rect.left() + indent;
            icons::paint(
                ui,
                Rect::from_center_size(pos2(x + 12.0, y), vec2(12.0, 12.0)),
                if open {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                11.0,
                t.text_faint,
            );
            icons::paint(
                ui,
                Rect::from_center_size(pos2(x + 28.0, y), vec2(14.0, 14.0)),
                "folder-open",
                13.0,
                t.text_dim,
            );
            ui.painter_at(rect).text(
                pos2(x + 42.0, y),
                Align2::LEFT_CENTER,
                &name,
                egui::FontId::proportional(12.5),
                t.text_dim,
            );
            if open {
                self.file_tree(ui, acts, sub, &path, depth + 1, rev, key);
            }
        }
        for f in &folder.files {
            let k = key(&f.path);
            let selected = self.open_key() == Some(&k);
            let r = widgets::file_row_in(
                ui,
                f.status,
                &f.path,
                f.old_path.as_deref(),
                Some(indent + 16.0),
                selected,
                RowExtra::Stats(f.added, f.removed),
            );
            widgets::file_menu(&r.row, &f.path, rev, false, acts);
            if r.row.clicked() {
                acts.push(Action::OpenDiff(k));
            }
        }
    }

    /// Right pane while comparing: the two ends, how far apart they are, and the files that
    /// differ.
    fn compare_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let t = Tokens::get(ui.ctx());
        let Some(c) = &self.compare else { return };
        widgets::panel_header(ui, "Compare", |ui| {
            if icons::small_button(ui, "x", "Close (Esc)").clicked() {
                acts.push(Action::CloseCompare);
            }
        });
        let data = c.data.as_ref().and_then(|d| d.as_ref().ok());
        let target_name = if c.target.is_some() {
            c.target_label.as_str()
        } else {
            "the working tree"
        };
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                egui::Grid::new("compare_ends")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        // A name, and the commit it stands for (`note` before the hash).
                        let end = |ui: &mut Ui, label: &str, hash: Option<&str>, note: &str| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(label).font(theme::medium(12.5)));
                                // The hash, unless the label already is it.
                                if let Some(hash) = hash.filter(|h| super::short(h) != label) {
                                    ui.label(
                                        RichText::new(format!("{note}{}", super::short(hash)))
                                            .font(theme::mono(11.5))
                                            .color(t.text_faint),
                                    )
                                    .on_hover_text(hash);
                                }
                            });
                        };
                        let note = if c.since_fork { "merge base " } else { "" };
                        ui.label(RichText::new("From").color(t.text_faint));
                        end(ui, &c.base_label, data.map(|d| d.from.as_str()), note);
                        ui.end_row();
                        ui.label(RichText::new("To").color(t.text_faint));
                        end(ui, target_name, data.and_then(|d| d.to.as_deref()), "");
                        ui.end_row();
                    });
                if let Some(d) = data {
                    let (behind, ahead) = d.apart;
                    let commits = |n: u32| format!("{n} commit{}", if n == 1 { "" } else { "s" });
                    let other = if c.target.is_some() {
                        target_name
                    } else {
                        "HEAD"
                    };
                    let text = match (ahead, behind) {
                        (0, 0) => "Same history".to_owned(),
                        (a, 0) => format!("{other} is {} ahead", commits(a)),
                        (0, b) => format!("{other} is {} behind", commits(b)),
                        (a, b) => format!("{other} is {} ahead, {b} behind", commits(a)),
                    };
                    ui.label(RichText::new(text).color(t.text_dim).small());
                }
                ui.horizontal(|ui| {
                    let mut since = c.since_fork;
                    let label = format!("Only what {target_name} changed since they diverged");
                    if ui
                        .checkbox(&mut since, label)
                        .on_hover_text(
                            "Diff from the merge base, as a pull request shows it, instead of \
                             every difference between the two",
                        )
                        .changed()
                    {
                        acts.push(Action::CompareSinceFork(since));
                    }
                });
                if c.target.is_some() && ui.button("Swap").clicked() {
                    acts.push(Action::SwapCompare);
                }
            });

        match &c.data {
            None => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.spinner());
            }
            Some(Err(e)) => {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.colored_label(t.danger, e);
                });
            }
            Some(Ok(d)) => {
                files_header(ui, &d.files, None);
                if d.files.is_empty() {
                    ui.add_space(10.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No differences").color(t.text_dim));
                    });
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("compare_scroll")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        self.file_list(ui, acts, &d.files, d.to.as_deref(), |path| {
                            DiffKey::Range {
                                from: d.from.clone(),
                                to: d.to.clone(),
                                path: path.to_owned(),
                            }
                        });
                    });
            }
        }
    }

    fn changes_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let t = Tokens::get(ui.ctx());
        let idle = self.busy_op.is_none();
        widgets::panel_header(ui, "Changes", |ui| {
            if icons::small_button(ui, "file-diff", "Apply a patch…").clicked() && idle {
                acts.push(Action::Dialog(Dialog::apply_patch(&self.path)));
            }
        });

        // The commit box stays at the bottom.
        egui::Panel::bottom("commit_box")
            .frame(
                egui::Frame::new()
                    .fill(t.card)
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .stroke(egui::Stroke::new(1.0, t.separator)),
            )
            .show(ui, |ui| self.commit_box(ui, acts));

        let mut staged_top = f32::MAX;
        let list = egui::ScrollArea::vertical()
            .id_salt("changes_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if let Some(state) = self.work.in_progress {
                    egui::Frame::new()
                        .fill(t.warning.gamma_multiply(0.14))
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                icons::inline(ui, "circle-alert", 16.0, t.warning);
                                ui.label(
                                    RichText::new(format!("{} in progress", state.label()))
                                        .color(t.warning)
                                        .font(theme::medium(12.5)),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(idle, egui::Button::new("Abort"))
                                            .clicked()
                                        {
                                            acts.push(abort_action(state));
                                        }
                                    },
                                );
                            });
                            let hint = if self.work.conflicted.is_empty() {
                                "All conflicts resolved: commit to finish."
                            } else {
                                "Fix the conflicted files, mark them resolved, then commit."
                            };
                            ui.label(RichText::new(hint).color(t.text_dim).small());
                        });
                }

                let head = self.refs.head.clone();
                // Rows are checked against this set, not the selection list, so Ctrl+A on a
                // thousand files stays cheap.
                let chosen: std::collections::HashSet<(&str, Area)> = self
                    .patch_sel
                    .iter()
                    .map(|s| (s.path.as_str(), s.area))
                    .collect();
                let picked = |f: &StatusFile| chosen.contains(&(f.path.as_str(), f.area));
                if !self.work.conflicted.is_empty() {
                    list_header(ui, "CONFLICTED", self.work.conflicted.len(), &[]);
                    for f in &self.work.conflicted {
                        match self.work_row(
                            ui,
                            acts,
                            f,
                            picked(f),
                            &[("check", "Mark resolved (stage)")],
                        ) {
                            RowClick::Open => acts.push(Action::OpenDiff(DiffKey::Work(f.clone()))),
                            RowClick::Button(_) => acts.push(stage_action(
                                "Mark resolved",
                                vec![vec!["add", "--", &f.path]],
                            )),
                            RowClick::None => {}
                        }
                    }
                }

                let unstaged = &self.work.unstaged;
                let n_unstaged = unstaged.iter().filter(|f| picked(f)).count();
                let header = if n_unstaged > 0 {
                    vec![
                        HeaderButton::new(
                            format!("Stage {n_unstaged}"),
                            format!("Stage the {n_unstaged} selected files"),
                        ),
                        HeaderButton::danger(
                            format!("Discard {n_unstaged}"),
                            format!("Discard the changes to the {n_unstaged} selected files"),
                        ),
                    ]
                } else if !unstaged.is_empty() {
                    vec![
                        HeaderButton::new("Stage all".to_owned(), "Stage every file".to_owned()),
                        HeaderButton::danger(
                            "Discard all".to_owned(),
                            "Throw away every unstaged change, and delete the new files".to_owned(),
                        ),
                    ]
                } else {
                    Vec::new()
                };
                match list_header(
                    ui,
                    "UNSTAGED",
                    unstaged.len(),
                    if idle { &header } else { &[] },
                ) {
                    Some(0) if n_unstaged > 0 => {
                        acts.push(stage_files(&self.selected_in(false)));
                    }
                    Some(0) => acts.push(stage_action("Stage all", vec![vec!["add", "-A"]])),
                    Some(_) if n_unstaged > 0 => acts.push(Action::Dialog(
                        discard_files_dialog(&self.selected_in(false), false),
                    )),
                    Some(_) => acts.push(Action::Dialog(discard_files_dialog(unstaged, true))),
                    None => {}
                }
                for f in unstaged {
                    let many = picked(f) && n_unstaged > 1;
                    let discard_tip = if many {
                        Cow::Owned(format!("Discard changes to the {n_unstaged} selected files…"))
                    } else {
                        Cow::Borrowed("Discard changes…")
                    };
                    let stage_tip = if many {
                        Cow::Owned(format!("Stage the {n_unstaged} selected files"))
                    } else {
                        Cow::Borrowed("Stage")
                    };
                    match self.work_row(
                        ui,
                        acts,
                        f,
                        picked(f),
                        &[("undo-2", &discard_tip), ("plus", &stage_tip)],
                    ) {
                        RowClick::Open => acts.push(Action::OpenDiff(DiffKey::Work(f.clone()))),
                        RowClick::Button(0) => acts.push(Action::Dialog(
                            discard_files_dialog(&self.targets_of(f), false),
                        )),
                        RowClick::Button(_) => acts.push(stage_files(&self.targets_of(f))),
                        RowClick::None => {}
                    }
                }
                if unstaged.is_empty() {
                    empty_hint(ui, "Nothing to stage");
                }

                let staged = &self.work.staged;
                staged_top = ui.cursor().top();
                let n_staged = staged.iter().filter(|f| picked(f)).count();
                let header = if n_staged > 0 {
                    vec![HeaderButton::new(
                        format!("Unstage {n_staged}"),
                        format!("Unstage the {n_staged} selected files"),
                    )]
                } else if !staged.is_empty() {
                    vec![HeaderButton::new(
                        "Unstage all".to_owned(),
                        "Unstage every file".to_owned(),
                    )]
                } else {
                    Vec::new()
                };
                if list_header(
                    ui,
                    "STAGED",
                    staged.len(),
                    if idle { &header } else { &[] },
                )
                .is_some()
                {
                    if n_staged > 0 {
                        acts.push(unstage_files(&head, &self.selected_in(true)));
                    } else {
                        let all: Vec<&str> = if head.is_empty() {
                            vec!["rm", "--cached", "-r", "-q", "--", "."]
                        } else {
                            vec!["reset", "-q"]
                        };
                        acts.push(stage_action("Unstage all", vec![all]));
                    }
                }
                for f in staged {
                    let tip = if picked(f) && n_staged > 1 {
                        Cow::Owned(format!("Unstage the {n_staged} selected files"))
                    } else {
                        Cow::Borrowed("Unstage")
                    };
                    match self.work_row(ui, acts, f, picked(f), &[("minus", &tip)]) {
                        RowClick::Open => acts.push(Action::OpenDiff(DiffKey::Work(f.clone()))),
                        RowClick::Button(_) => {
                            acts.push(unstage_files(&head, &self.targets_of(f)));
                        }
                        RowClick::None => {}
                    }
                }
                if staged.is_empty() {
                    empty_hint(ui, "Stage files to choose what goes in the commit");
                }
            });

        // Ctrl+A selects the list under the pointer, else the one last clicked.
        let ctx = ui.ctx().clone();
        let free = !self.has_dialog()
            && !self.keys_blocked
            && !egui::Popup::is_any_open(&ctx)
            && !ctx.egui_wants_keyboard_input();
        if free && ctx.input(|i| i.modifiers.command && !i.modifiers.shift && i.key_pressed(Key::A))
        {
            let staged = match ctx.pointer_hover_pos() {
                Some(p) if list.inner_rect.contains(p) => p.y >= staged_top,
                _ => self.sel_staged,
            };
            if !self.side(staged).is_empty() {
                acts.push(Action::Patch(super::patch::PatchAction::SelectAll(staged)));
            }
        }
    }

    /// One working-tree file with hover buttons (hidden while an operation runs). `picked`: it is
    /// in the multi-file selection.
    fn work_row(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        f: &StatusFile,
        picked: bool,
        buttons: &[(&str, &str)],
    ) -> RowClick {
        let key = DiffKey::Work(f.clone());
        let selected = self.open_key() == Some(&key) || picked;
        let buttons = if self.busy_op.is_none() { buttons } else { &[] };
        let r = widgets::file_row(
            ui,
            f.code,
            &f.path,
            f.old_path.as_deref(),
            selected,
            RowExtra::Buttons(buttons),
        );
        let menu = |ui: &mut Ui, acts: &mut Vec<Action>, leading_sep: bool| {
            self.stage_menu_items(ui, acts, f);
            self.patch_menu_items(ui, acts, f, leading_sep);
        };
        if f.code != 'D' {
            widgets::file_menu_with(&r.row, &f.path, None, true, acts, |ui, acts| {
                ui.separator();
                menu(ui, acts, false)
            });
        } else {
            // A deleted file has nothing to open, but its removal can go in a patch.
            egui::Popup::context_menu(&r.row)
                .style(crate::theme::menu_style)
                .show(|ui| menu(ui, acts, false));
        }
        let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        match r.button {
            Some(i) => RowClick::Button(i),
            // Ctrl+click picks files for a patch, stash or stage instead of opening the diff.
            None if r.row.clicked() && ctrl => {
                acts.push(Action::Patch(super::patch::PatchAction::Toggle(f.clone())));
                RowClick::None
            }
            // Shift+click picks everything from the last clicked file to this one.
            None if r.row.clicked() && shift && f.area != Area::Conflicted => {
                acts.push(Action::Patch(super::patch::PatchAction::Click {
                    file: f.clone(),
                    shift: true,
                }));
                RowClick::None
            }
            None if r.row.clicked() => {
                acts.push(Action::Patch(super::patch::PatchAction::Click {
                    file: f.clone(),
                    shift: false,
                }));
                RowClick::Open
            }
            None => RowClick::None,
        }
    }

    /// Stage / Unstage / Discard in a row's right-click menu, for the whole selection when the row
    /// is part of one.
    fn stage_menu_items(&self, ui: &mut Ui, acts: &mut Vec<Action>, f: &StatusFile) {
        if f.area == Area::Conflicted || self.busy_op.is_some() {
            return;
        }
        let targets = self.targets_of(f);
        let n = targets.len();
        let of = |what: &str| {
            if n > 1 {
                format!("{what} {n} selected files")
            } else {
                what.to_owned()
            }
        };
        if f.area == Area::Staged {
            if ui.button(of("Unstage")).clicked() {
                acts.push(unstage_files(&self.refs.head, &targets));
                ui.close();
            }
        } else {
            if ui.button(of("Stage")).clicked() {
                acts.push(stage_files(&targets));
                ui.close();
            }
            if ui
                .button(if n > 1 {
                    format!("Discard {n} selected files…")
                } else if f.area == Area::Untracked {
                    "Delete file…".to_owned()
                } else {
                    "Discard changes…".to_owned()
                })
                .clicked()
            {
                acts.push(Action::Dialog(discard_files_dialog(&targets, false)));
                ui.close();
            }
        }
        ui.separator();
    }

    fn commit_box(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let t = Tokens::get(ui.ctx());
        let idle = self.busy_op.is_none();
        let rebasing = self.work.in_progress == Some(crate::git::InProgress::Rebase);
        let can_amend = self.work.in_progress.is_none() && !self.refs.head.is_empty();
        if !can_amend {
            self.amend = false;
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(if rebasing {
                    "CONTINUE REBASE"
                } else {
                    "COMMIT"
                })
                .font(theme::semibold(10.5))
                .color(t.text_faint),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let amend = ui
                    .add_enabled(
                        can_amend,
                        egui::Checkbox::new(&mut self.amend, "Amend last commit"),
                    )
                    .on_hover_text(
                        "Rewrite the last commit with the staged changes and this message",
                    );
                if amend.changed() && self.amend && self.commit_summary.trim().is_empty()
                    && let Some(msg) = &self.head_message
                {
                    let (summary, body) = msg.split_once('\n').unwrap_or((msg.as_str(), ""));
                    self.commit_summary = summary.trim().to_owned();
                    self.commit_body = body.trim().to_owned();
                }
            });
        });
        if self.amend {
            let pushed = self
                .refs
                .current
                .as_ref()
                .and_then(|c| self.refs.local.iter().find(|b| &b.name == c))
                .is_some_and(|b| !b.upstream.is_empty() && b.ahead_behind().0 == 0);
            if pushed {
                ui.label(
                    RichText::new(
                        "This commit is already pushed: amending rewrites shared history.",
                    )
                    .color(t.warning)
                    .small(),
                );
            }
        }
        ui.add_space(2.0);
        let summary = ui.add_enabled(
            !rebasing,
            egui::TextEdit::singleline(&mut self.commit_summary)
                .hint_text(if rebasing {
                    "Rebase uses each commit's message"
                } else {
                    "Summary"
                })
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(8, 6)),
        );
        let body = ui.add_enabled(
            !rebasing,
            egui::TextEdit::multiline(&mut self.commit_body)
                .hint_text("Description (optional)")
                .desired_width(f32::INFINITY)
                .desired_rows(3)
                .margin(egui::Margin::symmetric(8, 6)),
        );
        if !rebasing {
            widgets::text_edit_menu(&summary, &mut self.commit_summary);
            widgets::text_edit_menu(&body, &mut self.commit_body);
        }
        let len = self.commit_summary.chars().count();
        if !rebasing && len > 72 {
            ui.label(
                RichText::new(format!("Summary is {len} characters; 50–72 reads best"))
                    .color(t.warning)
                    .small(),
            );
        }
        ui.add_space(4.0);
        let staged = self.work.staged.len();
        let label = if rebasing {
            "Continue rebase".to_owned()
        } else if self.amend {
            "Amend last commit".to_owned()
        } else if staged > 0 {
            format!("Commit {staged} file{}", if staged == 1 { "" } else { "s" })
        } else {
            "Stage all and commit".to_owned()
        };
        let can =
            idle && (rebasing || !self.commit_summary.trim().is_empty()) && self.work.conflicted.is_empty();
        // Only from the message fields, so Ctrl+Enter in the terminal stays the shell's.
        let shortcut = (summary.has_focus() || body.has_focus())
            && !self.keys_blocked
            && !self.has_dialog()
            && ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
        // Next to it, commit and push in one go (only when on a branch with a remote).
        let push_w = if rebasing || self.remotes.is_empty() || self.refs.current.is_none() {
            0.0
        } else {
            96.0
        };
        let resp = ui
            .horizontal(|ui| {
                let resp = widgets::primary_button(
                    ui,
                    &label,
                    can,
                    ui.available_width() - push_w - ui.spacing().item_spacing.x * (push_w > 0.0) as u8 as f32,
                );
                if push_w > 0.0
                    && ui
                        .add_enabled(can, egui::Button::new("& Push").min_size(egui::vec2(push_w, 30.0)))
                        .on_hover_text("Commit, then push the branch")
                        .clicked()
                {
                    acts.push(Action::Ext(super::ext::ExtAction::CommitPush));
                }
                resp
            })
            .inner;
        if (resp.clicked() || (shortcut && can)) && can {
            acts.push(Action::Commit);
        }
        resp.on_hover_text("Ctrl+Enter");

        // Who the commit will be by, and the saved identities to switch to.
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let id = &self.identity;
            let who = if id.name.is_empty() {
                "No identity set (git config user.name)".to_owned()
            } else {
                format!(
                    "as {} <{}>{}",
                    id.name,
                    id.email,
                    if id.local { "" } else { " · global" }
                )
            };
            ui.add(egui::Label::new(RichText::new(who).color(t.text_faint).small()).truncate());
            let profiles = crate::settings::Settings::get(ui.ctx()).profiles;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = icons::small_button(ui, "user-round", "Commit as…");
                egui::Popup::menu(&b)
                    .style(crate::theme::menu_style)
                    .show(|ui| {
                        if profiles.is_empty() {
                            ui.label(
                                RichText::new("Add identities in Settings").color(t.text_faint),
                            );
                        }
                        for p in &profiles {
                            let label = format!("{} — {} <{}>", p.label, p.name, p.email);
                            if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                                acts.push(Action::SetIdentity(p.name.clone(), p.email.clone()));
                            }
                        }
                    });
            });
        });
    }
}

enum RowClick {
    None,
    Open,
    Button(usize),
}

fn stage_action(label: &str, steps: Vec<Vec<&str>>) -> Action {
    Action::Ops {
        label: label.to_owned(),
        ops: steps
            .into_iter()
            .map(|s| s.into_iter().map(str::to_owned).collect())
            .collect(),
        refresh: Refresh::Status,
    }
}

pub(super) fn discard_dialog(f: &StatusFile) -> Dialog {
    if f.area == Area::Untracked {
        Dialog::confirm(
            "Delete new file",
            format!(
                "Delete “{}”? It isn't tracked by git, so this can't be undone.",
                f.path
            ),
            "Delete file",
            ops(&[&["clean", "-f", "-q", "--", &super::ext::literal(&f.path)]]),
            None,
        )
        .refresh(Refresh::Status)
    } else {
        Dialog::confirm(
            "Discard changes",
            format!(
                "Discard the unstaged changes to “{}”? This can't be undone.",
                f.path
            ),
            "Discard",
            ops(&[&["restore", "--", &super::ext::literal(&f.path)]]),
            None,
        )
        .refresh(Refresh::Status)
    }
}

/// How much further in each level of a file tree is.
const TREE_INDENT: f32 = 14.0;

/// Changed files grouped by folder, for the tree view.
#[derive(Default)]
struct FileTree<'a> {
    folders: std::collections::BTreeMap<String, FileTree<'a>>,
    files: Vec<&'a ChangedFile>,
}

impl<'a> FileTree<'a> {
    fn build(files: &'a [ChangedFile]) -> Self {
        let mut root = FileTree::default();
        for f in files {
            let mut node = &mut root;
            let mut parts: Vec<&str> = f.path.split('/').collect();
            parts.pop();
            for part in parts {
                node = node.folders.entry(part.to_owned()).or_default();
            }
            node.files.push(f);
        }
        root
    }
}

/// "N files changed +a −d", an optional note, and the Path / Tree switch on the right.
fn files_header(ui: &mut Ui, files: &[ChangedFile], note: Option<&str>) {
    let t = Tokens::get(ui.ctx());
    let (adds, dels) = files.iter().fold((0u64, 0u64), |(a, d), f| {
        (
            a.saturating_add(u64::from(f.added.unwrap_or(0))),
            d.saturating_add(u64::from(f.removed.unwrap_or(0))),
        )
    });
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, t.tab_strip);
    let y = rect.center().y;
    let r = p.text(
        pos2(rect.left() + 14.0, y),
        Align2::LEFT_CENTER,
        format!(
            "{} file{} changed",
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        ),
        theme::semibold(11.5),
        t.text_dim,
    );
    let r = p.text(
        pos2(r.right() + 10.0, y),
        Align2::LEFT_CENTER,
        format!("+{adds}"),
        theme::mono(11.0),
        t.success,
    );
    p.text(
        pos2(r.right() + 6.0, y),
        Align2::LEFT_CENTER,
        format!("−{dels}"),
        theme::mono(11.0),
        t.danger,
    );
    // Path / Tree, the one in use underlined.
    let mut settings = Settings::get(ui.ctx());
    let mut right = rect.right() - 6.0;
    for (tree, icon, tip) in [
        (true, "folder-tree", "Files in a tree of their folders"),
        (false, "list", "Files as a list of paths"),
    ] {
        let r = Rect::from_center_size(pos2(right - 10.0, y), vec2(20.0, 20.0));
        let b = ui
            .interact(r, ui.id().with(("files-view", tree)), Sense::click())
            .on_hover_text(tip);
        let on = settings.files_as_tree == tree;
        if b.hovered() {
            ui.painter().rect_filled(r, t.radius_sm, t.pressed);
        }
        icons::paint(
            ui,
            r,
            icon,
            13.0,
            if on || b.hovered() {
                t.text
            } else {
                t.text_faint
            },
        );
        if on {
            widgets::active_underline(ui, r, t.accent);
        }
        if b.clicked() && !on {
            settings.files_as_tree = tree;
            settings.set(ui.ctx());
        }
        right = r.left() - 2.0;
    }
    if let Some(note) = note {
        ui.painter().text(
            pos2(right - 6.0, y),
            Align2::RIGHT_CENTER,
            note,
            egui::FontId::proportional(11.0),
            t.text_faint,
        );
    }
}

/// A list title with a count and an optional text button on the right. Returns the click.
/// A text button on the right of a list header.
struct HeaderButton {
    label: String,
    tip: String,
    danger: bool,
}

impl HeaderButton {
    fn new(label: String, tip: String) -> Self {
        Self { label, tip, danger: false }
    }

    fn danger(label: String, tip: String) -> Self {
        Self { label, tip, danger: true }
    }
}

/// A list's title and count, and `buttons` from the right edge leftwards; the index of the one
/// clicked.
fn list_header(ui: &mut Ui, title: &str, count: usize, buttons: &[HeaderButton]) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, t.tab_strip);
    let y = rect.center().y;
    let r = p.text(
        pos2(rect.left() + 12.0, y),
        Align2::LEFT_CENTER,
        title,
        theme::semibold(11.0),
        t.text_dim,
    );
    p.text(
        pos2(r.right() + 8.0, y),
        Align2::LEFT_CENTER,
        count.to_string(),
        theme::medium(11.0),
        t.text_faint,
    );
    let mut clicked = None;
    let mut right = rect.right() - 12.0;
    for (i, button) in buttons.iter().enumerate() {
        let color = if button.danger { t.danger } else { t.accent };
        let galley = ui
            .painter()
            .layout_no_wrap(button.label.clone(), theme::medium(11.5), color);
        let w = galley.size().x + 12.0;
        let b = egui::Rect::from_min_size(pos2(right - w, y - 11.0), vec2(w, 22.0));
        let resp = ui
            .interact(b, ui.id().with(("list_btn", title, i)), Sense::click())
            .on_hover_text(button.tip.as_str());
        if resp.hovered() {
            ui.painter().rect_filled(b, t.radius_sm, t.hover);
        }
        ui.painter().galley(
            pos2(b.left() + 6.0, y - galley.size().y / 2.0),
            galley,
            color,
        );
        if resp.clicked() {
            clicked = Some(i);
        }
        right = b.left() - 2.0;
    }
    clicked
}

fn empty_hint(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(11.5),
        t.text_faint,
    );
}

impl Reword {
    /// The summary, then the description after a blank line.
    fn message(&self) -> String {
        let (subject, body) = (self.subject.trim(), self.body.trim());
        if body.is_empty() {
            subject.to_owned()
        } else {
            format!("{subject}\n\n{body}")
        }
    }
}

/// The summary and description being edited, with Save and Cancel: `Some(true)` to save,
/// `Some(false)` to cancel.
fn reword_editor(ui: &mut Ui, reword: &mut Reword, allow_keys: bool) -> Option<bool> {
    let t = Tokens::get(ui.ctx());
    let summary = ui.add(
        egui::TextEdit::singleline(&mut reword.subject)
            .font(theme::semibold(14.0))
            .hint_text("Summary")
            .desired_width(f32::INFINITY),
    );
    widgets::text_edit_menu(&summary, &mut reword.subject);
    if reword.focus {
        summary.request_focus();
        reword.focus = false;
    }
    ui.add_space(4.0);
    let body = ui.add(
        egui::TextEdit::multiline(&mut reword.body)
            .hint_text("Description (optional)")
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );
    widgets::text_edit_menu(&body, &mut reword.body);
    ui.add_space(6.0);
    let can_save = !reword.subject.trim().is_empty();
    let focused = summary.has_focus() || body.has_focus();
    let (save_keys, cancel_keys) = if allow_keys {
        ui.input(|i| {
            (
                focused && i.modifiers.command && i.key_pressed(Key::Enter),
                i.key_pressed(Key::Escape),
            )
        })
    } else {
        (false, false)
    };
    let mut out = None;
    ui.horizontal_wrapped(|ui| {
        if widgets::primary_button(ui, "Save message", can_save, 110.0).clicked() {
            out = Some(true);
        }
        if ui.button("Cancel").clicked() {
            out = Some(false);
        }
        ui.label(
            RichText::new("Ctrl+Enter saves · Esc cancels")
                .small()
                .color(t.text_faint),
        );
    });
    if save_keys && can_save {
        out = Some(true);
    } else if cancel_keys {
        out = Some(false);
    }
    out
}
