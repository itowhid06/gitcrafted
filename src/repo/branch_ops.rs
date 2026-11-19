//! Dialogs for merging (fast-forward, squash, no-commit, message), stashing (only the selected
//! files, keep the index) and for a checkout that local changes block (stash, switch, reapply).

use egui::{RichText, Ui};

use super::dialogs::{Outcome, buttons, title};
use super::ext::{DialogBody, dialog};
use super::sync::{pick, row};
use super::{Action, Dialog, Refresh, RepoTab, Undo, ops, widgets};
use crate::theme::Tokens;

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
        buttons(ui, "Merge", true, enter, || self.action())
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

struct BlockedCheckout {
    target: String,
    discard: bool,
}

/// The dialog to offer when `label` (a checkout) failed with `err` because of local changes.
pub(super) fn blocked_checkout(label: &str, err: &str) -> Option<Dialog> {
    let target = label.strip_prefix("Checkout ")?.trim();
    let blocked = err.contains("would be overwritten by checkout")
        || err.contains("commit your changes or stash them before you switch");
    (blocked && !target.is_empty()).then(|| {
        dialog(BlockedCheckout {
            target: target.to_owned(),
            discard: false,
        })
    })
}

/// Stash what is in the way (untracked files too), switch, bring the stash back.
fn switch_with_changes(target: &str) -> Action {
    Action::Ops {
        label: format!("Switch to {target} with your changes"),
        ops: vec![
            vec![
                "stash".into(),
                "push".into(),
                "--include-untracked".into(),
                "-m".into(),
                format!("gitcrafted: before switching to {target}"),
            ],
            vec!["checkout".into(), target.to_owned()],
            vec!["stash".into(), "pop".into(), "--index".into()],
        ],
        refresh: Refresh::All,
    }
}

/// Switch and lose the local changes.
fn discard_and_switch(target: &str) -> Action {
    Action::Ops {
        label: format!("Checkout {target}"),
        ops: ops(&[&["checkout", "--force", target]]),
        refresh: Refresh::All,
    }
}

impl DialogBody for BlockedCheckout {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, _enter: bool) -> Outcome {
        title(
            ui,
            "Local changes are in the way",
            &format!("Switching to {} would overwrite them.", self.target),
            t,
        );
        let mut outcome = Outcome::Open;
        if widgets::primary_button(ui, "Stash, switch and reapply", true, ui.available_width())
            .on_hover_text("Stashes your changes, checks out, then pops the stash back")
            .clicked()
        {
            outcome = Outcome::Done(switch_with_changes(&self.target));
        }
        ui.add_space(8.0);
        ui.checkbox(
            &mut self.discard,
            RichText::new(
                "Throw my local changes away (untracked files in its way are replaced too)",
            )
            .color(t.danger),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.discard,
                    egui::Button::new("Discard changes and switch"),
                )
                .clicked()
            {
                outcome = Outcome::Done(discard_and_switch(&self.target));
            }
            if ui.button("Cancel").clicked() {
                outcome = Outcome::Cancel;
            }
        });
        outcome
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
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
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
        assert!(again.is_some_and(|x| !f.tab.patch_selected(&x)));
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
        f.pump();
        assert!(f.tab.patch_sel.is_empty());
    }
}
