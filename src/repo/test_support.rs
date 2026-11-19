//! Helpers for tests that drive a real `RepoTab` against real repositories in temp folders:
//! actions go through `apply`, background answers are pumped until the tab is quiet.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::{Action, Refresh, RepoTab, ops};

/// Removes the folders earlier test runs left in the temp folder (once per run).
fn sweep_old() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let own = format!("-{}", std::process::id());
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(600));
            if name.starts_with("gitcrafted-t-") && !name.ends_with(&own) && old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    });
}

/// An empty, unique folder for one test.
pub fn scratch(name: &str) -> PathBuf {
    sweep_old();
    let dir = std::env::temp_dir().join(format!("gitcrafted-t-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .env("GIT_EDITOR", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "user.name=Tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args);
    cmd
}

/// What the repository in `dir` needs to behave the same on every machine: its own author (the
/// actions under test commit, stash, merge and rebase without the -c flags the helpers pass, and
/// CI machines have no global identity), no signing, and no line-ending conversion (Git for
/// Windows converts by default, which would change the bytes tests compare).
pub fn configure(dir: &Path) {
    for (key, value) in [
        ("user.name", "Tester"),
        ("user.email", "tester@example.com"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        let _ = command(dir, &["config", key, value]).output();
    }
}

/// Runs git in `dir`; fails the test with git's message when it fails. Returns stdout, trimmed.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = command(dir, args).output();
    assert!(out.is_ok(), "could not run git");
    let Ok(o) = out else {
        return String::new();
    };
    assert!(
        o.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    // A repository the command just made gets its configuration at once.
    match args.first() {
        Some(&"init") if !args.contains(&"--bare") => configure(dir),
        Some(&"clone") => {
            if let Some(target) = args.last() {
                let target = dir.join(target);
                if target.join(".git").exists() {
                    configure(&target);
                }
            }
        }
        _ => {}
    }
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

/// Whether the command succeeds (for things that should be refused).
pub fn git_ok(dir: &Path, args: &[&str]) -> bool {
    command(dir, args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    assert!(
        std::fs::write(&path, text).is_ok(),
        "could not write {name}"
    );
}

pub fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

/// A repository with one commit (`a.txt`).
pub fn repo(name: &str) -> PathBuf {
    let dir = scratch(name);
    git(&dir, &["init", "-q"]);
    write(&dir, "a.txt", "one\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "initial"]);
    dir
}

/// A repository whose `origin` is a bare one beside it, with `main` pushed and tracked.
/// Returns (work folder, bare folder).
pub fn repo_with_remote(name: &str) -> (PathBuf, PathBuf) {
    let base = scratch(name);
    let bare = base.join("remote.git");
    let work = base.join("work");
    let _ = std::fs::create_dir_all(&work);
    git(&base, &["init", "-q", "--bare", "remote.git"]);
    git(&work, &["init", "-q"]);
    write(&work, "a.txt", "one\n");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "initial"]);
    git(
        &work,
        &["remote", "add", "origin", &bare.display().to_string()],
    );
    git(&work, &["push", "-q", "-u", "origin", "main"]);
    (work, bare)
}

/// Another clone of `bare`, to make commits "from someone else".
pub fn clone_of(bare: &Path, name: &str) -> PathBuf {
    let dir = scratch(name);
    let _ = std::fs::remove_dir_all(&dir);
    git(
        dir.parent().unwrap_or(&dir),
        &[
            "clone",
            "-q",
            &bare.display().to_string(),
            &dir.display().to_string(),
        ],
    );
    dir
}

/// `Action::Ops` of one command, refreshing everything.
pub fn op(label: &str, args: &[&str]) -> Action {
    Action::Ops {
        label: label.to_owned(),
        ops: ops(&[args]),
        refresh: Refresh::All,
    }
}

pub struct Fixture {
    pub ctx: egui::Context,
    pub tab: RepoTab,
}

impl Fixture {
    /// Opens a tab on `dir` and waits for its first load.
    pub fn open(dir: &Path) -> Fixture {
        configure(dir);
        let ctx = egui::Context::default();
        let tab = RepoTab::new(dir.to_path_buf(), &ctx);
        let mut f = Fixture { ctx, tab };
        f.tab.reload(&f.ctx);
        f.pump();
        f
    }

    /// Handles background answers until nothing is loading or running for a moment.
    pub fn pump(&mut self) {
        let start = Instant::now();
        let mut quiet = 0;
        while quiet < 4 {
            let mut got = false;
            while let Ok(msg) = self.tab.rx.try_recv() {
                self.tab.handle(msg, &self.ctx);
                got = true;
            }
            if got || self.tab.loading || self.tab.busy_op.is_some() {
                quiet = 0;
            } else {
                quiet += 1;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "the tab never settled"
            );
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// Applies an action and waits for it and the reload after it.
    pub fn run(&mut self, action: Action) {
        self.tab.apply(action, &self.ctx);
        self.pump();
    }

    /// Re-reads the repository.
    pub fn reload(&mut self) {
        self.tab.reload(&self.ctx);
        self.pump();
    }

    /// The last status line and whether it is an error.
    pub fn status(&self) -> Option<(String, bool)> {
        self.tab.status.clone()
    }

    /// Whether the last action ended in an error.
    pub fn failed(&self) -> bool {
        self.tab.status.as_ref().is_some_and(|(_, error)| *error)
    }

    pub fn dir(&self) -> &Path {
        &self.tab.path
    }
}

/// Polls `cond` for up to ten seconds (for programs that were started and not waited for).
pub fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    cond()
}

/// Everything a user could lose in a repository: every file of the working tree (untracked and
/// ignored ones too) with its bytes, the index, every ref (stash included), every stash, HEAD.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Snapshot {
    pub files: std::collections::BTreeMap<String, Vec<u8>>,
    pub index: String,
    pub refs: String,
    pub stashes: String,
    pub head: String,
}

fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if e.file_name() == ".git" {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map(|p| {
                let p = p.to_string_lossy().into_owned();
                if cfg!(windows) {
                    p.replace('\\', "/")
                } else {
                    p
                }
            })
            .unwrap_or_default();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(root, &path, out),
            Ok(_) => {
                out.insert(rel, std::fs::read(&path).unwrap_or_default());
            }
            Err(_) => {}
        }
    }
}

impl Snapshot {
    pub fn take(dir: &Path) -> Snapshot {
        let mut files = std::collections::BTreeMap::new();
        walk(dir, dir, &mut files);
        let quiet = |args: &[&str]| {
            command(dir, args)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        };
        Snapshot {
            files,
            index: quiet(&["ls-files", "--stage"]),
            refs: quiet(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            stashes: quiet(&["stash", "list", "--format=%H %gs"]),
            head: quiet(&["rev-parse", "HEAD"]),
        }
    }

    /// Paths whose bytes changed or that are gone, compared with `later`.
    pub fn lost_in(&self, later: &Snapshot) -> Vec<String> {
        self.files
            .iter()
            .filter(|(path, bytes)| later.files.get(*path) != Some(*bytes))
            .map(|(path, _)| path.clone())
            .collect()
    }
}

/// Fails with the differences when `after` isn't `before`.
pub fn assert_same(before: &Snapshot, after: &Snapshot, what: &str) {
    assert_eq!(
        before.lost_in(after),
        Vec::<String>::new(),
        "{what}: files changed or lost"
    );
    let new: Vec<&String> = after
        .files
        .keys()
        .filter(|k| !before.files.contains_key(*k))
        .collect();
    assert!(new.is_empty(), "{what}: new files {new:?}");
    assert_eq!(before.index, after.index, "{what}: index changed");
    assert_eq!(before.refs, after.refs, "{what}: refs changed");
    assert_eq!(before.stashes, after.stashes, "{what}: stashes changed");
    assert_eq!(before.head, after.head, "{what}: HEAD moved");
}

/// [`assert_same`], except remote-tracking branches, which a fetch is allowed to move.
pub fn assert_same_locally(before: &Snapshot, after: &Snapshot, what: &str) {
    let local = |refs: &str| -> String {
        refs.lines()
            .filter(|l| !l.starts_with("refs/remotes/"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut b = before.clone();
    let mut a = after.clone();
    b.refs = local(&b.refs);
    a.refs = local(&a.refs);
    assert_same(&b, &a, what);
}

pub fn commit_file(dir: &Path, name: &str, text: &str, message: &str) -> String {
    write(dir, name, text);
    git(dir, &["add", "--", name]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// The commands behind a confirmation, as the button would run them.
pub fn confirmed(action: Action) -> Action {
    match action {
        Action::Dialog(super::Dialog::Confirm { ops, .. }) => Action::Ops {
            label: "Confirmed".to_owned(),
            ops,
            refresh: Refresh::All,
        },
        other => other,
    }
}
