use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn run_git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .env("LC_ALL", "C")
        .args([
            "-c",
            "user.name=Tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn settle(harness: &mut Harness<'_, gitcrafted::app::App>, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        harness.step();
        let rect = harness.ctx.content_rect();
        assert!(
            rect.width().is_finite()
                && rect.height().is_finite()
                && rect.width() > 0.0
                && rect.height() > 0.0,
            "non-finite content rect: {rect:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    harness.step();
}

fn build_stress_repo(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("gitcrafted-ui-stress-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::create_dir_all(&base);

    let bare = base.join("remote.git");
    let work = base.join("repo");
    assert!(run_git(&base, &["init", "-q", "--bare", "-b", "main", "remote.git"]));
    assert!(run_git(&base, &["init", "-q", "-b", "main", "repo"]));

    let _ = std::fs::write(work.join("README.md"), "# stress repo\nline 2\n");
    let _ = std::fs::write(work.join("tracked.txt"), "one\ntwo\nthree\n");
    assert!(run_git(&work, &["add", "."]));
    assert!(run_git(&work, &["commit", "-q", "-m", "Initial commit #1"]));
    assert!(run_git(&work, &["tag", "-a", "v0.1.0", "-m", "tag 1"]));
    assert!(run_git(
        &work,
        &["remote", "add", "origin", &bare.to_string_lossy()]
    ));
    assert!(run_git(&work, &["push", "-q", "-u", "origin", "main"]));

    // Side branch + stash + dirty working tree + conflict file.
    assert!(run_git(&work, &["checkout", "-q", "-b", "feature"]));
    let _ = std::fs::write(work.join("tracked.txt"), "one\ntwo from feature\nthree\n");
    let _ = std::fs::write(work.join("extra.rs"), "fn demo() -> usize { 42 }\n");
    assert!(run_git(&work, &["add", "."]));
    assert!(run_git(&work, &["commit", "-q", "-m", "Feature work #2"]));

    assert!(run_git(&work, &["checkout", "-q", "main"]));
    let _ = std::fs::write(work.join("tracked.txt"), "one\ntwo from main\nthree\n");
    assert!(run_git(&work, &["add", "tracked.txt"]));
    assert!(run_git(&work, &["commit", "-q", "-m", "Main change"]));

    let _ = std::fs::write(work.join("README.md"), "# stashed edit\n");
    assert!(run_git(&work, &["stash", "push", "-q", "-m", "temp stash"]));

    // Merge feature into main to produce a real conflicted state alongside unstaged edits.
    let _ = run_git(&work, &["merge", "--no-commit", "feature"]);
    let _ = std::fs::write(work.join("notes.txt"), "untracked note\n");

    work
}

fn run_palette_command(harness: &mut Harness<'_, gitcrafted::app::App>, query: &str) {
    harness.key_press_modifiers(Modifiers::COMMAND, Key::P);
    settle(harness, 120);
    harness.event(Event::Text(query.to_owned()));
    settle(harness, 120);
    harness.key_press(Key::Enter);
    settle(harness, 250);
}

fn exercise_viewport(size: [f32; 2], repo_path: &Path) {
    // 1. Start page, manual, clone dialog, and settings.
    let mut start_harness = Harness::builder()
        .with_size(size)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| gitcrafted::app::App::new(cc, None));
    settle(&mut start_harness, 250);

    // Command palette -> Clone dialog -> Escape.
    run_palette_command(&mut start_harness, "Clone repository");
    start_harness.key_press(Key::Escape);
    settle(&mut start_harness, 120);

    // Manual (F1) -> Escape.
    start_harness.key_press(Key::F1);
    settle(&mut start_harness, 150);
    start_harness.key_press(Key::Escape);
    settle(&mut start_harness, 120);

    // Settings tab.
    run_palette_command(&mut start_harness, "Settings");
    settle(&mut start_harness, 200);

    // 2. Active repository tab: graph, sidebar, changes, diff/conflict, search, terminal, dialogs.
    let open_path = repo_path.to_path_buf();
    let mut repo_harness = Harness::builder()
        .with_size(size)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(move |cc| gitcrafted::app::App::new(cc, Some(open_path)));
    settle(&mut repo_harness, 900);

    // Toggle Search bar, type query, then close.
    repo_harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
    settle(&mut repo_harness, 150);
    repo_harness.event(Event::Text("Initial".to_owned()));
    settle(&mut repo_harness, 250);
    repo_harness.key_press(Key::Escape);
    settle(&mut repo_harness, 120);

    // Exercise core dialogs through command palette and close cleanly with Escape.
    for cmd in ["Stash changes", "Apply patch", "Create branch", "Create tag", "Pull"] {
        run_palette_command(&mut repo_harness, cmd);
        repo_harness.key_press(Key::Escape);
        settle(&mut repo_harness, 150);
    }

    // Open a file diff / conflict view when visible in Changes.
    if let Some(node) = repo_harness.query_by_label("notes.txt") {
        node.click();
        settle(&mut repo_harness, 300);
    }
    if let Some(node) = repo_harness.query_by_label("tracked.txt") {
        node.click();
        settle(&mut repo_harness, 350);
    }

    // Open built-in terminal briefly and toggle it closed.
    run_palette_command(&mut repo_harness, "Toggle terminal");
    settle(&mut repo_harness, 300);
    run_palette_command(&mut repo_harness, "Toggle terminal");
    settle(&mut repo_harness, 150);

    // Open Settings tab inside repo session as well (covers packaged desktop check layout).
    run_palette_command(&mut repo_harness, "Settings");
    settle(&mut repo_harness, 250);
}

#[test]
fn ui_survives_normal_and_narrow_viewports_without_panics() {
    let repo_dir = build_stress_repo("dual");
    exercise_viewport([1280.0, 800.0], &repo_dir);
    exercise_viewport([320.0, 480.0], &repo_dir);
    if let Some(parent) = repo_dir.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}
