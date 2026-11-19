//! External tools: the editors and IDEs found on the PATH to open the repository in, and the
//! diff and merge tools git uses for "Open in external diff tool".

use std::path::PathBuf;
use std::sync::OnceLock;

use egui::{RichText, Ui};

use super::dialogs::{Outcome, buttons, title};
use super::ext::{DialogBody, dialog};
use super::sync::{pick, row};
use super::{Action, Dialog, Refresh, RepoTab};
use crate::git;
use crate::theme::Tokens;

pub struct App {
    pub label: String,
    pub program: String,
}

/// Editors and IDEs, by the command that starts each.
const APPS: &[(&str, &str)] = &[
    ("Visual Studio Code", "code"),
    ("VS Code Insiders", "code-insiders"),
    ("VSCodium", "codium"),
    ("Cursor", "cursor"),
    ("Windsurf", "windsurf"),
    ("Zed", "zed"),
    ("Sublime Text", "subl"),
    ("Sublime Merge", "smerge"),
    ("IntelliJ IDEA", "idea"),
    ("WebStorm", "webstorm"),
    ("PhpStorm", "phpstorm"),
    ("PyCharm", "pycharm"),
    ("GoLand", "goland"),
    ("RustRover", "rustrover"),
    ("CLion", "clion"),
    ("Rider", "rider"),
    ("Fleet", "fleet"),
    ("Kate", "kate"),
    ("GNOME Builder", "gnome-builder"),
];

/// Tools that need a terminal: gitcraft starts tools without one, where they'd hang unseen.
pub fn needs_terminal(tool: &str) -> bool {
    let base = tool.trim_end_matches(['1', '2', '3']);
    matches!(
        base,
        "vimdiff" | "nvimdiff" | "emerge" | "vimdiff-" | "nvimdiff-"
    )
}

/// Diff and merge tools git knows by name (graphical ones only).
const TOOLS: &[&str] = &[
    "vscode",
    "meld",
    "kdiff3",
    "bc",
    "p4merge",
    "smerge",
    "araxis",
    "diffmerge",
    "winmerge",
    "tortoisemerge",
    "xxdiff",
    "opendiff",
];

fn on_path(program: &str) -> bool {
    let path = crate::shell_path::path()
        .cloned()
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    let names: Vec<String> = if cfg!(windows) {
        ["", ".exe", ".cmd", ".bat"]
            .iter()
            .map(|ext| format!("{program}{ext}"))
            .collect()
    } else {
        vec![program.to_owned()]
    };
    std::env::split_paths(&path).any(|dir| names.iter().any(|n| dir.join(n).is_file()))
}

/// The apps of [`APPS`] that are installed, looked for once.
pub fn detected() -> &'static [App] {
    static FOUND: OnceLock<Vec<App>> = OnceLock::new();
    FOUND.get_or_init(|| {
        APPS.iter()
            .filter(|(_, program)| on_path(program))
            .map(|(label, program)| App {
                label: (*label).to_owned(),
                program: (*program).to_owned(),
            })
            .collect()
    })
}

struct ExternalToolsDialog {
    diff: String,
    merge: String,
    /// Save in the user's git config instead of this repository's.
    global: bool,
    _repo: PathBuf,
}

impl ExternalToolsDialog {
    /// The tools git is configured with now.
    fn load(repo: &std::path::Path) -> ExternalToolsDialog {
        let get = |key: &str| {
            git::run(Some(repo), &["config", "--get", key])
                .map(|v| v.trim().to_owned())
                .unwrap_or_default()
        };
        ExternalToolsDialog {
            diff: get("diff.tool"),
            merge: get("merge.tool"),
            global: true,
            _repo: repo.to_path_buf(),
        }
    }
}

impl RepoTab {
    pub(super) fn external_tools_dialog(&self) -> Dialog {
        dialog(ExternalToolsDialog::load(&self.path))
    }
}

/// The `git config` commands that save the choices.
fn config_steps(d: &ExternalToolsDialog) -> Vec<Vec<String>> {
    let scope = if d.global { "--global" } else { "--local" };
    let set = |key: &str, value: &str| {
        vec![
            "config".to_owned(),
            scope.to_owned(),
            key.to_owned(),
            value.to_owned(),
        ]
    };
    let mut steps = Vec::new();
    if !d.diff.trim().is_empty() {
        steps.push(set("diff.tool", d.diff.trim()));
        steps.push(set("difftool.prompt", "false"));
    }
    if !d.merge.trim().is_empty() {
        steps.push(set("merge.tool", d.merge.trim()));
        steps.push(set("mergetool.prompt", "false"));
        steps.push(set("mergetool.keepBackup", "false"));
    }
    steps
}

fn tool_row(ui: &mut Ui, t: &Tokens, label: &str, id: &str, value: &mut String) {
    row(ui, t, label, |ui| {
        let text = if value.is_empty() {
            "Not set".to_owned()
        } else {
            value.clone()
        };
        let changed = pick(ui, id, &text, |ui| {
            let mut changed = false;
            for tool in TOOLS {
                if ui.selectable_label(value == tool, *tool).clicked() {
                    *value = (*tool).to_owned();
                    changed = true;
                }
            }
            changed
        });
        let _ = changed;
    });
}

impl DialogBody for ExternalToolsDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, enter: bool) -> Outcome {
        title(
            ui,
            "External diff and merge tools",
            "What \"Open in external diff tool\" and \"Open in merge tool\" start (git difftool / mergetool).",
            t,
        );
        tool_row(ui, t, "Diff tool", "ext-diff", &mut self.diff);
        tool_row(ui, t, "Merge tool", "ext-merge", &mut self.merge);
        ui.add_space(4.0);
        ui.checkbox(
            &mut self.global,
            "For every repository (your global git config)",
        );
        ui.label(
            RichText::new("A tool that isn't listed: set difftool.<name>.cmd in your git config.")
                .color(t.text_faint)
                .small(),
        );
        ui.add_space(8.0);
        let valid = !self.diff.trim().is_empty() || !self.merge.trim().is_empty();
        buttons(ui, "Save", valid, enter, || Action::Ops {
            label: "Save external tools".to_owned(),
            ops: config_steps(self),
            refresh: Refresh::All,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_steps_set_tools_and_turn_prompts_off() {
        let d = ExternalToolsDialog {
            diff: "meld".into(),
            merge: String::new(),
            global: false,
            _repo: PathBuf::new(),
        };
        let steps = config_steps(&d);
        assert_eq!(steps[0], ["config", "--local", "diff.tool", "meld"]);
        assert_eq!(steps[1], ["config", "--local", "difftool.prompt", "false"]);
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn a_program_on_the_path_is_found() {
        // `git` runs these tests, so it is on the PATH.
        assert!(on_path("git"));
        assert!(!on_path("definitely-not-a-program-gitcraft"));
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::repo::Refresh;
    use crate::repo::ext::ExtAction;
    use crate::repo::test_support::*;

    #[test]
    fn saving_sets_the_tools_for_this_repository() {
        let dir = repo("xt-save");
        let mut f = Fixture::open(&dir);
        let d = ExternalToolsDialog {
            diff: "meld".to_owned(),
            merge: "kdiff3".to_owned(),
            global: false,
            _repo: dir.clone(),
        };
        f.run(Action::Ops {
            label: "Save external tools".to_owned(),
            ops: config_steps(&d),
            refresh: Refresh::All,
        });
        assert!(!f.failed(), "{:?}", f.status());
        assert_eq!(git(&dir, &["config", "--local", "diff.tool"]), "meld");
        assert_eq!(git(&dir, &["config", "--local", "merge.tool"]), "kdiff3");
        assert_eq!(
            git(&dir, &["config", "--local", "difftool.prompt"]),
            "false"
        );
        assert_eq!(
            git(&dir, &["config", "--local", "mergetool.keepBackup"]),
            "false"
        );

        // Opening the dialog again shows what was saved.
        let again = ExternalToolsDialog::load(&dir);
        assert_eq!(
            (again.diff.as_str(), again.merge.as_str()),
            ("meld", "kdiff3")
        );
        assert!(matches!(f.tab.external_tools_dialog(), Dialog::Ext(_)));
    }

    #[test]
    fn without_a_tool_the_actions_say_how_to_set_one() {
        let dir = repo("xt-unset");
        write(&dir, "a.txt", "changed\n");
        let mut f = Fixture::open(&dir);
        // Only matters when git has no tool anywhere (a global one would be used).
        if git_ok(&dir, &["config", "--get-regexp", r"^(diff|merge)\.tool$"]) {
            return;
        }
        f.tab.ext_action(
            ExtAction::DiffTool {
                path: "a.txt".into(),
                staged: false,
            },
            &f.ctx,
        );
        assert!(
            f.status()
                .is_some_and(|(m, e)| e && m.contains("No diff tool")),
            "{:?}",
            f.status()
        );
        f.tab.status = None;
        f.tab.ext_action(
            ExtAction::MergeTool {
                path: "a.txt".into(),
            },
            &f.ctx,
        );
        assert!(
            f.status()
                .is_some_and(|(m, e)| e && m.contains("No merge tool")),
            "{:?}",
            f.status()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_diff_tool_gets_the_staged_or_the_working_file() {
        let dir = repo("xt-diff");
        let out = scratch("xt-diff-out").join("seen.txt");
        git(&dir, &["config", "diff.tool", "fake"]);
        git(&dir, &["config", "difftool.prompt", "false"]);
        git(
            &dir,
            &[
                "config",
                "difftool.fake.cmd",
                &format!("cp \"$REMOTE\" '{}'", out.display()),
            ],
        );
        write(&dir, "a.txt", "staged\n");
        git(&dir, &["add", "a.txt"]);
        write(&dir, "a.txt", "worktree\n");
        let mut f = Fixture::open(&dir);

        f.tab.ext_action(
            ExtAction::DiffTool {
                path: "a.txt".into(),
                staged: true,
            },
            &f.ctx,
        );
        assert!(wait_for(
            || std::fs::read_to_string(&out).is_ok_and(|t| t == "staged\n")
        ));
        let _ = std::fs::remove_file(&out);
        f.tab.ext_action(
            ExtAction::DiffTool {
                path: "a.txt".into(),
                staged: false,
            },
            &f.ctx,
        );
        assert!(wait_for(
            || std::fs::read_to_string(&out).is_ok_and(|t| t == "worktree\n")
        ));
        assert!(!f.failed(), "{:?}", f.status());
    }

    #[cfg(unix)]
    #[test]
    fn the_merge_tool_resolves_a_conflicted_file() {
        let dir = repo("xt-merge");
        git(&dir, &["checkout", "-q", "-b", "theirs"]);
        write(&dir, "a.txt", "theirs\n");
        git(&dir, &["commit", "-q", "-am", "theirs"]);
        git(&dir, &["checkout", "-q", "main"]);
        write(&dir, "a.txt", "ours\n");
        git(&dir, &["commit", "-q", "-am", "ours"]);
        assert!(!git_ok(&dir, &["merge", "-q", "theirs"]), "must conflict");
        git(&dir, &["config", "merge.tool", "fake"]);
        git(&dir, &["config", "mergetool.prompt", "false"]);
        git(&dir, &["config", "mergetool.keepBackup", "false"]);
        git(&dir, &["config", "mergetool.fake.trustExitCode", "true"]);
        git(
            &dir,
            &["config", "mergetool.fake.cmd", "cp \"$REMOTE\" \"$MERGED\""],
        );
        let mut f = Fixture::open(&dir);
        assert_eq!(f.tab.work.conflicted.len(), 1);

        f.tab.ext_action(
            ExtAction::MergeTool {
                path: "a.txt".into(),
            },
            &f.ctx,
        );
        assert!(wait_for(|| git(
            &dir,
            &["diff", "--name-only", "--diff-filter=U"]
        )
        .is_empty()));
        assert_eq!(read(&dir, "a.txt"), "theirs\n");
        f.reload();
        assert!(f.tab.work.conflicted.is_empty());
    }

    #[test]
    fn detected_apps_are_real_and_labelled() {
        for app in detected() {
            assert!(on_path(&app.program), "{}", app.program);
            assert!(!app.label.is_empty());
        }
        let programs: Vec<&str> = APPS.iter().map(|(_, p)| *p).collect();
        let mut unique = programs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), programs.len(), "no duplicate commands");
        assert!(TOOLS.contains(&"meld") && TOOLS.contains(&"vscode"));
    }
}

/// Regression tests (review round 2): terminal-only tools aren't offered or started blind.
#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::repo::ext::ExtAction;
    use crate::repo::test_support::*;

    #[test]
    fn only_graphical_tools_are_offered() {
        for tool in TOOLS {
            assert!(!needs_terminal(tool), "{tool}");
        }
        for tool in [
            "vimdiff",
            "vimdiff1",
            "vimdiff3",
            "nvimdiff",
            "nvimdiff2",
            "emerge",
        ] {
            assert!(needs_terminal(tool), "{tool}");
        }
        for tool in ["gvimdiff", "meld", "vscode", "bc"] {
            assert!(!needs_terminal(tool), "{tool}");
        }
    }

    #[test]
    fn a_terminal_tool_is_refused_with_a_reason_instead_of_hanging() {
        let dir = repo("rv-terminal-tool");
        write(&dir, "a.txt", "changed\n");
        git(&dir, &["config", "diff.tool", "vimdiff"]);
        git(&dir, &["config", "merge.tool", "nvimdiff"]);
        // Should a regression start them anyway, these stand in for the real (blocking) tools,
        // so the test fails at once instead of hanging on an editor with no terminal.
        let started = dir.join("started.txt");
        let cmd = format!("echo started > '{}'", started.display());
        git(&dir, &["config", "difftool.vimdiff.cmd", &cmd]);
        git(&dir, &["config", "mergetool.nvimdiff.cmd", &cmd]);
        let mut f = Fixture::open(&dir);
        f.tab.ext_action(
            ExtAction::DiffTool {
                path: "a.txt".into(),
                staged: false,
            },
            &f.ctx,
        );
        assert!(
            f.status()
                .is_some_and(|(m, e)| e && m.contains("vimdiff runs in a terminal")),
            "{:?}",
            f.status()
        );
        f.tab.ext_action(
            ExtAction::MergeTool {
                path: "a.txt".into(),
            },
            &f.ctx,
        );
        assert!(
            f.status()
                .is_some_and(|(m, e)| e && m.contains("nvimdiff runs in a terminal")),
            "{:?}",
            f.status()
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!started.exists(), "no tool was started");
    }
}
