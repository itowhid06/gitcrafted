//! macOS apps opened from the Finder or the Dock get a bare PATH (`/usr/bin:/bin:/usr/sbin:/sbin`),
//! without Homebrew's folders, where git, gh, glab and editors' commands usually are. The
//! commands gitcrafted starts get the PATH of the user's login shell instead.

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Folders added in any case (Homebrew on Apple silicon, then on Intel).
const FALLBACK: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// The PATH for commands gitcrafted runs, when it differs from its own (macOS only).
pub fn path() -> Option<&'static OsString> {
    static PATH: OnceLock<Option<OsString>> = OnceLock::new();
    PATH.get_or_init(|| {
        if cfg!(target_os = "macos") {
            login_path()
        } else {
            None
        }
    })
    .as_ref()
}

/// Gives `cmd` that PATH.
pub fn apply(cmd: &mut Command) {
    if let Some(path) = path() {
        cmd.env("PATH", path);
    }
}

fn login_path() -> Option<OsString> {
    let own = std::env::var_os("PATH").unwrap_or_default();
    let shell = shell_path().unwrap_or_default();
    let joined = merge([&shell, &own]);
    (joined != own).then_some(joined)
}

/// Every folder of `paths` once, in order, then the fallbacks.
fn merge<const N: usize>(paths: [&OsString; N]) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let candidates = paths
        .iter()
        .flat_map(|p| std::env::split_paths(p).collect::<Vec<_>>())
        .chain(FALLBACK.iter().map(PathBuf::from));
    for dir in candidates {
        if !dir.as_os_str().is_empty() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

const START_MARK: &str = "__GC_PATH_START__";
const END_MARK: &str = "__GC_PATH_END__";

fn extract_marked_path(out: &str) -> Option<&str> {
    let (_, rest) = out.split_once(START_MARK)?;
    let (path, _) = rest.split_once(END_MARK)?;
    let path = path.trim();
    (!path.is_empty()).then_some(path)
}

fn posix_shell() -> OsString {
    if let Some(shell) = std::env::var_os("SHELL")
        && !shell.is_empty()
    {
        let p = std::path::Path::new(&shell);
        if let Some(name) = p.file_name().and_then(|n| n.to_str())
            && matches!(name, "zsh" | "bash" | "sh" | "ksh" | "dash" | "ash")
            && p.is_file()
        {
            return shell;
        }
    }
    "/bin/zsh".into()
}

/// `$PATH` as the user's login shell sets it, if it answers within a few seconds.
fn shell_path() -> Option<OsString> {
    let shell = posix_shell();
    let script = format!("printf '{START_MARK}%s{END_MARK}' \"$PATH\"");
    let mut child = Command::new(shell)
        .args(["-l", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let mut s = stdout;
        let _ = s.read_to_string(&mut out);
        out
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    }?;
    if !status.success() {
        return None;
    }
    let raw = reader.join().ok()?;
    extract_marked_path(&raw).map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_kept_once_in_order() {
        let shell = std::env::join_paths(["/opt/homebrew/bin", "/usr/bin"]).unwrap_or_default();
        let own = std::env::join_paths(["/usr/bin", "/bin"]).unwrap_or_default();
        let merged: Vec<PathBuf> = std::env::split_paths(&merge([&shell, &own])).collect();
        assert_eq!(
            merged,
            ["/opt/homebrew/bin", "/usr/bin", "/bin", "/usr/local/bin"].map(PathBuf::from)
        );
    }
}
