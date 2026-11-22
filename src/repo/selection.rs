//! Several files at once in the Changes lists: Ctrl+A, Shift+click ranges, and stage / unstage /
//! discard that act on everything selected (or on every file of the list).
//!
//! The selection itself is `RepoTab::patch_sel` (Ctrl+click, also used for patches and stashes);
//! this file adds the ways to fill it and the operations that use it.

use super::dialogs::Dialog;
use super::ext::literal;
use super::{Action, Refresh, RepoTab};
use crate::git::{Area, StatusFile};

/// Paths per git command, so a huge selection never overruns the OS's command line limit.
const CHUNK: usize = 100;

/// Whether `f` is in the Staged list (the other lists are the unstaged side).
pub(super) fn is_staged(f: &StatusFile) -> bool {
    f.area == Area::Staged
}

/// `git <prefix> -- <paths>`, split in as many commands as needed.
fn per_chunk(prefix: &[&str], paths: Vec<String>) -> Vec<Vec<String>> {
    paths
        .chunks(CHUNK)
        .map(|chunk| {
            let mut args: Vec<String> = prefix.iter().map(|s| (*s).to_owned()).collect();
            args.push("--".to_owned());
            args.extend(chunk.iter().cloned());
            args
        })
        .collect()
}

fn files_noun(n: usize) -> String {
    format!("{n} file{}", if n == 1 { "" } else { "s" })
}

pub(super) fn stage_files(files: &[StatusFile]) -> Action {
    let paths = files.iter().map(|f| literal(&f.path)).collect();
    Action::Ops {
        label: if files.len() == 1 {
            "Stage".to_owned()
        } else {
            format!("Stage {}", files_noun(files.len()))
        },
        ops: per_chunk(&["add"], paths),
        refresh: Refresh::Status,
    }
}

pub(super) fn unstage_files(head: &str, files: &[StatusFile]) -> Action {
    // A rename needs both names unstaged, or it would stay half staged.
    let mut paths: Vec<String> = Vec::new();
    for f in files {
        paths.push(literal(&f.path));
        paths.extend(f.old_path.as_deref().map(literal));
    }
    // Before the first commit there is no HEAD to restore from.
    let ops = if head.is_empty() {
        per_chunk(&["rm", "--cached", "--ignore-unmatch", "-q"], paths)
    } else {
        per_chunk(&["restore", "--staged"], paths)
    };
    Action::Ops {
        label: if files.len() == 1 {
            "Unstage".to_owned()
        } else {
            format!("Unstage {}", files_noun(files.len()))
        },
        ops,
        refresh: Refresh::Status,
    }
}

/// Asks before throwing away the unstaged changes to `files` (new files are deleted).
pub(super) fn discard_files_dialog(files: &[StatusFile], all: bool) -> Dialog {
    if let [one] = files {
        return super::detail::discard_dialog(one);
    }
    let (new, tracked): (Vec<&StatusFile>, Vec<&StatusFile>) =
        files.iter().partition(|f| f.area == Area::Untracked);
    let mut steps = per_chunk(
        &["restore"],
        tracked.iter().map(|f| literal(&f.path)).collect(),
    );
    steps.extend(per_chunk(
        &["clean", "-f", "-q"],
        new.iter().map(|f| literal(&f.path)).collect(),
    ));

    let mut body = if all {
        format!(
            "Discard all unstaged changes ({})?",
            files_noun(files.len())
        )
    } else {
        format!(
            "Discard the unstaged changes to {}?",
            files_noun(files.len())
        )
    };
    if !new.is_empty() {
        body.push_str(&format!(
            " {} {} new, so {} deleted.",
            new.len(),
            if new.len() == 1 { "is" } else { "are" },
            if new.len() == 1 { "it is" } else { "they are" },
        ));
    }
    body.push_str("\n\n");
    const SHOWN: usize = 8;
    for f in files.iter().take(SHOWN) {
        body.push_str(&format!("  {}\n", f.path));
    }
    if files.len() > SHOWN {
        body.push_str(&format!("  … and {} more\n", files.len() - SHOWN));
    }
    body.push_str("\nThis can't be undone.");
    Dialog::confirm(
        if all {
            "Discard all changes"
        } else {
            "Discard changes"
        },
        body,
        &format!("Discard {}", files_noun(files.len())),
        steps,
        None,
    )
    .refresh(Refresh::Status)
}

impl RepoTab {
    /// The unstaged (with the new files) or the staged list.
    pub(super) fn side(&self, staged: bool) -> &[StatusFile] {
        if staged {
            &self.work.staged
        } else {
            &self.work.unstaged
        }
    }

    /// What a button or menu entry on `f` acts on: every selected file of its list when `f` is one
    /// of them, `f` alone otherwise.
    pub(super) fn targets_of(&self, f: &StatusFile) -> Vec<StatusFile> {
        if self.is_patch_selected(f) {
            let selected = self.selected_in(is_staged(f));
            if selected.len() > 1 {
                return selected;
            }
        }
        vec![f.clone()]
    }

    /// Ctrl+A: every file of one list, and only that list.
    pub(super) fn select_all_in(&mut self, staged: bool) {
        self.patch_sel = self.side(staged).to_vec();
        self.sel_staged = staged;
        self.sel_anchor = self.patch_sel.first().cloned();
    }

    /// Plain click: selects just this file (and remembers it as where a Shift+click starts).
    pub(super) fn select_only(&mut self, f: &StatusFile) {
        self.patch_sel.clear();
        self.sel_staged = is_staged(f);
        self.sel_anchor = Some(f.clone());
    }

    /// Ctrl+click's side of the bookkeeping.
    pub(super) fn note_toggled(&mut self, f: &StatusFile) {
        self.sel_staged = is_staged(f);
        self.sel_anchor = Some(f.clone());
    }

    /// Shift+click: everything from the last clicked file of this list to `f`.
    pub(super) fn select_range_to(&mut self, f: &StatusFile) {
        let staged = is_staged(f);
        let list = self.side(staged);
        let anchor = self
            .sel_anchor
            .as_ref()
            .filter(|a| is_staged(a) == staged)
            .and_then(|a| list.iter().position(|x| x.path == a.path && x.area == a.area));
        let Some(to) = list.iter().position(|x| x.path == f.path && x.area == f.area) else {
            return;
        };
        let from = anchor.unwrap_or(to);
        let (lo, hi) = (from.min(to), from.max(to));
        let range = list[lo..=hi].to_vec();
        let anchor = anchor.map_or_else(|| f.clone(), |_| list[from].clone());
        self.patch_sel = range;
        self.sel_staged = staged;
        self.sel_anchor = Some(anchor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::patch::PatchAction;
    use crate::repo::test_support::*;

    fn dirty(name: &str) -> (std::path::PathBuf, Fixture) {
        let dir = repo(name);
        write(&dir, "a.txt", "changed\n");
        write(&dir, "n1.txt", "new\n");
        write(&dir, "n2.txt", "new\n");
        write(&dir, "n3.txt", "new\n");
        let f = Fixture::open(&dir);
        (dir, f)
    }

    #[test]
    fn select_all_picks_one_list_and_range_follows_the_anchor() {
        let (dir, mut f) = dirty("sel-all");
        git(&dir, &["add", "n3.txt"]);
        f.reload();
        assert_eq!(f.tab.work.unstaged.len(), 3);
        assert_eq!(f.tab.work.staged.len(), 1);

        f.tab.patch_action(PatchAction::SelectAll(false), &f.ctx);
        assert_eq!(f.tab.selected_in(false).len(), 3);
        assert!(f.tab.selected_in(true).is_empty());
        f.tab.patch_action(PatchAction::SelectAll(true), &f.ctx);
        assert_eq!(f.tab.selected_in(true).len(), 1);
        assert!(f.tab.selected_in(false).is_empty(), "the other list is let go");

        let files = f.tab.work.unstaged.clone();
        f.tab.patch_action(
            PatchAction::Click { file: files[0].clone(), shift: false },
            &f.ctx,
        );
        assert!(f.tab.patch_sel.is_empty());
        f.tab.patch_action(
            PatchAction::Click { file: files[2].clone(), shift: true },
            &f.ctx,
        );
        assert_eq!(f.tab.selected_in(false).len(), 3, "from the first click to the shift+click");
        f.tab.patch_action(
            PatchAction::Click { file: files[1].clone(), shift: true },
            &f.ctx,
        );
        assert_eq!(f.tab.selected_in(false).len(), 2, "a range always starts at the anchor");
    }

    #[test]
    fn stage_unstage_and_discard_all_selected() {
        let (dir, mut f) = dirty("sel-ops");
        write(&dir, "*.md", "star\n");
        f.reload();
        let all = f.tab.work.unstaged.clone();
        assert_eq!(all.len(), 5);

        // Stage the first three, then unstage them again.
        f.run(stage_files(&all[..3]));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(f.tab.work.staged.len(), 3);
        let staged = f.tab.work.staged.clone();
        f.run(unstage_files(&f.tab.refs.head.clone(), &staged));
        assert!(!f.failed(), "{:?}", f.status());
        assert!(f.tab.work.staged.is_empty());

        // Discard everything: the edit is reverted, the new files are gone.
        let all = f.tab.work.unstaged.clone();
        f.run(confirmed(Action::Dialog(discard_files_dialog(&all, true))));
        assert!(!f.failed(), "{:?}", f.status());
        assert!(f.tab.work.unstaged.is_empty());
        assert_eq!(read(&dir, "a.txt"), "one\n");
        assert!(!dir.join("n1.txt").exists());
    }

    #[test]
    fn a_pattern_looking_name_is_staged_alone() {
        let dir = repo("sel-glob");
        write(&dir, "a.md", "a\n");
        write(&dir, "b.md", "b\n");
        let mut f = Fixture::open(&dir);
        let one: Vec<StatusFile> = f
            .tab
            .work
            .unstaged
            .iter()
            .filter(|x| x.path == "a.md")
            .cloned()
            .collect();
        f.run(stage_files(&one));
        assert_eq!(f.tab.work.staged.len(), 1);
        assert_eq!(f.tab.work.unstaged.len(), 1);
    }

    #[test]
    fn ctrl_a_in_the_ui_selects_the_unstaged_files() {
        let (_dir, mut f) = dirty("sel-ui");
        assert!(f.tab.showing_local_changes());
        let mut harness = egui_kittest::Harness::builder()
            .with_size([900.0, 600.0])
            .build_ui(|ui| {
                if ui.ctx().cumulative_frame_nr() == 0 {
                    crate::theme::install_fonts(ui.ctx());
                    ui.ctx().request_repaint();
                    return;
                }
                f.tab.ui(ui);
            });
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.run();
        drop(harness);
        assert_eq!(f.tab.selected_in(false).len(), 4, "Ctrl+A selected the list");
    }
}
