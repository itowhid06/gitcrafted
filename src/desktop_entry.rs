//! Linux: a menu entry and the icons for a gitcrafted that no package installed (an AppImage, a
//! binary built from source), so the shell, the dock and the top bar show its icon and name.
//!
//! On Wayland a window gets its icon and name only from an installed `<app id>.desktop` file (the
//! window icon the toolkit is given is ignored there), so without one the desktop shows a generic
//! icon and the raw app id. The .deb and .rpm packages install their own entry; for everything
//! else gitcrafted writes one into the user's own data folder at start-up, and keeps it pointing
//! at the file that is running. It can be removed in Settings, and then stays removed.

use std::path::{Path, PathBuf};

use crate::APP_ID;

/// File name of the entry. It must be the app id: that is how Wayland desktops match a window to
/// its entry (X11 ones also use `StartupWMClass`).
const ENTRY: &str = "gitcrafted.desktop";
/// Inside `<data>/gitcrafted/`: the user removed the entry, so it isn't added again.
const DECLINED: &str = "no-menu-entry";

/// The icon theme folders gitcrafted installs: (size folder, file extension, bytes).
const ICONS: &[(&str, &str, &[u8])] = &[
    (
        "16x16",
        "png",
        include_bytes!("../assets/app-icon/hicolor/16x16/apps/gitcrafted.png"),
    ),
    (
        "22x22",
        "png",
        include_bytes!("../assets/app-icon/hicolor/22x22/apps/gitcrafted.png"),
    ),
    (
        "24x24",
        "png",
        include_bytes!("../assets/app-icon/hicolor/24x24/apps/gitcrafted.png"),
    ),
    (
        "32x32",
        "png",
        include_bytes!("../assets/app-icon/hicolor/32x32/apps/gitcrafted.png"),
    ),
    (
        "48x48",
        "png",
        include_bytes!("../assets/app-icon/hicolor/48x48/apps/gitcrafted.png"),
    ),
    (
        "64x64",
        "png",
        include_bytes!("../assets/app-icon/hicolor/64x64/apps/gitcrafted.png"),
    ),
    (
        "128x128",
        "png",
        include_bytes!("../assets/app-icon/hicolor/128x128/apps/gitcrafted.png"),
    ),
    (
        "256x256",
        "png",
        include_bytes!("../assets/app-icon/hicolor/256x256/apps/gitcrafted.png"),
    ),
    (
        "512x512",
        "png",
        include_bytes!("../assets/app-icon/hicolor/512x512/apps/gitcrafted.png"),
    ),
    (
        "scalable",
        "svg",
        include_bytes!("../assets/app-icon/hicolor/scalable/apps/gitcrafted.svg"),
    ),
];

/// The file gitcrafted runs from and the menu entry should start: the AppImage (its runtime sets
/// `APPIMAGE`) or the program itself, on Linux, unless a package installed it.
pub fn target() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") || packaged(&system_data_dirs()) {
        return None;
    }
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| std::env::current_exe().ok().filter(|p| p.is_file()))
}

/// Whether the AppImage is what runs.
pub fn is_appimage() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .is_some_and(|p| p.is_file())
}

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

/// `$XDG_DATA_DIRS`, or the defaults.
fn system_data_dirs() -> Vec<PathBuf> {
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let mut list: Vec<PathBuf> = dirs
        .split(':')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect();
    if list.is_empty() {
        list = vec![
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ];
    }
    list
}

/// Whether a package (or an admin) installed an entry for gitcrafted in one of `dirs`.
fn packaged(dirs: &[PathBuf]) -> bool {
    dirs.iter()
        .any(|d| d.join("applications").join(ENTRY).is_file())
}

fn entry_path(data: &Path) -> PathBuf {
    data.join("applications").join(ENTRY)
}

fn icon_path(data: &Path, folder: &str, ext: &str) -> PathBuf {
    data.join("icons/hicolor")
        .join(folder)
        .join("apps")
        .join(format!("{APP_ID}.{ext}"))
}

fn declined_path(data: &Path) -> PathBuf {
    data.join(APP_ID).join(DECLINED)
}

/// Whether the user's menu entry exists.
pub fn installed() -> bool {
    data_home().is_some_and(|d| entry_path(&d).is_file())
}

/// Adds (or rewrites) the menu entry for `exe`, and forgets that it was ever removed.
pub fn install(exe: &Path) -> Result<(), String> {
    let data = data_home().ok_or("No home folder")?;
    let _ = std::fs::remove_file(declined_path(&data));
    let changed = write_all(&data, exe).map_err(|e| e.to_string())?;
    if changed {
        refresh_icon_cache(&data);
    }
    Ok(())
}

/// Removes the menu entry and the icons, and keeps them from being added again.
pub fn uninstall() -> Result<(), String> {
    let data = data_home().ok_or("No home folder")?;
    remove_all(&data).map_err(|e| e.to_string())?;
    refresh_icon_cache(&data);
    Ok(())
}

/// At start-up: makes sure there is an entry pointing at the file running now (a binary that was
/// moved, rebuilt elsewhere or replaced by a newer AppImage keeps its entry right), unless a
/// package provides one or the user removed it.
pub fn ensure() {
    let (Some(exe), Some(data)) = (target(), data_home()) else {
        return;
    };
    if declined_path(&data).exists() {
        return;
    }
    if write_all(&data, &exe).unwrap_or(false) {
        refresh_icon_cache(&data);
    }
}

/// Writes the entry and every icon that is missing or different. Returns whether anything changed.
fn write_all(data: &Path, exe: &Path) -> std::io::Result<bool> {
    let mut changed = write_if_different(&entry_path(data), entry(exe).as_bytes())?;
    for (folder, ext, bytes) in ICONS {
        changed |= write_if_different(&icon_path(data, folder, ext), bytes)?;
    }
    Ok(changed)
}

fn write_if_different(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)?;
    Ok(true)
}

/// Deletes the entry and the icons, then records that the user doesn't want them.
fn remove_all(data: &Path) -> std::io::Result<()> {
    let mut paths = vec![entry_path(data)];
    paths.extend(
        ICONS
            .iter()
            .map(|(folder, ext, _)| icon_path(data, folder, ext)),
    );
    for path in paths {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("{}: {e}", path.display()),
                ));
            }
        }
    }
    let marker = declined_path(data);
    if let Some(dir) = marker.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(marker, "")
}

/// Lets the desktop notice the new icons right away instead of at its next cache refresh. Best
/// effort: the tool may be missing, and the entry works without it.
fn refresh_icon_cache(data: &Path) {
    let theme = data.join("icons/hicolor");
    let _ = std::process::Command::new("gtk-update-icon-cache")
        .args(["--quiet", "--force", "--ignore-theme-index"])
        .arg(&theme)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut child| {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        });
}

fn entry(exe: &Path) -> String {
    let exec = quote_exec(&exe.to_string_lossy());
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=gitcrafted\n\
         GenericName=Git client\n\
         Comment=A visual git client\n\
         Exec={exec} %f\n\
         TryExec={}\n\
         Terminal=false\n\
         Categories=Development;RevisionControl;\n\
         Keywords=git;vcs;version control;repository;commit;branch;merge;diff;\n\
         Icon={APP_ID}\n\
         StartupWMClass={APP_ID}\n",
        escape_value(&exe.to_string_lossy())
    )
}

/// A program path as the quoted first word of `Exec` (Desktop Entry spec: `"`, `` ` ``, `$` and
/// `\` escaped inside the quotes, then the value's own escaping, `%` doubled).
fn quote_exec(path: &str) -> String {
    let mut quoted = String::from("\"");
    for c in path.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    escape_value(&quoted).replace('%', "%%")
}

/// A string value of a desktop file: backslashes and line breaks escaped.
fn escape_value(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gitcrafted-desktop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn quotes_the_executable_path() {
        assert_eq!(
            quote_exec("/home/me/Apps/gitcrafted.AppImage"),
            "\"/home/me/Apps/gitcrafted.AppImage\""
        );
        assert_eq!(
            quote_exec("/home/me/My Apps/$x 100%/a\"b"),
            "\"/home/me/My Apps/\\\\$x 100%%/a\\\\\"b\""
        );
    }

    #[test]
    fn the_entry_matches_the_app_id_the_window_uses() {
        // Wayland desktops find a window's entry by `<app id>.desktop`; X11 ones by the class.
        assert_eq!(ENTRY, format!("{APP_ID}.desktop"));
        let text = entry(Path::new("/opt/gitcrafted"));
        assert!(text.starts_with("[Desktop Entry]\nType=Application\nName=gitcrafted\n"));
        assert!(text.contains(&format!("Icon={APP_ID}\n")));
        assert!(text.contains(&format!("StartupWMClass={APP_ID}\n")));
        assert!(text.contains("Exec=\"/opt/gitcrafted\" %f\n"));
        assert!(text.contains("TryExec=/opt/gitcrafted\n"));
        assert!(text.contains("Categories=Development;RevisionControl;\n"));
    }

    #[test]
    fn writes_the_entry_and_every_icon_in_the_right_size() {
        let data = data_dir("write");
        let ok = write_all(&data, Path::new("/opt/gitcrafted")).unwrap_or(false);
        assert!(ok);
        assert!(entry_path(&data).is_file());
        let mut sizes = Vec::new();
        for (folder, ext, bytes) in ICONS {
            let path = icon_path(&data, folder, ext);
            assert_eq!(std::fs::read(&path).unwrap_or_default(), *bytes, "{folder}");
            if let Some((w, _)) = folder.split_once('x') {
                let image = image::load_from_memory(bytes);
                let want: u32 = w.parse().unwrap_or(0);
                assert!(
                    image.is_ok_and(|i| i.width() == want && i.height() == want),
                    "{folder} holds a {want} px picture"
                );
                sizes.push(want);
            } else {
                assert!(std::str::from_utf8(bytes).is_ok_and(|s| s.contains("<svg")));
            }
        }
        assert_eq!(sizes, [16, 22, 24, 32, 48, 64, 128, 256, 512]);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn rewrites_only_what_changed() {
        let data = data_dir("refresh");
        let old = Path::new("/opt/gitcrafted-v0.1.AppImage");
        assert!(write_all(&data, old).unwrap_or(false));
        assert!(
            !write_all(&data, old).unwrap_or(true),
            "nothing to do the second time"
        );

        // The file moved: only the entry changes.
        let new = Path::new("/opt/gitcrafted-v0.2.AppImage");
        assert!(write_all(&data, new).unwrap_or(false));
        let text = std::fs::read_to_string(entry_path(&data)).unwrap_or_default();
        assert!(text.contains("Exec=\"/opt/gitcrafted-v0.2.AppImage\" %f\n"));
        assert!(!text.contains("v0.1"));

        // An icon the user (or a cleaner) deleted or edited comes back.
        let icon = icon_path(&data, "48x48", "png");
        let _ = std::fs::remove_file(&icon);
        assert!(write_all(&data, new).unwrap_or(false));
        assert!(icon.is_file());
        let _ = std::fs::write(&icon, b"garbage");
        assert!(write_all(&data, new).unwrap_or(false));
        assert_ne!(std::fs::read(&icon).unwrap_or_default(), b"garbage");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn removing_it_leaves_no_trace_but_a_marker_that_keeps_it_removed() {
        let data = data_dir("remove");
        assert!(write_all(&data, Path::new("/opt/gitcrafted")).unwrap_or(false));
        assert!(remove_all(&data).is_ok());
        assert!(!entry_path(&data).exists());
        for (folder, ext, _) in ICONS {
            assert!(!icon_path(&data, folder, ext).exists(), "{folder}");
        }
        assert!(declined_path(&data).exists());
        // Removing again, with nothing there, is fine.
        assert!(remove_all(&data).is_ok());
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_packaged_entry_is_found_in_the_system_data_dirs() {
        let sys = data_dir("system");
        assert!(!packaged(std::slice::from_ref(&sys)));
        let _ = std::fs::create_dir_all(sys.join("applications"));
        let _ = std::fs::write(sys.join("applications").join(ENTRY), "[Desktop Entry]\n");
        assert!(packaged(std::slice::from_ref(&sys)));
        assert!(!packaged(&[PathBuf::from("/definitely/not/a/data/dir")]));
        let _ = std::fs::remove_dir_all(&sys);
    }
}
