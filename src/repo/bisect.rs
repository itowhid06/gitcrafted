//! Bisect: find the commit that broke something by marking commits good or bad. While a bisect
//! runs, a bar under the toolbar shows the commit to test and the buttons to mark it.

use egui::{RichText, Ui};

use super::dialogs::{Outcome, buttons, title};
use super::ext::{DialogBody, dialog};
use super::sync::row;
use super::{Action, Dialog, Refresh, RepoTab, ops, short};
use crate::git;
use crate::theme::Tokens;

/// What the bar shows, read again when HEAD moves.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct Info {
    head: String,
    /// When the bisect log last changed: marking can leave HEAD where it was.
    stamp: Option<std::time::SystemTime>,
    good: usize,
    bad: usize,
    /// Steps left, once there is a good and a bad commit.
    steps: Option<u32>,
    /// `[hash] subject` once the culprit is known.
    first_bad: Option<String>,
}

/// `git bisect log` lines → (good marks, bad marks, the culprit if found).
fn parse_log(log: &str) -> (usize, usize, Option<String>) {
    let (mut good, mut bad, mut found) = (0, 0, None);
    for line in log.lines() {
        // The `# good:` / `# bad:` comments are written for every mark, whether it came from
        // `bisect start <bad> <good>` (which logs no `git bisect good` line) or from `bisect good`.
        if line.starts_with("# good:") {
            good += 1;
        } else if line.starts_with("# bad:") {
            bad += 1;
        } else if let Some(rest) = line
            .strip_prefix("# first 'bad' commit:")
            .or_else(|| line.strip_prefix("# first bad commit:"))
        {
            found = Some(rest.trim().to_owned());
        }
    }
    (good, bad, found)
}

/// `rev-list --bisect-vars` prints `bisect_steps=N` among other `key=value` lines.
fn parse_steps(vars: &str) -> Option<u32> {
    vars.lines()
        .find_map(|l| l.strip_prefix("bisect_steps="))
        .and_then(|n| n.trim().parse().ok())
}

/// The bar's data, read from the bisect log and `rev-list --bisect-vars`.
fn read_info(repo: &std::path::Path, head: &str, stamp: Option<std::time::SystemTime>) -> Info {
    let log = git::run(Some(repo), &["bisect", "log"]).unwrap_or_default();
    let (good, bad, first_bad) = parse_log(&log);
    let steps = (good > 0 && bad > 0 && first_bad.is_none())
        .then(|| {
            let goods = git::run(
                Some(repo),
                &["for-each-ref", "--format=%(refname)", "refs/bisect/good-*"],
            )
            .unwrap_or_default();
            let mut args = vec!["rev-list", "--bisect-vars", "refs/bisect/bad", "--not"];
            args.extend(goods.lines());
            git::run(Some(repo), &args).ok()
        })
        .flatten()
        .and_then(|vars| parse_steps(&vars));
    Info {
        head: head.to_owned(),
        stamp,
        good,
        bad,
        steps,
        first_bad,
    }
}

pub(super) fn stop_action() -> Action {
    Action::Ops {
        label: "Stop bisect".to_owned(),
        ops: ops(&[&["bisect", "reset"]]),
        refresh: Refresh::All,
    }
}

fn mark(label: &str, verb: &str, rev: Option<&str>) -> Action {
    let mut args = vec!["bisect", verb];
    args.extend(rev);
    Action::Ops {
        label: format!("Bisect: {label}"),
        ops: ops(&[&args]),
        refresh: Refresh::All,
    }
}

impl RepoTab {
    fn bisect_log_path(&self, ctx: &egui::Context) -> Option<std::path::PathBuf> {
        let id = egui::Id::new(("gitcraft-bisect-log", &self.path));
        if let Some(p) = ctx.data(|d| d.get_temp::<Option<std::path::PathBuf>>(id)) {
            return p;
        }
        let path = git::run(Some(&self.path), &["rev-parse", "--git-path", "BISECT_LOG"])
            .ok()
            .map(|p| self.path.join(p.trim()));
        ctx.data_mut(|d| d.insert_temp(id, path.clone()));
        path
    }

    /// A bisect is running (a stat of one file).
    pub(super) fn bisecting(&self, ctx: &egui::Context) -> bool {
        self.bisect_log_path(ctx).is_some_and(|p| p.exists())
    }

    fn bisect_info(&self, ctx: &egui::Context) -> Info {
        let id = egui::Id::new(("gitcraft-bisect-info", &self.path));
        let stamp = self
            .bisect_log_path(ctx)
            .and_then(|p| p.metadata().and_then(|m| m.modified()).ok());
        if let Some(info) = ctx.data(|d| d.get_temp::<Info>(id))
            && info.head == self.refs.head
            && info.stamp == stamp
        {
            return info;
        }
        let info = read_info(&self.path, &self.refs.head, stamp);
        ctx.data_mut(|d| d.insert_temp(id, info.clone()));
        info
    }

    /// The bar under the toolbar while a bisect runs.
    pub(super) fn bisect_bar(&self, ui: &mut Ui, acts: &mut Vec<Action>) {
        if !self.bisecting(ui.ctx()) {
            return;
        }
        let t = Tokens::get(ui.ctx());
        let info = self.bisect_info(ui.ctx());
        let idle = self.busy_op.is_none();
        egui::Panel::top("bisect_bar")
            .frame(
                egui::Frame::new()
                    .fill(t.chrome.lerp_to_gamma(t.warning, 0.18))
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .stroke(egui::Stroke::new(1.0, t.separator)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Bisecting").font(crate::theme::semibold(12.5)));
                    match &info.first_bad {
                        Some(found) => {
                            ui.label(
                                RichText::new(format!("First bad commit: {found}")).color(t.danger),
                            );
                        }
                        None => {
                            let text = match (info.good, info.bad, info.steps) {
                                (_, _, Some(n)) => format!(
                                    "testing {} · about {n} step{} left",
                                    short(&self.refs.head),
                                    if n == 1 { "" } else { "s" }
                                ),
                                (0, _, _) => "mark a commit that was good".to_owned(),
                                (_, 0, _) => "mark a commit that is bad".to_owned(),
                                _ => format!("testing {}", short(&self.refs.head)),
                            };
                            ui.label(RichText::new(text).color(t.text_dim));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(idle, egui::Button::new("Stop")).clicked() {
                            acts.push(stop_action());
                        }
                        if info.first_bad.is_none() {
                            if ui.add_enabled(idle, egui::Button::new("Skip")).clicked() {
                                acts.push(mark("skip", "skip", None));
                            }
                            if ui.add_enabled(idle, egui::Button::new("Bad")).clicked() {
                                acts.push(mark("bad", "bad", None));
                            }
                            if ui.add_enabled(idle, egui::Button::new("Good")).clicked() {
                                acts.push(mark("good", "good", None));
                            }
                        }
                    });
                });
            });
    }

    /// The bisect entries of a commit's menu.
    pub(super) fn bisect_menu_items(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        hash: &str,
        idle: bool,
    ) {
        ui.separator();
        let name = short(hash);
        if self.bisecting(ui.ctx()) {
            for (label, verb) in [("good", "good"), ("bad", "bad"), ("skip", "skip")] {
                if ui
                    .add_enabled(
                        idle,
                        egui::Button::new(format!("Bisect: mark {name} as {label}")),
                    )
                    .clicked()
                {
                    acts.push(mark(&format!("{name} is {label}"), verb, Some(hash)));
                }
            }
        } else {
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new("Bisect: start, this commit is bad…"),
                )
                .clicked()
            {
                acts.push(Action::Dialog(self.bisect_dialog(Some(hash), None)));
            }
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new("Bisect: start, this commit is good…"),
                )
                .clicked()
            {
                acts.push(Action::Dialog(self.bisect_dialog(None, Some(hash))));
            }
        }
    }

    pub(super) fn bisect_dialog(&self, bad: Option<&str>, good: Option<&str>) -> Dialog {
        dialog(BisectDialog {
            bad: bad.unwrap_or("HEAD").to_owned(),
            good: good.unwrap_or_default().to_owned(),
            focus: true,
        })
    }
}

struct BisectDialog {
    bad: String,
    good: String,
    focus: bool,
}

impl BisectDialog {
    fn steps(&self) -> Vec<Vec<String>> {
        let (bad, good) = (self.bad.trim(), self.good.trim());
        if good.is_empty() {
            vec![
                vec!["bisect".to_owned(), "start".to_owned()],
                vec!["bisect".to_owned(), "bad".to_owned(), bad.to_owned()],
            ]
        } else {
            vec![vec![
                "bisect".to_owned(),
                "start".to_owned(),
                bad.to_owned(),
                good.to_owned(),
            ]]
        }
    }
}

impl DialogBody for BisectDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        title(
            ui,
            "Start bisect",
            "Git checks out commits between a good and a bad one; you mark each as good or bad until the first bad commit is found.",
            t,
        );
        row(ui, t, "Bad commit", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.bad)
                    .hint_text("HEAD, a hash or a branch")
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(8, 4)),
            );
            if self.focus {
                edit.request_focus();
                self.focus = false;
            }
        });
        row(ui, t, "Good commit", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.good)
                    .hint_text("A hash, tag or branch that worked (optional)")
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(8, 4)),
            )
        });
        ui.label(
            RichText::new(
                "Leave the good commit empty to mark it later from the bar or a commit's menu.",
            )
            .color(t.text_faint)
            .small(),
        );
        ui.add_space(8.0);
        let valid = !self.bad.trim().is_empty();
        buttons(ui, "Start", valid, enter, || Action::Ops {
            label: "Start bisect".to_owned(),
            ops: self.steps(),
            refresh: Refresh::All,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_is_counted_and_the_culprit_read() {
        let log = "git bisect start\n# status: waiting\ngit bisect bad abc\n# bad: [abc] x\n# good: [def] y\ngit bisect good def\n# good: [123] z\ngit bisect good 123\n# skip: [456] w\n# first 'bad' commit: [abc] broke it\n";
        assert_eq!(parse_log(log), (2, 1, Some("[abc] broke it".to_owned())));
        assert_eq!(
            parse_steps("bisect_rev='a'\nbisect_nr=3\nbisect_steps=2\n"),
            Some(2)
        );
        assert_eq!(parse_steps("nothing"), None);
    }

    /// Against real git: start, mark, find the culprit, stop; the bar's data follows along.
    #[test]
    fn bisect_finds_the_first_bad_commit() {
        let dir = std::env::temp_dir().join(format!("gitcraft-bisect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let run = |args: &[&str]| {
            git::run(
                Some(&dir),
                &[&["-c", "user.name=t", "-c", "user.email=t@t"][..], args].concat(),
            )
        };
        assert!(run(&["init", "-q", "-b", "main"]).is_ok());
        let mut hashes = Vec::new();
        for i in 0..8 {
            std::fs::write(
                dir.join("f.txt"),
                format!("{} {i}", if i < 5 { "ok" } else { "broken" }),
            )
            .ok();
            assert!(run(&["add", "."]).is_ok());
            assert!(run(&["commit", "-q", "-m", &format!("c{i}")]).is_ok());
            hashes.push(
                run(&["rev-parse", "HEAD"])
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            );
        }
        let d = BisectDialog {
            bad: "HEAD".into(),
            good: hashes[0].clone(),
            focus: false,
        };
        for step in d.steps() {
            let args: Vec<&str> = step.iter().map(String::as_str).collect();
            assert!(run(&args).is_ok());
        }
        let goods =
            run(&["for-each-ref", "--format=%(refname)", "refs/bisect/good-*"]).unwrap_or_default();
        let mut args = vec!["rev-list", "--bisect-vars", "refs/bisect/bad", "--not"];
        args.extend(goods.lines());
        assert!(parse_steps(&run(&args).unwrap_or_default()).is_some());
        // Test whatever git checked out, until it names the culprit.
        for _ in 0..6 {
            let broken = std::fs::read_to_string(dir.join("f.txt"))
                .unwrap_or_default()
                .starts_with("broken");
            let out = run(&["bisect", if broken { "bad" } else { "good" }]).unwrap_or_default();
            if out.contains("is the first") {
                break;
            }
        }
        let (_, _, found) = parse_log(&run(&["bisect", "log"]).unwrap_or_default());
        assert!(found.is_some_and(|f| f.contains("c5")));
        assert!(run(&["bisect", "reset"]).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::test_support::*;

    /// c0 … c7 where c5 is the first commit with a broken file. Returns (folder, hashes).
    fn history(name: &str) -> (std::path::PathBuf, Vec<String>) {
        let dir = scratch(name);
        git(&dir, &["init", "-q"]);
        let mut hashes = Vec::new();
        for i in 0..8 {
            let state = if i < 5 { "ok" } else { "broken" };
            write(&dir, "f.txt", &format!("{state} {i}\n"));
            git(&dir, &["add", "."]);
            git(&dir, &["commit", "-q", "-m", &format!("c{i}")]);
            hashes.push(git(&dir, &["rev-parse", "HEAD"]));
        }
        (dir, hashes)
    }

    fn mark_what_is_checked_out(f: &mut Fixture) {
        let broken = read(f.dir(), "f.txt").starts_with("broken");
        f.run(if broken {
            mark("bad", "bad", None)
        } else {
            mark("good", "good", None)
        });
        assert!(!f.failed(), "{:?}", f.status());
    }

    #[test]
    fn a_whole_bisect_through_the_tab() {
        let (dir, hashes) = history("bs-flow");
        let mut f = Fixture::open(&dir);
        assert!(!f.tab.bisecting(&f.ctx));

        // Start with only the bad commit: the bar asks for a good one.
        let start = BisectDialog {
            bad: "HEAD".to_owned(),
            good: String::new(),
            focus: false,
        };
        f.run(Action::Ops {
            label: "Start bisect".to_owned(),
            ops: start.steps(),
            refresh: Refresh::All,
        });
        assert!(!f.failed(), "{:?}", f.status());
        assert!(f.tab.bisecting(&f.ctx));
        let info = f.tab.bisect_info(&f.ctx);
        assert_eq!(
            (info.good, info.bad, info.steps, info.first_bad),
            (0, 1, None, None)
        );

        // Marking a commit from its menu: git checks out one to test and counts the steps.
        f.run(mark("c0 is good", "good", Some(&hashes[0])));
        assert!(!f.failed(), "{:?}", f.status());
        let info = f.tab.bisect_info(&f.ctx);
        assert_eq!((info.good, info.bad), (1, 1));
        assert!(info.steps.is_some_and(|n| n >= 1), "{info:?}");
        assert_ne!(f.tab.refs.head, hashes[7], "a middle commit is checked out");

        // Mark what is checked out until the culprit is named.
        for _ in 0..6 {
            if f.tab.bisect_info(&f.ctx).first_bad.is_some() {
                break;
            }
            mark_what_is_checked_out(&mut f);
        }
        let info = f.tab.bisect_info(&f.ctx);
        assert!(
            info.first_bad
                .as_deref()
                .is_some_and(|s| s.contains(&hashes[5]) && s.contains("c5")),
            "{info:?}"
        );
        assert_eq!(info.steps, None, "nothing left to test");

        // Stop: back on the branch, no bar.
        f.run(stop_action());
        assert!(!f.failed(), "{:?}", f.status());
        assert!(!f.tab.bisecting(&f.ctx));
        assert!(
            f.tab.refs.current.is_some(),
            "back on a branch, not detached"
        );
    }

    #[test]
    fn skipping_a_commit_moves_on_and_good_and_bad_given_with_the_start() {
        let (dir, hashes) = history("bs-skip");
        let mut f = Fixture::open(&dir);
        let start = BisectDialog {
            bad: "HEAD".to_owned(),
            good: hashes[0].clone(),
            focus: false,
        };
        f.run(Action::Ops {
            label: "Start bisect".to_owned(),
            ops: start.steps(),
            refresh: Refresh::All,
        });
        assert!(!f.failed(), "{:?}", f.status());
        let info = f.tab.bisect_info(&f.ctx);
        assert_eq!((info.good, info.bad), (1, 1));
        let first_test = f.tab.refs.head.clone();

        f.run(mark("skip", "skip", None));
        assert!(!f.failed(), "{:?}", f.status());
        assert_ne!(
            f.tab.refs.head, first_test,
            "git picks another commit to test"
        );
        assert!(f.tab.bisecting(&f.ctx));
        f.run(stop_action());
        assert!(!f.tab.bisecting(&f.ctx));
    }

    #[test]
    fn the_bar_data_follows_the_log_even_when_head_stays() {
        let (dir, hashes) = history("bs-stamp");
        let mut f = Fixture::open(&dir);
        f.run(Action::Ops {
            label: "Start bisect".to_owned(),
            ops: BisectDialog {
                bad: "HEAD".to_owned(),
                good: String::new(),
                focus: false,
            }
            .steps(),
            refresh: Refresh::All,
        });
        let before = f.tab.bisect_info(&f.ctx);
        assert_eq!(before.bad, 1);
        // Marking another bad commit doesn't move HEAD (it is already the newest bad one), but the
        // bar must still see the new log line.
        std::thread::sleep(std::time::Duration::from_millis(1100)); // the log's mtime must differ
        f.run(mark("c6 is bad", "bad", Some(&hashes[6])));
        let after = f.tab.bisect_info(&f.ctx);
        assert_eq!(after.bad, 2, "{after:?}");
        f.run(stop_action());
    }

    #[test]
    fn start_commands_and_marks() {
        let both = BisectDialog {
            bad: " abc ".to_owned(),
            good: " def ".to_owned(),
            focus: false,
        };
        assert_eq!(both.steps(), [["bisect", "start", "abc", "def"]]);
        let only_bad = BisectDialog {
            bad: "abc".to_owned(),
            good: "  ".to_owned(),
            focus: false,
        };
        assert_eq!(
            only_bad.steps(),
            [vec!["bisect", "start"], vec!["bisect", "bad", "abc"]]
        );
        let Action::Ops { label, ops, .. } = mark("abc is good", "good", Some("abc")) else {
            unreachable!("an op")
        };
        assert_eq!(label, "Bisect: abc is good");
        assert_eq!(ops, [["bisect", "good", "abc"]]);
        let Action::Ops { ops, .. } = stop_action() else {
            unreachable!("an op")
        };
        assert_eq!(ops, [["bisect", "reset"]]);
    }

    #[test]
    fn a_menu_start_offers_a_prefilled_dialog() {
        let (dir, hashes) = history("bs-menu");
        let f = Fixture::open(&dir);
        assert!(matches!(
            f.tab.bisect_dialog(Some(&hashes[7]), None),
            Dialog::Ext(_)
        ));
        assert!(matches!(
            f.tab.bisect_dialog(None, Some(&hashes[0])),
            Dialog::Ext(_)
        ));
    }
}

/// Data-safety tests: a bisect leaves uncommitted work and the branch as they were.
#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::repo::test_support::*;

    #[test]
    fn bisecting_keeps_uncommitted_work_and_returns_to_the_branch() {
        let dir = repo("sf-bisect-dirty");
        commit_file(&dir, "notes.txt", "committed notes\n", "notes");
        let mut hashes = Vec::new();
        for i in 0..6 {
            hashes.push(commit_file(
                &dir,
                "f.txt",
                &format!("{} {i}\n", if i < 3 { "ok" } else { "broken" }),
                &format!("c{i}"),
            ));
        }
        // An edit to a file no commit in the range touches, and an untracked file.
        write(&dir, "notes.txt", "my uncommitted notes\n");
        write(&dir, "u.txt", "untracked\n");
        let head = git(&dir, &["rev-parse", "HEAD"]);
        let mut f = Fixture::open(&dir);
        f.run(Action::Ops {
            label: "Start bisect".to_owned(),
            ops: BisectDialog {
                bad: "HEAD".to_owned(),
                good: hashes[0].clone(),
                focus: false,
            }
            .steps(),
            refresh: Refresh::All,
        });
        assert!(!f.failed(), "{:?}", f.status());
        for _ in 0..4 {
            if f.tab.bisect_info(&f.ctx).first_bad.is_some() {
                break;
            }
            let broken = read(&dir, "f.txt").starts_with("broken");
            f.run(mark("step", if broken { "bad" } else { "good" }, None));
            assert_eq!(read(&dir, "notes.txt"), "my uncommitted notes\n");
        }
        f.run(stop_action());
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head);
        assert!(f.tab.refs.current.is_some(), "back on the branch");
        assert_eq!(read(&dir, "notes.txt"), "my uncommitted notes\n");
        assert_eq!(read(&dir, "u.txt"), "untracked\n");
    }

    #[test]
    fn starting_over_a_conflicting_edit_is_refused_and_changes_nothing() {
        let dir = repo("sf-bisect-refused");
        let first = commit_file(&dir, "f.txt", "v1\n", "c1");
        commit_file(&dir, "f.txt", "v2\n", "c2");
        commit_file(&dir, "f.txt", "v3\n", "c3");
        write(&dir, "f.txt", "my edit of a file the range changes\n");
        let before = Snapshot::take(&dir);
        let mut f = Fixture::open(&dir);
        f.run(Action::Ops {
            label: "Start bisect".to_owned(),
            ops: BisectDialog {
                bad: "HEAD".to_owned(),
                good: first,
                focus: false,
            }
            .steps(),
            refresh: Refresh::All,
        });
        if f.tab.bisecting(&f.ctx) {
            f.run(stop_action());
        }
        let after = Snapshot::take(&dir);
        assert_eq!(before.lost_in(&after), Vec::<String>::new());
        assert_eq!(before.head, after.head);
    }
}
