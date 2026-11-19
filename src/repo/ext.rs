//! The plumbing for the extra repository features (reflog, bisect, LFS, maintenance, external
//! tools, issue links, merge / stash options…): one dialog hook, one action hook and the
//! toolbar's Tools menu. Each feature lives in a file of its own.
//!
//! Kept apart so the rest of the tree only carries a few hooks and a rebase from upstream stays
//! easy.

use std::path::PathBuf;

use egui::Ui;

use super::dialogs::Outcome;
use super::{Dialog, RepoTab};
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

pub enum ExtAction {
    /// Start a program without waiting for it (an IDE, a diff tool).
    Spawn {
        program: String,
        args: Vec<String>,
        dir: Option<PathBuf>,
    },
}

impl RepoTab {
    pub(super) fn ext_action(&mut self, action: ExtAction, ctx: &egui::Context) {
        match action {
            ExtAction::Spawn { program, args, dir } => {
                if let Err(e) = spawn_detached(ctx, &program, &args, dir.as_deref()) {
                    self.status = Some((e, true));
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::test_support::*;

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
                program: "definitely-not-a-program-gitcraft".to_owned(),
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

