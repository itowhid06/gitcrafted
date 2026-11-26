//! The manual (F1, or the book in the title bar): what gitcrafted does and its keyboard shortcuts.

use egui::{RichText, Ui};

use crate::theme::{self, Tokens};

/// Ctrl, or Cmd on macOS, as the shortcuts are written.
fn command() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

/// What each part of the window does: (title, points).
const FEATURES: &[(&str, &[&str])] = &[
    (
        "Repositories and tabs",
        &[
            "Open, create or clone a repository from the start page; recent ones are listed there.",
            "One tab per repository. Changes made elsewhere (editor, terminal) show up by themselves.",
            "Right-click a tab to give it its own icon and highlight colour.",
            "Settings › Themes: four code styles, or a VS Code theme imported from your VS Code, which colours the whole window. The palette icon switches between Dark, Medium Gray and Light.",
            "Drag a tab along the strip to reorder the tabs.",
        ],
    ),
    (
        "Graph",
        &[
            "Every branch with its pill on its latest commit: solid for the checked-out one, a laptop when it is local, a cloud when a remote has it.",
            "Double-click a pill to check that branch out; hover +N to see every branch of the commit, and right-click any of them.",
            "Double-click a branch's pill further on than the branch (its remote, say) to move the branch there: your uncommitted changes are stashed and brought back on top. When the two have diverged, the toolbar offers to rebase, merge or reset instead.",
            "Hover a branch in the left pane for its eye: hide it from the graph, or right-click it and Solo to draw only that one. The headers count the branches shown; the toolbar shows them all again.",
            "Right-click a commit or a pill for branches, tags, merge, rebase, cherry-pick, revert, reset and more.",
            "Search messages, authors and hashes, or switch to Code to find the commits that added or removed some text.",
            "Interactive rebase: reorder, reword, squash, fix up or drop commits, then start it.",
            "Right-click a commit of the checked-out branch to edit its message, move it up or down, or drop it, without the rebase screen.",
            "Stashes show in the graph as dashed boxes above the commit they were made on; click one to see it, right-click it to apply, pop or drop it.",
        ],
    ),
    (
        "Compare",
        &[
            "Ctrl+click a second commit to list the files that differ between the two (the changes row is the working tree).",
            "On a branch's menu, “Changes x adds to main” shows what it changed since they diverged, like a pull request; “Compare main with x” shows every difference.",
        ],
    ),
    (
        "Changes and commits",
        &[
            "Stage, unstage or discard whole files from the right pane. Ctrl+A selects every file of a list, Shift+click a range, Ctrl+click single files: Stage, Unstage and Discard then act on all of them.",
            "In a file's diff, stage, unstage or discard one hunk, or click the lines you want and act on only those.",
            "A diff shows only the changes or the whole file, unified or side by side, and remembers both. Drag over its lines to select text and Ctrl+C to copy it; the strip by the scrollbar marks where the changes are, and the arrows (Alt+Up / Alt+Down) jump between them.",
            "A commit's files as a list of paths or a tree of folders: the switch above them.",
            "Double-click a line of a diff (or right-click it) to open the file in your editor at that line; the <> button opens it at the first change. The editor is set in Settings.",
            "Write the summary and description and commit; amend the last commit; pick the identity you commit as.",
            "Paste a whole message (several lines) into the summary: its first line stays there and the rest goes into the description. The merge and pull request dialogs do the same with their message and title.",
            "Right-click a text field to cut, copy, paste, select all or clear it (password fields only paste and clear). A pasted password or token drops the line break copied with it.",
            "Double-click a commit's message in the right pane, or right-click the commit and choose Edit message…, to edit it (any commit of the checked-out branch with no merges after it).",
            "Commit & Push, next to the commit button, commits and then pushes the branch.",
            "Undo and redo commits, merges, rebases, resets, checkouts, branches, tags and stashes.",
        ],
    ),
    (
        "Fetch, pull, push, merge and stash",
        &[
            "Each opens a small dialog with its options instead of acting at once.",
            "Fetch: one remote or all, prune deleted branches, skip tags, force. Pull: remote and branch, then fast-forward only, merge or rebase, with an optional auto-stash.",
            "Push: branch, remote and remote branch, set upstream, push tags, check submodules, and force push. Force uses a lease: git refuses when the remote has commits you haven't looked at.",
            "Merge: fast-forward when possible, always a merge commit, or fast-forward only; squash; don't commit yet; a message.",
            "Stash: a message, untracked files, keep the staged changes, or only the files you Ctrl+clicked in the Changes lists.",
            "A checkout that local changes block offers to stash them, switch and bring them back.",
        ],
    ),
    (
        "Submodules",
        &[
            "Listed under the worktrees with their branch or commit, how far ahead or behind they are, and a dot when they moved or have changes.",
            "Double-click one to open it in its own tab. Right-click to initialize, update (to the recorded commit or the latest remote one), fetch or deinitialize it.",
            "Before an update would leave commits that are on no branch behind, you are asked first. The + adds a submodule.",
        ],
    ),
    (
        "Patches",
        &[
            "Ctrl+click files in the Changes lists to pick several, then right-click to save the picked (or all) staged or unstaged changes as a patch file.",
            "A stash's menu and a commit's menu save them as patches too.",
            "The icon in the Changes header applies a patch file: whitespace handling, stage it, three-way merge, or in reverse.",
        ],
    ),
    (
        "Tools menu",
        &[
            "The wrench in the toolbar: Reflog to find a lost commit again, Bisect, Git LFS, Maintenance and Clean untracked files.",
            "Bisect shows a bar under the toolbar with Good, Bad, Skip and Stop; start it there or from a commit's menu.",
            "Open the repository in an editor or IDE found on your PATH, and choose the diff and merge tools used by Open in external diff tool.",
            "Issue links turns #123 (and ABC-123 or your own prefix) in a commit message into links to your tracker.",
        ],
    ),
    (
        "Branches, remotes and more",
        &[
            "Left pane: local branches with ahead / behind counts, remotes, worktrees, stashes, pull requests and tags.",
            "Conflicts open in a three-pane editor with Ours, Theirs and Both for each conflict.",
            "Right-click a file for its history and blame.",
            "A terminal panel with tabs, in the repository's folder.",
            "When ssh needs a password, a key's passphrase or to trust a new host, gitcrafted asks in a dialog (macOS and Linux).",
        ],
    ),
];

/// Keyboard and mouse shortcuts: (keys, what they do). `Cmd` stands for Ctrl outside macOS.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Window",
        &[
            ("Cmd+T", "New tab"),
            ("Cmd+W", "Close the tab (or middle-click it)"),
            ("Cmd+Shift+T", "Reopen the tab closed last"),
            ("Cmd+P", "Command palette: every action and branch"),
            ("F5", "Reload the repository"),
            ("F1", "This manual"),
        ],
    ),
    (
        "Graph",
        &[
            ("↑ / ↓", "Previous / next commit"),
            ("Cmd+F", "Search commits"),
            ("Enter / Shift+Enter", "Next / previous search result"),
            ("Cmd+click", "Compare with the selected commit"),
            (
                "Double-click a pill",
                "Check out that branch or tag, or move it there",
            ),
            ("Esc", "Close the diff, comparison or search"),
        ],
    ),
    (
        "Changes",
        &[
            ("Cmd+Enter", "Commit (from the message fields)"),
            ("Alt+↑ / Alt+↓", "Previous / next change in a diff"),
            (
                "Click / Shift+click",
                "Pick lines in a diff, or a range of them",
            ),
            ("Cmd+click a file", "Pick it for a patch or a partial stash"),
            ("Cmd+Z", "Undo"),
            ("Cmd+Y / Cmd+Shift+Z", "Redo"),
        ],
    ),
    (
        "Terminal",
        &[
            (
                "Cmd+C",
                "Copy the selection (without one, it goes to the shell)",
            ),
            ("Cmd+Shift+C", "Copy the selection"),
        ],
    ),
];

/// Draws the manual's contents.
pub fn ui(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let heading = |ui: &mut Ui, text: &str| {
        ui.add_space(10.0);
        ui.label(
            RichText::new(text.to_uppercase())
                .font(theme::semibold(10.5))
                .color(t.text_faint),
        );
        ui.add_space(2.0);
    };

    for (title, points) in FEATURES {
        heading(ui, title);
        for point in *points {
            // The bullet on the left, the text wrapping beside it.
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("•").color(t.text_faint));
                ui.add(
                    egui::Label::new(
                        RichText::new(point.replace("Ctrl", command())).color(t.text_dim),
                    )
                    .wrap(),
                );
            });
        }
    }

    ui.add_space(8.0);
    ui.separator();
    for (title, keys) in SHORTCUTS {
        heading(ui, &format!("{title} shortcuts"));
        egui::Grid::new(("manual_keys", *title))
            .num_columns(2)
            .spacing([16.0, 6.0])
            .min_col_width(170.0)
            .show(ui, |ui| {
                for (combo, what) in *keys {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for (n, part) in combo.split(" / ").enumerate() {
                            if n > 0 {
                                ui.label(RichText::new("/").color(t.text_faint));
                            }
                            key_chip(ui, &part.replace("Cmd", command()));
                        }
                    });
                    ui.label(RichText::new(*what).color(t.text_dim));
                    ui.end_row();
                }
            });
    }
}

/// A key combination drawn like a keycap.
fn key_chip(ui: &mut Ui, keys: &str) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::new()
        .fill(t.field)
        .stroke(egui::Stroke::new(1.0, t.field_border))
        .corner_radius(t.radius_sm)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(keys).font(theme::mono(11.5)).color(t.text));
        });
}
