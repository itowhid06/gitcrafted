//! Git LFS: install, track and untrack patterns, fetch, pull, push, prune, lock and unlock.

use std::sync::OnceLock;

use egui::{RichText, Ui};

use super::dialogs::{Outcome, title};
use super::ext::{DialogBody, dialog};
use super::{Action, Dialog, Refresh, RepoTab, ops};
use crate::git;
use crate::theme::Tokens;

/// Whether `git lfs` is installed (looked at once).
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| git::run(None, &["lfs", "version"]).is_ok())
}

/// The patterns `git lfs track` lists, from its output:
///
/// ```text
/// Listing tracked patterns
///     *.psd (.gitattributes)
/// Listing excluded patterns
/// ```
fn parse_tracked(out: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut tracked = false;
    for line in out.lines() {
        if line.starts_with("Listing tracked") {
            tracked = true;
        } else if line.starts_with("Listing excluded") {
            tracked = false;
        } else if tracked && line.starts_with(char::is_whitespace) {
            let line = line.trim();
            let pattern = line.rsplit_once(" (").map_or(line, |(p, _)| p);
            if !pattern.is_empty() {
                patterns.push(pattern.to_owned());
            }
        }
    }
    // `git lfs track` lists them in no fixed order; a stable list doesn't reshuffle between opens.
    patterns.sort();
    patterns.dedup();
    patterns
}

struct LfsDialog {
    patterns: Vec<String>,
    new_pattern: String,
    lock_path: String,
    remote: String,
}

impl RepoTab {
    pub(super) fn lfs_dialog(&self) -> Dialog {
        let patterns = git::run(Some(&self.path), &["lfs", "track"])
            .map(|o| parse_tracked(&o))
            .unwrap_or_default();
        dialog(LfsDialog {
            patterns,
            new_pattern: String::new(),
            lock_path: String::new(),
            remote: self.default_remote().unwrap_or_else(|| "origin".to_owned()),
        })
    }
}

fn run(label: &str, args: &[&str]) -> Outcome {
    Outcome::Done(Action::Ops {
        label: label.to_owned(),
        ops: ops(&[args]),
        refresh: Refresh::All,
    })
}

impl DialogBody for LfsDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, _enter: bool) -> Outcome {
        title(
            ui,
            "Git LFS",
            "Large files kept outside the repository's history.",
            t,
        );
        let mut outcome = Outcome::Open;
        ui.label(RichText::new("Tracked patterns").color(t.text_dim));
        if self.patterns.is_empty() {
            ui.label(RichText::new("None yet").color(t.text_faint));
        }
        for p in &self.patterns {
            ui.horizontal(|ui| {
                ui.label(RichText::new(p).font(crate::theme::mono(12.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Untrack").clicked() {
                        outcome = run(&format!("Untrack {p}"), &["lfs", "untrack", "--", p]);
                    }
                });
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.new_pattern)
                    .hint_text("*.psd or assets/**")
                    .desired_width(ui.available_width() - 70.0)
                    .margin(egui::Margin::symmetric(8, 4)),
            );
            let can = !self.new_pattern.trim().is_empty();
            if (ui.add_enabled(can, egui::Button::new("Track")).clicked()
                || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && can))
                && can
            {
                let p = self.new_pattern.trim().to_owned();
                outcome = run(&format!("Track {p}"), &["lfs", "track", "--", &p]);
            }
        });
        ui.label(
            RichText::new("Tracking edits .gitattributes: commit it so others get the rule.")
                .color(t.text_faint)
                .small(),
        );
        ui.add_space(8.0);
        ui.label(RichText::new("Objects").color(t.text_dim));
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Fetch")
                .on_hover_text(
                    "Download the LFS files of every branch and tag (git lfs fetch --all)",
                )
                .clicked()
            {
                outcome = run("LFS fetch", &["lfs", "fetch", "--all"]);
            }
            if ui
                .button("Pull")
                .on_hover_text("Fetch, then replace pointers in the working tree (git lfs pull)")
                .clicked()
            {
                outcome = run("LFS pull", &["lfs", "pull"]);
            }
            if ui
                .button(format!("Push to {}", self.remote))
                .on_hover_text("Upload every local LFS object (git lfs push --all)")
                .clicked()
            {
                outcome = run("LFS push", &["lfs", "push", "--all", &self.remote]);
            }
            if ui
                .button("Prune")
                .on_hover_text("Delete local LFS files that are old and already pushed")
                .clicked()
            {
                outcome = run("LFS prune", &["lfs", "prune"]);
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Locks").color(t.text_dim));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.lock_path)
                    .hint_text("path/to/file.psd")
                    .desired_width(ui.available_width() - 130.0)
                    .margin(egui::Margin::symmetric(8, 4)),
            );
            let can = !self.lock_path.trim().is_empty();
            let path = self.lock_path.trim().to_owned();
            if ui.add_enabled(can, egui::Button::new("Lock")).clicked() {
                outcome = run(&format!("Lock {path}"), &["lfs", "lock", "--", &path]);
            }
            if ui.add_enabled(can, egui::Button::new("Unlock")).clicked() {
                outcome = run(&format!("Unlock {path}"), &["lfs", "unlock", "--", &path]);
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .button("Install in this repository")
                .on_hover_text("Sets up the LFS filters and hooks here (git lfs install --local)")
                .clicked()
            {
                outcome = run("LFS install", &["lfs", "install", "--local"]);
            }
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
    fn tracked_patterns_come_from_the_listing() {
        let out = "Listing tracked patterns\n    *.psd (.gitattributes)\n    assets/big files/** (sub/.gitattributes)\nListing excluded patterns\n    secret.bin (.gitattributes)\n";
        assert_eq!(parse_tracked(out), ["*.psd", "assets/big files/**"]);
        // Listed in any order, shown in one.
        let shuffled = "Listing tracked patterns\n    zeta/** (.gitattributes)\n    *.psd (.gitattributes)\n    zeta/** (sub/.gitattributes)\nListing excluded patterns\n";
        assert_eq!(parse_tracked(shuffled), ["*.psd", "zeta/**"]);
        assert!(parse_tracked("Listing tracked patterns\n").is_empty());
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::test_support::*;

    fn done(outcome: Outcome) -> Action {
        let Outcome::Done(action) = outcome else {
            unreachable!("the button acts")
        };
        action
    }

    fn tracked(dir: &std::path::Path) -> Vec<String> {
        parse_tracked(&git::run(Some(dir), &["lfs", "track"]).unwrap_or_default())
    }

    #[test]
    fn track_commit_and_untrack_through_the_dialog_actions() {
        if !available() {
            eprintln!("git-lfs is not installed: skipping");
            return;
        }
        let dir = repo("lfs-flow");
        let mut f = Fixture::open(&dir);
        f.run(done(run("LFS install", &["lfs", "install", "--local"])));
        assert!(!f.failed(), "{:?}", f.status());

        f.run(done(run("Track *.bin", &["lfs", "track", "--", "*.bin"])));
        assert!(!f.failed(), "{:?}", f.status());
        assert!(read(&dir, ".gitattributes").contains("*.bin filter=lfs"));
        assert_eq!(tracked(&dir), ["*.bin"]);

        // A tracked file is committed as a pointer, and LFS lists it.
        write(&dir, "big.bin", "pretend this is large\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "add big file"]);
        assert!(git(&dir, &["show", "HEAD:big.bin"]).starts_with("version https://git-lfs"));
        assert!(git(&dir, &["lfs", "ls-files"]).contains("big.bin"));

        // Prune works offline.
        f.run(done(run("LFS prune", &["lfs", "prune"])));
        assert!(!f.failed(), "{:?}", f.status());

        f.run(done(run(
            "Untrack *.bin",
            &["lfs", "untrack", "--", "*.bin"],
        )));
        assert!(!f.failed(), "{:?}", f.status());
        assert!(tracked(&dir).is_empty());
        assert!(!read(&dir, ".gitattributes").contains("*.bin"));
    }

    #[test]
    fn the_dialog_lists_what_is_tracked_and_locks_need_a_server() {
        if !available() {
            eprintln!("git-lfs is not installed: skipping");
            return;
        }
        let dir = repo("lfs-dialog");
        git(&dir, &["lfs", "track", "*.psd", "assets/**"]);
        let f = Fixture::open(&dir);
        assert!(matches!(f.tab.lfs_dialog(), Dialog::Ext(_)));
        assert_eq!(tracked(&dir), ["*.psd", "assets/**"]);

        // No LFS server here: lock and push fail with a message, they don't crash.
        let mut f = f;
        f.run(done(run("Lock x", &["lfs", "lock", "--", "x.psd"])));
        assert!(f.failed());
        f.run(done(run("LFS push", &["lfs", "push", "--all", "nowhere"])));
        assert!(f.failed());
    }

    #[test]
    fn the_actions_name_what_they_do() {
        let Action::Ops { label, ops, .. } =
            done(run("Track *.psd", &["lfs", "track", "--", "*.psd"]))
        else {
            unreachable!("an op")
        };
        assert_eq!(label, "Track *.psd");
        assert_eq!(ops, [["lfs", "track", "--", "*.psd"]]);
    }
}
