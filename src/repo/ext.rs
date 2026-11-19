//! The plumbing for the extra repository features (reflog, bisect, LFS, maintenance, external
//! tools, issue links, merge / stash options…): one dialog hook, one action hook and the
//! toolbar's Tools menu. Each feature lives in a file of its own.
//!
//! Kept apart so the rest of the tree only carries a few hooks and a rebase from upstream stays
//! easy.

use std::path::PathBuf;

use egui::Ui;

use super::dialogs::Outcome;
use super::{Action, Dialog, OpKind, Refresh, RepoTab, Undo, WIP_HASH};
use crate::git;
use crate::theme::Tokens;

/// The body of a dialog that lives outside `dialogs.rs`.
pub trait DialogBody {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome;
}

/// Wraps a body as a dialog.
pub(super) fn dialog(body: impl DialogBody + 'static) -> Dialog {
    Dialog::Ext(Box::new(body))
}

/// The label of the push a Commit & Push runs once the commit is made.
pub(super) const PUSH_AFTER_COMMIT: &str = "Push after commit";

/// Commit & Push runs as two operations: the commit (undoable, clears the commit box like any
/// commit), then the push, which a sign-in retry can run again on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PushFollow {
    #[default]
    Idle,
    /// The commit is running; push when it succeeds.
    AfterCommit,
    /// The push is running (or waits for a sign-in to be retried).
    Pushing,
}

pub enum ExtAction {
    /// Commit, then push the branch.
    CommitPush,
    /// Start a program without waiting for it (an IDE, a diff tool).
    Spawn {
        program: String,
        args: Vec<String>,
        dir: Option<PathBuf>,
    },
    /// A file's changes in the diff tool git is configured with.
    DiffTool { path: String, staged: bool },
    /// A conflicted file in the merge tool git is configured with.
    MergeTool { path: String },
    /// Replace the repository's issue link rules.
    SaveIssueRules(Vec<super::issues::Rule>),
}

impl RepoTab {
    pub(super) fn ext_action(&mut self, action: ExtAction, ctx: &egui::Context) {
        match action {
            ExtAction::CommitPush => {
                if self.busy_op.is_some() {
                    return;
                }
                self.push_follow = PushFollow::AfterCommit;
                self.commit(ctx);
                if self.busy_op.is_none() {
                    // The commit didn't start (no summary, conflicts…): nothing to push after.
                    self.push_follow = PushFollow::Idle;
                }
            }
            ExtAction::Spawn { program, args, dir } => {
                if let Err(e) = spawn_detached(ctx, &program, &args, dir.as_deref()) {
                    self.status = Some((e, true));
                }
            }
            ExtAction::DiffTool { path, staged } => {
                if self.usable_tool("diff.tool", "diff").is_some() {
                    let mut args = vec!["difftool".to_owned(), "-y".to_owned()];
                    if staged {
                        args.push("--cached".into());
                    }
                    args.extend(["--".to_owned(), literal(&path)]);
                    self.spawn_git(ctx, args);
                }
            }
            ExtAction::MergeTool { path } => {
                if self.usable_tool("merge.tool", "merge").is_some() {
                    self.spawn_git(
                        ctx,
                        vec![
                            "mergetool".to_owned(),
                            "-y".to_owned(),
                            "--".to_owned(),
                            literal(&path),
                        ],
                    );
                }
            }
            ExtAction::SaveIssueRules(rules) => {
                self.save_issue_rules(&rules);
                self.forget_issue_rules(ctx);
                self.status = Some(("Issue links saved".to_owned(), false));
            }
        }
    }

    /// The push that follows a commit: a plain push when the branch tracks a remote one, else
    /// publish it. `None` without a branch or a remote.
    pub(super) fn push_step(&self) -> Option<Vec<String>> {
        let current = self.refs.current.clone()?;
        let tracked = self
            .refs
            .local
            .iter()
            .any(|b| b.name == current && !b.upstream.is_empty() && !b.upstream_gone());
        push_step_for(&current, tracked, self.default_remote())
    }

    /// Called when any operation ends: carries a Commit & Push on to its push.
    pub(super) fn after_op(
        &mut self,
        label: &str,
        result: &Result<String, String>,
        ctx: &egui::Context,
    ) {
        match (self.push_follow, label) {
            (PushFollow::AfterCommit, "Commit" | "Amend") => {
                self.push_follow = PushFollow::Idle;
                if result.is_ok()
                    && let Some(step) = self.push_step()
                {
                    self.push_follow = PushFollow::Pushing;
                    self.run_ops(
                        PUSH_AFTER_COMMIT,
                        vec![step],
                        Refresh::All,
                        OpKind::Normal(Undo::None, String::new()),
                        ctx,
                    );
                }
            }
            (PushFollow::Pushing, PUSH_AFTER_COMMIT) => match result {
                Ok(_) => {
                    self.push_follow = PushFollow::Idle;
                    // The commit is published: undoing it (or anything before it) with a reset
                    // would only make the branch diverge from the remote.
                    self.undo_stack.retain(|e| {
                        !e.undo
                            .iter()
                            .any(|op| op.first().is_some_and(|c| c == "reset"))
                    });
                    self.redo_stack.clear();
                }
                // The sign-in dialog runs the push again; stay on it.
                Err(e) if git::is_auth_error(e) => {}
                Err(_) => self.push_follow = PushFollow::Idle,
            },
            _ => {}
        }
    }

    /// The diff or merge tool git is configured with, when gitcrafted can start it; otherwise
    /// says why not.
    fn usable_tool(&mut self, key: &str, what: &str) -> Option<String> {
        let tool = git::run(Some(&self.path), &["config", "--get", key])
            .map(|v| v.trim().to_owned())
            .unwrap_or_default();
        if tool.is_empty() {
            self.status = Some((
                format!("No {what} tool is set: choose one in Tools → External tools…"),
                true,
            ));
            return None;
        }
        if super::external::needs_terminal(&tool) {
            self.status = Some((
                format!(
                    "{tool} runs in a terminal: use `git {what}tool` in the terminal panel, or choose a graphical tool in Tools → External tools…"
                ),
                true,
            ));
            return None;
        }
        Some(tool)
    }

    fn spawn_git(&mut self, ctx: &egui::Context, args: Vec<String>) {
        let mut full = vec!["-C".to_owned(), self.path.display().to_string()];
        full.extend(args);
        if let Err(e) = spawn_detached(ctx, "git", &full, None) {
            self.status = Some((e, true));
        }
    }

    /// The entries of the toolbar's Tools menu.
    pub(super) fn tools_menu(&self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let idle = self.busy_op.is_none();
        if ui.button("Reflog…").clicked() {
            acts.push(Action::Dialog(self.reflog_dialog()));
        }
        if self.bisecting(ui.ctx()) {
            if ui
                .add_enabled(idle, egui::Button::new("Stop bisect"))
                .clicked()
            {
                acts.push(super::bisect::stop_action());
            }
        } else if ui
            .add_enabled(idle, egui::Button::new("Start bisect…"))
            .clicked()
        {
            acts.push(Action::Dialog(self.bisect_dialog(None, None)));
        }
        if super::lfs::available() && ui.button("Git LFS…").clicked() {
            acts.push(Action::Dialog(self.lfs_dialog()));
        }
        ui.separator();
        if ui.button("Maintenance…").clicked() {
            acts.push(Action::Dialog(self.maintenance_dialog()));
        }
        if ui.button("Clean untracked files…").clicked() {
            acts.push(Action::Dialog(self.clean_dialog()));
        }
        ui.separator();
        ui.menu_button("Open repository in", |ui| {
            let apps = super::external::detected();
            if apps.is_empty() {
                ui.label("No editor found on the PATH");
            }
            for app in apps {
                if ui.button(&app.label).clicked() {
                    acts.push(Action::Ext(ExtAction::Spawn {
                        program: app.program.clone(),
                        args: vec![self.path.display().to_string()],
                        dir: None,
                    }));
                    ui.close();
                }
            }
        });
        if ui.button("External diff and merge tools…").clicked() {
            acts.push(Action::Dialog(self.external_tools_dialog()));
        }
        if ui.button("Issue links…").clicked() {
            acts.push(Action::Dialog(self.issue_dialog(ui.ctx())));
        }
    }

    /// The extra entries of a commit's right-click menu.
    pub(super) fn commit_menu_extras(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        hash: &str,
        idle: bool,
    ) {
        if hash == WIP_HASH {
            return;
        }
        self.edit_message_item(ui, acts, hash, idle);
        if ui
            .button("Save as patch…")
            .on_hover_text("This commit as a patch file (git format-patch)")
            .clicked()
        {
            acts.push(Action::Patch(super::patch::PatchAction::Save(
                super::patch::PatchSource::Commit {
                    hash: hash.to_owned(),
                },
            )));
        }
        self.bisect_menu_items(ui, acts, hash, idle);
    }
}

/// Starts `program` and returns at once; a thread reaps it and wakes the UI when it ends.
pub(super) fn spawn_detached(
    ctx: &egui::Context,
    program: &str,
    args: &[String],
    dir: Option<&std::path::Path>,
) -> Result<(), String> {
    let mut cmd = if program == "git" {
        git::git_cmd()
    } else {
        let mut c = std::process::Command::new(program);
        crate::shell_path::apply(&mut c);
        c
    };
    cmd.args(args);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    // A tool that reads the terminal would hang on nothing.
    cmd.stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not run {program}: {e}"))?;
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        ctx.request_repaint();
    });
    Ok(())
}

/// A plain push when the branch tracks a remote one, else publish it to `remote`.
pub(super) fn push_step_for(
    branch: &str,
    tracked: bool,
    remote: Option<String>,
) -> Option<Vec<String>> {
    if tracked {
        Some(vec!["push".to_owned()])
    } else {
        Some(vec![
            "push".to_owned(),
            "-u".to_owned(),
            remote?,
            branch.to_owned(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::test_support::*;

    #[test]
    fn the_push_after_a_commit_publishes_only_when_untracked() {
        assert_eq!(
            push_step_for("main", true, Some("origin".into())),
            Some(vec!["push".to_owned()])
        );
        assert_eq!(
            push_step_for("feat", false, Some("up".into())),
            Some(vec!["push".into(), "-u".into(), "up".into(), "feat".into()])
        );
        assert_eq!(push_step_for("feat", false, None), None);
    }

    fn stage_change(f: &mut Fixture, text: &str) {
        write(f.dir(), "a.txt", text);
        git(f.dir(), &["add", "a.txt"]);
        f.reload();
    }

    #[test]
    fn commit_and_push_commits_then_pushes() {
        let (work, bare) = repo_with_remote("ext-commit-push");
        let mut f = Fixture::open(&work);
        stage_change(&mut f, "changed\n");
        f.tab.commit_summary = "Change a".to_owned();
        f.tab.commit_body = "Because.".to_owned();
        f.tab.ext_action(ExtAction::CommitPush, &f.ctx);
        f.pump();
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(f.tab.push_follow, PushFollow::Idle, "done with this commit");
        assert_eq!(git(&work, &["log", "-1", "--format=%s"]), "Change a");
        assert_eq!(
            git(&bare, &["log", "-1", "--format=%s", "main"]),
            "Change a"
        );
        assert_eq!(
            git(&bare, &["log", "-1", "--format=%b", "main"]),
            "Because."
        );
    }

    #[test]
    fn commit_and_push_publishes_a_new_branch() {
        let (work, bare) = repo_with_remote("ext-commit-push-new");
        git(&work, &["checkout", "-q", "-b", "topic"]);
        let mut f = Fixture::open(&work);
        stage_change(&mut f, "topic\n");
        f.tab.commit_summary = "Topic work".to_owned();
        f.tab.ext_action(ExtAction::CommitPush, &f.ctx);
        f.pump();
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&bare, &["log", "-1", "--format=%s", "topic"]),
            "Topic work"
        );
        assert_eq!(git(&work, &["config", "branch.topic.remote"]), "origin");
    }

    #[test]
    fn commit_and_push_needs_a_summary_and_does_nothing_without() {
        let (work, bare) = repo_with_remote("ext-commit-push-empty");
        let mut f = Fixture::open(&work);
        stage_change(&mut f, "changed\n");
        let remote_head = git(&bare, &["rev-parse", "main"]);
        f.tab.ext_action(ExtAction::CommitPush, &f.ctx);
        f.pump();
        assert!(f.failed());
        assert_eq!(git(&work, &["log", "-1", "--format=%s"]), "initial");
        assert_eq!(git(&bare, &["rev-parse", "main"]), remote_head);
        assert_eq!(f.tab.push_follow, PushFollow::Idle);
    }

    #[test]
    fn without_a_remote_it_just_commits() {
        let dir = repo("ext-commit-no-remote");
        let mut f = Fixture::open(&dir);
        stage_change(&mut f, "changed\n");
        f.tab.commit_summary = "Local only".to_owned();
        f.tab.ext_action(ExtAction::CommitPush, &f.ctx);
        f.pump();
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "Local only");
    }

    #[test]
    fn amend_and_push_surfaces_the_rejected_push() {
        let (work, bare) = repo_with_remote("ext-amend-push");
        let mut f = Fixture::open(&work);
        let remote_head = git(&bare, &["rev-parse", "main"]);
        f.tab.amend = true;
        f.tab.commit_summary = "initial, amended".to_owned();
        f.tab.ext_action(ExtAction::CommitPush, &f.ctx);
        f.pump();
        // The amend happened locally; the push of rewritten history is refused.
        assert_eq!(
            git(&work, &["log", "-1", "--format=%s"]),
            "initial, amended"
        );
        assert!(f.failed());
        assert_eq!(git(&bare, &["rev-parse", "main"]), remote_head);
    }

    #[test]
    fn spawn_runs_a_program_and_reports_a_missing_one() {
        let dir = scratch("ext-spawn");
        let mut f = Fixture::open(&repo("ext-spawn-repo"));
        if cfg!(unix) {
            f.tab.ext_action(
                ExtAction::Spawn {
                    program: "sh".to_owned(),
                    args: vec!["-c".to_owned(), "echo spawned > out.txt".to_owned()],
                    dir: Some(dir.clone()),
                },
                &f.ctx,
            );
            let mut waited = 0;
            while !dir.join("out.txt").exists() && waited < 200 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                waited += 1;
            }
            assert_eq!(read(&dir, "out.txt").trim(), "spawned");
        }
        f.tab.ext_action(
            ExtAction::Spawn {
                program: "definitely-not-a-program-gitcrafted".to_owned(),
                args: vec![],
                dir: None,
            },
            &f.ctx,
        );
        assert!(f.failed());
    }
}

/// `path` as a pathspec git takes as it is: a file named `*.txt` or `[ab].c` means that file only,
/// never every file the pattern would match.
pub(super) fn literal(path: &str) -> String {
    format!(":(literal){path}")
}

/// Regression tests for Commit & Push (review round 2).
#[cfg(test)]
mod commit_push_tests {
    use super::*;
    use crate::repo::test_support::*;

    fn commit_and_push(f: &mut Fixture, summary: &str) {
        write(f.dir(), "a.txt", &format!("{summary}\n"));
        git(f.dir(), &["add", "a.txt"]);
        f.reload();
        f.tab.commit_summary = summary.to_owned();
        f.tab.commit_body = "body".to_owned();
        f.tab.ext_action(ExtAction::CommitPush, &f.ctx);
        f.pump();
    }

    #[test]
    fn the_commit_box_is_cleared_after_commit_and_push() {
        let (work, _bare) = repo_with_remote("cp-clear");
        let mut f = Fixture::open(&work);
        commit_and_push(&mut f, "first");
        assert!(!f.failed(), "{:?}", f.status());
        assert!(f.tab.commit_summary.is_empty() && f.tab.commit_body.is_empty());
        assert!(!f.tab.amend);
    }

    #[test]
    fn after_commit_and_push_undo_cannot_rewind_the_published_branch() {
        let (work, bare) = repo_with_remote("cp-undo");
        let mut f = Fixture::open(&work);
        // A local commit first (undoable), then Commit & Push publishes both.
        write(&work, "b.txt", "local\n");
        git(&work, &["add", "b.txt"]);
        f.reload();
        f.tab.commit_summary = "local".to_owned();
        f.run(Action::Commit);
        assert_eq!(f.tab.undo_stack.len(), 1);
        commit_and_push(&mut f, "published");
        assert!(!f.failed(), "{:?}", f.status());
        let pushed = git(&bare, &["rev-parse", "main"]);
        assert_eq!(git(&work, &["rev-parse", "HEAD"]), pushed);
        assert!(
            f.tab.undo_stack.is_empty(),
            "nothing left that would reset a published branch"
        );
        f.run(Action::Undo);
        assert_eq!(
            git(&work, &["rev-parse", "HEAD"]),
            pushed,
            "Ctrl+Z doesn't diverge from origin"
        );
    }

    #[test]
    fn a_rejected_push_leaves_the_commit_undoable() {
        let (work, bare) = repo_with_remote("cp-rejected");
        let other = clone_of(&bare, "cp-rejected-other");
        commit_file(&other, "o.txt", "theirs\n", "theirs");
        git(&other, &["push", "-q"]);
        let mut f = Fixture::open(&work);
        let before = git(&work, &["rev-parse", "HEAD"]);
        commit_and_push(&mut f, "mine");
        assert!(f.failed(), "behind the remote: rejected");
        assert_eq!(f.tab.push_follow, PushFollow::Idle);
        // Only local: the commit can still be undone.
        f.run(Action::Undo);
        assert_eq!(git(&work, &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn a_retry_after_a_failed_push_runs_only_the_push() {
        if !cfg!(unix) {
            return;
        }
        let (work, bare) = repo_with_remote("cp-retry");
        // The remote refuses the first push only (as a server asking for credentials would).
        let marker = bare.join("refused-once");
        let hook = bare.join("hooks/pre-receive");
        write(
            &bare,
            "hooks/pre-receive",
            &format!(
                "#!/bin/sh\nif [ -e '{m}' ]; then exit 0; fi\ntouch '{m}'\necho 'refused once' >&2\nexit 1\n",
                m = marker.display()
            ),
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755));
        }
        let mut f = Fixture::open(&work);
        commit_and_push(&mut f, "once");
        assert!(f.failed(), "the first push is refused");
        assert_eq!(
            f.tab.last_op.as_ref().map(|o| o.label.as_str()),
            Some(PUSH_AFTER_COMMIT)
        );
        assert_eq!(
            f.tab.last_op.as_ref().map(|o| o.ops.clone()),
            Some(vec![vec!["push".to_owned()]])
        );

        // What the sign-in dialog does: run the last operation again.
        f.run(Action::SignIn(git::Credentials {
            username: "u".into(),
            password: "p".into(),
        }));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&work, &["log", "--format=%s"]),
            "once\ninitial",
            "committed exactly once"
        );
        assert_eq!(
            git(&bare, &["rev-parse", "main"]),
            git(&work, &["rev-parse", "HEAD"])
        );
    }

    #[test]
    fn a_push_waiting_for_a_sign_in_keeps_its_place() {
        let dir = repo("cp-auth");
        let mut f = Fixture::open(&dir);
        f.tab.push_follow = PushFollow::Pushing;
        let auth = Err(
            "fatal: could not read Username for 'https://x': terminal prompts disabled".to_owned(),
        );
        f.tab.after_op(PUSH_AFTER_COMMIT, &auth, &f.ctx);
        assert_eq!(f.tab.push_follow, PushFollow::Pushing);
        f.tab.after_op("Fetch", &Ok(String::new()), &f.ctx);
        assert_eq!(
            f.tab.push_follow,
            PushFollow::Pushing,
            "other operations don't disturb it"
        );
        f.tab
            .after_op(PUSH_AFTER_COMMIT, &Err("rejected".to_owned()), &f.ctx);
        assert_eq!(f.tab.push_follow, PushFollow::Idle);
    }
}
