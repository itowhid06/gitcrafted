//! Patches: save the staged or unstaged files you selected (or all of them), or a stash, as a
//! patch file, and apply a patch file to the working tree.
//!
//! Kept in a file of its own so the rest of the tree only carries small hooks.

use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::{RichText, Ui};

use super::dialogs::{Outcome, buttons, title};
use super::ext::{ExtAction, literal};
use super::sync::{pick, row};
use super::{Action, Dialog, Msg, OpKind, Refresh, RepoTab, Undo, spawn};
use crate::git::{self, Area, StatusFile};
use crate::theme::Tokens;

/// What a patch is made of.
#[derive(Clone, Debug)]
pub enum PatchSource {
    /// Staged (index against HEAD) or unstaged (working tree against index) changes of `files`.
    Work {
        staged: bool,
        files: Vec<StatusFile>,
    },
    /// A stash's changes (`stash@{0}`), untracked files included.
    Stash { name: String },
    /// One commit, as `git format-patch` writes it (which `git am` can apply).
    Commit { hash: String },
}

#[derive(Clone, Debug)]
pub enum PatchAction {
    /// Ctrl+click on a file of the Changes lists.
    Toggle(StatusFile),
    ClearSelection,
    /// Ask where to save it, then write it.
    Save(PatchSource),
}

// ---- making a patch -------------------------------------------------------------------------

/// `git` output as raw bytes (a patch may hold text that isn't UTF-8), exit codes in `ok` being
/// success (`diff --no-index` exits 1 when the files differ).
fn git_bytes(repo: &Path, args: &[&str], ok: &[i32]) -> Result<Vec<u8>, String> {
    let out = git::git_cmd()
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.code().is_some_and(|c| ok.contains(&c)) {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(if err.is_empty() {
            format!("git {} failed ({})", args.join(" "), out.status)
        } else {
            err
        })
    }
}

/// The patch of `source`; empty when there's nothing to save.
fn make_patch(repo: &Path, source: &PatchSource) -> Result<Vec<u8>, String> {
    match source {
        PatchSource::Work {
            staged: true,
            files,
        } => {
            // A rename needs both names, or it would read as an added file.
            let mut paths: Vec<String> = Vec::new();
            for f in files {
                paths.push(literal(&f.path));
                paths.extend(f.old_path.as_deref().map(literal));
            }
            let mut args = vec!["diff", "--cached", "--binary", "-M", "--"];
            args.extend(paths.iter().map(String::as_str));
            git_bytes(repo, &args, &[0])
        }
        PatchSource::Work {
            staged: false,
            files,
        } => {
            let (untracked, tracked): (Vec<&StatusFile>, Vec<&StatusFile>) =
                files.iter().partition(|f| f.area == Area::Untracked);
            let mut patch = Vec::new();
            if !tracked.is_empty() {
                let paths: Vec<String> = tracked.iter().map(|f| literal(&f.path)).collect();
                let mut args = vec!["diff", "--binary", "--"];
                args.extend(paths.iter().map(String::as_str));
                patch.extend(git_bytes(repo, &args, &[0])?);
            }
            // A new file isn't in the index: diff it against nothing.
            for f in untracked {
                patch.extend(git_bytes(
                    repo,
                    &["diff", "--no-index", "--binary", "--", "/dev/null", &f.path],
                    &[0, 1],
                )?);
            }
            Ok(patch)
        }
        PatchSource::Commit { hash } => {
            // `format-patch` skips a merge and writes its first parent's commit instead.
            let parents = git_bytes(repo, &["rev-list", "--parents", "-n", "1", hash], &[0])?;
            let is_merge = String::from_utf8_lossy(&parents).split_whitespace().count() > 2;
            if !is_merge {
                let patch = git_bytes(repo, &["format-patch", "-1", "--stdout", hash], &[0])?;
                if !patch.is_empty() {
                    return Ok(patch);
                }
            }
            // A merge commit has no patch of its own: its changes against the first parent.
            git_bytes(
                repo,
                &["diff", "--binary", &format!("{hash}^1"), hash],
                &[0],
            )
        }
        PatchSource::Stash { name } => {
            // `--include-untracked` needs git 2.32; older ones leave those files out.
            git_bytes(
                repo,
                &[
                    "stash",
                    "show",
                    "-p",
                    "--binary",
                    "--include-untracked",
                    name,
                ],
                &[0],
            )
            .or_else(|_| git_bytes(repo, &["stash", "show", "-p", "--binary", name], &[0]))
        }
    }
}

/// A file name to suggest for the patch.
fn suggested_name(source: &PatchSource, branch: Option<&str>) -> String {
    match source {
        PatchSource::Commit { hash } => format!("{}.patch", hash.get(..8).unwrap_or(hash)),
        PatchSource::Stash { name } => {
            let n: String = name.chars().filter(char::is_ascii_digit).collect();
            format!("stash-{n}.patch")
        }
        PatchSource::Work { staged, files } => {
            let base = match files.as_slice() {
                [one] => Path::new(&one.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                _ => branch.unwrap_or("changes").replace('/', "-"),
            };
            format!(
                "{base}-{}.patch",
                if *staged { "staged" } else { "unstaged" }
            )
        }
    }
}

// ---- the tab's side -------------------------------------------------------------------------

impl RepoTab {
    fn is_patch_selected(&self, f: &StatusFile) -> bool {
        self.patch_sel
            .iter()
            .any(|s| s.path == f.path && s.area == f.area)
    }

    /// Forgets selected files that are no longer listed (committed, stashed, discarded), so a
    /// stash or patch never names a file without changes, and a file that changes again later
    /// doesn't come back selected.
    pub(super) fn prune_patch_selection(&mut self) {
        let work = &self.work;
        self.patch_sel.retain(|s| {
            work.staged
                .iter()
                .chain(&work.unstaged)
                .any(|f| f.path == s.path && f.area == s.area)
        });
    }

    /// Whether a Changes row is drawn as selected for a patch.
    pub(super) fn patch_selected(&self, f: &StatusFile) -> bool {
        self.is_patch_selected(f)
    }

    /// The selected files of the staged or unstaged list, as the lists show them now.
    fn selected_in(&self, staged: bool) -> Vec<StatusFile> {
        let list = if staged {
            &self.work.staged
        } else {
            &self.work.unstaged
        };
        list.iter()
            .filter(|f| self.is_patch_selected(f))
            .cloned()
            .collect()
    }

    pub(super) fn patch_action(&mut self, action: PatchAction, ctx: &egui::Context) {
        match action {
            PatchAction::Toggle(file) => {
                match self
                    .patch_sel
                    .iter()
                    .position(|s| s.path == file.path && s.area == file.area)
                {
                    Some(i) => {
                        self.patch_sel.remove(i);
                    }
                    None => self.patch_sel.push(file),
                }
            }
            PatchAction::ClearSelection => self.patch_sel.clear(),
            PatchAction::Save(source) => self.save_patch(source, ctx),
        }
    }

    fn save_patch(&mut self, source: PatchSource, ctx: &egui::Context) {
        if self.busy_op.is_some() {
            return;
        }
        let Some(dest) = rfd::FileDialog::new()
            .set_title("Save patch")
            .set_file_name(suggested_name(&source, self.refs.current.as_deref()))
            .add_filter("Patch", &["patch", "diff"])
            .save_file()
        else {
            return;
        };
        let file = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let label = format!("Save patch {file}");
        self.busy_op = Some(label.clone());
        self.op_started = Some(Instant::now());
        self.status = None;
        let path = self.path.clone();
        spawn(&self.tx, ctx, move || Msg::OpDone {
            label,
            result: write_patch(&path, &source, &dest),
            refresh: Refresh::All,
            kind: OpKind::Normal(Undo::None, String::new()),
            quiet: false,
        });
    }

    /// The patch entries of a Changes row's right-click menu.
    pub(super) fn patch_menu_items(&self, ui: &mut Ui, acts: &mut Vec<Action>, f: &StatusFile) {
        if f.area == Area::Conflicted {
            ui.separator();
            if ui.button("Open in external merge tool").clicked() {
                acts.push(Action::Ext(ExtAction::MergeTool {
                    path: f.path.clone(),
                }));
            }
            return;
        }
        let staged = f.area == Area::Staged;
        let side = if staged { "staged" } else { "unstaged" };
        ui.separator();
        let selected = self.selected_in(staged);
        if self.is_patch_selected(f) && selected.len() > 1 {
            if ui
                .button(format!("Save {} selected files as patch…", selected.len()))
                .clicked()
            {
                acts.push(Action::Patch(PatchAction::Save(PatchSource::Work {
                    staged,
                    files: selected,
                })));
            }
        } else if ui.button("Save this file as patch…").clicked() {
            acts.push(Action::Patch(PatchAction::Save(PatchSource::Work {
                staged,
                files: vec![f.clone()],
            })));
        }
        if f.code != '?' && ui.button("Open in external diff tool").clicked() {
            acts.push(Action::Ext(ExtAction::DiffTool {
                path: f.path.clone(),
                staged,
            }));
        }
        let all = if staged {
            &self.work.staged
        } else {
            &self.work.unstaged
        };
        if ui
            .button(format!("Save all {side} changes as patch…"))
            .clicked()
        {
            acts.push(Action::Patch(PatchAction::Save(PatchSource::Work {
                staged,
                files: all.clone(),
            })));
        }
        if !self.patch_sel.is_empty() {
            if ui.button("Clear selection").clicked() {
                acts.push(Action::Patch(PatchAction::ClearSelection));
            }
        } else {
            ui.label(
                RichText::new("Ctrl+click selects several files")
                    .small()
                    .weak(),
            );
        }
    }
}

fn write_patch(repo: &Path, source: &PatchSource, dest: &Path) -> Result<String, String> {
    let patch = make_patch(repo, source)?;
    if patch.is_empty() {
        return Err("There are no changes to save".to_owned());
    }
    std::fs::write(dest, &patch).map_err(|e| format!("could not write {}: {e}", dest.display()))?;
    Ok(String::new())
}

/// The "Save as patch…" entry of a stash's menu.
pub(super) fn stash_patch_item(ui: &mut Ui, acts: &mut Vec<Action>, name: &str) {
    if ui
        .button("Save as patch…")
        .on_hover_text("Everything the stash holds, untracked files included")
        .clicked()
    {
        acts.push(Action::Patch(PatchAction::Save(PatchSource::Stash {
            name: name.to_owned(),
        })));
    }
}

// ---- applying a patch -----------------------------------------------------------------------

/// What `git apply` does about whitespace errors in the patch.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Whitespace {
    NoWarn,
    Warn,
    Error,
    ErrorAll,
}

impl Whitespace {
    const ALL: [Whitespace; 4] = [
        Whitespace::NoWarn,
        Whitespace::Warn,
        Whitespace::Error,
        Whitespace::ErrorAll,
    ];

    fn label(self) -> &'static str {
        match self {
            Whitespace::NoWarn => "Apply, don't warn",
            Whitespace::Warn => "Apply, warn",
            Whitespace::Error => "Refuse on errors",
            Whitespace::ErrorAll => "Refuse on errors, all files",
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Whitespace::NoWarn => "--whitespace=nowarn",
            Whitespace::Warn => "--whitespace=warn",
            Whitespace::Error => "--whitespace=error",
            Whitespace::ErrorAll => "--whitespace=error-all",
        }
    }
}

pub struct ApplyDialog {
    /// Relative paths are the repository's, as git (run in it) reads them.
    repo: PathBuf,
    file: String,
    whitespace: Whitespace,
    /// Stage what the patch changes too (`--index`).
    index: bool,
    /// Fall back on a three-way merge when it doesn't apply cleanly (`--3way`).
    three_way: bool,
    /// Undo the patch instead (`-R`).
    reverse: bool,
    focus: bool,
}

impl Dialog {
    /// Apply a patch file to the working tree.
    pub fn apply_patch(repo: &Path) -> Self {
        Dialog::Apply(ApplyDialog {
            repo: repo.to_path_buf(),
            file: String::new(),
            whitespace: Whitespace::Warn,
            index: false,
            three_way: false,
            reverse: false,
            focus: true,
        })
    }
}

/// The `git apply` arguments.
fn apply_args(d: &ApplyDialog) -> Vec<String> {
    let mut args = vec!["apply".to_owned(), d.whitespace.flag().to_owned()];
    if d.index {
        args.push("--index".into());
    }
    if d.three_way {
        args.push("--3way".into());
    }
    if d.reverse {
        args.push("--reverse".into());
    }
    args.push("--".into());
    args.push(d.patch_path().display().to_string());
    args
}

impl ApplyDialog {
    /// The patch file: as typed when absolute, else inside the repository.
    fn patch_path(&self) -> PathBuf {
        let typed = PathBuf::from(self.file.trim());
        if typed.is_absolute() {
            typed
        } else {
            self.repo.join(typed)
        }
    }

    pub(super) fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        title(
            ui,
            "Apply patch",
            "Apply the changes of a patch file (from git diff, format-patch or Save as patch).",
            t,
        );
        row(ui, t, "Patch file", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.file)
                    .hint_text("/path/to/changes.patch")
                    .desired_width(ui.available_width() - 92.0)
                    .margin(egui::Margin::symmetric(8, 4)),
            );
            if self.focus {
                edit.request_focus();
                self.focus = false;
            }
            if ui.button("Browse…").clicked()
                && let Some(picked) = rfd::FileDialog::new()
                    .set_title("Choose a patch")
                    .add_filter("Patch", &["patch", "diff", "txt"])
                    .pick_file()
            {
                self.file = picked.display().to_string();
            }
        });
        row(ui, t, "Whitespace", |ui| {
            pick(ui, "apply-whitespace", self.whitespace.label(), |ui| {
                let mut changed = false;
                for w in Whitespace::ALL {
                    if ui
                        .selectable_label(self.whitespace == w, w.label())
                        .clicked()
                    {
                        self.whitespace = w;
                        changed = true;
                    }
                }
                changed
            })
        });
        ui.add_space(6.0);
        ui.checkbox(&mut self.index, "Also stage the changes (--index)");
        ui.checkbox(
            &mut self.three_way,
            "Three-way merge when it doesn't fit (--3way)",
        );
        ui.checkbox(&mut self.reverse, "Apply in reverse (undo this patch)");
        ui.add_space(8.0);
        let file = self.file.trim();
        let valid = !file.is_empty() && self.patch_path().is_file();
        if !file.is_empty() && !valid {
            ui.label(
                RichText::new("That file doesn't exist.")
                    .color(t.danger)
                    .small(),
            );
        }
        buttons(ui, "Apply", valid, enter, || self.action())
    }

    /// The commands: staging variants first refresh the index, whose cached file stats go stale
    /// when a file was rewritten (and `status` runs with optional locks off, so it never fixes
    /// them), which `git apply --index` reports as "does not match index".
    fn steps(&self) -> Vec<Vec<String>> {
        let mut steps = Vec::new();
        if self.index || self.three_way {
            steps.push(vec![
                "update-index".to_owned(),
                "-q".to_owned(),
                "--refresh".to_owned(),
            ]);
        }
        steps.push(apply_args(self));
        steps
    }

    fn action(&self) -> Action {
        let name = PathBuf::from(self.file.trim())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Action::Ops {
            label: format!("Apply patch {name}"),
            ops: self.steps(),
            refresh: Refresh::All,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_ok(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn file(path: &str, code: char, area: Area) -> StatusFile {
        StatusFile {
            path: path.to_owned(),
            old_path: None,
            code,
            area,
        }
    }

    /// A repository with a modified, a staged, a new tracked and an untracked file, and a stash.
    #[test]
    fn saves_staged_unstaged_and_stash_patches_that_apply_back() {
        let base = std::env::temp_dir().join(format!("gitcrafted-patch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (repo, copy) = (base.join("repo"), base.join("copy"));
        std::fs::create_dir_all(&repo).ok();
        assert!(git_ok(&repo, &["init", "-q", "-b", "main"]));
        let w = |name: &str, text: &str| std::fs::write(repo.join(name), text).unwrap_or_default();
        w("a.txt", "a1\n");
        w("b.txt", "b1\n");
        w("c.txt", "c1\n");
        assert!(git_ok(&repo, &["add", "."]));
        assert!(git_ok(&repo, &["commit", "-q", "-m", "base"]));
        assert!(git_ok(
            &base,
            &["clone", "-q", repo.to_str().unwrap_or(""), "copy"]
        ));

        // Staged: a.txt. Unstaged: b.txt (tracked) and new.txt (untracked).
        w("a.txt", "a1\na2\n");
        assert!(git_ok(&repo, &["add", "a.txt"]));
        w("b.txt", "b1\nb2\n");
        w("new.txt", "brand new\n");

        let staged = make_patch(
            &repo,
            &PatchSource::Work {
                staged: true,
                files: vec![file("a.txt", 'M', Area::Staged)],
            },
        )
        .unwrap_or_default();
        let text = String::from_utf8_lossy(&staged).into_owned();
        assert!(text.contains("+a2") && !text.contains("b2"), "{text}");

        // Only the selected unstaged files, the untracked one included.
        let unstaged = make_patch(
            &repo,
            &PatchSource::Work {
                staged: false,
                files: vec![
                    file("b.txt", 'M', Area::Unstaged),
                    file("new.txt", '?', Area::Untracked),
                ],
            },
        )
        .unwrap_or_default();
        let text = String::from_utf8_lossy(&unstaged).into_owned();
        assert!(
            text.contains("+b2") && text.contains("+brand new"),
            "{text}"
        );
        assert!(!text.contains("a2"), "{text}");

        // Applying it into another clone gives the same files.
        let patch_file = base.join("u.patch");
        assert!(
            write_patch(
                &repo,
                &PatchSource::Work {
                    staged: false,
                    files: vec![
                        file("b.txt", 'M', Area::Unstaged),
                        file("new.txt", '?', Area::Untracked),
                    ],
                },
                &patch_file
            )
            .is_ok()
        );
        let dialog = ApplyDialog {
            repo: PathBuf::new(),
            file: patch_file.display().to_string(),
            whitespace: Whitespace::Warn,
            index: false,
            three_way: false,
            reverse: false,
            focus: false,
        };
        let args = apply_args(&dialog);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        assert!(git_ok(&copy, &args));
        assert_eq!(
            std::fs::read_to_string(copy.join("b.txt")).unwrap_or_default(),
            "b1\nb2\n"
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("new.txt")).unwrap_or_default(),
            "brand new\n"
        );
        // And in reverse it goes away again.
        let mut back = apply_args(&ApplyDialog {
            repo: PathBuf::new(),
            reverse: true,
            ..dialog
        });
        let back: Vec<&str> = back.iter_mut().map(|s| s.as_str()).collect();
        assert!(git_ok(&copy, &back));
        assert!(!copy.join("new.txt").exists());

        // A stash, untracked files included.
        assert!(git_ok(
            &repo,
            &["stash", "push", "-q", "--include-untracked"]
        ));
        let stash = make_patch(
            &repo,
            &PatchSource::Stash {
                name: "stash@{0}".to_owned(),
            },
        )
        .unwrap_or_default();
        let text = String::from_utf8_lossy(&stash).into_owned();
        assert!(
            text.contains("+a2") && text.contains("+b2") && text.contains("+brand new"),
            "{text}"
        );

        // Nothing selected: nothing to save.
        assert!(
            write_patch(
                &repo,
                &PatchSource::Work {
                    staged: true,
                    files: vec![]
                },
                &base.join("none.patch")
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn suggested_names() {
        let one = PatchSource::Work {
            staged: true,
            files: vec![file("src/lib.rs", 'M', Area::Staged)],
        };
        assert_eq!(suggested_name(&one, Some("main")), "lib.rs-staged.patch");
        let many = PatchSource::Work {
            staged: false,
            files: vec![
                file("a", 'M', Area::Unstaged),
                file("b", 'M', Area::Unstaged),
            ],
        };
        assert_eq!(
            suggested_name(&many, Some("feat/x")),
            "feat-x-unstaged.patch"
        );
        let stash = PatchSource::Stash {
            name: "stash@{3}".to_owned(),
        };
        assert_eq!(suggested_name(&stash, None), "stash-3.patch");
    }

    #[test]
    fn apply_flags() {
        let d = ApplyDialog {
            repo: PathBuf::new(),
            file: " /tmp/x.patch ".to_owned(),
            whitespace: Whitespace::ErrorAll,
            index: true,
            three_way: true,
            reverse: true,
            focus: false,
        };
        assert_eq!(
            apply_args(&d),
            [
                "apply",
                "--whitespace=error-all",
                "--index",
                "--3way",
                "--reverse",
                "--",
                "/tmp/x.patch"
            ]
        );
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::test_support::*;

    fn dialog(file: &Path) -> ApplyDialog {
        ApplyDialog {
            repo: PathBuf::new(),
            file: file.display().to_string(),
            whitespace: Whitespace::Warn,
            index: false,
            three_way: false,
            reverse: false,
            focus: false,
        }
    }

    /// A clone of `repo` to apply patches into.
    fn twin(of: &Path, name: &str) -> std::path::PathBuf {
        let dir = scratch(name);
        git(
            dir.parent().unwrap_or(&dir),
            &[
                "clone",
                "-q",
                &of.display().to_string(),
                &dir.display().to_string(),
            ],
        );
        dir
    }

    #[test]
    fn ctrl_click_toggles_files_in_the_selection() {
        let dir = repo("pt-select");
        write(&dir, "a.txt", "changed\n");
        write(&dir, "n.txt", "new\n");
        let mut f = Fixture::open(&dir);
        let files = f.tab.work.unstaged.clone();
        assert_eq!(files.len(), 2);
        assert!(!f.tab.patch_selected(&files[0]));

        f.tab
            .patch_action(PatchAction::Toggle(files[0].clone()), &f.ctx);
        f.tab
            .patch_action(PatchAction::Toggle(files[1].clone()), &f.ctx);
        assert_eq!(f.tab.selected_in(false).len(), 2);
        assert!(f.tab.selected_in(true).is_empty());

        f.tab
            .patch_action(PatchAction::Toggle(files[0].clone()), &f.ctx);
        assert_eq!(f.tab.selected_in(false).len(), 1);
        assert!(f.tab.patch_selected(&files[1]));

        f.tab.patch_action(PatchAction::ClearSelection, &f.ctx);
        assert!(f.tab.selected_in(false).is_empty());

        // A selected file that stops being listed (committed, say) drops out by itself.
        f.tab
            .patch_action(PatchAction::Toggle(files[0].clone()), &f.ctx);
        git(&dir, &["add", "a.txt"]);
        git(&dir, &["commit", "-q", "-m", "commit a"]);
        f.reload();
        assert!(f.tab.selected_in(false).is_empty());
    }

    #[test]
    fn a_commit_patch_applies_with_am_and_keeps_the_message() {
        let dir = repo("pt-commit");
        write(&dir, "b.txt", "b\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "add b", "-m", "the body"]);
        let hash = git(&dir, &["rev-parse", "HEAD"]);
        let patch =
            make_patch(&dir, &PatchSource::Commit { hash: hash.clone() }).unwrap_or_default();
        assert!(String::from_utf8_lossy(&patch).starts_with("From "));

        let copy = twin(&dir, "pt-commit-copy");
        git(&copy, &["reset", "-q", "--hard", "HEAD~1"]);
        let file = scratch("pt-commit-file").join("c.patch");
        assert!(write_patch(&dir, &PatchSource::Commit { hash }, &file).is_ok());
        git(&copy, &["am", "-q", &file.display().to_string()]);
        assert_eq!(git(&copy, &["log", "-1", "--format=%s"]), "add b");
        assert_eq!(git(&copy, &["log", "-1", "--format=%b"]), "the body");
        assert_eq!(read(&copy, "b.txt"), "b\n");
    }

    #[test]
    fn a_merge_commit_falls_back_to_its_diff_against_the_first_parent() {
        let dir = repo("pt-merge");
        git(&dir, &["checkout", "-q", "-b", "side"]);
        write(&dir, "s.txt", "side\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "side work"]);
        git(&dir, &["checkout", "-q", "main"]);
        write(&dir, "m.txt", "main\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "main work"]);
        git(
            &dir,
            &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
        );
        let hash = git(&dir, &["rev-parse", "HEAD"]);
        let patch = make_patch(&dir, &PatchSource::Commit { hash }).unwrap_or_default();
        let text = String::from_utf8_lossy(&patch).into_owned();
        assert!(text.contains("s.txt") && !text.contains("m.txt"), "{text}");
        // It applies to the first parent's tree.
        let copy = twin(&dir, "pt-merge-copy");
        git(&copy, &["reset", "-q", "--hard", "HEAD^1"]);
        let file = scratch("pt-merge-file").join("m.patch");
        assert!(std::fs::write(&file, &patch).is_ok());
        git(&copy, &["apply", &file.display().to_string()]);
        assert_eq!(read(&copy, "s.txt"), "side\n");
    }

    #[test]
    fn staged_renames_binary_and_non_utf8_files_survive() {
        let dir = repo("pt-special");
        git(&dir, &["mv", "a.txt", "renamed.txt"]);
        let bytes: Vec<u8> = (0..=255u8).collect();
        assert!(std::fs::write(dir.join("blob.bin"), &bytes).is_ok());
        // Latin-1 text: not valid UTF-8.
        assert!(std::fs::write(dir.join("latin.txt"), [0x63u8, 0x61, 0x66, 0xe9, 0x0a]).is_ok());
        git(&dir, &["add", "."]);
        let f = Fixture::open(&dir);
        let staged = f.tab.work.staged.clone();
        assert_eq!(staged.len(), 3, "{staged:?}");
        let patch = make_patch(
            &dir,
            &PatchSource::Work {
                staged: true,
                files: staged,
            },
        )
        .unwrap_or_default();
        let text = String::from_utf8_lossy(&patch).into_owned();
        assert!(
            text.contains("rename from a.txt") && text.contains("GIT binary patch"),
            "{text}"
        );
        assert!(
            patch
                .windows(5)
                .any(|w| w == [0x63, 0x61, 0x66, 0xe9, 0x0a]),
            "bytes kept as they were"
        );

        // Applied to a clean clone with --index, everything comes back exactly.
        let copy = twin(&dir, "pt-special-copy");
        let file = scratch("pt-special-file").join("s.patch");
        assert!(std::fs::write(&file, &patch).is_ok());
        let mut sink = Fixture::open(&copy);
        let mut d = dialog(&file);
        d.index = true;
        sink.run(d.action());
        assert!(!sink.failed(), "{:?}", sink.status());
        assert_eq!(
            std::fs::read(copy.join("blob.bin")).unwrap_or_default(),
            bytes
        );
        assert_eq!(
            std::fs::read(copy.join("latin.txt")).unwrap_or_default(),
            [0x63, 0x61, 0x66, 0xe9, 0x0a]
        );
        assert!(copy.join("renamed.txt").exists() && !copy.join("a.txt").exists());
        assert_eq!(sink.tab.work.staged.len(), 3, "--index staged it all");
    }

    #[test]
    fn apply_options_change_what_happens() {
        let dir = repo("pt-apply");
        write(&dir, "a.txt", "one\ntwo \n");
        let patch = scratch("pt-apply-file").join("w.patch");
        assert!(
            write_patch(
                &dir,
                &PatchSource::Work {
                    staged: false,
                    files: vec![file("a.txt", 'M', Area::Unstaged)]
                },
                &patch
            )
            .is_ok()
        );
        git(&dir, &["checkout", "--", "a.txt"]);
        let mut f = Fixture::open(&dir);

        // The patch adds trailing whitespace: strict whitespace handling refuses it.
        let mut d = dialog(&patch);
        d.whitespace = Whitespace::Error;
        f.run(d.action());
        assert!(f.failed());
        assert_eq!(read(&dir, "a.txt"), "one\n");

        d.whitespace = Whitespace::NoWarn;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(read(&dir, "a.txt"), "one\ntwo \n");
        assert!(f.tab.work.staged.is_empty(), "not staged by default");

        // In reverse it goes away again.
        d.reverse = true;
        f.run(d.action());
        assert_eq!(read(&dir, "a.txt"), "one\n");

        // --index stages it.
        d.reverse = false;
        d.index = true;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(f.tab.work.staged.len(), 1);
    }

    #[test]
    fn three_way_applies_where_the_context_moved() {
        let dir = repo("pt-3way");
        write(&dir, "a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "nine lines"]);
        write(&dir, "a.txt", "1\n2\n3\n4\n5\n6\n7\n8\nnine\n");
        let patch = scratch("pt-3way-file").join("n.patch");
        assert!(
            write_patch(
                &dir,
                &PatchSource::Work {
                    staged: false,
                    files: vec![file("a.txt", 'M', Area::Unstaged)]
                },
                &patch
            )
            .is_ok()
        );
        // The first line changes under it, in the index and the tree.
        git(&dir, &["checkout", "--", "a.txt"]);
        write(&dir, "a.txt", "one\n2\n3\n4\n5\n6\n7\n8\n9\n");
        git(&dir, &["commit", "-q", "-am", "first line"]);
        let mut f = Fixture::open(&dir);
        let mut d = dialog(&patch);
        d.three_way = true;
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(read(&dir, "a.txt"), "one\n2\n3\n4\n5\n6\n7\n8\nnine\n");
    }

    #[test]
    fn a_missing_patch_file_is_an_error_not_a_change() {
        let dir = repo("pt-missing");
        let mut f = Fixture::open(&dir);
        f.run(dialog(&dir.join("nope.patch")).action());
        assert!(f.failed());
        assert_eq!(read(&dir, "a.txt"), "one\n");
    }

    fn file(path: &str, code: char, area: Area) -> StatusFile {
        StatusFile {
            path: path.to_owned(),
            old_path: None,
            code,
            area,
        }
    }

    #[test]
    fn a_stash_patch_can_be_applied_elsewhere() {
        let dir = repo("pt-stash");
        write(&dir, "a.txt", "stashed\n");
        write(&dir, "u.txt", "untracked\n");
        git(&dir, &["stash", "push", "-q", "--include-untracked"]);
        let file = scratch("pt-stash-file").join("s.patch");
        assert!(
            write_patch(
                &dir,
                &PatchSource::Stash {
                    name: "stash@{0}".into()
                },
                &file
            )
            .is_ok()
        );
        let copy = twin(&dir, "pt-stash-copy");
        git(&copy, &["apply", &file.display().to_string()]);
        assert_eq!(read(&copy, "a.txt"), "stashed\n");
        assert_eq!(read(&copy, "u.txt"), "untracked\n");
    }
}

/// Data-safety tests: saving never changes the repository; a failed apply changes nothing.
#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::repo::test_support::*;

    fn work_file(path: &str, area: Area) -> StatusFile {
        StatusFile {
            path: path.to_owned(),
            old_path: None,
            code: 'M',
            area,
        }
    }

    #[test]
    fn a_failed_apply_changes_nothing_at_all() {
        let dir = repo("sf-apply-atomic");
        commit_file(&dir, "b.txt", "b\n", "b");
        write(&dir, "a.txt", "one\npatched\n");
        write(&dir, "b.txt", "b\npatched\n");
        let patch = scratch("sf-apply-atomic-file").join("p.patch");
        let files = vec![
            work_file("a.txt", Area::Unstaged),
            work_file("b.txt", Area::Unstaged),
        ];
        assert!(
            write_patch(
                &dir,
                &PatchSource::Work {
                    staged: false,
                    files
                },
                &patch
            )
            .is_ok()
        );
        git(&dir, &["checkout", "--", "."]);
        // b.txt moves on: the patch's second file no longer fits; a.txt still would.
        commit_file(&dir, "b.txt", "something else\n", "b changed");
        write(&dir, "u.txt", "untracked\n");
        let mut f = Fixture::open(&dir);
        let before = Snapshot::take(&dir);
        for index in [false, true] {
            let d = ApplyDialog {
                repo: PathBuf::new(),
                file: patch.display().to_string(),
                whitespace: Whitespace::Warn,
                index,
                three_way: false,
                reverse: false,
                focus: false,
            };
            f.run(d.action());
            assert!(f.failed());
            assert_same(
                &before,
                &Snapshot::take(&dir),
                &format!("failed apply, index {index}"),
            );
        }
    }

    #[test]
    fn a_patch_of_a_file_named_like_a_pattern_holds_only_that_file() {
        let dir = repo("sf-patch-glob");
        commit_file(&dir, "*.txt", "committed\n", "star");
        write(&dir, "*.txt", "edited\n");
        write(&dir, "a.txt", "edited\n");
        git(&dir, &["add", "*.txt"]);
        git(&dir, &["add", "a.txt"]);
        for staged in [true, false] {
            if !staged {
                git(&dir, &["reset", "-q"]);
            }
            let area = if staged { Area::Staged } else { Area::Unstaged };
            let patch = make_patch(
                &dir,
                &PatchSource::Work {
                    staged,
                    files: vec![work_file("*.txt", area)],
                },
            )
            .unwrap_or_default();
            let text = String::from_utf8_lossy(&patch).into_owned();
            assert!(
                text.contains("b/*.txt") && !text.contains("a.txt"),
                "staged {staged}: {text}"
            );
        }
    }

    #[test]
    fn saving_any_patch_leaves_the_repository_untouched() {
        let dir = repo("sf-save-readonly");
        commit_file(&dir, "b.txt", "b\n", "b");
        write(&dir, "a.txt", "stashed\n");
        git(&dir, &["stash", "push", "-q"]);
        write(&dir, "a.txt", "edited\n");
        write(&dir, "b.txt", "staged\n");
        git(&dir, &["add", "b.txt"]);
        write(&dir, "u.txt", "untracked\n");
        let head = git(&dir, &["rev-parse", "HEAD"]);
        let before = Snapshot::take(&dir);
        let out = scratch("sf-save-readonly-out");
        let sources = [
            PatchSource::Work {
                staged: true,
                files: vec![work_file("b.txt", Area::Staged)],
            },
            PatchSource::Work {
                staged: false,
                files: vec![
                    work_file("a.txt", Area::Unstaged),
                    StatusFile {
                        code: '?',
                        ..work_file("u.txt", Area::Untracked)
                    },
                ],
            },
            PatchSource::Stash {
                name: "stash@{0}".into(),
            },
            PatchSource::Commit { hash: head },
        ];
        for (i, source) in sources.iter().enumerate() {
            assert!(
                write_patch(&dir, source, &out.join(format!("{i}.patch"))).is_ok(),
                "{source:?}"
            );
            assert_same(&before, &Snapshot::take(&dir), &format!("{source:?}"));
        }
    }
}

/// Regression tests (review round 2): a relative patch path is the repository's.
#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::repo::test_support::*;

    #[test]
    fn a_relative_patch_path_is_found_in_the_repository() {
        let dir = repo("rv-relative-patch");
        write(&dir, "a.txt", "one\npatched\n");
        let file = StatusFile {
            path: "a.txt".into(),
            old_path: None,
            code: 'M',
            area: Area::Unstaged,
        };
        assert!(
            write_patch(
                &dir,
                &PatchSource::Work {
                    staged: false,
                    files: vec![file]
                },
                &dir.join("fix.patch")
            )
            .is_ok()
        );
        git(&dir, &["checkout", "--", "a.txt"]);
        let mut d = ApplyDialog {
            repo: dir.clone(),
            file: " fix.patch ".into(),
            whitespace: Whitespace::Warn,
            index: false,
            three_way: false,
            reverse: false,
            focus: false,
        };
        assert_eq!(d.patch_path(), dir.join("fix.patch"));
        assert!(d.patch_path().is_file());
        let mut f = Fixture::open(&dir);
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(read(&dir, "a.txt"), "one\npatched\n");

        // An absolute path is used as it is; a missing relative one is not looked for elsewhere.
        d.file = dir.join("fix.patch").display().to_string();
        assert_eq!(d.patch_path(), dir.join("fix.patch"));
        d.file = "no-such.patch".into();
        assert!(!d.patch_path().is_file());
    }
}
