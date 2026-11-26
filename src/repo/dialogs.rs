//! Modal dialogs: create a branch, confirm a destructive operation.

use std::path::{Path, PathBuf};

use egui::{Key, RichText};

use super::widgets;
use super::{Action, Refresh, RepoTab, Undo};
use crate::theme::{self, Tokens};

pub enum Dialog {
    CreateBranch {
        /// Commit the branch starts at.
        at: String,
        /// How to show `at` (branch name or short hash).
        at_label: String,
        name: String,
        checkout: bool,
        focus: bool,
    },
    AddWorktree {
        /// The repository, to suggest a folder next to it.
        repo: PathBuf,
        path: String,
        /// The user typed a path; stop suggesting one.
        path_edited: bool,
        /// Create a new branch instead of checking out an existing one.
        new_branch: bool,
        branch: String,
        new_name: String,
        /// Where a new branch starts (HEAD's commit).
        base: String,
        focus: bool,
    },
    Confirm {
        title: String,
        body: String,
        button: String,
        ops: Vec<Vec<String>>,
        /// An optional checkbox (label, ops to run instead when ticked), e.g. force delete.
        alternative: Option<(String, Vec<Vec<String>>)>,
        use_alternative: bool,
        /// Optional checkbox (label, extra ops) run after the chosen delete ops when ticked.
        also: Option<(String, Vec<Vec<String>>)>,
        also_checked: bool,
        refresh: Refresh,
        undo: Undo,
    },
    #[cfg_attr(not(test), allow(dead_code))]
    Stash {
        message: String,
        focus: bool,
    },
    Tag {
        at: String,
        at_label: String,
        name: String,
        message: String,
        /// Remote to push the tag to, if any.
        remote: Option<String>,
        push: bool,
        focus: bool,
    },
    RenameBranch {
        old: String,
        new: String,
        focus: bool,
    },
    /// Add a remote (`original` = None) or edit one's name and URL.
    Remote {
        original: Option<(String, String)>,
        name: String,
        url: String,
        focus: bool,
    },
    /// Apply a patch file.
    Apply(super::patch::ApplyDialog),
    /// A dialog defined in its own file (see `ext.rs`).
    Ext(Box<dyn super::ext::DialogBody>),
    /// Fetch, pull or push, with their options.
    Sync(super::sync::SyncDialog),
    /// Add a submodule: its URL and the folder it goes in.
    AddSubmodule {
        url: String,
        path: String,
        /// The user typed a folder; stop suggesting one from the URL.
        path_edited: bool,
        focus: bool,
    },
    /// Username and password / token for an HTTPS remote that asked for them.
    Credentials {
        url: String,
        /// The last ones typed were refused.
        refused: bool,
        username: String,
        password: String,
        focus: bool,
    },
    CreatePr {
        noun: String,
        branch: String,
        base: String,
        title: String,
        body: String,
        draft: bool,
        focus: bool,
    },
}

impl Dialog {
    pub fn create_branch(at: &str, at_label: &str) -> Self {
        Dialog::CreateBranch {
            at: at.to_owned(),
            at_label: at_label.to_owned(),
            name: String::new(),
            checkout: true,
            focus: true,
        }
    }

    pub fn confirm(
        title: &str,
        body: String,
        button: &str,
        ops: Vec<Vec<String>>,
        alternative: Option<(String, Vec<Vec<String>>)>,
    ) -> Self {
        Dialog::Confirm {
            title: title.to_owned(),
            body,
            button: button.to_owned(),
            ops,
            alternative,
            use_alternative: false,
            also: None,
            also_checked: false,
            refresh: Refresh::All,
            undo: Undo::None,
        }
    }

    /// Adds an optional second checkbox that appends more git steps when ticked.
    pub fn also(mut self, label: String, ops: Vec<Vec<String>>) -> Self {
        if let Dialog::Confirm {
            also,
            also_checked,
            ..
        } = &mut self
        {
            *also = Some((label, ops));
            *also_checked = false;
        }
        self
    }

    /// Makes a confirmed operation undoable.
    pub fn undo(mut self, how: Undo) -> Self {
        if let Dialog::Confirm { undo, .. } = &mut self {
            *undo = how;
        }
        self
    }

    pub fn credentials(url: &str, refused: bool) -> Self {
        Dialog::Credentials {
            url: url.to_owned(),
            refused,
            username: String::new(),
            password: String::new(),
            focus: true,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stash() -> Self {
        Dialog::Stash {
            message: String::new(),
            focus: true,
        }
    }

    pub fn tag(at: &str, at_label: &str, remote: Option<String>) -> Self {
        Dialog::Tag {
            at: at.to_owned(),
            at_label: at_label.to_owned(),
            name: String::new(),
            message: String::new(),
            remote,
            push: false,
            focus: true,
        }
    }

    pub fn rename_branch(old: &str) -> Self {
        Dialog::RenameBranch {
            old: old.to_owned(),
            new: old.to_owned(),
            focus: true,
        }
    }

    pub fn remote(original: Option<(String, String)>) -> Self {
        let (name, url) = original.clone().unwrap_or_default();
        Dialog::Remote {
            original,
            name,
            url,
            focus: true,
        }
    }

    pub fn create_pr(noun: &str, branch: &str, base: &str, title: &str) -> Self {
        Dialog::CreatePr {
            noun: noun.to_owned(),
            branch: branch.to_owned(),
            base: base.to_owned(),
            title: title.to_owned(),
            body: String::new(),
            draft: false,
            focus: true,
        }
    }

    /// Add a worktree, checking out `branch` (or a new branch from `base` when `None`).
    pub fn add_worktree(repo: &Path, branch: Option<&str>, base: &str) -> Self {
        let mut dialog = Dialog::AddWorktree {
            repo: repo.to_path_buf(),
            path: String::new(),
            path_edited: false,
            new_branch: branch.is_none(),
            branch: branch.unwrap_or_default().to_owned(),
            new_name: String::new(),
            base: base.to_owned(),
            focus: true,
        };
        dialog.suggest_path();
        dialog
    }

    /// A folder beside the repository named after the branch: `app` + `feature/x` → `app-feature-x`.
    fn suggest_path(&mut self) {
        if let Dialog::AddWorktree {
            repo,
            path,
            path_edited: false,
            new_branch,
            branch,
            new_name,
            ..
        } = self
        {
            let name = if *new_branch {
                new_name.as_str()
            } else {
                branch.as_str()
            };
            let repo_name = repo
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "worktree".to_owned());
            let folder = if name.trim().is_empty() {
                format!("{repo_name}-worktree")
            } else {
                format!("{repo_name}-{}", name.trim().replace(['/', '\\', ' '], "-"))
            };
            let parent = repo.parent().unwrap_or(repo.as_path());
            *path = parent.join(folder).display().to_string();
        }
    }

    pub fn refresh(mut self, what: Refresh) -> Self {
        if let Dialog::Confirm { refresh, .. } = &mut self {
            *refresh = what;
        }
        self
    }
}

pub(crate) enum Outcome {
    Open,
    /// Still open; the branch changed, so suggest a folder again.
    Suggest,
    /// Still open; start the non-blocking Apply Patch file picker.
    PickApplyPatch,
    Cancel,
    Done(Action),
}

pub(super) fn valid_ref_name(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && !s.starts_with('-')
        && !s.starts_with('/')
        && !s.ends_with('/')
        && !s.ends_with('.')
        && !s.ends_with(".lock")
        && !s.contains("..")
        && !s.contains("//")
        && !s.contains('\\')
        && !s.contains('\0')
        && !s.chars().any(char::is_whitespace)
}

fn dialog_id(dialog: &Dialog) -> egui::Id {
    let tag = match dialog {
        Dialog::CreateBranch { .. } => "create-branch",
        Dialog::AddWorktree { .. } => "add-worktree",
        Dialog::Confirm { .. } => "confirm",
        Dialog::Stash { .. } => "stash",
        Dialog::Tag { .. } => "tag",
        Dialog::RenameBranch { .. } => "rename-branch",
        Dialog::Remote { .. } => "remote",
        Dialog::Apply(_) => "apply-patch",
        Dialog::Ext(_) => "ext",
        Dialog::Sync(_) => "sync",
        Dialog::AddSubmodule { .. } => "add-submodule",
        Dialog::Credentials { .. } => "credentials",
        Dialog::CreatePr { .. } => "create-pr",
    };
    egui::Id::new(("gitcrafted-dialog", tag))
}

impl RepoTab {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context, acts: &mut Vec<Action>) {
        let idle = self.busy_op.is_none();
        let allow_keys = !self.keys_blocked && !egui::Popup::is_any_open(ctx);
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let t = Tokens::get(ctx);
        let frame = egui::Frame::new()
            .fill(t.card)
            .stroke(egui::Stroke::new(1.0, t.field_border))
            .corner_radius(t.radius_lg)
            .inner_margin(egui::Margin::same(18))
            .shadow(ctx.global_style().visuals.window_shadow);
        let modal = egui::Modal::new(dialog_id(dialog))
            .frame(frame)
            .backdrop_color(egui::Color32::from_black_alpha(110));

        let local = &self.refs.local;
        let worktrees = &self.worktrees;
        // What a new branch's undo checks out again.
        let previous = self
            .refs
            .current
            .clone()
            .unwrap_or_else(|| self.refs.head.clone());
        let modal_w = (ctx.content_rect().width() - 32.0).clamp(260.0, 380.0);
        let mut pick_worktree = false;
        let response = modal.show(ctx, |ui| {
            ui.set_width(modal_w);
            let enter = allow_keys && idle && ui.input(|i| i.key_pressed(Key::Enter));
            match dialog {
                Dialog::CreateBranch {
                    at,
                    at_label,
                    name,
                    checkout,
                    focus,
                } => {
                    ui.label(RichText::new("Create branch").font(theme::semibold(15.0)));
                    ui.label(RichText::new(format!("Starting at {at_label}")).color(t.text_dim));
                    ui.add_space(10.0);
                    let edit = ui.add(
                        egui::TextEdit::singleline(name)
                            .hint_text("feature/my-branch")
                            .desired_width(f32::INFINITY)
                            .margin(egui::Margin::symmetric(8, 6)),
                    );
                    widgets::text_edit_menu(&edit, name);
                    if *focus {
                        edit.request_focus();
                        *focus = false;
                    }
                    // Spaces are never valid in a branch name; typing one gives a dash.
                    if name.contains(' ') {
                        *name = name.replace(' ', "-");
                    }
                    ui.add_space(6.0);
                    ui.checkbox(checkout, "Check it out");
                    ui.add_space(12.0);
                    let valid = idle && valid_ref_name(name);
                    let mut outcome = Outcome::Open;
                    ui.horizontal(|ui| {
                        if widgets::primary_button(ui, "Create", valid, 90.0).clicked()
                            || (enter && valid)
                        {
                            let name = name.trim().to_owned();
                            let args: Vec<String> = if *checkout {
                                vec!["switch".into(), "-c".into(), name.clone(), at.clone()]
                            } else {
                                vec!["branch".into(), name.clone(), at.clone()]
                            };
                            let mut undo = Vec::new();
                            if *checkout && !previous.is_empty() {
                                undo.push(vec!["checkout".to_owned(), previous.clone()]);
                            }
                            undo.push(vec!["branch".to_owned(), "-D".to_owned(), name.clone()]);
                            outcome = Outcome::Done(Action::UndoableOps {
                                label: format!("Create branch {name}"),
                                ops: vec![args.clone()],
                                refresh: Refresh::All,
                                undo: Undo::Custom { undo, redo: vec![args] },
                            });
                        }
                        if ui
                            .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 30.0)))
                            .clicked()
                        {
                            outcome = Outcome::Cancel;
                        }
                    });
                    outcome
                }
                Dialog::AddWorktree {
                    path,
                    path_edited,
                    new_branch,
                    branch,
                    new_name,
                    base,
                    focus,
                    ..
                } => {
                    ui.label(RichText::new("Add worktree").font(theme::semibold(15.0)));
                    ui.label(
                        RichText::new(
                            "A second working folder of this repository, on its own branch.",
                        )
                        .color(t.text_dim),
                    );
                    ui.add_space(10.0);
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        changed |= ui
                            .radio_value(new_branch, false, "Existing branch")
                            .changed();
                        changed |= ui.radio_value(new_branch, true, "New branch").changed();
                    });
                    ui.add_space(4.0);
                    if *new_branch {
                        let edit = ui.add(
                            egui::TextEdit::singleline(new_name)
                                .hint_text("feature/my-branch")
                                .desired_width(f32::INFINITY)
                                .margin(egui::Margin::symmetric(8, 6)),
                        );
                        widgets::text_edit_menu(&edit, new_name);
                        if *focus {
                            edit.request_focus();
                            *focus = false;
                        }
                        if new_name.contains(' ') {
                            *new_name = new_name.replace(' ', "-");
                        }
                        changed |= edit.changed();
                        ui.label(
                            RichText::new(format!(
                                "Starting at HEAD ({})",
                                base.get(..8).unwrap_or(base)
                            ))
                            .color(t.text_faint)
                            .small(),
                        );
                    } else {
                        // Branches that aren't checked out in any worktree yet.
                        let busy: Vec<&str> = worktrees
                            .iter()
                            .filter_map(|w| w.branch.as_deref())
                            .collect();
                        let free: Vec<&str> = local
                            .iter()
                            .map(|b| b.name.as_str())
                            .filter(|n| !busy.contains(n))
                            .collect();
                        let selected_idx = free.iter().position(|&n| n == branch.as_str());
                        let selected_text = if branch.is_empty() {
                            "Choose a branch".to_owned()
                        } else {
                            branch.clone()
                        };
                        if super::sync::pick_searchable(
                            ui,
                            "worktree-branch",
                            &selected_text,
                            &free,
                            selected_idx,
                            "Every branch is checked out somewhere",
                            "Search branches…",
                            |_, name| *branch = name.to_owned(),
                        ) {
                            changed = true;
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("Folder").color(t.text_dim).small());
                    ui.horizontal(|ui| {
                        let mut edit = ui.add(
                            egui::TextEdit::singleline(path)
                                .desired_width((ui.available_width() - 90.0).max(80.0))
                                .margin(egui::Margin::symmetric(8, 6)),
                        );
                        if widgets::text_edit_menu(&edit, path) {
                            edit.mark_changed();
                        }
                        if edit.changed() {
                            *path_edited = true;
                        }
                        if ui
                            .add(egui::Button::new("Browse…").min_size(egui::vec2(80.0, 28.0)))
                            .clicked()
                        {
                            pick_worktree = true;
                        }
                    });
                    ui.add_space(12.0);
                    let name_ok = if *new_branch {
                        valid_ref_name(new_name)
                    } else {
                        !branch.is_empty() && valid_ref_name(branch)
                    };
                    let p = path.trim();
                    let valid = idle && name_ok && !p.is_empty() && !p.starts_with('-');
                    let mut outcome = Outcome::Open;
                    ui.horizontal(|ui| {
                        if widgets::primary_button(ui, "Add", valid, 90.0).clicked()
                            || (enter && valid)
                        {
                            let path = path.trim().to_owned();
                            let args: Vec<String> = if *new_branch {
                                vec![
                                    "worktree".into(),
                                    "add".into(),
                                    "-b".into(),
                                    new_name.trim().to_owned(),
                                    path.clone(),
                                    base.clone(),
                                ]
                            } else {
                                vec![
                                    "worktree".into(),
                                    "add".into(),
                                    path.clone(),
                                    branch.clone(),
                                ]
                            };
                            outcome = Outcome::Done(Action::Ops {
                                label: "Add worktree".to_owned(),
                                ops: vec![args],
                                refresh: Refresh::All,
                            });
                        }
                        if ui
                            .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 30.0)))
                            .clicked()
                        {
                            outcome = Outcome::Cancel;
                        }
                    });
                    if changed {
                        outcome = match outcome {
                            Outcome::Open => Outcome::Suggest,
                            other => other,
                        };
                    }
                    outcome
                }
                Dialog::Stash { message, focus } => {
                    title(ui, "Stash changes", "Puts every change, untracked files included, aside and cleans the working tree.", &t);
                    let edit = field(ui, message, "Message (optional)", focus);
                    let _ = edit;
                    buttons(ui, "Stash", idle, enter, || Action::StashPush(message.trim().to_owned()))
                }
                Dialog::Tag { at, at_label, name, message, remote, push, focus } => {
                    title(ui, "Create tag", &format!("At {at_label}"), &t);
                    field(ui, name, "v1.2.0", focus);
                    if name.contains(' ') {
                        *name = name.replace(' ', "-");
                    }
                    ui.add_space(4.0);
                    let edit = ui.add(
                        egui::TextEdit::multiline(message)
                            .hint_text("Message (optional: makes an annotated tag)")
                            .desired_width(f32::INFINITY)
                            .desired_rows(2)
                            .margin(egui::Margin::symmetric(8, 6)),
                    );
                    widgets::text_edit_menu(&edit, message);
                    if let Some(remote) = remote {
                        ui.add_space(4.0);
                        ui.checkbox(push, format!("Push it to {remote}"));
                    }
                    ui.add_space(8.0);
                    let valid = idle && valid_ref_name(name);
                    buttons(ui, "Create", valid, enter && !message.contains('\n'), || {
                        let name = name.trim().to_owned();
                        let mut create = vec!["tag".to_owned()];
                        if !message.trim().is_empty() {
                            create.extend(["-a".to_owned(), "-m".to_owned(), message.trim().to_owned()]);
                        }
                        create.extend([name.clone(), at.clone()]);
                        let mut steps = vec![create.clone()];
                        if *push && let Some(remote) = remote {
                            steps.push(vec!["push".to_owned(), remote.clone(), format!("refs/tags/{name}")]);
                        }
                        Action::UndoableOps {
                            label: format!("Create tag {name}"),
                            ops: steps,
                            refresh: Refresh::All,
                            undo: Undo::Custom {
                                undo: vec![vec!["tag".to_owned(), "-d".to_owned(), name.clone()]],
                                redo: vec![create],
                            },
                        }
                    })
                }
                Dialog::RenameBranch { old, new, focus } => {
                    title(ui, "Rename branch", &format!("Renames {old} on this computer; its remote copy keeps its name."), &t);
                    field(ui, new, "new-name", focus);
                    if new.contains(' ') {
                        *new = new.replace(' ', "-");
                    }
                    ui.add_space(8.0);
                    let valid = idle && valid_ref_name(new) && new.trim() != old.as_str();
                    buttons(ui, "Rename", valid, enter, || {
                        let new = new.trim().to_owned();
                        Action::UndoableOps {
                            label: format!("Rename {old} to {new}"),
                            ops: vec![vec!["branch".into(), "-m".into(), old.clone(), new.clone()]],
                            refresh: Refresh::All,
                            undo: Undo::Custom {
                                undo: vec![vec!["branch".into(), "-m".into(), new.clone(), old.clone()]],
                                redo: vec![vec!["branch".into(), "-m".into(), old.clone(), new]],
                            },
                        }
                    })
                }
                Dialog::Remote { original, name, url, focus } => {
                    let heading = if original.is_some() { "Edit remote" } else { "Add remote" };
                    title(ui, heading, "A name (like origin or upstream) and the repository's URL.", &t);
                    field(ui, name, "origin", focus);
                    ui.add_space(4.0);
                    let edit = ui.add(
                        egui::TextEdit::singleline(url)
                            .hint_text("https://github.com/user/repo.git")
                            .desired_width(f32::INFINITY)
                            .margin(egui::Margin::symmetric(8, 6)),
                    );
                    widgets::text_edit_menu(&edit, url);
                    ui.add_space(8.0);
                    let url_t = url.trim();
                    let valid = idle && valid_ref_name(name) && !url_t.is_empty() && !url_t.starts_with('-');
                    buttons(ui, if original.is_some() { "Save" } else { "Add" }, valid, enter, || {
                        let (name, url) = (name.trim().to_owned(), url.trim().to_owned());
                        let steps = match original {
                            None => vec![
                                vec!["remote".into(), "add".into(), name.clone(), url],
                                vec!["fetch".into(), name.clone()],
                            ],
                            Some((old_name, old_url)) => {
                                let mut steps = Vec::new();
                                if *old_name != name {
                                    steps.push(vec!["remote".into(), "rename".into(), old_name.clone(), name.clone()]);
                                }
                                if *old_url != url {
                                    steps.push(vec!["remote".into(), "set-url".into(), name.clone(), url]);
                                }
                                steps
                            }
                        };
                        Action::Ops {
                            label: format!("Remote {name}"),
                            ops: steps,
                            refresh: Refresh::All,
                        }
                    })
                }
                Dialog::Sync(sync) => {
                    if !idle {
                        ui.label(
                            RichText::new("Wait for the running operation to finish…")
                                .color(t.warning)
                                .small(),
                        );
                        ui.add_space(4.0);
                    }
                    sync.ui(ui, &t, enter)
                }
                Dialog::Apply(apply) => {
                    if !idle {
                        ui.label(
                            RichText::new("Wait for the running operation to finish…")
                                .color(t.warning)
                                .small(),
                        );
                        ui.add_space(4.0);
                    }
                    apply.ui(ui, &t, enter)
                }
                Dialog::Ext(body) => {
                    if !idle {
                        ui.label(
                            RichText::new("Wait for the running operation to finish…")
                                .color(t.warning)
                                .small(),
                        );
                        ui.add_space(4.0);
                    }
                    body.ui(ui, &t, enter)
                }
                Dialog::AddSubmodule { url, path, path_edited, focus } => {
                    title(ui, "Add submodule", "The repository to embed and the folder it goes in.", &t);
                    if field(ui, url, "https://github.com/user/repo.git", focus).changed() && !*path_edited {
                        *path = super::submodules::folder_from_url(url);
                    }
                    ui.add_space(4.0);
                    let mut edit = ui.add(
                        egui::TextEdit::singleline(path)
                            .hint_text("libs/repo")
                            .desired_width(f32::INFINITY)
                            .margin(egui::Margin::symmetric(8, 6)),
                    );
                    if widgets::text_edit_menu(&edit, path) {
                        edit.mark_changed();
                    }
                    if edit.changed() {
                        *path_edited = true;
                    }
                    ui.add_space(8.0);
                    let url_t = url.trim();
                    let path_t = path.trim();
                    let path_safe = !path_t.is_empty()
                        && !path_t.starts_with('-')
                        && !Path::new(path_t).is_absolute()
                        && !Path::new(path_t)
                            .components()
                            .any(|c| matches!(c, std::path::Component::ParentDir));
                    let valid = idle && !url_t.is_empty() && !url_t.starts_with('-') && path_safe;
                    buttons(ui, "Add", valid, enter, || Action::Ops {
                        label: format!("Add submodule {}", path.trim()),
                        ops: super::submodules::add_ops(url.trim(), path.trim()),
                        refresh: Refresh::All,
                    })
                }
                Dialog::Credentials { url, refused, username, password, focus } => {
                    let host = host_of(url);
                    title(
                        ui,
                        &format!("Sign in to {host}"),
                        "git needs a username and password for this remote. Most hosts want a personal access token as the password.",
                        &t,
                    );
                    if *refused {
                        ui.colored_label(t.danger, "Those credentials were refused. Check them and try again.");
                        ui.add_space(6.0);
                    }
                    field(ui, username, "Username", focus);
                    ui.add_space(4.0);
                    let password_id = ui.make_persistent_id("credentials_password");
                    widgets::trim_pasted_secret(ui, password_id);
                    let edit = ui.add(
                        egui::TextEdit::singleline(password)
                            .id(password_id)
                            .password(true)
                            .hint_text("Password or token")
                            .desired_width(f32::INFINITY)
                            .margin(egui::Margin::symmetric(8, 6)),
                    );
                    widgets::secret_edit_menu(&edit, password);
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Used for this tab until you close it. If git has a credential helper (such as Git Credential Manager), it saves them once they work.")
                            .color(t.text_faint)
                            .small(),
                    );
                    ui.add_space(8.0);
                    let valid = !username.trim().is_empty() && !password.is_empty();
                    buttons(ui, "Sign in", valid, enter, || {
                        Action::SignIn(crate::git::Credentials {
                            username: username.trim().to_owned(),
                            password: password.clone(),
                        })
                    })
                }
                Dialog::CreatePr { noun, branch, base, title: pr_title, body, draft, focus } => {
                    title(ui, &format!("Create {noun}"), &format!("{branch} → {base} (the branch is pushed first)"), &t);
                    field(ui, pr_title, "Title", focus);
                    ui.add_space(4.0);
                    let edit = ui.add(
                        egui::TextEdit::multiline(body)
                            .hint_text("Description")
                            .desired_width(f32::INFINITY)
                            .desired_rows(5)
                            .margin(egui::Margin::symmetric(8, 6)),
                    );
                    widgets::text_edit_menu(&edit, body);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Into").color(t.text_dim));
                        let edit = ui.add(egui::TextEdit::singleline(base).desired_width(160.0));
                        widgets::text_edit_menu(&edit, base);
                        ui.checkbox(draft, "Draft");
                    });
                    ui.add_space(8.0);
                    let valid = idle && !pr_title.trim().is_empty() && valid_ref_name(base);
                    buttons(ui, "Create", valid, false, || Action::CreatePr {
                        branch: branch.clone(),
                        base: base.trim().to_owned(),
                        title: pr_title.trim().to_owned(),
                        body: body.trim().to_owned(),
                        draft: *draft,
                    })
                }
                Dialog::Confirm {
                    title,
                    body,
                    button,
                    ops,
                    alternative,
                    use_alternative,
                    also,
                    also_checked,
                    refresh,
                    undo,
                } => {
                    ui.label(RichText::new(title.as_str()).font(theme::semibold(15.0)));
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(body.as_str()).wrap());
                    if let Some((label, _)) = alternative {
                        ui.add_space(6.0);
                        ui.checkbox(use_alternative, label.as_str());
                    }
                    if let Some((label, _)) = also {
                        ui.add_space(6.0);
                        ui.checkbox(also_checked, label.as_str());
                    }
                    ui.add_space(14.0);
                    let mut outcome = Outcome::Open;
                    ui.horizontal(|ui| {
                        let danger = egui::Button::new(
                            RichText::new(button.as_str())
                                .color(egui::Color32::WHITE)
                                .font(theme::medium(12.5)),
                        )
                        .fill(t.danger)
                        .min_size(egui::vec2(90.0, 30.0));
                        if ui.add_enabled(idle, danger).clicked() {
                            let mut steps = match (alternative, *use_alternative) {
                                (Some((_, alt)), true) => alt.clone(),
                                _ => ops.clone(),
                            };
                            if let Some((_, extra)) = also
                                && *also_checked
                            {
                                steps.extend(extra.clone());
                            }
                            outcome = Outcome::Done(match undo {
                                Undo::None => Action::Ops {
                                    label: title.clone(),
                                    ops: steps,
                                    refresh: *refresh,
                                },
                                undo => Action::UndoableOps {
                                    label: title.clone(),
                                    ops: steps,
                                    refresh: *refresh,
                                    undo: undo.clone(),
                                },
                            });
                        }
                        if ui
                            .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 30.0)))
                            .clicked()
                        {
                            outcome = Outcome::Cancel;
                        }
                    });
                    outcome
                }
            }
        });

        let is_credentials = matches!(dialog, Dialog::Credentials { .. });
        let mut dismissed_credentials = false;
        match response.inner {
            Outcome::Done(action)
                if idle || matches!(action, Action::SelectHash(_) | Action::Dialog(_)) =>
            {
                acts.push(action);
                self.dialog = None;
            }
            Outcome::Done(_) => {
                self.status = Some(("Wait for the running operation to finish".to_owned(), true));
            }
            Outcome::Cancel => {
                dismissed_credentials = is_credentials;
                self.dialog = None;
            }
            Outcome::Open if allow_keys && response.should_close() => {
                dismissed_credentials = is_credentials;
                self.dialog = None;
            }
            Outcome::Open => {}
            Outcome::Suggest => dialog.suggest_path(),
            Outcome::PickApplyPatch => self.pick_apply_patch(ctx),
        }
        if dismissed_credentials {
            self.push_follow = super::ext::PushFollow::Idle;
            self.pending_pr = None;
            self.last_op = None;
        }
        if pick_worktree {
            self.pick_worktree_dir(ctx);
        }
    }
}

/// A dialog's heading and one line under it.
/// `github.com` from `https://user@github.com/owner/repo.git`.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    rest.split(['/', ':'])
        .next()
        .filter(|h| !h.is_empty())
        .unwrap_or("the remote")
}

pub(super) fn title(ui: &mut egui::Ui, heading: &str, sub: &str, t: &Tokens) {
    ui.label(RichText::new(heading).font(theme::semibold(15.0)));
    ui.label(RichText::new(sub).color(t.text_dim));
    ui.add_space(10.0);
}

/// A single-line field that takes the keyboard when the dialog opens, with the usual
/// right-click menu.
fn field(ui: &mut egui::Ui, text: &mut String, hint: &str, focus: &mut bool) -> egui::Response {
    let mut edit = ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(8, 6)),
    );
    // A menu entry that changes the text counts as an edit (fields derived from it follow).
    if widgets::text_edit_menu(&edit, text) {
        edit.mark_changed();
    }
    if *focus {
        edit.request_focus();
        *focus = false;
    }
    edit
}

/// The primary button (also Enter, when `enter`) and Cancel.
pub(super) fn buttons(
    ui: &mut egui::Ui,
    label: &str,
    valid: bool,
    enter: bool,
    done: impl FnOnce() -> Action,
) -> Outcome {
    let mut outcome = Outcome::Open;
    ui.horizontal(|ui| {
        if widgets::primary_button(ui, label, valid, 90.0).clicked() || (enter && valid) {
            outcome = Outcome::Done(done());
        }
        if ui
            .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 30.0)))
            .clicked()
        {
            outcome = Outcome::Cancel;
        }
    });
    outcome
}
