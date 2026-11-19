//! gitcrafted: a visual git client (egui front end over the git command line).

/// The app id: the window's class on Linux, the menu entry's file name and the folder its
/// settings are saved in.
pub const APP_ID: &str = "gitcrafted";

/// Before the window opens: makes sure the desktop shows the app's icon and name (Linux). Only
/// the program does this, never tests.
pub fn prepare() {
    desktop_entry::ensure();
}

pub mod app;
mod avatars;
mod desktop_entry;
pub mod file_dialog;
mod forge;
mod git;
mod graph;
mod icons;
mod manual;
mod repo;
mod settings;
mod shell_path;
mod tab_look;
mod terminal;
mod theme;
mod update;
mod vscode_theme;
