//! Maintenance: garbage collection, repack, verify, expire the reflog, and a dry-run-first
//! clean of untracked files.

use egui::{RichText, Ui};

use super::dialogs::{Outcome, title};
use super::ext::{DialogBody, dialog};
use super::{Action, Dialog, Refresh, RepoTab, ops};
use crate::git;
use crate::theme::Tokens;

struct Task {
    label: &'static str,
    help: &'static str,
    /// What to run, or `None` for tasks that need one step per remote.
    args: &'static [&'static str],
    /// Asked about first: it can't be undone.
    danger: Option<&'static str>,
    /// Run once with HEAD and every ref but the stash appended (see [`per_ref_steps`]).
    per_ref: bool,
}

const TASKS: &[Task] = &[
    Task {
        label: "Garbage collect",
        help: "Packs loose objects and drops unreachable ones older than two weeks.",
        args: &["gc"],
        danger: None,
        per_ref: false,
    },
    Task {
        label: "Repack",
        help: "Packs every object into one file (git repack -a -d).",
        args: &["repack", "-a", "-d"],
        danger: None,
        per_ref: false,
    },
    Task {
        label: "Update the commit graph",
        help: "Speeds up history views in big repositories.",
        args: &["commit-graph", "write", "--reachable"],
        danger: None,
        per_ref: false,
    },
    Task {
        label: "Verify the repository",
        help: "Checks every object for corruption (git fsck).",
        args: &["fsck"],
        danger: None,
        per_ref: false,
    },
    Task {
        label: "Aggressive garbage collect",
        help: "Recompresses everything and removes unreachable objects now. Slow.",
        args: &["gc", "--aggressive", "--prune=now"],
        danger: Some(
            "This removes every unreachable object at once: commits only the reflog still knows are lost for good.",
        ),
        per_ref: false,
    },
    Task {
        label: "Expire the reflog",
        help: "Forgets where HEAD and the branches have been. Stashes are kept.",
        // Not `--all`: stashes *are* the reflog of refs/stash, and would all be deleted.
        args: &[
            "reflog",
            "expire",
            "--expire=now",
            "--expire-unreachable=now",
        ],
        danger: Some(
            "The reflog is how lost commits are found again. Expiring it can't be undone. Your stashes are kept.",
        ),
        per_ref: true,
    },
];

/// What a task does when clicked: run it, or ask first when it can't be undone.
/// Refs per command, to stay far below the command line's length limit.
const REFS_PER_STEP: usize = 200;

/// The refs that have a reflog (`reflog expire` fails on one that hasn't, like most tags).
fn refs_with_reflog(repo: &std::path::Path) -> Vec<String> {
    // git 2.44+ lists them.
    if let Ok(out) = git::run(Some(repo), &["reflog", "list"]) {
        return out.lines().map(str::to_owned).collect();
    }
    reflog_files(repo)
}

/// Before git 2.44: the files under logs/ (HEAD in the worktree's folder, the rest shared).
fn reflog_files(repo: &std::path::Path) -> Vec<String> {
    let path = |p: &str| {
        git::run(Some(repo), &["rev-parse", "--git-path", p])
            .map(|out| repo.join(out.trim()))
            .ok()
    };
    let mut refs = Vec::new();
    if path("logs/HEAD").is_some_and(|p| p.is_file()) {
        refs.push("HEAD".to_owned());
    }
    if let Some(logs) = path("logs/refs") {
        let mut stack = vec![logs.clone()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(rel) = p.strip_prefix(&logs) {
                    refs.push(format!("refs/{}", rel.to_string_lossy().replace('\\', "/")));
                }
            }
        }
    }
    refs
}

/// `args` followed by every ref with a reflog except `refs/stash` (a stash *is* that reflog),
/// a few hundred refs per command.
fn per_ref_steps(repo: &std::path::Path, args: &[&str]) -> Vec<Vec<String>> {
    let mut refs: Vec<String> = refs_with_reflog(repo)
        .into_iter()
        .filter(|r| !r.is_empty() && r != "refs/stash")
        .collect();
    refs.sort();
    refs.dedup();
    refs.chunks(REFS_PER_STEP)
        .map(|chunk| {
            let mut step: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            step.extend(chunk.iter().cloned());
            step
        })
        .collect()
}

fn task_action(task: &Task, repo: &std::path::Path) -> Action {
    let steps = if task.per_ref {
        per_ref_steps(repo, task.args)
    } else {
        ops(&[task.args])
    };
    match task.danger {
        None => Action::Ops {
            label: task.label.to_owned(),
            ops: steps,
            refresh: Refresh::All,
        },
        Some(warning) => Action::Dialog(Dialog::confirm(
            task.label,
            warning.to_owned(),
            task.label,
            steps,
            None,
        )),
    }
}

/// One `remote prune` per remote.
fn prune_remotes_action(remotes: &[String]) -> Action {
    Action::Ops {
        label: "Prune remote branches".to_owned(),
        ops: remotes
            .iter()
            .map(|r| vec!["remote".to_owned(), "prune".to_owned(), r.clone()])
            .collect(),
        refresh: Refresh::All,
    }
}

struct MaintenanceDialog {
    remotes: Vec<String>,
    repo: std::path::PathBuf,
}

impl RepoTab {
    pub(super) fn maintenance_dialog(&self) -> Dialog {
        dialog(MaintenanceDialog {
            remotes: self.remotes.iter().map(|r| r.name.clone()).collect(),
            repo: self.path.clone(),
        })
    }
}

impl DialogBody for MaintenanceDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, _enter: bool) -> Outcome {
        title(
            ui,
            "Maintenance",
            "Housekeeping for this repository. None of it changes your files or branches.",
            t,
        );
        let mut outcome = Outcome::Open;
        let task_row = |ui: &mut Ui, label: &str, help: &str| -> bool {
            let clicked = ui
                .horizontal(|ui| {
                    let b = ui
                        .add_sized([210.0, 26.0], egui::Button::new(label))
                        .clicked();
                    ui.label(RichText::new(help).color(t.text_faint).small());
                    b
                })
                .inner;
            ui.add_space(2.0);
            clicked
        };
        for task in TASKS {
            if task_row(ui, task.label, task.help) {
                outcome = Outcome::Done(task_action(task, &self.repo));
            }
        }
        if !self.remotes.is_empty()
            && task_row(
                ui,
                "Prune stale remote branches",
                "Forgets remote-tracking branches deleted on the server.",
            )
        {
            outcome = Outcome::Done(prune_remotes_action(&self.remotes));
        }
        ui.add_space(6.0);
        if ui.button("Close").clicked() {
            outcome = Outcome::Cancel;
        }
        outcome
    }
}

// ---- clean ------------------------------------------------------------------------------------

struct CleanDialog {
    dirs: bool,
    /// 0: untracked only, 1: untracked and ignored, 2: only ignored.
    ignored: usize,
    items: Vec<String>,
    error: Option<String>,
    repo: std::path::PathBuf,
}

/// `clean -n` lines: `Would remove path`, the path C-quoted when it holds quotes, backslashes or
/// control characters (`"a \"b\".txt"`).
fn parse_would_remove(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.strip_prefix("Would remove "))
        .map(unquote)
        .collect()
}

/// Undoes git's C-style quoting of a path; a path git didn't quote comes back as it is.
fn unquote(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_owned();
    };
    let mut bytes: Vec<u8> = Vec::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            bytes.extend(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => bytes.push(b'\n'),
            Some('t') => bytes.push(b'\t'),
            Some('r') => bytes.push(b'\r'),
            Some('a') => bytes.push(0x07),
            Some('b') => bytes.push(0x08),
            Some('f') => bytes.push(0x0c),
            Some('v') => bytes.push(0x0b),
            Some(d @ '0'..='7') => {
                // Three octal digits: one byte of a UTF-8 sequence.
                let mut value = d.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    if let Some(n) = chars.peek().and_then(|c| c.to_digit(8)) {
                        value = value * 8 + n;
                        chars.next();
                    }
                }
                bytes.push(u8::try_from(value).unwrap_or(b'?'));
            }
            Some(other) => {
                let mut buf = [0u8; 4];
                bytes.extend(other.encode_utf8(&mut buf).as_bytes());
            }
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Paths per `clean` command, to stay below the command line's length limit.
const PATHS_PER_STEP: usize = 200;

impl CleanDialog {
    fn flags(&self) -> Vec<&'static str> {
        let mut flags = Vec::new();
        if self.dirs {
            flags.push("-d");
        }
        match self.ignored {
            1 => flags.push("-x"),
            2 => flags.push("-X"),
            _ => {}
        }
        flags
    }

    /// Deletes exactly what the preview listed: each item as a literal pathspec, so a file that
    /// appeared since the preview, or one whose name looks like a pattern, is never touched.
    fn action(&self) -> Action {
        let steps = self
            .items
            .chunks(PATHS_PER_STEP)
            .map(|chunk| {
                let mut args = vec!["clean".to_owned(), "-f".to_owned()];
                args.extend(self.flags().into_iter().map(str::to_owned));
                args.push("--".to_owned());
                args.extend(chunk.iter().map(|p| super::ext::literal(p)));
                args
            })
            .collect();
        Action::Ops {
            label: "Clean".to_owned(),
            ops: steps,
            refresh: Refresh::All,
        }
    }

    fn refresh(&mut self) {
        // Unquoted non-ASCII names; the few quotes left are undone by `unquote`.
        let mut args = vec!["-c", "core.quotePath=false", "clean", "-n"];
        args.extend(self.flags());
        match git::run(Some(&self.repo), &args) {
            Ok(out) => {
                self.items = parse_would_remove(&out);
                self.error = None;
            }
            Err(e) => {
                self.items.clear();
                self.error = Some(e);
            }
        }
    }
}

impl RepoTab {
    pub(super) fn clean_dialog(&self) -> Dialog {
        let mut d = CleanDialog {
            dirs: true,
            ignored: 0,
            items: Vec::new(),
            error: None,
            repo: self.path.clone(),
        };
        d.refresh();
        dialog(d)
    }
}

impl DialogBody for CleanDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, _enter: bool) -> Outcome {
        title(
            ui,
            "Clean untracked files",
            "Deletes files git doesn't track. Deleted files can't be brought back.",
            t,
        );
        let mut changed = ui
            .checkbox(&mut self.dirs, "Include untracked folders")
            .changed();
        ui.horizontal(|ui| {
            for (i, label) in [
                "Not ignored files",
                "Ignored files too",
                "Only ignored files",
            ]
            .iter()
            .enumerate()
            {
                if ui.radio_value(&mut self.ignored, i, *label).changed() {
                    changed = true;
                }
            }
        });
        if changed {
            self.refresh();
        }
        ui.add_space(4.0);
        if let Some(e) = &self.error {
            ui.colored_label(t.danger, e);
        }
        ui.label(
            RichText::new(format!("{} item(s) would be deleted", self.items.len()))
                .color(t.text_dim),
        );
        egui::ScrollArea::vertical()
            .max_height(200.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for item in &self.items {
                    ui.label(RichText::new(item).font(crate::theme::mono(11.5)));
                }
            });
        ui.add_space(8.0);
        let mut outcome = Outcome::Open;
        ui.horizontal(|ui| {
            let can = !self.items.is_empty();
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(
                        RichText::new(format!("Delete {} item(s)", self.items.len()))
                            .color(t.danger),
                    ),
                )
                .clicked()
            {
                outcome = Outcome::Done(self.action());
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
    fn clean_preview_lines_are_read() {
        let out = "Would remove a.txt\nWould remove build/\nWould skip repository sub/\n";
        assert_eq!(parse_would_remove(out), ["a.txt", "build/"]);
    }

    /// Against real git: the preview lists what `clean -f` then removes.
    #[test]
    fn clean_removes_what_the_preview_listed() {
        let dir = std::env::temp_dir().join(format!("gitcrafted-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("build")).ok();
        let run = |args: &[&str]| git::run(Some(&dir), args);
        assert!(run(&["init", "-q"]).is_ok());
        std::fs::write(dir.join("junk.txt"), "x").ok();
        std::fs::write(dir.join("build/out.o"), "x").ok();
        std::fs::write(dir.join(".gitignore"), "*.log\n").ok();
        std::fs::write(dir.join("a.log"), "x").ok();
        let mut d = CleanDialog {
            dirs: true,
            ignored: 0,
            items: Vec::new(),
            error: None,
            repo: dir.clone(),
        };
        d.refresh();
        assert!(d.items.contains(&"junk.txt".to_owned()));
        assert!(d.items.contains(&"build/".to_owned()));
        assert!(!d.items.contains(&"a.log".to_owned()));
        d.ignored = 2;
        d.refresh();
        assert_eq!(d.items, ["a.log"]);
        let mut args = vec!["clean", "-f"];
        args.extend(d.flags());
        assert!(run(&args).is_ok());
        assert!(!dir.join("a.log").exists());
        assert!(dir.join("junk.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::test_support::*;

    /// A repository with some history and reflog entries (a reset and a branch).
    fn lived_in(name: &str) -> std::path::PathBuf {
        let dir = repo(name);
        for i in 1..=4 {
            write(&dir, &format!("f{i}.txt"), &format!("file {i}\n"));
            git(&dir, &["add", "."]);
            git(&dir, &["commit", "-q", "-m", &format!("commit {i}")]);
        }
        git(&dir, &["branch", "side", "HEAD~2"]);
        git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
        dir
    }

    /// The commands a task's click ends up running: its own, or those behind its confirmation.
    fn commands(action: Action) -> Vec<Vec<String>> {
        match action {
            Action::Ops { ops, .. } => ops,
            Action::Dialog(Dialog::Confirm { ops, .. }) => ops,
            _ => unreachable!("a task runs commands or asks first"),
        }
    }

    #[test]
    fn only_the_irreversible_tasks_ask_first() {
        let asking: Vec<&str> = TASKS
            .iter()
            .filter(|t| {
                matches!(
                    task_action(t, &std::env::temp_dir()),
                    Action::Dialog(Dialog::Confirm { .. })
                )
            })
            .map(|t| t.label)
            .collect();
        assert_eq!(asking, ["Aggressive garbage collect", "Expire the reflog"]);
        for t in TASKS.iter().filter(|t| t.danger.is_some()) {
            let Action::Dialog(Dialog::Confirm { body, button, .. }) =
                task_action(t, &std::env::temp_dir())
            else {
                unreachable!("a confirmation")
            };
            assert!(!body.is_empty() && button == t.label);
        }
        for t in TASKS {
            assert!(!t.args.is_empty() && !t.label.is_empty() && !t.help.is_empty());
        }
    }

    #[test]
    fn every_task_runs_and_leaves_a_healthy_repository() {
        let dir = lived_in("mt-all");
        let mut f = Fixture::open(&dir);
        let log_before = git(&dir, &["log", "--all", "--format=%s"]);
        for task in TASKS {
            f.run(Action::Ops {
                label: task.label.to_owned(),
                ops: commands(task_action(task, &dir)),
                refresh: Refresh::All,
            });
            assert!(!f.failed(), "{}: {:?}", task.label, f.status());
        }
        assert!(git_ok(&dir, &["fsck", "--no-progress"]));
        assert_eq!(
            git(&dir, &["log", "--all", "--format=%s"]),
            log_before,
            "no reachable history lost"
        );
        // Expiring the reflog really emptied it.
        assert!(git(&dir, &["reflog", "show", "HEAD"]).is_empty());
        assert_eq!(
            f.tab.history.len(),
            4,
            "the graph still shows every branch's commits"
        );
    }

    #[test]
    fn aggressive_gc_drops_what_only_the_reflog_knew() {
        let dir = lived_in("mt-aggressive");
        let lost = git(&dir, &["rev-parse", "HEAD@{1}"]);
        let mut f = Fixture::open(&dir);
        assert!(git_ok(&dir, &["cat-file", "-e", &lost]));
        let task = TASKS
            .iter()
            .find(|t| t.label == "Aggressive garbage collect");
        assert!(task.is_some());
        let ops = commands(task_action(task.unwrap_or(&TASKS[0]), &dir));
        // Expire first, as the user would have: then nothing keeps the commit alive.
        f.run(Action::Ops {
            label: "Expire".to_owned(),
            ops: vec![vec![
                "reflog".into(),
                "expire".into(),
                "--expire=now".into(),
                "--all".into(),
            ]],
            refresh: Refresh::All,
        });
        f.run(Action::Ops {
            label: "gc".to_owned(),
            ops,
            refresh: Refresh::All,
        });
        assert!(!f.failed(), "{:?}", f.status());
        assert!(
            !git_ok(&dir, &["cat-file", "-e", &lost]),
            "unreachable commit pruned"
        );
    }

    #[test]
    fn pruning_remote_branches_forgets_the_ones_deleted_on_the_server() {
        let (work, bare) = repo_with_remote("mt-prune");
        git(&work, &["push", "-q", "origin", "HEAD:refs/heads/gone"]);
        git(&work, &["push", "-q", "origin", "HEAD:refs/heads/stays"]);
        git(&work, &["fetch", "-q", "--no-prune"]);
        git(&bare, &["branch", "-q", "-D", "gone"]);
        // A second remote with its own stale branch: one step per remote.
        let (_w2, bare2) = repo_with_remote("mt-prune-2");
        git(
            &work,
            &["remote", "add", "second", &bare2.display().to_string()],
        );
        git(&work, &["fetch", "-q", "--no-prune", "second"]);
        git(&bare2, &["branch", "-q", "main2", "main"]);
        git(&work, &["fetch", "-q", "--no-prune", "second"]);
        git(&bare2, &["branch", "-q", "-D", "main2"]);
        let mut f = Fixture::open(&work);
        let remotes: Vec<String> = f.tab.remotes.iter().map(|r| r.name.clone()).collect();
        assert_eq!(remotes.len(), 2);
        let Action::Ops { ops, .. } = prune_remotes_action(&remotes) else {
            unreachable!("an op")
        };
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|o| o[..2] == ["remote", "prune"]));

        f.run(prune_remotes_action(&remotes));
        assert!(!f.failed(), "{:?}", f.status());
        assert!(!git_ok(
            &work,
            &["rev-parse", "--verify", "-q", "origin/gone"]
        ));
        assert!(git_ok(
            &work,
            &["rev-parse", "--verify", "-q", "origin/stays"]
        ));
        assert!(!git_ok(
            &work,
            &["rev-parse", "--verify", "-q", "second/main2"]
        ));
    }

    fn clean_fixture(name: &str) -> std::path::PathBuf {
        let dir = repo(name);
        write(&dir, ".gitignore", "*.log\nbuild/\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "ignore rules"]);
        write(&dir, "junk.txt", "x");
        write(&dir, "tmp/inside.txt", "x");
        write(&dir, "debug.log", "x");
        write(&dir, "build/out.o", "x");
        dir
    }

    fn clean_dialog(dir: &std::path::Path) -> CleanDialog {
        let mut d = CleanDialog {
            dirs: true,
            ignored: 0,
            items: Vec::new(),
            error: None,
            repo: dir.to_path_buf(),
        };
        d.refresh();
        d
    }

    #[test]
    fn the_preview_matches_what_clean_removes_for_every_option() {
        // Not ignored: untracked files and folders.
        let dir = clean_fixture("mt-clean-a");
        let mut f = Fixture::open(&dir);
        let mut d = clean_dialog(&dir);
        assert_eq!(d.items, ["junk.txt", "tmp/"]);
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        assert!(!dir.join("junk.txt").exists() && !dir.join("tmp").exists());
        assert!(
            dir.join("debug.log").exists() && dir.join("build/out.o").exists(),
            "ignored files stay"
        );

        // Without folders, the untracked folder stays.
        let dir = clean_fixture("mt-clean-b");
        let mut f = Fixture::open(&dir);
        d = clean_dialog(&dir);
        d.dirs = false;
        d.refresh();
        assert_eq!(d.items, ["junk.txt"]);
        f.run(d.action());
        assert!(dir.join("tmp/inside.txt").exists() && !dir.join("junk.txt").exists());

        // Ignored too.
        let dir = clean_fixture("mt-clean-c");
        let mut f = Fixture::open(&dir);
        d = clean_dialog(&dir);
        d.ignored = 1;
        d.refresh();
        assert_eq!(d.items, ["build/", "debug.log", "junk.txt", "tmp/"]);
        f.run(d.action());
        assert!(git(&dir, &["status", "--porcelain", "--ignored"]).is_empty());

        // Only ignored: the untracked, not ignored files stay.
        let dir = clean_fixture("mt-clean-d");
        let mut f = Fixture::open(&dir);
        d = clean_dialog(&dir);
        d.ignored = 2;
        d.refresh();
        assert_eq!(d.items, ["build/", "debug.log"]);
        f.run(d.action());
        assert!(dir.join("junk.txt").exists() && !dir.join("debug.log").exists());
    }

    #[test]
    fn clean_never_touches_tracked_files_and_a_clean_tree_offers_nothing() {
        let dir = repo("mt-clean-tracked");
        write(&dir, "a.txt", "modified\n");
        let mut f = Fixture::open(&dir);
        let d = clean_dialog(&dir);
        assert!(d.items.is_empty());
        assert!(matches!(f.tab.clean_dialog(), Dialog::Ext(_)));
        f.run(d.action());
        assert_eq!(read(&dir, "a.txt"), "modified\n");
    }

    #[test]
    fn the_maintenance_dialog_opens_with_the_tabs_remotes() {
        let (work, _bare) = repo_with_remote("mt-dialog");
        let f = Fixture::open(&work);
        assert!(matches!(f.tab.maintenance_dialog(), Dialog::Ext(_)));
    }
}

/// Data-safety tests: maintenance must never cost a branch, tag, stash or file.
#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::repo::test_support::*;

    /// A repository with something to lose everywhere: two branches, a tag, three stashes (one
    /// with an untracked file), a staged file, an edit, an untracked and an ignored file.
    fn precious(name: &str) -> std::path::PathBuf {
        let dir = repo(name);
        write(&dir, ".gitignore", "*.log\n");
        commit_file(&dir, ".gitignore", "*.log\n", "ignore logs");
        commit_file(&dir, "b.txt", "b\n", "second");
        git(&dir, &["branch", "side", "HEAD~1"]);
        git(&dir, &["tag", "-a", "v1", "-m", "v1"]);
        for i in 1..=3 {
            write(&dir, "a.txt", &format!("stashed {i}\n"));
            if i == 2 {
                write(&dir, "stashed-untracked.txt", "only in a stash\n");
            }
            git(
                &dir,
                &[
                    "stash",
                    "push",
                    "-q",
                    "--include-untracked",
                    "-m",
                    &format!("stash {i}"),
                ],
            );
        }
        write(&dir, "staged.txt", "staged\n");
        git(&dir, &["add", "staged.txt"]);
        write(&dir, "a.txt", "uncommitted\n");
        write(&dir, "new.txt", "untracked\n");
        write(&dir, "keep.log", "ignored\n");
        dir
    }

    fn stash_commits(dir: &std::path::Path) -> Vec<String> {
        git(dir, &["stash", "list", "--format=%H"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn expiring_the_reflog_keeps_every_stash() {
        let dir = precious("sf-expire");
        let before = Snapshot::take(&dir);
        let task = TASKS.iter().find(|t| t.per_ref);
        assert!(task.is_some_and(|t| t.label == "Expire the reflog"));
        let mut f = Fixture::open(&dir);
        f.run(confirmed(task_action(task.unwrap_or(&TASKS[0]), &dir)));
        assert!(!f.failed(), "{:?}", f.status());
        assert_same(&before, &Snapshot::take(&dir), "expire the reflog");
        assert_eq!(stash_commits(&dir).len(), 3);
        for i in 0..3 {
            assert!(git_ok(
                &dir,
                &["stash", "show", "-p", &format!("stash@{{{i}}}")]
            ));
        }
        assert!(
            git(&dir, &["reflog", "show", "HEAD"]).is_empty(),
            "the reflog itself is gone"
        );
    }

    #[test]
    fn every_task_keeps_branches_tags_stashes_and_files() {
        let dir = precious("sf-every");
        let before = Snapshot::take(&dir);
        let stashes = stash_commits(&dir);
        let mut f = Fixture::open(&dir);
        for task in TASKS {
            f.run(confirmed(task_action(task, &dir)));
            assert!(!f.failed(), "{}: {:?}", task.label, f.status());
            assert_same(&before, &Snapshot::take(&dir), task.label);
        }
        // Even after expiring and an aggressive gc, every stash's commits (and the untracked
        // files of the one that has them) are still in the object store.
        for s in &stashes {
            assert!(git_ok(&dir, &["cat-file", "-e", s]));
        }
        assert!(git_ok(
            &dir,
            &["cat-file", "-e", "stash@{1}^3:stashed-untracked.txt"]
        ));
        assert!(git_ok(&dir, &["fsck", "--no-progress", "--no-dangling"]));
    }

    #[test]
    fn expire_then_aggressive_gc_still_keeps_stashes() {
        let dir = precious("sf-expire-gc");
        let stashes = stash_commits(&dir);
        let mut f = Fixture::open(&dir);
        for label in ["Expire the reflog", "Aggressive garbage collect"] {
            let task = TASKS.iter().find(|t| t.label == label).unwrap_or(&TASKS[0]);
            f.run(confirmed(task_action(task, &dir)));
            assert!(!f.failed(), "{label}: {:?}", f.status());
        }
        assert_eq!(stash_commits(&dir), stashes);
        // And they still apply.
        git(&dir, &["stash", "push", "-q", "--include-untracked"]);
        assert!(git_ok(&dir, &["stash", "apply", "-q", "stash@{2}"]));
    }

    #[test]
    fn expiring_hundreds_of_refs_runs_in_short_commands() {
        let dir = repo("sf-many-refs");
        let mut input = String::new();
        for i in 0..450 {
            input.push_str(&format!("create refs/heads/b{i} HEAD\n"));
        }
        let mut child = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["update-ref", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .spawn();
        if let Ok(c) = child.as_mut() {
            use std::io::Write;
            if let Some(stdin) = c.stdin.as_mut() {
                let _ = stdin.write_all(input.as_bytes());
            }
        }
        if let Ok(mut c) = child {
            c.stdin.take();
            let _ = c.wait();
        }
        write(&dir, "a.txt", "stashed\n");
        git(&dir, &["stash", "push", "-q"]);
        let task = TASKS.iter().find(|t| t.per_ref).unwrap_or(&TASKS[0]);
        let steps = per_ref_steps(&dir, task.args);
        let n = refs_with_reflog(&dir)
            .into_iter()
            .filter(|r| r != "refs/stash")
            .count();
        assert!(
            n >= 451,
            "HEAD, main and the 450 branches have reflogs: {n}"
        );
        assert_eq!(steps.len(), n.div_ceil(REFS_PER_STEP));
        assert!(
            steps
                .iter()
                .all(|s| s.len() <= task.args.len() + REFS_PER_STEP)
        );
        assert!(!steps.iter().flatten().any(|a| a == "refs/stash"));
        let mut f = Fixture::open(&dir);
        f.run(confirmed(task_action(task, &dir)));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(stash_commits(&dir).len(), 1);
        assert_eq!(
            git(
                &dir,
                &["for-each-ref", "--count=1000", "--format=x", "refs/heads"]
            )
            .lines()
            .count(),
            451
        );
    }

    fn odd_names() -> Vec<&'static str> {
        let mut names = vec![
            "*.txt",
            "[ab].txt",
            " lead space.txt",
            "-dash.txt",
            "ümlaut.txt",
            "quote\"s.txt",
            "semi;colon.txt",
        ];
        if cfg!(unix) {
            names.extend(["back\\slash.txt", "tab\tname.txt", "new\nline.txt"]);
        }
        names.sort_unstable();
        names
    }

    fn preview(dir: &std::path::Path, dirs: bool, ignored: usize) -> CleanDialog {
        let mut d = CleanDialog {
            dirs,
            ignored,
            items: Vec::new(),
            error: None,
            repo: dir.to_path_buf(),
        };
        d.refresh();
        d
    }

    #[test]
    fn clean_lists_odd_names_exactly_and_deletes_only_what_it_listed() {
        let dir = repo("sf-clean-odd");
        for name in odd_names() {
            write(&dir, name, "untracked\n");
        }
        let d = preview(&dir, true, 0);
        assert_eq!(d.error, None);
        let mut listed = d.items.clone();
        listed.sort();
        assert_eq!(listed, odd_names(), "the preview shows the real names");

        // Created after the preview, and matched by "*.txt" and "[ab].txt" read as patterns.
        write(&dir, "late.txt", "arrived after the preview\n");
        write(&dir, "b.txt", "matches [ab].txt as a pattern\n");
        let before = Snapshot::take(&dir);
        let mut f = Fixture::open(&dir);
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        let mut lost = before.lost_in(&Snapshot::take(&dir));
        lost.sort();
        assert_eq!(
            lost,
            odd_names(),
            "exactly the previewed files, nothing else"
        );
        assert!(
            dir.join("late.txt").exists()
                && dir.join("b.txt").exists()
                && dir.join("a.txt").exists()
        );
    }

    #[test]
    fn clean_skips_nested_repositories_staged_files_and_tracked_ignored_ones() {
        let dir = repo("sf-clean-nested");
        commit_file(&dir, ".gitignore", "*.log\n", "ignore logs");
        let nested = dir.join("nested");
        let _ = std::fs::create_dir_all(&nested);
        git(&nested, &["init", "-q"]);
        commit_file(&nested, "inside.txt", "a whole repository\n", "nested work");
        write(&dir, "added.txt", "staged, never committed\n");
        git(&dir, &["add", "added.txt"]);
        write(&dir, "config.log", "tracked although ignored\n");
        git(&dir, &["add", "-f", "config.log"]);
        git(&dir, &["commit", "-q", "-m", "tracked log"]);
        write(&dir, "junk.txt", "x\n");
        write(&dir, "debug.log", "x\n");

        for (ignored, expected) in [
            (0, vec!["junk.txt"]),
            (1, vec!["debug.log", "junk.txt"]),
            (2, vec!["debug.log"]),
        ] {
            let dir = dir.clone();
            let d = preview(&dir, true, ignored);
            let mut items = d.items.clone();
            items.sort();
            assert_eq!(items, expected, "ignored mode {ignored}");
        }
        let before = Snapshot::take(&dir);
        let mut f = Fixture::open(&dir);
        f.run(preview(&dir, true, 1).action());
        assert!(!f.failed(), "{:?}", f.status());
        let mut lost = before.lost_in(&Snapshot::take(&dir));
        lost.sort();
        assert_eq!(lost, ["debug.log", "junk.txt"]);
        assert_eq!(read(&dir, "nested/inside.txt"), "a whole repository\n");
        assert!(dir.join("added.txt").exists() && dir.join("config.log").exists());
    }

    #[test]
    fn cleaning_hundreds_of_files_runs_in_short_commands() {
        let dir = repo("sf-clean-many");
        for i in 0..450 {
            write(&dir, &format!("junk-{i}.tmp"), "x");
        }
        let d = preview(&dir, false, 0);
        assert_eq!(d.items.len(), 450);
        let Action::Ops { ops, .. } = d.action() else {
            unreachable!("an op")
        };
        assert_eq!(ops.len(), 3);
        let mut f = Fixture::open(&dir);
        f.run(d.action());
        assert!(!f.failed(), "{:?}", f.status());
        let left: Vec<String> = Snapshot::take(&dir).files.into_keys().collect();
        assert_eq!(left, ["a.txt"]);
    }

    #[test]
    fn the_reflog_list_of_older_gits_matches_the_new_one() {
        let dir = precious("sf-reflog-files");
        git(&dir, &["checkout", "-q", "-b", "feat/deep/name"]);
        git(&dir, &["checkout", "-q", "main"]);
        let mut new: Vec<String> = git(&dir, &["reflog", "list"])
            .lines()
            .map(str::to_owned)
            .collect();
        let mut old = reflog_files(&dir);
        new.sort();
        old.sort();
        assert_eq!(old, new);
        assert!(old.contains(&"refs/heads/feat/deep/name".to_owned()));
        assert!(
            old.contains(&"refs/stash".to_owned()),
            "listed here, left out by per_ref_steps"
        );
    }

    #[test]
    fn quoted_names_are_unquoted() {
        assert_eq!(unquote("plain name.txt"), "plain name.txt");
        assert_eq!(unquote(r#""quote\"s.txt""#), "quote\"s.txt");
        assert_eq!(unquote(r#""back\\slash""#), "back\\slash");
        assert_eq!(unquote(r#""tab\tname\nline""#), "tab\tname\nline");
        assert_eq!(unquote(r#""\303\274mlaut.txt""#), "ümlaut.txt");
        assert_eq!(unquote(r#""""#), "");
    }
}
