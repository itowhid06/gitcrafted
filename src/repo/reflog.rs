//! The reflog: where HEAD has been, to get back to a commit that no branch points at any more.

use egui::{RichText, Ui};

use super::dialogs::{Outcome, title};
use super::ext::{DialogBody, dialog};
use super::{Action, Dialog, RepoTab, short};
use crate::git;
use crate::theme::{self, Tokens};

/// How many entries the dialog loads.
const LIMIT: usize = 500;

struct Entry {
    hash: String,
    /// `HEAD@{3}`.
    selector: String,
    message: String,
    /// How long ago HEAD moved there ("3 days ago").
    when: String,
}

struct ReflogDialog {
    entries: Vec<Entry>,
    selected: Option<usize>,
    filter: String,
    error: Option<String>,
    loading: bool,
    rx: Option<std::sync::mpsc::Receiver<Result<Vec<Entry>, String>>>,
}

/// `reflog --date=unix --format=hash FS selector FS subject`: with that date format the selector
/// is `HEAD@{<seconds>}`, when HEAD moved there. Entries are numbered in order (`HEAD@{0}` first).
fn parse(out: &str, now: i64) -> Vec<Entry> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split('\u{1f}');
            let hash = f.next()?.to_owned();
            let stamp = f.next()?;
            let message = f.next()?.to_owned();
            let time: i64 = stamp
                .rsplit_once('{')
                .and_then(|(_, t)| t.strip_suffix('}'))
                .and_then(|t| t.parse().ok())?;
            Some((hash, message, time))
        })
        .enumerate()
        .map(|(i, (hash, message, time))| Entry {
            hash,
            selector: format!("HEAD@{{{i}}}"),
            message,
            when: git::relative_time(time, now),
        })
        .collect()
}

/// The reflog of HEAD, newest first.
fn load(repo: &std::path::Path) -> Result<Vec<Entry>, String> {
    let max = format!("--max-count={LIMIT}");
    git::run(
        Some(repo),
        &[
            "reflog",
            "show",
            "--date=unix",
            "--format=%H%x1f%gd%x1f%gs",
            &max,
        ],
    )
    .map(|out| parse(&out, git::now()))
}

impl RepoTab {
    pub(super) fn reflog_dialog(&self, ctx: &egui::Context) -> Dialog {
        let (tx, rx) = std::sync::mpsc::channel();
        let repo = self.path.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(load(&repo));
            wake.request_repaint();
        });
        dialog(ReflogDialog {
            entries: Vec::new(),
            selected: None,
            filter: String::new(),
            error: None,
            loading: true,
            rx: Some(rx),
        })
    }
}

impl DialogBody for ReflogDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, _enter: bool) -> Outcome {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(res) => {
                    self.loading = false;
                    self.rx = None;
                    match res {
                        Ok(entries) => {
                            self.entries = entries;
                            self.error = None;
                        }
                        Err(e) => {
                            self.entries.clear();
                            self.error = Some(e);
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.loading = false;
                    self.rx = None;
                    if self.entries.is_empty() && self.error.is_none() {
                        self.error = Some("Reflog loader terminated unexpectedly".to_owned());
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let width = (ui.ctx().content_rect().width() - 48.0).clamp(260.0, 640.0);
        ui.set_width(width);
        title(
            ui,
            "Reflog",
            "Where HEAD has been. Pick an entry to find a commit again.",
            t,
        );
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Filter by message or hash")
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(8, 4)),
        );
        ui.add_space(6.0);
        if let Some(e) = &self.error {
            ui.colored_label(t.danger, e);
        }
        let filter = self.filter.to_lowercase();
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if self.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Loading reflog…").color(t.text_dim));
                    });
                    return;
                }
                for (i, e) in self.entries.iter().enumerate() {
                    if !filter.is_empty()
                        && !e.message.to_lowercase().contains(&filter)
                        && !e.hash.starts_with(&filter)
                    {
                        continue;
                    }
                    let selected = self.selected == Some(i);
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 24.0),
                        egui::Sense::click(),
                    );
                    if selected {
                        ui.painter().rect_filled(rect, 3.0, t.row_selected);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 3.0, t.hover);
                    }
                    let y = rect.center().y;
                    let p = ui.painter_at(rect);
                    p.text(
                        egui::pos2(rect.left() + 8.0, y),
                        egui::Align2::LEFT_CENTER,
                        short(&e.hash),
                        theme::mono(11.5),
                        t.text_dim,
                    );
                    p.text(
                        egui::pos2(rect.left() + 82.0, y),
                        egui::Align2::LEFT_CENTER,
                        &e.selector,
                        theme::mono(11.0),
                        t.text_faint,
                    );
                    let msg_left = (rect.left() + 170.0).min(rect.right());
                    let msg_right = (rect.right() - 96.0).max(msg_left);
                    p.with_clip_rect(egui::Rect::from_min_max(
                        egui::pos2(msg_left, rect.top()),
                        egui::pos2(msg_right, rect.bottom()),
                    ))
                    .text(
                        egui::pos2(msg_left, y),
                        egui::Align2::LEFT_CENTER,
                        &e.message,
                        egui::FontId::proportional(12.5),
                        t.text,
                    );
                    p.text(
                        egui::pos2(rect.right() - 8.0, y),
                        egui::Align2::RIGHT_CENTER,
                        &e.when,
                        egui::FontId::proportional(11.0),
                        t.text_faint,
                    );
                    if resp.clicked() {
                        self.selected = Some(i);
                    }
                }
                if self.entries.is_empty() && self.error.is_none() {
                    ui.label(RichText::new("The reflog is empty").color(t.text_faint));
                }
            });
        ui.add_space(8.0);
        let mut outcome = Outcome::Open;
        let picked = self.selected.and_then(|i| self.entries.get(i));
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(picked.is_some(), |ui| {
                let hash = picked.map(|e| e.hash.clone()).unwrap_or_default();
                if ui.button("Show in the graph").clicked() {
                    outcome = Outcome::Done(Action::SelectHash(hash.clone()));
                }
                if ui.button("Check out").clicked() {
                    outcome = Outcome::Done(Action::Checkout(hash.clone()));
                }
                if ui.button("New branch here…").clicked() {
                    outcome =
                        Outcome::Done(Action::Dialog(Dialog::create_branch(&hash, short(&hash))));
                }
                if ui.button("Copy hash").clicked() {
                    ui.ctx().copy_text(hash);
                }
            });
            if ui.button("Close").clicked() {
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
    fn entries_are_read_from_the_format() {
        let now = 1_700_000_000;
        let out = format!(
            "aaa\u{1f}HEAD@{{{}}}\u{1f}commit: one\nbbb\u{1f}HEAD@{{{}}}\u{1f}checkout: moving from a to b\n",
            now - 30,
            now - 3 * 86_400
        );
        let e = parse(&out, now);
        assert_eq!(e.len(), 2);
        assert_eq!(
            (e[0].selector.as_str(), e[0].when.as_str()),
            ("HEAD@{0}", "just now")
        );
        assert_eq!(e[1].selector, "HEAD@{1}");
        assert!(e[1].when.contains('3'), "{}", e[1].when);
        assert!(parse("garbage without separators", now).is_empty());
        assert!(parse("aaa\u{1f}HEAD@{not a time}\u{1f}x", now).is_empty());
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::test_support::*;

    #[test]
    fn a_lost_commit_is_in_the_reflog_and_comes_back() {
        let dir = repo("rl-lost");
        for name in ["two", "three"] {
            write(&dir, &format!("{name}.txt"), name);
            git(&dir, &["add", "."]);
            git(&dir, &["commit", "-q", "-m", name]);
        }
        let lost = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);

        let entries = load(&dir).unwrap_or_default();
        assert_eq!(entries[0].selector, "HEAD@{0}");
        assert!(
            entries[0].message.contains("reset"),
            "{}",
            entries[0].message
        );
        assert!(entries.iter().all(|e| !e.when.is_empty()));
        let found = entries.iter().find(|e| e.hash == lost);
        assert!(found.is_some_and(|e| e.message.contains("commit: three")));

        // The graph no longer shows it; checking out the entry's hash does.
        let mut f = Fixture::open(&dir);
        assert!(!f.tab.history.iter().any(|c| c.hash == lost));
        f.run(Action::Checkout(lost.clone()));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(f.tab.refs.head, lost);
        assert_eq!(f.tab.refs.current, None, "detached");
    }

    #[test]
    fn a_branch_can_be_made_from_an_entry() {
        let dir = repo("rl-branch");
        write(&dir, "b.txt", "b");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "to lose"]);
        let lost = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
        let mut f = Fixture::open(&dir);
        // "New branch here…" opens the create-branch dialog for that hash.
        f.tab.apply(
            Action::Dialog(Dialog::create_branch(&lost, short(&lost))),
            &f.ctx,
        );
        assert!(f.tab.dialog.is_some());
        f.run(op("Create branch", &["branch", "rescued", &lost]));
        assert_eq!(git(&dir, &["rev-parse", "rescued"]), lost);
    }

    #[test]
    fn the_dialog_opens_and_a_folder_without_a_repository_is_an_error() {
        let dir = repo("rl-dialog");
        let f = Fixture::open(&dir);
        assert!(matches!(f.tab.reflog_dialog(&f.ctx), Dialog::Ext(_)));
        let plain = scratch("rl-not-a-repo");
        assert!(load(&plain).is_err());
        assert_eq!(load(&dir).map(|e| e.len()).unwrap_or(0), 1);
    }

    #[test]
    fn the_reflog_notes_checkouts_and_stays_newest_first() {
        let dir = repo("rl-order");
        git(&dir, &["checkout", "-q", "-b", "side"]);
        git(&dir, &["checkout", "-q", "main"]);
        let entries = load(&dir).unwrap_or_default();
        let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();
        assert!(
            messages[0].contains("moving from side to main"),
            "{messages:?}"
        );
        assert!(
            messages[1].contains("moving from main to side"),
            "{messages:?}"
        );
        assert!(messages[2].contains("initial"));
        let selectors: Vec<&str> = entries.iter().map(|e| e.selector.as_str()).collect();
        assert_eq!(selectors, ["HEAD@{0}", "HEAD@{1}", "HEAD@{2}"]);
    }
}

/// Regression tests (review round 2): the time shown is when HEAD moved.
#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::repo::test_support::*;

    #[test]
    fn the_time_shown_is_when_head_moved_not_the_commit_date() {
        let dir = repo("rv-reflog-time");
        // A commit dated two years back, checked out now.
        write(&dir, "old.txt", "old\n");
        git(&dir, &["add", "."]);
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00")
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "core.autocrlf=false",
                "commit",
                "-q",
                "-m",
                "old",
            ])
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok);
        git(&dir, &["checkout", "-q", "--detach", "HEAD"]);
        let entries = load(&dir).unwrap_or_default();
        assert!(entries.len() >= 2);
        assert_eq!(
            entries[0].when,
            "just now",
            "{entries:?}",
            entries = entries.iter().map(|e| &e.when).collect::<Vec<_>>()
        );
        // (The commit's own entry carries 2020: GIT_COMMITTER_DATE stamps reflog entries too.)
        let old = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["reflog", "-n", "2", "--date=unix", "--format=%gd"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        assert!(
            old.lines().nth(1).is_some_and(|l| l.contains("{157")),
            "{old}"
        );
        assert_eq!(entries[0].selector, "HEAD@{0}");
    }
}
