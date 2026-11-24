//! Rewriting history you may already have published: force push (with a lease, behind a
//! confirmation), and the "Edit message…" entry of a commit's menu.
//!
//! Kept in a file of its own so the rest of the tree only carries small hooks.

use egui::Ui;

use super::{Action, Dialog, RepoTab, Reword, WIP_HASH, ops};
use crate::git::Branch;

impl RepoTab {
    /// Where `branch` is pushed: its upstream's remote and branch name, else the default remote
    /// and the same name. `None` without a remote.
    fn push_target(&self, branch: &str) -> Option<(String, String, bool)> {
        if !super::dialogs::valid_ref_name(branch) {
            return None;
        }
        let b = self.refs.local.iter().find(|b| b.name == branch);
        if let Some(b) = b
            && !b.upstream_gone()
            && let Some((remote, name)) = b.upstream_remote_branch()
            && super::dialogs::valid_ref_name(remote)
            && super::dialogs::valid_ref_name(name)
        {
            return Some((remote.to_owned(), name.to_owned(), true));
        }
        let remote = self.default_remote()?;
        super::dialogs::valid_ref_name(&remote).then(|| (remote, branch.to_owned(), false))
    }

    /// The confirmation that force-pushes `branch`. By default `--force-with-lease`: git refuses
    /// when the remote has commits this repository hasn't fetched. The checkbox drops that check.
    pub(super) fn force_push_dialog(&self, branch: &str) -> Option<Dialog> {
        let (remote, name, tracked) = self.push_target(branch)?;
        let (lease, plain) = force_push_args(&remote, branch, &name, tracked);
        Some(Dialog::confirm(
            "Force push",
            format!(
                "Replace {remote}/{name} with {branch}? Commits on the remote that {branch} doesn't have are removed for everyone. \
                 git refuses if the remote has commits you haven't fetched."
            ),
            "Force push",
            ops(&[&lease.iter().map(String::as_str).collect::<Vec<_>>()]),
            Some((
                "Even if the remote has commits I haven't fetched (--force)".to_owned(),
                ops(&[&plain.iter().map(String::as_str).collect::<Vec<_>>()]),
            )),
        ))
    }

    /// The "Force push…" entry of a local branch's menu.
    pub(super) fn force_push_item(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        b: &Branch,
        idle: bool,
    ) {
        let Some((remote, _, _)) = self.push_target(&b.name) else {
            return;
        };
        if ui
            .add_enabled(idle, egui::Button::new(format!("Force push to {remote}…")))
            .on_hover_text("Overwrite the remote branch with this one (with a lease)")
            .clicked()
            && let Some(dialog) = self.force_push_dialog(&b.name)
        {
            acts.push(Action::Dialog(dialog));
        }
    }

    /// The "Edit message…" entry of a commit's menu: shows the commit and opens its message editor.
    pub(super) fn edit_message_item(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        hash: &str,
        idle: bool,
    ) {
        if ui
            .add_enabled(idle && hash != WIP_HASH, egui::Button::new("Edit message…"))
            .on_hover_text("Only for commits of the checked-out branch; amends the latest, rebases for an older one")
            .clicked()
        {
            acts.push(Action::EditMessage(hash.to_owned()));
        }
    }

    /// Selects `hash` and opens its message editor once its details are loaded.
    pub(super) fn start_edit_message(&mut self, hash: String, ctx: &egui::Context) {
        self.pending_edit = None;
        self.compare = None;
        self.show_local_changes = false;
        if self.detail.as_ref().is_some_and(|d| d.hash == hash) {
            self.open_message_editor();
            return;
        }
        self.pending_edit = Some(hash.clone());
        if self.detail_error.is_some() && self.detail_hash.as_deref() == Some(hash.as_str()) {
            self.load_detail(hash, ctx);
        } else {
            self.goto(&hash, ctx);
        }
    }

    /// Opens the editor for the commit shown in the detail pane, when its message can be edited:
    /// the same rules as the double-click on the message.
    pub(super) fn open_message_editor(&mut self) {
        let Some(d) = &self.detail else { return };
        let is_head = d.hash == self.refs.head;
        let problem = if self.busy_op.is_some() {
            Some("Wait for the running operation to finish")
        } else if !d.in_head {
            Some("Only commits of the checked-out branch can be edited: check the branch out first")
        } else if !is_head && d.merges_after {
            Some("Merges came after this commit: editing its message would flatten them")
        } else if !is_head && d.parents.is_empty() {
            Some("The first commit of a repository can't be edited this way")
        } else {
            None
        };
        if let Some(problem) = problem {
            self.status = Some((problem.to_owned(), true));
            return;
        }
        let (subject, body) = d
            .message
            .split_once('\n')
            .map(|(s, b)| (s, b.trim()))
            .unwrap_or((d.message.as_str(), ""));
        self.reword = Some(Reword {
            hash: d.hash.clone(),
            subject: subject.to_owned(),
            body: body.to_owned(),
            focus: true,
        });
    }
}

/// The force push commands: with a lease, and plain `--force`. `-u` when the branch has no
/// upstream yet.
fn force_push_args(
    remote: &str,
    branch: &str,
    remote_branch: &str,
    tracked: bool,
) -> (Vec<String>, Vec<String>) {
    let build = |flags: &[&str]| {
        let mut args = vec!["push".to_owned()];
        args.extend(flags.iter().map(|f| (*f).to_owned()));
        if !tracked {
            args.push("-u".to_owned());
        }
        args.push(remote.to_owned());
        args.push(format!("{branch}:{remote_branch}"));
        args
    };
    // `--force-if-includes`: a background fetch must not turn the lease into a plain force.
    (
        build(&["--force-with-lease", "--force-if-includes"]),
        build(&["--force"]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn force_push_args_track_new_branches_only() {
        let (lease, plain) = force_push_args("origin", "feat", "feat", true);
        assert_eq!(
            lease,
            [
                "push",
                "--force-with-lease",
                "--force-if-includes",
                "origin",
                "feat:feat"
            ]
        );
        assert_eq!(plain, ["push", "--force", "origin", "feat:feat"]);
        let (lease, _) = force_push_args("up", "a", "b", false);
        assert_eq!(
            lease,
            [
                "push",
                "--force-with-lease",
                "--force-if-includes",
                "-u",
                "up",
                "a:b"
            ]
        );
    }

    /// Against real git: a plain push is rejected after amending, the lease push goes through,
    /// and the lease refuses when the remote moved without us knowing.
    #[test]
    fn lease_push_replaces_amended_commit_but_not_unseen_ones() {
        let git = |dir: &Path, args: &[&str]| -> bool {
            Command::new("git")
                .arg("-C")
                .arg(dir)
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
        let base = std::env::temp_dir().join(format!("gitcrafted-force-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (remote, a, b) = (base.join("remote.git"), base.join("a"), base.join("b"));
        std::fs::create_dir_all(&base).ok();
        assert!(git(
            &base,
            &["init", "-q", "--bare", "-b", "main", "remote.git"]
        ));
        let url = remote.display().to_string();
        assert!(git(&base, &["clone", "-q", &url, "a"]));
        assert!(git(&a, &["commit", "-q", "--allow-empty", "-m", "one"]));
        assert!(git(&a, &["push", "-q", "-u", "origin", "main"]));
        assert!(git(&base, &["clone", "-q", &url, "b"]));

        // Amend in a: a plain push now fails, the lease push works.
        assert!(git(
            &a,
            &[
                "commit",
                "-q",
                "--amend",
                "--allow-empty",
                "-m",
                "one, amended"
            ]
        ));
        assert!(!git(&a, &["push", "-q"]));
        let (lease, plain) = force_push_args("origin", "main", "main", true);
        let lease: Vec<&str> = lease.iter().map(String::as_str).collect();
        let plain: Vec<&str> = plain.iter().map(String::as_str).collect();
        assert!(git(&a, &lease));

        // b pushes a commit a hasn't fetched; a amends again: the lease refuses, --force wins.
        assert!(git(&b, &["pull", "-q", "--rebase"]));
        assert!(git(&b, &["commit", "-q", "--allow-empty", "-m", "two"]));
        assert!(git(&b, &["push", "-q"]));
        assert!(git(
            &a,
            &[
                "commit",
                "-q",
                "--amend",
                "--allow-empty",
                "-m",
                "one, again"
            ]
        ));
        assert!(!git(&a, &lease));
        assert!(git(&a, &plain));
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::test_support::*;
    use crate::repo::{Refresh, short};

    fn commit(dir: &std::path::Path, name: &str, message: &str) -> String {
        write(dir, name, &format!("{message}\n"));
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// The commands of a force push confirmation: (with a lease, plain --force).
    fn confirm_ops(dialog: Dialog) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
        let Dialog::Confirm {
            ops, alternative, ..
        } = dialog
        else {
            unreachable!("a confirmation")
        };
        (ops, alternative.map(|(_, alt)| alt).unwrap_or_default())
    }

    fn run_ops(f: &mut Fixture, ops: Vec<Vec<String>>) {
        f.run(Action::Ops {
            label: "Force push".to_owned(),
            ops,
            refresh: Refresh::All,
        });
    }

    #[test]
    fn force_push_confirmation_uses_a_lease_unless_overridden() {
        let (work, bare) = repo_with_remote("rw-force");
        commit(&work, "b.txt", "second");
        git(&work, &["push", "-q"]);
        let other = clone_of(&bare, "rw-force-other");
        let mut f = Fixture::open(&work);

        let dialog = f.tab.force_push_dialog("main");
        assert!(dialog.is_some());
        let (lease, plain) = confirm_ops(dialog.unwrap_or_else(|| Dialog::stash()));
        assert_eq!(
            lease[0],
            [
                "push",
                "--force-with-lease",
                "--force-if-includes",
                "origin",
                "main:main"
            ]
        );
        assert_eq!(plain[0], ["push", "--force", "origin", "main:main"]);

        // Amend: the lease goes through.
        git(&work, &["commit", "-q", "--amend", "-m", "second, amended"]);
        f.reload();
        run_ops(&mut f, lease.clone());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&bare, &["rev-parse", "main"]),
            git(&work, &["rev-parse", "HEAD"])
        );

        // Others pushed meanwhile: the lease refuses, the plain force overrides.
        git(&other, &["fetch", "-q"]);
        git(&other, &["reset", "-q", "--hard", "origin/main"]);
        commit(&other, "o.txt", "theirs");
        git(&other, &["push", "-q"]);
        git(&work, &["commit", "-q", "--amend", "-m", "second, again"]);
        f.reload();
        run_ops(&mut f, lease);
        assert!(f.failed());
        run_ops(&mut f, plain);
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&bare, &["log", "-1", "--format=%s", "main"]),
            "second, again"
        );
    }

    #[test]
    fn force_push_publishes_an_untracked_branch_and_needs_a_remote() {
        let (work, bare) = repo_with_remote("rw-force-new");
        git(&work, &["checkout", "-q", "-b", "topic"]);
        commit(&work, "t.txt", "topic work");
        let mut f = Fixture::open(&work);
        let (lease, _) = confirm_ops(
            f.tab
                .force_push_dialog("topic")
                .unwrap_or_else(|| Dialog::stash()),
        );
        assert_eq!(
            lease[0],
            [
                "push",
                "--force-with-lease",
                "--force-if-includes",
                "-u",
                "origin",
                "topic:topic"
            ]
        );
        run_ops(&mut f, lease);
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&bare, &["rev-parse", "topic"]),
            git(&work, &["rev-parse", "HEAD"])
        );
        assert_eq!(git(&work, &["config", "branch.topic.remote"]), "origin");

        // No remote: nothing to push to.
        let lonely = repo("rw-force-none");
        let f = Fixture::open(&lonely);
        assert!(f.tab.force_push_dialog("main").is_none());
    }

    #[test]
    fn push_targets_follow_the_upstream() {
        let (work, _bare) = repo_with_remote("rw-target");
        git(&work, &["checkout", "-q", "-b", "feat"]);
        let f = Fixture::open(&work);
        assert_eq!(
            f.tab.push_target("main"),
            Some(("origin".into(), "main".into(), true))
        );
        assert_eq!(
            f.tab.push_target("feat"),
            Some(("origin".into(), "feat".into(), false))
        );
    }

    /// A history: initial, "second" (with a body), "third".
    fn history(name: &str) -> (std::path::PathBuf, Vec<String>) {
        let dir = repo(name);
        let root = git(&dir, &["rev-parse", "HEAD"]);
        write(&dir, "b.txt", "b\n");
        git(&dir, &["add", "."]);
        git(
            &dir,
            &[
                "commit",
                "-q",
                "-m",
                "second",
                "-m",
                "details of the second",
            ],
        );
        let second = git(&dir, &["rev-parse", "HEAD"]);
        let third = commit(&dir, "c.txt", "third");
        (dir, vec![root, second, third])
    }

    #[test]
    fn the_editor_opens_for_the_latest_and_for_older_commits() {
        let (dir, hashes) = history("rw-edit-open");
        let mut f = Fixture::open(&dir);

        f.tab.start_edit_message(hashes[2].clone(), &f.ctx);
        f.pump();
        let r = f.tab.reword.as_ref();
        assert_eq!(r.map(|r| r.hash.as_str()), Some(hashes[2].as_str()));
        assert_eq!(r.map(|r| r.subject.as_str()), Some("third"));

        f.tab.reword = None;
        f.tab.start_edit_message(hashes[1].clone(), &f.ctx);
        f.pump();
        let r = f.tab.reword.as_ref();
        assert_eq!(r.map(|r| r.subject.as_str()), Some("second"));
        assert_eq!(r.map(|r| r.body.as_str()), Some("details of the second"));
        assert!(!f.failed());
    }

    #[test]
    fn the_editor_refuses_commits_that_cannot_be_reworded() {
        let (dir, hashes) = history("rw-edit-refuse");
        // The first commit has no parent to rebase from.
        let mut f = Fixture::open(&dir);
        f.tab.start_edit_message(hashes[0].clone(), &f.ctx);
        f.pump();
        assert!(f.tab.reword.is_none());
        assert!(
            f.status()
                .is_some_and(|(m, e)| e && m.contains("first commit")),
            "{:?}",
            f.status()
        );

        // A commit of another branch.
        git(&dir, &["checkout", "-q", "-b", "other", &hashes[1]]);
        let on_other = commit(&dir, "o.txt", "only on other");
        git(&dir, &["checkout", "-q", "main"]);
        f.reload();
        f.tab.status = None;
        f.tab.start_edit_message(on_other, &f.ctx);
        f.pump();
        assert!(f.tab.reword.is_none());
        assert!(
            f.status()
                .is_some_and(|(m, e)| e && m.contains("checked-out branch")),
            "{:?}",
            f.status()
        );

        // Merges came after it: rewording would flatten them.
        git(
            &dir,
            &["merge", "-q", "--no-ff", "-m", "merge other", "other"],
        );
        f.reload();
        f.tab.status = None;
        f.tab.start_edit_message(hashes[1].clone(), &f.ctx);
        f.pump();
        assert!(f.tab.reword.is_none());
        assert!(
            f.status()
                .is_some_and(|(m, e)| e && m.contains("Merges came after")),
            "{:?}",
            f.status()
        );

        // The latest commit is still fine, even though it is a merge.
        let head = git(&dir, &["rev-parse", "HEAD"]);
        f.tab.status = None;
        f.tab.start_edit_message(head, &f.ctx);
        f.pump();
        assert!(f.tab.reword.is_some(), "{:?}", f.status());
    }

    #[test]
    fn the_editor_waits_for_a_running_operation() {
        let (dir, hashes) = history("rw-edit-busy");
        let mut f = Fixture::open(&dir);
        f.tab.start_edit_message(hashes[2].clone(), &f.ctx);
        f.pump();
        f.tab.reword = None;
        f.tab.busy_op = Some("Fetch".to_owned());
        f.tab.open_message_editor();
        assert!(f.tab.reword.is_none());
        assert!(f.status().is_some_and(|(m, e)| e && m.contains("Wait")));
    }

    #[test]
    fn rewording_the_latest_commit_amends_and_can_be_undone() {
        let (dir, hashes) = history("rw-reword-head");
        let mut f = Fixture::open(&dir);
        let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
        f.run(Action::Reword {
            hash: hashes[2].clone(),
            message: "third, reworded\n\nwith a body".to_owned(),
        });
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "third, reworded");
        assert_eq!(git(&dir, &["log", "-1", "--format=%b"]), "with a body");
        assert_eq!(git(&dir, &["rev-parse", "HEAD^{tree}"]), tree);
        f.run(Action::Undo);
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), hashes[2]);
    }

    #[test]
    fn rewording_an_older_commit_rebases_what_follows() {
        let (dir, hashes) = history("rw-reword-old");
        let mut f = Fixture::open(&dir);
        let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
        f.run(Action::Reword {
            hash: hashes[1].clone(),
            message: "second, reworded".to_owned(),
        });
        let dir = f.dir().to_path_buf();
        f.pump_until(|_| git(&dir, &["log", "--format=%s"]).contains("second, reworded"));
        assert!(!f.failed(), "{:?}", f.status());
        let subjects = git(&dir, &["log", "--format=%s"]);
        assert_eq!(subjects, "third\nsecond, reworded\ninitial");
        assert_eq!(
            git(&dir, &["rev-parse", "HEAD^{tree}"]),
            tree,
            "only messages change"
        );
        assert_ne!(
            short(&git(&dir, &["rev-parse", "HEAD~1"])),
            short(&hashes[1])
        );
    }

    #[test]
    fn reword_editor_renders_and_handles_keys_without_deadlocking() {
        let (dir, hashes) = history("rw-reword-ui");
        let f = std::rc::Rc::new(std::cell::RefCell::new(Fixture::open(&dir)));

        // Verify rendering at both standard and narrow viewports does not deadlock or panic,
        // and Esc cancels the inline editor without emitting Action::Reword.
        for size in [[640.0, 480.0], [280.0, 360.0]] {
            {
                let mut fix = f.borrow_mut();
                let ctx = fix.ctx.clone();
                fix.tab.start_edit_message(hashes[2].clone(), &ctx);
                fix.pump();
                assert!(fix.tab.reword.is_some());
            }
            let acts = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let fix_ref = f.clone();
            let acts_ref = acts.clone();
            let mut harness = egui_kittest::Harness::builder()
                .with_size(size)
                .build_ui(move |ui| {
                    if ui.ctx().cumulative_frame_nr() == 0 {
                        crate::theme::install_fonts(ui.ctx());
                        ui.ctx().request_repaint();
                        return;
                    }
                    fix_ref
                        .borrow_mut()
                        .tab
                        .detail_panel(ui, &mut acts_ref.borrow_mut());
                });
            harness.run();
            harness.step();
            harness.key_press(egui::Key::Escape);
            harness.run();
            drop(harness);
            assert!(f.borrow().tab.reword.is_none());
            assert!(acts.borrow().is_empty());
        }

        // Open compare mode on the same commit, then verify `start_edit_message` clears `compare`,
        // focuses the summary field, and `Ctrl+Enter` emits `Action::Reword`.
        {
            let mut fix = f.borrow_mut();
            fix.run(Action::Compare {
                base: hashes[1].clone(),
                base_label: "second".to_owned(),
                target: Some(hashes[2].clone()),
                target_label: "third".to_owned(),
                since_fork: false,
            });
            assert!(fix.tab.compare.is_some());
            let ctx = fix.ctx.clone();
            fix.tab.start_edit_message(hashes[2].clone(), &ctx);
            fix.pump();
            assert!(fix.tab.compare.is_none());
            assert!(fix.tab.reword.is_some());
        }

        let acts = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let fix_ref = f.clone();
        let acts_ref = acts.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size([640.0, 480.0])
            .build_ui(move |ui| {
                if ui.ctx().cumulative_frame_nr() == 0 {
                    crate::theme::install_fonts(ui.ctx());
                    ui.ctx().request_repaint();
                    return;
                }
                fix_ref
                    .borrow_mut()
                    .tab
                    .detail_panel(ui, &mut acts_ref.borrow_mut());
            });
        harness.run();
        harness.step();
        harness.event(egui::Event::Text(" edited".to_owned()));
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        harness.run();
        drop(harness);

        assert!(f.borrow().tab.reword.is_none());
        let recorded = acts.borrow();
        assert_eq!(recorded.len(), 1);
        match &recorded[0] {
            Action::Reword { hash, message } => {
                assert_eq!(hash, &hashes[2]);
                assert!(message.contains("edited"), "unexpected message: {message}");
            }
            _ => unreachable!("expected Action::Reword"),
        }
    }
}

/// Data-safety tests for force pushing and rewording.
#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::repo::Refresh;
    use crate::repo::test_support::*;

    #[test]
    fn the_force_push_confirmation_is_safe_after_a_background_fetch() {
        let (work, bare) = repo_with_remote("sf-confirm-lease");
        commit_file(&work, "mine.txt", "mine\n", "mine");
        git(&work, &["push", "-q"]);
        let other = clone_of(&bare, "sf-confirm-lease-other");
        commit_file(&other, "theirs.txt", "theirs\n", "theirs");
        git(&other, &["push", "-q"]);
        git(&work, &["fetch", "-q"]);
        git(&work, &["commit", "-q", "--amend", "-m", "mine, amended"]);
        let mut f = Fixture::open(&work);
        let dialog = f.tab.force_push_dialog("main");
        let Some(Dialog::Confirm { ops, .. }) = dialog else {
            unreachable!("a confirmation")
        };
        f.run(Action::Ops {
            label: "Force push".to_owned(),
            ops,
            refresh: Refresh::All,
        });
        assert!(f.failed());
        assert_eq!(git(&bare, &["log", "-1", "--format=%s", "main"]), "theirs");
    }

    #[test]
    fn rewording_an_older_commit_over_unstaged_work_keeps_it() {
        let dir = repo("sf-reword-unstaged");
        let second = commit_file(&dir, "b.txt", "b\n", "second");
        commit_file(&dir, "c.txt", "c\n", "third");
        write(&dir, "a.txt", "uncommitted edit\n");
        write(&dir, "u.txt", "untracked\n");
        let before = Snapshot::take(&dir);
        let mut f = Fixture::open(&dir);
        f.run(Action::Reword {
            hash: second,
            message: "second, reworded".to_owned(),
        });
        let dir = f.dir().to_path_buf();
        f.pump_until(|_| git(&dir, &["log", "--format=%s"]).contains("second, reworded"));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&dir, &["log", "--format=%s"]),
            "third\nsecond, reworded\ninitial"
        );
        let after = Snapshot::take(&dir);
        assert_eq!(before.lost_in(&after), Vec::<String>::new());
        assert_eq!(before.index, after.index);
        assert!(
            git(&dir, &["stash", "list"]).is_empty(),
            "the autostash was given back"
        );
    }

    #[test]
    fn rewording_keeps_uncommitted_and_untracked_work() {
        let dir = repo("sf-reword-dirty");
        let second = commit_file(&dir, "b.txt", "b\n", "second");
        commit_file(&dir, "c.txt", "c\n", "third");
        write(&dir, "a.txt", "uncommitted edit\n");
        write(&dir, "b.txt", "staged edit\n");
        git(&dir, &["add", "b.txt"]);
        write(&dir, "u.txt", "untracked\n");
        for what in ["an older commit", "the latest"] {
            let hash = if what == "the latest" {
                git(&dir, &["rev-parse", "HEAD"])
            } else {
                second.clone()
            };
            let before = Snapshot::take(&dir);
            let mut f = Fixture::open(&dir);
            f.run(Action::Reword {
                hash,
                message: format!("{what}, reworded"),
            });
            f.pump();
            let after = Snapshot::take(&dir);
            assert_eq!(
                before.lost_in(&after),
                Vec::<String>::new(),
                "{what}: files"
            );
            assert_eq!(
                before.index, after.index,
                "{what}: staged changes stay staged"
            );
            assert_eq!(
                f.tab.work.in_progress, None,
                "{what}: no rebase left half done"
            );
            let reworded =
                git(&dir, &["log", "--format=%s"]).contains(&format!("{what}, reworded"));
            if what == "the latest" {
                assert!(reworded, "an amend of the message works over a dirty tree");
            } else {
                // Something is staged: refused, with nothing changed.
                assert!(!reworded && f.failed(), "{what}: {:?}", f.status());
                assert!(f.status().is_some_and(|(m, _)| m.contains("unstage")));
                assert_same(&before, &after, what);
            }
        }
    }
}
