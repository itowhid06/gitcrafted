//! The name, the logo and the links: what the packages, the installers and the desktop read, kept
//! consistent. These break when a file is renamed on one side only, an icon size is added without
//! being packaged, or a link still points at the old repository.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const REPO_URL: &str = "https://github.com/itowhid06/gitcrafted";
const NAME: &str = "gitcrafted";
const OLD_NAME: &str = "gitcraft";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel))
        .unwrap_or_else(|e| format!("<could not read {rel}: {e}>"))
}

/// Whether `word` occurs in `line` as a whole word (not inside `gitcrafted`, `gitcraft-x` is a hit).
fn has_word(line: &str, word: &str) -> bool {
    let bytes = line.as_bytes();
    let alnum = |b: u8| b.is_ascii_alphanumeric();
    line.match_indices(word).any(|(i, _)| {
        let before = i == 0 || !alnum(bytes[i - 1]);
        let after = bytes.get(i + word.len()).is_none_or(|b| !alnum(*b));
        before && after
    })
}

/// The `key = "value"` entries of Cargo.toml's package section.
fn cargo_value(key: &str) -> Option<String> {
    read("Cargo.toml")
        .lines()
        .find_map(|l| {
            l.strip_prefix(&format!("{key} = \""))
                .and_then(|r| r.strip_suffix('"'))
        })
        .map(str::to_owned)
}

/// Every file path a `.deb` or `.rpm` asset line names as its source, from Cargo.toml.
fn packaged_sources() -> Vec<String> {
    let mut sources = Vec::new();
    for line in read("Cargo.toml").lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("[\"") {
            sources.extend(rest.split('"').next().map(str::to_owned));
        } else if let Some(rest) = line.strip_prefix("{ source = \"") {
            sources.extend(rest.split('"').next().map(str::to_owned));
        }
    }
    sources
}

fn png_size(path: &Path) -> Option<(u32, u32)> {
    let bytes = std::fs::read(path).ok()?;
    // 8-byte signature, then the IHDR chunk: length, "IHDR", width, height.
    if bytes.get(..8)? != [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        || bytes.get(12..16)? != b"IHDR"
    {
        return None;
    }
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((be(16), be(20)))
}

#[test]
fn the_package_is_named_and_linked_as_gitcrafted() {
    assert_eq!(cargo_value("name").as_deref(), Some(NAME));
    assert_eq!(cargo_value("repository").as_deref(), Some(REPO_URL));
    let lock = read("Cargo.lock");
    assert!(
        lock.contains(&format!("name = \"{NAME}\"\nversion")),
        "Cargo.lock names the package"
    );
    // The program is built, packaged and started under that name.
    let deb = read("Cargo.toml");
    assert!(deb.contains(&format!(
        "[\"target/release/{NAME}\", \"usr/bin/\", \"755\"]"
    )));
    assert!(deb.contains(&format!("dest = \"/usr/bin/{NAME}\"")));
    assert!(deb.contains(&format!("\"packaging/{NAME}.desktop\"")));
}

#[test]
fn everything_the_packages_install_exists() {
    let sources = packaged_sources();
    assert!(
        sources.len() >= 14,
        "icons, desktop file, docs and the program: {sources:?}"
    );
    for source in sources {
        // The program itself is built by cargo.
        if source.starts_with("target/") {
            continue;
        }
        assert!(
            root().join(&source).exists(),
            "{source} is packaged but missing"
        );
    }
}

#[test]
fn every_icon_size_is_drawn_right_and_packaged() {
    let hicolor = root().join("assets/app-icon/hicolor");
    let mut on_disk = BTreeSet::new();
    for entry in std::fs::read_dir(&hicolor).into_iter().flatten().flatten() {
        let folder = entry.file_name().to_string_lossy().into_owned();
        let icon = entry.path().join("apps").join(format!("{NAME}.png"));
        if folder == "scalable" {
            let svg =
                std::fs::read_to_string(entry.path().join("apps").join(format!("{NAME}.svg")));
            assert!(svg.is_ok_and(|s| s.contains("<svg")), "the scalable icon");
            continue;
        }
        let side: u32 = folder
            .split('x')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        assert_eq!(
            png_size(&icon),
            Some((side, side)),
            "{folder} holds a {side} px picture"
        );
        on_disk.insert(side);
    }
    assert_eq!(
        on_disk,
        BTreeSet::from([16, 22, 24, 32, 48, 64, 128, 256, 512])
    );

    // Both package formats install exactly the sizes that exist, plus the scalable one.
    let cargo = read("Cargo.toml");
    for side in &on_disk {
        let deb = format!("\"usr/share/icons/hicolor/{side}x{side}/apps/{NAME}.png\"");
        let rpm = format!("dest = \"/usr/share/icons/hicolor/{side}x{side}/apps/{NAME}.png\"");
        assert!(
            cargo.contains(&deb),
            "the .deb doesn't install {side}x{side}"
        );
        assert!(
            cargo.contains(&rpm),
            "the .rpm doesn't install {side}x{side}"
        );
    }
    assert!(cargo.contains(&format!(
        "usr/share/icons/hicolor/scalable/apps/{NAME}.svg\""
    )));
    assert!(cargo.contains(&format!(
        "/usr/share/icons/hicolor/scalable/apps/{NAME}.svg\""
    )));
    // The AppImage copies the whole tree.
    assert!(read("packaging/linux/build-appimage.sh").contains("cp -R assets/app-icon/hicolor"));
}

#[test]
fn the_window_and_macos_icons_have_their_sizes() {
    assert_eq!(
        png_size(&root().join("assets/app-icon/gitcrafted-256.png")),
        Some((256, 256))
    );
    assert_eq!(
        png_size(&root().join("assets/app-icon/gitcrafted-1024.png")),
        Some((1024, 1024))
    );
    let script = read("packaging/macos/build-app.sh");
    assert!(script.contains("assets/app-icon/gitcrafted-1024.png"));
    // The retina variants up to 512@2x come from the 1024 master.
    for entry in ["512x512:512", "512x512@2x:1024", "256x256@2x:512"] {
        assert!(script.contains(entry), "{entry}");
    }
}

#[test]
fn the_windows_icon_holds_every_size() {
    let ico = std::fs::read(root().join("assets/app-icon/gitcrafted.ico")).unwrap_or_default();
    // ICONDIR: reserved 0, type 1 (icon), count; then 16-byte entries starting with width, height
    // (0 meaning 256).
    assert_eq!(ico.get(..4), Some(&[0u8, 0, 1, 0][..]));
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let sizes: BTreeSet<u32> = (0..count)
        .map(|i| match ico[6 + i * 16] {
            0 => 256,
            w => u32::from(w),
        })
        .collect();
    assert_eq!(sizes, BTreeSet::from([16, 24, 32, 48, 64, 128, 256]));
    assert!(read("build.rs").contains("assets/app-icon/gitcrafted.ico"));
    assert!(root().join("assets/app-icon/gitcrafted.ico").is_file());
}

#[test]
fn the_logo_files_are_complete() {
    for (file, simplified) in [("gitcrafted.svg", false), ("gitcrafted-small.svg", true)] {
        let svg = read(&format!("assets/app-icon/{file}"));
        assert!(
            svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"),
            "{file}"
        );
        assert!(svg.contains("viewBox=\"0 0 512 512\""), "{file}");
        assert!(svg.contains("<title>gitcrafted</title>"), "{file}");
        // Nothing the viewer would have to fetch.
        assert!(
            !svg.contains("href=\"http") && !svg.contains("<image"),
            "{file}"
        );
        // The full logo has the ringed nodes; the small one solid ones.
        assert_eq!(svg.contains("stroke-width=\"26\""), !simplified, "{file}");
    }
    assert!(
        root().join("assets/app-icon/render.py").is_file(),
        "the generator stays with the icons"
    );
}

#[test]
fn the_desktop_entry_matches_the_app_id_and_the_program() {
    let app_id = gitcrafted::APP_ID;
    assert_eq!(app_id, NAME);
    let desktop = read(&format!("packaging/{app_id}.desktop"));
    let field = |key: &str| {
        desktop
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .map(str::to_owned)
    };
    assert_eq!(field("Name").as_deref(), Some(NAME));
    assert_eq!(
        field("Icon").as_deref(),
        Some(app_id),
        "the icon is looked up by this name"
    );
    assert_eq!(
        field("StartupWMClass").as_deref(),
        Some(app_id),
        "how X11 matches the window"
    );
    assert_eq!(field("Exec").as_deref(), Some(&format!("{NAME} %f")[..]));
    assert_eq!(field("Terminal").as_deref(), Some("false"));
    assert!(field("Categories").is_some_and(|c| c.contains("Development")));
    // A desktop file's keys come once.
    let keys: Vec<&str> = desktop
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, _)| k)
        .collect();
    let unique: BTreeSet<&str> = keys.iter().copied().collect();
    assert_eq!(keys.len(), unique.len(), "{keys:?}");
    // The window reports that id.
    assert!(read("src/main.rs").contains(".with_app_id(APP_ID)"));
}

#[test]
fn the_installers_use_the_new_name_icon_and_links() {
    let iss = read("packaging/windows/gitcrafted.iss");
    assert!(iss.contains("SetupIconFile={#Root}\\assets\\app-icon\\gitcrafted.ico"));
    assert!(iss.contains(&format!("AppPublisherURL={REPO_URL}\n")));
    assert!(iss.contains(&format!("AppSupportURL={REPO_URL}/issues\n")));
    assert!(iss.contains(&format!("AppUpdatesURL={REPO_URL}/releases\n")));
    assert!(iss.contains("DefaultDirName={autopf}\\gitcrafted"));
    assert!(iss.contains("Source: \"{#Root}\\target\\release\\gitcrafted.exe\""));

    let plist = read("packaging/macos/Info.plist");
    assert!(plist.contains("<string>io.github.itowhid06.gitcrafted</string>"));
    assert!(plist.contains("<string>gitcrafted</string>"));

    let workflow = read(".github/workflows/release.yml");
    assert!(workflow.contains("name=\"gitcrafted-${{ github.ref_name }}-${{ matrix.name }}\""));
    assert!(workflow.contains("check/gitcrafted.app/Contents/MacOS/gitcrafted"));
    assert!(read("packaging/linux/build-appimage.sh").contains("packaging/gitcrafted.desktop"));
}

#[test]
fn the_update_check_and_the_readme_point_at_the_new_repository() {
    let update = read("src/update.rs");
    assert!(update.contains(&format!("\"{REPO_URL}/releases/latest\"")));
    let readme = read("README.md");
    assert!(
        readme.contains(&format!("{REPO_URL}/releases")),
        "the download link"
    );
    assert!(
        readme.contains("assets/app-icon/gitcrafted-256.png"),
        "the logo in the header"
    );
    assert!(
        readme.starts_with("<p align=\"center\"><img src=\"assets/app-icon/gitcrafted-256.png\"")
    );
}

#[test]
fn no_file_still_carries_the_old_name_or_repository() {
    let files = [
        "Cargo.toml",
        "build.rs",
        "README.md",
        "ATTRIBUTION.md",
        "examples/ui_shot.rs",
        ".github/workflows/release.yml",
        "packaging/gitcrafted.desktop",
        "packaging/linux/build-appimage.sh",
        "packaging/macos/Info.plist",
        "packaging/macos/build-app.sh",
        "packaging/windows/gitcrafted.iss",
        "src/main.rs",
        "src/lib.rs",
        "src/theme.rs",
        "src/update.rs",
        "src/repo/dialogs.rs",
        "src/repo/bisect.rs",
        "src/repo/issues.rs",
        "src/app.rs",
        "src/settings.rs",
        "src/manual.rs",
        "src/desktop_entry.rs",
    ];
    for file in files {
        for (n, line) in read(file).lines().enumerate() {
            // The README credits the project this one started from.
            if file == "README.md" && line.contains("Vancete") {
                continue;
            }
            assert!(!has_word(line, OLD_NAME), "{file}:{}: {line}", n + 1);
            assert!(
                !line.contains("Vancete") && !line.contains("vancete"),
                "{file}:{}: {line}",
                n + 1
            );
            assert!(!line.contains("GitCraft"), "{file}:{}: {line}", n + 1);
        }
    }
}

#[test]
fn the_readme_credits_the_original_and_keeps_the_licences() {
    let readme = read("README.md");
    assert!(
        readme.contains("github.com/Vancete/gitcraft"),
        "the project this was forked from"
    );
    for licence in ["LICENSE-MIT", "LICENSE-APACHE"] {
        assert!(root().join(licence).is_file(), "{licence} stays");
    }
    // The licence texts keep their original copyright holders.
    assert!(!read("LICENSE-MIT").contains("<<"), "no placeholder");
    // The logo has its own row in the asset table.
    assert!(read("ATTRIBUTION.md").contains("gitcrafted.svg"));
}

#[test]
fn the_has_word_helper_knows_whole_words() {
    assert!(has_word("run gitcraft now", "gitcraft"));
    assert!(has_word("gitcraft-256.png", "gitcraft"));
    assert!(has_word("gitcraft_x", "gitcraft"));
    assert!(has_word("gitcraft", "gitcraft"));
    assert!(!has_word("gitcrafted", "gitcraft"));
    assert!(!has_word("mygitcraft", "gitcraft"));
    assert!(!has_word("nothing here", "gitcraft"));
}
