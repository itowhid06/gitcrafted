//! Submodules: what the repository records, what is checked out in each, and the sidebar
//! section that lists them. Clicking one opens it in its own tab (it is a repository).
//!
//! Kept in one file of its own so the rest of the tree only carries small hooks (a field and a
//! load in `mod.rs`, one call in `sidebar.rs`, one dialog arm in `dialogs.rs`).

use std::collections::HashMap;
use std::path::Path;

use egui::{Align2, Rect, Sense, Ui, pos2, vec2};

use super::ext::literal;
use super::widgets;
use super::{Action, Dialog, Refresh, RepoTab, ops};
use crate::git;
use crate::icons;
use crate::theme::{self, Tokens};

/// The pane's rows, as the branch rows.
const ROW_H: f32 = widgets::ROW_H + 4.0;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Submodule {
    /// The name in `.gitmodules` (usually the path).
    pub name: String,
    /// Folder relative to the repository, with `/` separators.
    pub path: String,
    /// The URL in `.gitmodules`; empty when the file doesn't list it.
    pub url: String,
    /// The commit this repository records for it (the index's).
    pub recorded: String,
    /// The commit checked out in it; empty while it isn't initialized.
    pub head: String,
    /// Its checked-out branch; `None` when detached.
    pub branch: Option<String>,
    /// Commits its branch has that its upstream hasn't, and the other way round.
    pub ahead: u32,
    pub behind: u32,
    /// Cloned into the folder (the folder has a `.git`).
    pub initialized: bool,
    /// The submodule folder currently exists inside the repository.
    pub dir_exists: bool,
    /// The repository's merge left it conflicted.
    pub conflicted: bool,
    /// Detached, with commits no branch, tag or remote branch has: checking out another commit
    /// would leave them behind (only its reflog would still know them).
    pub unsaved: bool,
}

impl Submodule {
    /// A different commit is checked out than the one recorded.
    pub fn moved(&self) -> bool {
        self.initialized && !self.head.is_empty() && self.head != self.recorded
    }

    /// The branch, or the short hash when detached.
    pub fn short_head(&self) -> String {
        match &self.branch {
            Some(b) => b.clone(),
            None => self.head.get(..8).unwrap_or(&self.head).to_owned(),
        }
    }
}

/// The repository's submodules, sorted by path. Empty (not an error) when it has none.
pub fn list(repo: &Path) -> Vec<Submodule> {
    let Ok(files) = git::run(Some(repo), &["ls-files", "--stage", "-z"]) else {
        return Vec::new();
    };
    let gitlinks = parse_gitlinks(&files);
    if gitlinks.is_empty() {
        return Vec::new();
    }
    // No `.gitmodules` (yet): git exits non-zero, and the entries just lack a name and URL.
    let config = git::run(
        Some(repo),
        &[
            "config",
            "-f",
            ".gitmodules",
            "-z",
            "--get-regexp",
            r"^submodule\.",
        ],
    )
    .unwrap_or_default();
    let declared = parse_gitmodules(&config);

    let mut list: Vec<Submodule> = gitlinks
        .into_iter()
        .map(|link| {
            let (name, url) = declared
                .get(&link.path)
                .cloned()
                .unwrap_or_else(|| (link.path.clone(), String::new()));
            let dir = repo.join(&link.path);
            // Only a folder that really is inside the repository counts (no `..`, no symlink out).
            let in_repo = inside(repo, &dir);
            let dir_exists = in_repo && dir.is_dir();
            let initialized = in_repo && dir.join(".git").exists();
            let (head, branch) = if initialized {
                checked_out(&dir)
            } else {
                (String::new(), None)
            };
            let (ahead, behind) = match (initialized, &branch) {
                (true, Some(_)) => tracking(&dir),
                _ => (0, 0),
            };
            Submodule {
                name,
                path: link.path,
                url,
                recorded: link.sha,
                head,
                branch,
                ahead,
                behind,
                initialized,
                dir_exists,
                conflicted: link.conflicted,
                unsaved: false,
            }
        })
        .collect();
    for s in &mut list {
        s.unsaved = has_unsaved_commits(&repo.join(&s.path), s);
    }
    list.sort_by(|a, b| a.path.cmp(&b.path));
    list
}

/// Whether the checkout has commits on no branch, tag or remote branch (and not in the
/// recorded commit's history): a detached HEAD someone committed on.
fn has_unsaved_commits(dir: &Path, s: &Submodule) -> bool {
    if !s.initialized || s.branch.is_some() || s.head.is_empty() || s.head == s.recorded {
        return false;
    }
    let base = [
        "rev-list",
        "-n",
        "1",
        "HEAD",
        "--not",
        "--branches",
        "--tags",
        "--remotes",
    ];
    let mut with_recorded = base.to_vec();
    with_recorded.push(&s.recorded);
    // The recorded commit may not be in the submodule at all (not fetched): then without it.
    git::run(Some(dir), &with_recorded)
        .or_else(|_| git::run(Some(dir), &base))
        .is_ok_and(|out| !out.trim().is_empty())
}

/// `action`, or a confirmation first when any submodule has commits on no branch.
fn guarded_all(all: &[Submodule], action: Action) -> Action {
    let unsaved: Vec<&str> = all
        .iter()
        .filter(|s| s.unsaved)
        .map(|s| s.path.as_str())
        .collect();
    if unsaved.is_empty() {
        return action;
    }
    let Action::Ops { ops, .. } = action else {
        return action;
    };
    Action::Dialog(Dialog::confirm(
        "Commits on no branch",
        format!(
            "{} {} commits on no branch. Updating checks out other commits and leaves them behind: only the reflog would still know them. Create branches on them first, or go ahead anyway.",
            unsaved.join(", "),
            if unsaved.len() == 1 { "has" } else { "have" }
        ),
        "Go ahead anyway",
        ops,
        None,
    ))
}

/// `action`, or when `sub` has commits on no branch, a confirmation that says so first.
fn guarded(sub: &Submodule, action: Action) -> Action {
    if !sub.unsaved {
        return action;
    }
    let Action::Ops { label, ops, .. } = action else {
        return action;
    };
    Action::Dialog(Dialog::confirm(
        "Commits on no branch",
        format!(
            "{} has commits that are on no branch. {label} checks out another commit and leaves them behind: only its reflog would still know them. Open it in a tab and create a branch on them first, or go ahead anyway.",
            sub.path
        ),
        "Go ahead anyway",
        ops,
        None,
    ))
}

/// The commit and branch checked out in a submodule's folder.
fn checked_out(dir: &Path) -> (String, Option<String>) {
    // `rev-parse HEAD --abbrev-ref HEAD` prints the hash, then the branch (`HEAD` if detached).
    let Ok(out) = git::run(Some(dir), &["rev-parse", "HEAD", "--abbrev-ref", "HEAD"]) else {
        return (String::new(), None);
    };
    let mut lines = out.lines();
    let head = lines.next().unwrap_or_default().trim().to_owned();
    let branch = lines
        .next()
        .map(str::trim)
        .filter(|b| !b.is_empty() && *b != "HEAD")
        .map(str::to_owned);
    (head, branch)
}

/// Whether `dir` resolves to somewhere under `repo`.
fn inside(repo: &Path, dir: &Path) -> bool {
    match (std::fs::canonicalize(repo), std::fs::canonicalize(dir)) {
        (Ok(root), Ok(dir)) => dir.starts_with(root),
        _ => false,
    }
}

/// How far the checked-out branch is ahead of / behind its upstream; zeros without one.
fn tracking(dir: &Path) -> (u32, u32) {
    let Ok(out) = git::run(
        Some(dir),
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    ) else {
        return (0, 0);
    };
    let mut parts = out.split_whitespace().map(|n| n.parse().unwrap_or(0));
    let behind = parts.next().unwrap_or(0);
    let ahead = parts.next().unwrap_or(0);
    (ahead, behind)
}

struct Gitlink {
    path: String,
    sha: String,
    conflicted: bool,
}

/// `ls-files --stage -z` entries are `mode sha stage\tpath\0`; a submodule has mode 160000.
/// A conflicted one appears at stages 1 to 3, and counts once.
fn parse_gitlinks(out: &str) -> Vec<Gitlink> {
    let mut links: Vec<Gitlink> = Vec::new();
    for entry in out.split('\0') {
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split(' ');
        let (Some("160000"), Some(sha), Some(stage)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if let Some(existing) = links.iter_mut().find(|l| l.path == path) {
            existing.conflicted = true;
            continue;
        }
        links.push(Gitlink {
            path: path.to_owned(),
            sha: sha.to_owned(),
            conflicted: stage != "0",
        });
    }
    links
}

/// `config -z --get-regexp` entries are `key\nvalue\0`, keys `submodule.<name>.<variable>`
/// (the name may hold dots). Returns path → (name, url).
fn parse_gitmodules(out: &str) -> HashMap<String, (String, String)> {
    let mut paths: HashMap<String, String> = HashMap::new(); // name → path
    let mut urls: HashMap<String, String> = HashMap::new(); // name → url
    for entry in out.split('\0') {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        let Some(rest) = key.strip_prefix("submodule.") else {
            continue;
        };
        let Some((name, variable)) = rest.rsplit_once('.') else {
            continue;
        };
        match variable {
            "path" => {
                paths.insert(name.to_owned(), value.to_owned());
            }
            "url" => {
                urls.insert(name.to_owned(), value.to_owned());
            }
            _ => {}
        }
    }
    paths
        .into_iter()
        .map(|(name, path)| {
            let url = urls.remove(&name).unwrap_or_default();
            (path, (name, url))
        })
        .collect()
}

/// A folder name to suggest for a submodule cloned from `url`: `git@host:user/repo.git` → `repo`.
pub fn folder_from_url(url: &str) -> String {
    let url = url.trim().trim_end_matches(['/', '\\']);
    let url = url.strip_suffix(".git").unwrap_or(url);
    url.rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The commands that add a submodule.
pub fn add_ops(url: &str, path: &str) -> Vec<Vec<String>> {
    ops(&[&["submodule", "add", "--", url, path]])
}

impl Dialog {
    /// Add a submodule: its URL and the folder it goes in.
    pub fn add_submodule() -> Self {
        Dialog::AddSubmodule {
            url: String::new(),
            path: String::new(),
            path_edited: false,
            focus: true,
        }
    }
}

impl RepoTab {
    pub(super) fn submodule_section(
        &self,
        ui: &mut Ui,
        acts: &mut Vec<Action>,
        idle: bool,
        filter: &str,
    ) {
        let shown: Vec<&Submodule> = self
            .submodules
            .iter()
            .filter(|s| filter.is_empty() || s.path.to_lowercase().contains(filter))
            .collect();
        let (open, add) = widgets::section_with_button(
            ui,
            "submodules",
            "package",
            "SUBMODULES",
            shown.len(),
            true,
            Some(("plus", "Add a submodule…")),
        );
        if add && idle {
            acts.push(Action::Dialog(Dialog::add_submodule()));
        }
        if !open {
            return;
        }
        for sub in shown {
            ui.push_id(&sub.path, |ui| self.submodule_row(ui, acts, idle, sub));
        }
    }

    fn submodule_row(&self, ui: &mut Ui, acts: &mut Vec<Action>, idle: bool, sub: &Submodule) {
        let t = Tokens::get(ui.ctx());
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let painter = ui.painter_at(rect);
        if resp.hovered() {
            painter.rect_filled(rect, 0.0, t.hover);
        }
        let y = rect.center().y;
        // Changed since the repository recorded it: new commits in it, or edits inside it.
        let changed = self
            .work
            .unstaged
            .iter()
            .chain(&self.work.staged)
            .any(|f| f.path == sub.path);
        let icon_color = if !sub.initialized {
            t.text_faint
        } else {
            t.text_dim
        };
        icons::paint(
            ui,
            Rect::from_center_size(pos2(rect.left() + 34.0, y), vec2(16.0, 16.0)),
            "package",
            13.0,
            icon_color,
        );

        let mut right = rect.right() - 10.0;
        let flag = if sub.conflicted {
            Some(t.danger)
        } else if changed || sub.moved() {
            Some(t.warning)
        } else {
            None
        };
        if let Some(color) = flag {
            painter.circle_filled(pos2(right - 3.5, y), 3.5, color);
            right -= 7.0 + 8.0;
        }
        // The path comes first; the branch and the counts only get what it leaves free.
        let text_color = if sub.initialized {
            t.text
        } else {
            t.text_faint
        };
        let path_font = egui::FontId::proportional(12.5);
        let path_w = painter
            .layout_no_wrap(sub.path.clone(), path_font.clone(), text_color)
            .size()
            .x;
        let free = right - (rect.left() + 48.0 + path_w + 12.0);
        if sub.initialized {
            let small = theme::mono(11.0);
            let width = |text: &str| {
                painter
                    .layout_no_wrap(text.to_owned(), small.clone(), t.text_dim)
                    .size()
                    .x
            };
            // Each count is its arrow and number.
            let counts: Vec<(u32, &str)> = [(sub.behind, "arrow-down"), (sub.ahead, "arrow-up")]
                .into_iter()
                .filter(|(n, _)| *n > 0)
                .collect();
            let counts_w: f32 = counts
                .iter()
                .map(|(n, _)| width(&n.to_string()) + 7.0 + 12.0 + 6.0)
                .sum();
            let head = sub.short_head();
            let head_w = width(&head) + 8.0;
            let show_counts = counts_w <= free;
            let show_head = counts_w + head_w <= free && (show_counts || counts.is_empty());
            if show_head {
                let r = painter.text(
                    pos2(right, y),
                    Align2::RIGHT_CENTER,
                    head,
                    small.clone(),
                    t.text_faint,
                );
                right = r.left() - 8.0;
            }
            if show_counts {
                for (n, icon) in counts {
                    let r = painter.text(
                        pos2(right, y),
                        Align2::RIGHT_CENTER,
                        n.to_string(),
                        small.clone(),
                        t.text_dim,
                    );
                    let ir = Rect::from_center_size(pos2(r.left() - 7.0, y), vec2(12.0, 12.0));
                    icons::paint(ui, ir, icon, 11.0, t.text_dim);
                    right = ir.left() - 6.0;
                }
            }
        }
        painter
            .with_clip_rect(Rect::from_min_max(rect.min, pos2(right, rect.bottom())))
            .text(
                pos2(rect.left() + 48.0, y),
                Align2::LEFT_CENTER,
                &sub.path,
                path_font,
                text_color,
            );

        let abs = self.path.join(&sub.path);
        let mut tip = sub.path.clone();
        if !sub.url.is_empty() {
            tip.push_str(&format!("\n{}", sub.url));
        }
        if !sub.initialized {
            tip.push_str("\nNot initialized: right-click to initialize it");
        } else {
            tip.push_str(&format!(
                "\nChecked out: {}",
                sub.head.get(..8).unwrap_or(&sub.head)
            ));
            tip.push_str(&format!(
                "\nRecorded here: {}",
                sub.recorded.get(..8).unwrap_or(&sub.recorded)
            ));
            if sub.moved() {
                tip.push_str("\nIt is on a different commit than this repository records");
            }
            if changed && !sub.moved() {
                tip.push_str("\nIt has changes inside");
            }
        }
        if sub.unsaved {
            tip.push_str("\nHas commits on no branch: create a branch to keep them");
        }
        if sub.conflicted {
            tip.push_str("\nIn conflict");
        }
        if sub.initialized {
            tip.push_str("\nDouble-click to open in a tab");
        }
        let resp = resp.on_hover_text(tip);
        if sub.initialized && resp.double_clicked() {
            acts.push(Action::OpenTab(abs.clone()));
        }
        egui::Popup::context_menu(&resp)
            .style(theme::menu_style)
            .show(|ui| submodule_menu_items(ui, acts, idle, sub, &abs, &self.submodules));
    }
}

fn ops_action(label: String, args: &[&str]) -> Action {
    Action::Ops {
        label,
        ops: ops(&[args]),
        refresh: Refresh::All,
    }
}

/// Clone it and check out the recorded commit.
fn init_action(path: &str) -> Action {
    ops_action(
        format!("Initialize {path}"),
        &["submodule", "update", "--init", "--", &literal(path)],
    )
}

fn fetch_action(path: &str) -> Action {
    ops_action(format!("Fetch {path}"), &["-C", path, "fetch", "--prune"])
}

/// Check out the commit the repository records.
fn update_recorded_action(path: &str) -> Action {
    ops_action(
        format!("Update {path}"),
        &["submodule", "update", "--", &literal(path)],
    )
}

/// Check out the tip of its tracked branch.
fn update_remote_action(path: &str) -> Action {
    ops_action(
        format!("Update {path} from its remote"),
        &[
            "submodule",
            "update",
            "--init",
            "--remote",
            "--",
            &literal(path),
        ],
    )
}

fn update_all_action() -> Action {
    ops_action(
        "Update submodules".to_owned(),
        &["submodule", "update", "--init", "--recursive"],
    )
}

fn deinit_dialog(path: &str) -> Dialog {
    Dialog::confirm(
        "Deinitialize submodule",
        format!(
            "Empty the folder of {path}? It stays listed and can be initialized again; its commits stay in .git/modules."
        ),
        "Deinitialize",
        ops(&[&["submodule", "deinit", "--", &literal(path)]]),
        Some((
            "Force, even with changes inside it".to_owned(),
            ops(&[&["submodule", "deinit", "--force", "--", &literal(path)]]),
        )),
    )
}

fn submodule_menu_items(
    ui: &mut Ui,
    acts: &mut Vec<Action>,
    idle: bool,
    sub: &Submodule,
    abs: &Path,
    all: &[Submodule],
) {
    let path = sub.path.as_str();
    if ui
        .add_enabled(sub.initialized, egui::Button::new("Open in a tab"))
        .on_disabled_hover_text("Initialize it first")
        .clicked()
    {
        acts.push(Action::OpenTab(abs.to_path_buf()));
    }
    if ui
        .add_enabled(sub.initialized, egui::Button::new("Open terminal here"))
        .clicked()
    {
        acts.push(Action::NewTerminal {
            shell: None,
            cwd: Some(abs.to_path_buf()),
        });
    }
    if ui
        .add_enabled(
            sub.dir_exists && !sub.path.is_empty() && !sub.path.starts_with('-'),
            egui::Button::new(super::reveal_label()),
        )
        .clicked()
    {
        let _ = open::that_detached(abs);
    }
    ui.separator();
    if !sub.initialized {
        if ui
            .add_enabled(idle, egui::Button::new("Initialize and update"))
            .on_hover_text("Clone it and check out the commit this repository records")
            .clicked()
        {
            acts.push(init_action(path));
        }
    } else {
        if ui.add_enabled(idle, egui::Button::new("Fetch")).clicked() {
            acts.push(fetch_action(path));
        }
        if ui
            .add_enabled(
                idle && sub.moved(),
                egui::Button::new("Update to the recorded commit"),
            )
            .on_hover_text("Check out the commit this repository records")
            .on_disabled_hover_text("It is already on the recorded commit")
            .clicked()
        {
            acts.push(guarded(sub, update_recorded_action(path)));
        }
        if ui
            .add_enabled(idle, egui::Button::new("Update to the latest remote commit"))
            .on_hover_text("Fetch and check out its tracked branch's tip; the change then shows as uncommitted here")
            .clicked()
        {
            acts.push(guarded(sub, update_remote_action(path)));
        }
    }
    if ui
        .add_enabled(idle, egui::Button::new("Initialize and update all"))
        .on_hover_text("Every submodule, nested ones too")
        .clicked()
    {
        acts.push(guarded_all(all, update_all_action()));
    }
    ui.separator();
    if ui.button("Copy path").clicked() {
        ui.ctx().copy_text(abs.display().to_string());
    }
    if ui
        .add_enabled(!sub.url.is_empty(), egui::Button::new("Copy URL"))
        .clicked()
    {
        ui.ctx().copy_text(sub.url.clone());
    }
    if sub.initialized {
        ui.separator();
        if ui
            .add_enabled(idle, egui::Button::new("Deinitialize…"))
            .clicked()
        {
            acts.push(Action::Dialog(deinit_dialog(path)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitlinks_skip_regular_files_and_merge_conflict_stages() {
        let out = "100644 aaaa 0\tREADME.md\0\
                   160000 1111111111111111111111111111111111111111 0\tcore\0\
                   160000 2222222222222222222222222222222222222222 1\tcommon\0\
                   160000 3333333333333333333333333333333333333333 2\tcommon\0\
                   160000 4444444444444444444444444444444444444444 3\tcommon\0";
        let links = parse_gitlinks(out);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].path, "core");
        assert!(!links[0].conflicted);
        assert_eq!(links[1].path, "common");
        assert!(links[1].conflicted);
    }

    #[test]
    fn gitlinks_keep_paths_with_spaces() {
        let out = "160000 1111111111111111111111111111111111111111 0\tmy libs/a b\0";
        let links = parse_gitlinks(out);
        assert_eq!(links[0].path, "my libs/a b");
    }

    #[test]
    fn gitmodules_pair_names_paths_and_urls() {
        let out = "submodule.includes/builder-5.path\nincludes/builder-5\0\
                   submodule.includes/builder-5.url\ngit@github.com:et/builder-5.git\0\
                   submodule.dotted.name.path\nvendor/x\0\
                   submodule.dotted.name.url\nhttps://example.com/x.git\0\
                   submodule.dotted.name.branch\nmain\0";
        let map = parse_gitmodules(out);
        assert_eq!(
            map["includes/builder-5"],
            (
                "includes/builder-5".to_owned(),
                "git@github.com:et/builder-5.git".to_owned()
            )
        );
        assert_eq!(
            map["vendor/x"],
            (
                "dotted.name".to_owned(),
                "https://example.com/x.git".to_owned()
            )
        );
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn gitmodules_without_a_url_still_name_the_path() {
        let map = parse_gitmodules("submodule.a.path\nsub/a\0");
        assert_eq!(map["sub/a"], ("a".to_owned(), String::new()));
    }

    #[test]
    fn folder_suggested_from_the_url() {
        assert_eq!(folder_from_url("https://github.com/user/repo.git"), "repo");
        assert_eq!(folder_from_url("git@github.com:user/repo.git"), "repo");
        assert_eq!(folder_from_url("https://host/user/repo/"), "repo");
        assert_eq!(folder_from_url("../sibling"), "sibling");
        assert_eq!(folder_from_url(""), "");
    }

    #[test]
    fn moved_needs_an_initialized_checkout_on_another_commit() {
        let mut s = Submodule {
            recorded: "aaa".into(),
            head: "aaa".into(),
            initialized: true,
            ..Submodule::default()
        };
        assert!(!s.moved());
        s.head = "bbb".into();
        assert!(s.moved());
        s.initialized = false;
        assert!(!s.moved());
    }

    /// Against a real repository: a submodule added, then one never initialized.
    #[test]
    fn lists_real_submodules() {
        use std::process::Command;
        let git_ok = |dir: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args([
                    "-c",
                    "protocol.file.allow=always",
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
                .unwrap_or(false);
            assert!(ok, "git {args:?} failed");
        };
        let base = std::env::temp_dir().join(format!("gitcrafted-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (inner, outer) = (base.join("inner"), base.join("outer"));
        for d in [&inner, &outer] {
            std::fs::create_dir_all(d).ok();
            git_ok(d, &["init", "-q", "-b", "main"]);
            git_ok(d, &["commit", "-q", "--allow-empty", "-m", "first"]);
        }
        let url = inner.display().to_string();
        git_ok(
            &outer,
            &["submodule", "add", "-q", "--", &url, "libs/inner"],
        );
        git_ok(&outer, &["commit", "-q", "-m", "add it"]);

        let subs = list(&outer);
        assert_eq!(subs.len(), 1);
        let s = &subs[0];
        assert_eq!(s.path, "libs/inner");
        assert_eq!(s.name, "libs/inner");
        assert_eq!(s.url, url);
        assert!(s.initialized);
        assert!(!s.moved());
        assert_eq!(s.branch.as_deref(), Some("main"));

        // A new commit inside it moves it off the recorded one.
        git_ok(
            &outer.join("libs/inner"),
            &["commit", "-q", "--allow-empty", "-m", "second"],
        );
        assert!(list(&outer)[0].moved());

        // Emptied (deinitialized): listed, not initialized.
        git_ok(
            &outer,
            &[
                "submodule",
                "deinit",
                "-q",
                "--force",
                "--",
                ":(literal)libs/inner",
            ],
        );
        let subs = list(&outer);
        assert!(!subs[0].initialized);
        assert!(subs[0].head.is_empty());

        // A repository with none.
        assert!(list(&inner).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::test_support::*;

    /// An outer repository with `libs/inner` as a submodule of `inner`.
    fn with_submodule(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let inner = repo(&format!("{name}-inner"));
        let outer = repo(&format!("{name}-outer"));
        git(&outer, &["config", "protocol.file.allow", "always"]);
        git(
            &outer,
            &[
                "submodule",
                "add",
                "-q",
                "--",
                &inner.display().to_string(),
                "libs/inner",
            ],
        );
        git(&outer, &["commit", "-q", "-m", "add inner"]);
        // `submodule update --remote` fetches inside the submodule, which has its own config.
        git(
            &outer.join("libs/inner"),
            &["config", "protocol.file.allow", "always"],
        );
        (outer, inner)
    }

    fn confirm_ops(dialog: Dialog) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
        let Dialog::Confirm {
            ops, alternative, ..
        } = dialog
        else {
            unreachable!("a confirmation")
        };
        (ops, alternative.map(|(_, alt)| alt).unwrap_or_default())
    }

    #[test]
    fn the_section_data_follows_the_submodule() {
        let (outer, _inner) = with_submodule("sm-data");
        let mut f = Fixture::open(&outer);
        assert_eq!(f.tab.submodules.len(), 1);
        let s = &f.tab.submodules[0];
        assert_eq!(
            (s.path.as_str(), s.initialized, s.moved()),
            ("libs/inner", true, false)
        );
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert!(!f.tab.work.unstaged.iter().any(|x| x.path == "libs/inner"));

        // A commit inside it: moved, and the parent's Changes list says so.
        git(
            &outer.join("libs/inner"),
            &["commit", "-q", "--allow-empty", "-m", "inside"],
        );
        f.reload();
        assert!(f.tab.submodules[0].moved());
        assert!(f.tab.work.unstaged.iter().any(|x| x.path == "libs/inner"));

        // The recorded commit is restored by the menu's update.
        f.run(update_recorded_action("libs/inner"));
        assert!(!f.failed(), "{:?}", f.status());
        assert!(!f.tab.submodules[0].moved());
    }

    #[test]
    fn initialize_deinitialize_and_update_all() {
        let (outer, _inner) = with_submodule("sm-init");
        let mut f = Fixture::open(&outer);

        // The confirmation's plain command leaves it listed but empty; --force is the checkbox.
        let (plain, forced) = confirm_ops(deinit_dialog("libs/inner"));
        assert_eq!(
            plain[0],
            ["submodule", "deinit", "--", ":(literal)libs/inner"]
        );
        assert_eq!(
            forced[0],
            [
                "submodule",
                "deinit",
                "--force",
                "--",
                ":(literal)libs/inner"
            ]
        );
        f.run(Action::Ops {
            label: "Deinitialize".to_owned(),
            ops: plain,
            refresh: Refresh::All,
        });
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(f.tab.submodules.len(), 1);
        assert!(!f.tab.submodules[0].initialized);
        assert!(f.tab.submodules[0].head.is_empty());

        f.run(init_action("libs/inner"));
        assert!(!f.failed(), "{:?}", f.status());
        assert!(f.tab.submodules[0].initialized);

        // Deinitialize again, then bring everything back in one go.
        f.run(Action::Ops {
            label: "Deinitialize".to_owned(),
            ops: forced,
            refresh: Refresh::All,
        });
        assert!(!f.tab.submodules[0].initialized);
        f.run(update_all_action());
        assert!(!f.failed(), "{:?}", f.status());
        assert!(f.tab.submodules[0].initialized);
    }

    #[test]
    fn fetch_and_update_from_the_remote() {
        let (outer, inner) = with_submodule("sm-remote");
        let mut f = Fixture::open(&outer);
        git(
            &inner,
            &["commit", "-q", "--allow-empty", "-m", "news upstream"],
        );
        let news = git(&inner, &["rev-parse", "HEAD"]);

        f.run(fetch_action("libs/inner"));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(
            git(&outer.join("libs/inner"), &["rev-parse", "origin/main"]),
            news
        );
        assert!(
            !f.tab.submodules[0].moved(),
            "fetching doesn't move the checkout"
        );

        f.run(update_remote_action("libs/inner"));
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(f.tab.submodules[0].head, news);
        assert!(f.tab.submodules[0].moved());
    }

    #[test]
    fn adding_a_submodule_from_the_dialog_commands() {
        let inner = repo("sm-add-inner");
        let outer = repo("sm-add-outer");
        let mut f = Fixture::open(&outer);
        assert!(f.tab.submodules.is_empty());
        // The commands the dialog runs, with git's file transport allowed for the fixture only
        // (git blocks it for submodules unless the user's own config says otherwise).
        for step in add_ops(&inner.display().to_string(), "vendor/inner") {
            let args: Vec<&str> = step.iter().map(String::as_str).collect();
            git(&outer, &args);
        }
        f.reload();
        assert_eq!(f.tab.submodules.len(), 1);
        assert_eq!(f.tab.submodules[0].path, "vendor/inner");
        assert_eq!(f.tab.submodules[0].url, inner.display().to_string());
        assert!(outer.join(".gitmodules").exists());
    }

    #[test]
    fn a_submodule_opens_as_a_repository_of_its_own() {
        let (outer, _inner) = with_submodule("sm-open");
        let mut f = Fixture::open(&outer);
        let abs = outer.join("libs/inner");
        f.tab.apply(Action::OpenTab(abs.clone()), &f.ctx);
        assert_eq!(f.tab.open_request, Some(abs.clone()));
        // The tab the app would open for it is the submodule's own repository.
        let sub = Fixture::open(&abs);
        assert!(sub.tab.submodules.is_empty());
        assert_eq!(sub.tab.refs.current.as_deref(), Some("main"));
    }

    #[test]
    fn nested_and_uninitialized_submodules_are_listed_sorted() {
        let a = repo("sm-sort-a");
        let b = repo("sm-sort-b");
        let outer = repo("sm-sort-outer");
        git(&outer, &["config", "protocol.file.allow", "always"]);
        for (src, path) in [(&b, "zeta"), (&a, "alpha")] {
            git(
                &outer,
                &[
                    "submodule",
                    "add",
                    "-q",
                    "--",
                    &src.display().to_string(),
                    path,
                ],
            );
        }
        git(&outer, &["commit", "-q", "-m", "two"]);
        git(
            &outer,
            &["submodule", "deinit", "-q", "--force", "--", "zeta"],
        );
        let mut f = Fixture::open(&outer);
        f.reload();
        let paths: Vec<&str> = f.tab.submodules.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["alpha", "zeta"]);
        assert!(f.tab.submodules[0].initialized && !f.tab.submodules[1].initialized);
    }
}

/// Data-safety tests: work inside a submodule is never silently left behind or overwritten.
#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::repo::test_support::*;

    /// `outer` with `sub` at the newest of two commits of `inner`.
    fn setup(name: &str) -> std::path::PathBuf {
        let inner = repo(&format!("{name}-inner"));
        commit_file(&inner, "i.txt", "newer\n", "newer");
        let outer = repo(&format!("{name}-outer"));
        git(&outer, &["config", "protocol.file.allow", "always"]);
        git(
            &outer,
            &[
                "submodule",
                "add",
                "-q",
                "--",
                &inner.display().to_string(),
                "sub",
            ],
        );
        git(&outer, &["commit", "-q", "-m", "add sub"]);
        git(
            &outer.join("sub"),
            &["config", "protocol.file.allow", "always"],
        );
        outer
    }

    fn sub(f: &Fixture) -> Submodule {
        f.tab.submodules.first().cloned().unwrap_or_default()
    }

    fn asks_first(action: &Action) -> bool {
        matches!(action, Action::Dialog(Dialog::Confirm { .. }))
    }

    #[test]
    fn commits_on_no_branch_are_flagged_and_every_update_asks_first() {
        let outer = setup("sf-sub-orphan");
        let inside = outer.join("sub");
        git(&inside, &["checkout", "-q", "--detach"]);
        let precious = commit_file(&inside, "work.txt", "precious\n", "precious work");
        let mut f = Fixture::open(&outer);
        let s = sub(&f);
        assert!(s.unsaved && s.moved());
        assert!(asks_first(&guarded(&s, update_recorded_action("sub"))));
        assert!(asks_first(&guarded(&s, update_remote_action("sub"))));
        assert!(asks_first(&guarded_all(
            &f.tab.submodules,
            update_all_action()
        )));
        let Dialog::Confirm { body, .. } = deinit_dialog("sub") else {
            unreachable!("a confirmation")
        };
        assert!(!body.is_empty());

        // A branch on the work: no longer at risk, and updating keeps it on that branch.
        git(&inside, &["branch", "keep"]);
        f.reload();
        let s = sub(&f);
        assert!(!s.unsaved);
        let action = guarded(&s, update_recorded_action("sub"));
        assert!(!asks_first(&action));
        f.run(action);
        assert!(!f.failed(), "{:?}", f.status());
        assert!(!sub(&f).moved());
        assert_eq!(git(&inside, &["rev-parse", "keep"]), precious);
    }

    #[test]
    fn checkouts_that_lose_nothing_are_not_flagged() {
        let outer = setup("sf-sub-safe");
        let inside = outer.join("sub");
        let mut f = Fixture::open(&outer);
        // On a branch (as after `submodule add`).
        assert!(!sub(&f).unsaved);
        // Detached at the recorded commit.
        git(&inside, &["checkout", "-q", "--detach"]);
        f.reload();
        assert!(!sub(&f).unsaved && !sub(&f).moved());
        // Detached at an older commit its branches have.
        git(&inside, &["checkout", "-q", "--detach", "HEAD~1"]);
        f.reload();
        assert!(sub(&f).moved());
        assert!(!sub(&f).unsaved);
        assert!(!asks_first(&guarded(
            &sub(&f),
            update_recorded_action("sub")
        )));

        // Detached at a newer commit that only its remote has (no local branch, not in the
        // recorded commit's history): the remote keeps it, nothing would be lost.
        let inner = std::path::PathBuf::from(git(&inside, &["remote", "get-url", "origin"]));
        commit_file(&inner, "i.txt", "newest\n", "newest upstream");
        git(&inside, &["fetch", "-q"]);
        git(&inside, &["checkout", "-q", "--detach", "origin/main"]);
        git(&inside, &["branch", "-q", "-D", "main"]);
        f.reload();
        assert!(sub(&f).moved());
        assert!(!sub(&f).unsaved, "the remote branch has it");
    }

    #[test]
    fn updating_never_overwrites_uncommitted_work_inside_a_submodule() {
        let outer = setup("sf-sub-dirty");
        let inside = outer.join("sub");
        git(&inside, &["checkout", "-q", "--detach", "HEAD~1"]);
        write(&inside, "i.txt", "my uncommitted edit\n");
        write(&inside, "scratch.txt", "untracked inside\n");
        let mut f = Fixture::open(&outer);
        assert!(sub(&f).moved());
        for action in [update_recorded_action("sub"), update_all_action()] {
            f.run(action);
            assert!(f.failed(), "git refuses to overwrite the edit");
            assert_eq!(read(&inside, "i.txt"), "my uncommitted edit\n");
            assert_eq!(read(&inside, "scratch.txt"), "untracked inside\n");
        }
    }

    #[test]
    fn a_plain_deinitialize_refuses_while_the_submodule_has_changes() {
        let outer = setup("sf-sub-deinit");
        let inside = outer.join("sub");
        write(&inside, "i.txt", "my uncommitted edit\n");
        let mut f = Fixture::open(&outer);
        let Dialog::Confirm { ops, .. } = deinit_dialog("sub") else {
            unreachable!("a confirmation")
        };
        f.run(Action::Ops {
            label: "Deinitialize".to_owned(),
            ops,
            refresh: Refresh::All,
        });
        assert!(
            f.failed(),
            "only the --force checkbox may throw changes away"
        );
        assert_eq!(read(&inside, "i.txt"), "my uncommitted edit\n");
    }
}
