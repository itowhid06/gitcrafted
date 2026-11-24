// No console window behind the app on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use gitcrafted::{APP_ID, app};

fn append_crash_log(entry: &str) {
    let Some(dir) = eframe::storage_dir(APP_ID) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("crash.log");
    // Rotate oversized logs so repeated failures stay bounded.
    if std::fs::metadata(&path)
        .map(|m| m.len() > 256 * 1024)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(entry.as_bytes());
    }
}

fn install_crash_log() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_owned());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic payload");
        let line = format!("[{secs}] panic in thread '{name}' at {loc}: {msg}\n");
        eprintln!("{line}");
        append_crash_log(&line);
        prev(info);
    }));
}

fn decode_percent_utf8(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(&hi), Some(&lo)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(h), Some(l)) = ((hi as char).to_digit(16), (lo as char).to_digit(16))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_open_arg(raw: std::ffi::OsString) -> PathBuf {
    let s = raw.to_string_lossy();
    if let Some(rest) = s.strip_prefix("file://") {
        let path_part = rest.strip_prefix("localhost").unwrap_or(rest);
        #[cfg(windows)]
        let path_part = path_part.strip_prefix('/').unwrap_or(path_part);
        return PathBuf::from(decode_percent_utf8(path_part));
    }
    PathBuf::from(raw)
}

fn main() -> eframe::Result {
    // Started by ssh to ask the user something (see `askpass`): ask the window and exit.
    if let Some(code) = gitcrafted::askpass::run_if_asked() {
        std::process::exit(code);
    }
    install_crash_log();
    gitcrafted::prepare();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("gitcrafted")
            // Matches gitcrafted.desktop, so Linux desktops show its icon and group its windows.
            .with_app_id(APP_ID)
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([800.0, 500.0])
            .with_icon(app::window_icon()),
        // Runs after eframe restored the last session's size and position.
        window_builder: Some(Box::new(|builder| {
            if cfg!(windows) && builder.maximized == Some(true) {
                app::MAXIMIZE_WHEN_SHOWN.store(true, Ordering::Relaxed);
                builder.with_maximized(false)
            } else {
                builder
            }
        })),
        ..Default::default()
    };
    let result = eframe::run_native(
        APP_ID,
        options,
        Box::new(|cc| {
            let open_arg = std::env::args_os().nth(1).map(parse_open_arg);
            Ok(Box::new(app::App::new(cc, open_arg)))
        }),
    );
    if let Err(ref err) = result {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let msg = format!("[{secs}] startup/render error: {err}\n");
        eprintln!("{msg}");
        append_crash_log(&msg);
    }
    result
}
