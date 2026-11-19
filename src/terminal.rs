//! An integrated terminal: the user's shell in a pseudo-terminal (ConPTY on Windows), its output
//! interpreted by `vt100` and painted with egui.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use egui::{
    Color32, Event, FontId, Key, Modifiers, Rect, Sense, Stroke, Ui, pos2,
    text::{LayoutJob, TextFormat},
    vec2,
};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::theme::{self, Tokens};

const SCROLLBACK: usize = 5000;

/// A shell the terminal can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSpec {
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
}

impl ShellSpec {
    fn new(label: &str, program: &str, args: &[&str]) -> Self {
        Self {
            label: label.to_owned(),
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }
}

/// The shells installed on this machine, the platform default first (cached once).
pub fn available_shells() -> Vec<ShellSpec> {
    static SHELLS: OnceLock<Vec<ShellSpec>> = OnceLock::new();
    SHELLS
        .get_or_init(|| {
            let mut list = Vec::new();
            #[cfg(windows)]
            {
                list.push(ShellSpec::new(
                    "Windows PowerShell",
                    "powershell.exe",
                    &["-NoLogo"],
                ));
                if on_path("pwsh.exe") {
                    list.insert(0, ShellSpec::new("PowerShell 7", "pwsh.exe", &["-NoLogo"]));
                }
                list.push(ShellSpec::new("Command Prompt", "cmd.exe", &[]));
                for dir in [
                    r"C:\Program Files\Git\bin",
                    r"C:\Program Files (x86)\Git\bin",
                ] {
                    let bash = Path::new(dir).join("bash.exe");
                    if bash.is_file() {
                        let program = bash.display().to_string();
                        list.push(ShellSpec::new("Git Bash", &program, &["--login", "-i"]));
                        break;
                    }
                }
                if on_path("wsl.exe") {
                    list.push(ShellSpec::new("WSL", "wsl.exe", &[]));
                }
            }
            #[cfg(not(windows))]
            {
                if let Ok(shell) = std::env::var("SHELL")
                    && !shell.is_empty()
                    && Path::new(&shell).is_file()
                {
                    let name = Path::new(&shell)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| shell.clone());
                    list.push(ShellSpec::new(&name, &shell, &[]));
                }
                for (label, program) in [
                    ("bash", "/bin/bash"),
                    ("zsh", "/bin/zsh"),
                    ("fish", "/usr/bin/fish"),
                    ("sh", "/bin/sh"),
                ] {
                    if Path::new(program).is_file() && !list.iter().any(|s| s.program == program) {
                        list.push(ShellSpec::new(label, program, &[]));
                    }
                }
            }
            list
        })
        .clone()
}

/// The shell to start: the one chosen in the settings when installed, else the default.
pub fn default_shell(ctx: &egui::Context) -> ShellSpec {
    let wanted = crate::settings::Settings::get(ctx).shell;
    let shells = available_shells();
    shells
        .iter()
        .find(|s| !wanted.is_empty() && s.program == wanted)
        .or_else(|| shells.first())
        .cloned()
        .unwrap_or_else(|| ShellSpec::new("sh", "sh", &[]))
}

pub struct Terminal {
    id: u64,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Channel to a dedicated writer thread so neither the UI nor reader thread blocks on PTY writes.
    writer_tx: Sender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    exited: Arc<AtomicBool>,
    /// (rows, cols) the pty was last sized to.
    size: (u16, u16),
    /// Lines scrolled back into history (0 = following the output).
    scroll: usize,
    /// Fractional wheel scrolling not yet turned into lines.
    scroll_rest: f32,
    /// Selected cells, (row, col) on screen, from where the drag started to where it is.
    selection: Option<((u16, u16), (u16, u16))>,
    pub shell: String,
}

impl Terminal {
    /// Starts `shell` in `cwd`.
    pub fn spawn(cwd: &Path, shell: &ShellSpec, ctx: &egui::Context) -> Result<Self, String> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let (rows, cols) = (24, 100);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("could not open a terminal: {e}"))?;
        let program = shell.program.clone();
        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&shell.args);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(path) = crate::shell_path::path() {
            cmd.env("PATH", path);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("could not start {program}: {e}"))?;
        // The child holds its own handle; ours would keep the pty open after it exits.
        drop(pair.slave);
        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("terminal: {e}"));
            }
        };
        let mut raw_writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("terminal: {e}"));
            }
        };
        let (writer_tx, writer_rx) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Ok(bytes) = writer_rx.recv() {
                if raw_writer.write_all(&bytes).is_err() || raw_writer.flush().is_err() {
                    break;
                }
            }
        });

        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            nz(rows),
            nz(cols),
            SCROLLBACK,
        )));
        let exited = Arc::new(AtomicBool::new(false));
        {
            let parser = Arc::clone(&parser);
            let exited = Arc::clone(&exited);
            let writer_tx = writer_tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let bytes = buf.get(..n).unwrap_or_default();
                            let dsr_reply = {
                                let mut parser = lock(&parser);
                                parser.process(bytes);
                                // ConPTY (and some programs) ask where the cursor is and wait for
                                // the answer before printing anything (DSR 6 -> CPR).
                                if contains(bytes, b"\x1b[6n") {
                                    let (row, col) = parser.screen().cursor_position();
                                    Some(format!("\x1b[{};{}R", row + 1, col + 1))
                                } else {
                                    None
                                }
                            };
                            if let Some(reply) = dsr_reply {
                                let _ = writer_tx.send(reply.into_bytes());
                            }
                            ctx.request_repaint();
                        }
                    }
                }
                exited.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            });
        }

        let shell = shell.label.clone();
        Ok(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            parser,
            writer_tx,
            master: pair.master,
            child: Some(child),
            exited,
            size: (rows, cols),
            scroll: 0,
            scroll_rest: 0.0,
            selection: None,
            shell,
        })
    }
}

fn sanitized_terminal_font(raw_font: f32) -> f32 {
    if raw_font.is_finite() {
        raw_font.clamp(9.0, 20.0)
    } else {
        12.5
    }
}

fn format_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let safe = text.replace('\x1b', "");
        format!("\x1b[200~{safe}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

impl Terminal {
    pub fn has_exited(&mut self) -> bool {
        if !self.exited.load(Ordering::Relaxed)
            && let Some(child) = &mut self.child
            && let Ok(Some(_)) = child.try_wait()
        {
            // ConPTY doesn't always end the read when the shell exits.
            self.exited.store(true, Ordering::Relaxed);
        }
        self.exited.load(Ordering::Relaxed)
    }

    fn send(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let _ = self.writer_tx.send(bytes.to_vec());
        self.selection = None;
        // Typing follows the output again.
        if self.scroll != 0 {
            self.scroll = 0;
            lock(&self.parser).screen_mut().set_scrollback(0);
        }
    }

    /// Paints the screen into all of `ui`'s remaining space and handles input.
    pub fn ui(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let font_pt = sanitized_terminal_font(crate::settings::Settings::get(ui.ctx()).terminal_font);
        let font = theme::mono(font_pt);
        let glyph = ui
            .painter()
            .layout_no_wrap("M".to_owned(), font.clone(), t.text);
        let cell = vec2(glyph.size().x.max(1.0), glyph.size().y.max(1.0));

        let rect = ui.available_rect_before_wrap();
        let id = ui.id().with(("terminal", self.id));
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        ui.allocate_rect(rect, Sense::hover());
        let inner = rect.shrink2(vec2(8.0, 6.0));

        // Size the pty to the panel before mapping pointer coordinates or painting.
        let cols = ((inner.width().max(0.0) / cell.x).floor() as u16).clamp(2, 1000);
        let rows = ((inner.height().max(0.0) / cell.y).floor() as u16).clamp(1, 500);
        if (rows, cols) != self.size {
            self.size = (rows, cols);
            self.selection = None;
            let _ = self.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
            let mut parser = lock(&self.parser);
            parser.set_size(nz(rows), nz(cols));
            self.scroll = parser.screen().scrollback();
        }

        let cell_at = |p: egui::Pos2| -> (u16, u16) {
            let col = ((p.x - inner.left()) / cell.x).floor().max(0.0) as u16;
            let row = ((p.y - inner.top()) / cell.y).floor().max(0.0) as u16;
            (row.min(self.size.0.saturating_sub(1)), col.min(self.size.1))
        };
        // Drag to select; right-click copies the selection (like Windows Terminal).
        if resp.drag_started_by(egui::PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos()
        {
            let c = cell_at(p);
            self.selection = Some((c, c));
            resp.request_focus();
        } else if resp.dragged_by(egui::PointerButton::Primary)
            && let (Some(p), Some((start, _))) = (resp.interact_pointer_pos(), self.selection)
        {
            self.selection = Some((start, cell_at(p)));
        }
        if resp.secondary_clicked() && self.copy_selection(ui.ctx()) {
            resp.request_focus();
        } else if resp.clicked() || resp.secondary_clicked() {
            self.selection = None;
            resp.request_focus();
        } else if resp.has_focus()
            && ui.input(|i| i.pointer.any_pressed())
            && !ui.rect_contains_pointer(rect)
        {
            // Like a text field: a click anywhere else gives the keyboard back to the app.
            resp.surrender_focus();
        }
        let focused = resp.has_focus();
        if focused {
            // Keep Tab, arrows and Escape for the shell.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        // Input.
        if focused {
            let (events, shift_held, ctrl_held) =
                ui.input(|i| (i.events.clone(), i.modifiers.shift, i.modifiers.ctrl));
            let (app_cursor, bracketed) = {
                let parser = lock(&self.parser);
                let screen = parser.screen();
                (screen.application_cursor(), screen.bracketed_paste())
            };
            for event in events {
                match event {
                    Event::Text(text) => self.send(text.as_bytes()),
                    Event::Paste(text) => {
                        let bytes = format_paste(&text, bracketed);
                        self.send(&bytes);
                    }
                    // egui turns Ctrl+C / Ctrl+Shift+C / Ctrl+X into clipboard events:
                    // copy a selection; only plain Ctrl+C on non-macOS sends SIGINT.
                    Event::Copy => {
                        if !self.copy_selection(ui.ctx())
                            && !cfg!(target_os = "macos")
                            && !shift_held
                        {
                            self.send(b"\x03");
                        }
                    }
                    Event::Cut => {
                        if !cfg!(target_os = "macos") && ctrl_held && !shift_held {
                            self.send(b"\x18");
                        }
                    }
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if key == Key::C && (modifiers.ctrl || modifiers.mac_cmd) && modifiers.shift
                        {
                            self.copy_selection(ui.ctx());
                        } else if let Some(bytes) = key_bytes(key, modifiers, app_cursor) {
                            self.send(&bytes);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Wheel scrolls through history.
        if resp.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                self.scroll_rest += dy / cell.y;
                let lines = self.scroll_rest.trunc();
                self.scroll_rest -= lines;
                let mut parser = lock(&self.parser);
                let wanted = (self.scroll as f32 + lines).max(0.0) as usize;
                parser.screen_mut().set_scrollback(wanted);
                self.scroll = parser.screen().scrollback();
            }
        }

        // Paint.
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.field);
        let parser = lock(&self.parser);
        let screen = parser.screen();
        self.scroll = screen.scrollback();
        let (rows, cols) = screen.size();
        let (rows, cols) = (rows.get(), cols.get());
        let palette = Palette::new(&t);
        let selected = self
            .selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        for row in 0..rows {
            let y = inner.top() + row as f32 * cell.y;
            // Selection highlight, under the text.
            if let Some(((r0, c0), (r1, c1))) = selected
                && row >= r0
                && row <= r1
            {
                let from = if row == r0 { c0.min(cols) } else { 0 };
                let to = if row == r1 { c1.min(cols) } else { cols };
                if to > from {
                    painter.rect_filled(
                        Rect::from_min_size(
                            pos2(inner.left() + from as f32 * cell.x, y),
                            vec2((to - from) as f32 * cell.x, cell.y),
                        ),
                        0.0,
                        t.accent.gamma_multiply(0.35),
                    );
                }
            }
            let mut job = LayoutJob::default();
            let mut col = 0;
            while col < cols {
                let Some(c) = screen.cell(row, col) else {
                    break;
                };
                let (fg, bg) = palette.colors(c);
                if let Some(bg) = bg {
                    let x = inner.left() + col as f32 * cell.x;
                    let w = if c.is_wide() { 2.0 } else { 1.0 };
                    painter.rect_filled(
                        Rect::from_min_size(pos2(x, y), vec2(cell.x * w, cell.y)),
                        0.0,
                        bg,
                    );
                }
                if !c.is_wide_continuation() {
                    let text = if c.has_contents() { c.contents() } else { " " };
                    job.append(
                        text,
                        0.0,
                        TextFormat {
                            font_id: font.clone(),
                            color: fg,
                            underline: if c.underline() {
                                Stroke::new(1.0, fg)
                            } else {
                                Stroke::NONE
                            },
                            italics: c.italic(),
                            ..Default::default()
                        },
                    );
                }
                col += 1;
            }
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(pos2(inner.left(), y), galley, t.text);
        }

        // Cursor (only when following the output).
        if self.scroll == 0 && !screen.hide_cursor() && !self.exited.load(Ordering::Relaxed) {
            let (cr, cc) = screen.cursor_position();
            let r = Rect::from_min_size(
                pos2(
                    inner.left() + cc as f32 * cell.x,
                    inner.top() + cr as f32 * cell.y,
                ),
                cell,
            );
            if focused {
                painter.rect_filled(r, 0.0, t.text.gamma_multiply(0.7));
            } else {
                painter.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.0, t.text_faint),
                    egui::StrokeKind::Inside,
                );
            }
        }
        if self.scroll > 0 {
            painter.text(
                rect.right_top() + vec2(-10.0, 8.0),
                egui::Align2::RIGHT_TOP,
                format!("↑ {} lines", self.scroll),
                FontId::proportional(11.0),
                t.text_faint,
            );
        }
    }
}

fn selection_text_clamped(
    screen: &vt100::Screen,
    a: (u16, u16),
    b: (u16, u16),
) -> Option<String> {
    let (rows, cols) = screen.size();
    let max_row = rows.get().saturating_sub(1);
    let max_col = cols.get();
    let a = (a.0.min(max_row), a.1.min(max_col));
    let b = (b.0.min(max_row), b.1.min(max_col));
    let ((r0, c0), (r1, c1)) = if a <= b { (a, b) } else { (b, a) };
    if (r0, c0) >= (r1, c1) {
        return None;
    }
    let text = screen.contents_between(r0, c0, r1, c1);
    Some(
        text.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

impl Terminal {
    /// Copies the selected text; false when nothing is selected.
    fn copy_selection(&mut self, ctx: &egui::Context) -> bool {
        let Some((a, b)) = self.selection else {
            return false;
        };
        let parser = lock(&self.parser);
        let copied = selection_text_clamped(parser.screen(), a, b);
        drop(parser);
        let Some(text) = copied else {
            self.selection = None;
            return false;
        };
        ctx.copy_text(text);
        self.selection = None;
        true
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

#[cfg(windows)]
fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(exe).is_file()))
        .unwrap_or(false)
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A poisoned lock still holds usable data.
fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// What a key sends to the shell (xterm sequences).
fn key_bytes(key: Key, mods: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let csi_or_ss3 = |c: char| {
        if app_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    if mods.ctrl && !mods.alt {
        // Ctrl+letter → control character (Ctrl+C and Ctrl+X arrive as Copy / Cut).
        let name = key.name();
        if name.len() == 1
            && let Some(c) = name.chars().next()
            && c.is_ascii_alphabetic()
        {
            return Some(vec![(c.to_ascii_uppercase() as u8) - b'A' + 1]);
        }
    }
    let s: String = match key {
        Key::Enter => "\r".into(),
        Key::Tab if mods.shift => "\x1b[Z".into(),
        Key::Tab => "\t".into(),
        Key::Backspace => "\x7f".into(),
        Key::Escape => "\x1b".into(),
        Key::ArrowUp => csi_or_ss3('A'),
        Key::ArrowDown => csi_or_ss3('B'),
        Key::ArrowRight => csi_or_ss3('C'),
        Key::ArrowLeft => csi_or_ss3('D'),
        Key::Home => csi_or_ss3('H'),
        Key::End => csi_or_ss3('F'),
        Key::PageUp => "\x1b[5~".into(),
        Key::PageDown => "\x1b[6~".into(),
        Key::Insert => "\x1b[2~".into(),
        Key::Delete => "\x1b[3~".into(),
        Key::F1 => "\x1bOP".into(),
        Key::F2 => "\x1bOQ".into(),
        Key::F3 => "\x1bOR".into(),
        Key::F4 => "\x1bOS".into(),
        _ => return None,
    };
    Some(s.into_bytes())
}

/// Terminal colours tuned to the app theme.
struct Palette {
    fg: Color32,
    default_bg: Color32,
    ansi: [Color32; 16],
}

impl Palette {
    fn new(t: &Tokens) -> Self {
        let ansi = if t.dark() {
            [
                Color32::from_rgb(40, 40, 40),
                Color32::from_rgb(236, 91, 98),
                Color32::from_rgb(92, 196, 122),
                Color32::from_rgb(232, 176, 70),
                Color32::from_rgb(64, 156, 255),
                Color32::from_rgb(200, 110, 230),
                Color32::from_rgb(60, 205, 210),
                Color32::from_rgb(200, 200, 200),
                Color32::from_rgb(110, 110, 110),
                Color32::from_rgb(255, 120, 125),
                Color32::from_rgb(130, 225, 150),
                Color32::from_rgb(250, 205, 100),
                Color32::from_rgb(110, 180, 255),
                Color32::from_rgb(220, 150, 245),
                Color32::from_rgb(110, 225, 230),
                Color32::from_rgb(245, 245, 245),
            ]
        } else {
            [
                Color32::from_rgb(20, 20, 20),
                Color32::from_rgb(200, 40, 50),
                Color32::from_rgb(30, 140, 70),
                Color32::from_rgb(170, 110, 0),
                Color32::from_rgb(20, 100, 210),
                Color32::from_rgb(150, 60, 190),
                Color32::from_rgb(0, 140, 150),
                Color32::from_rgb(110, 110, 110),
                Color32::from_rgb(90, 90, 90),
                Color32::from_rgb(230, 60, 70),
                Color32::from_rgb(40, 170, 90),
                Color32::from_rgb(200, 140, 0),
                Color32::from_rgb(40, 120, 240),
                Color32::from_rgb(180, 80, 220),
                Color32::from_rgb(0, 170, 180),
                Color32::from_rgb(40, 40, 40),
            ]
        };
        Self {
            fg: t.text,
            default_bg: t.field,
            ansi,
        }
    }

    fn color(&self, c: vt100::Color) -> Option<Color32> {
        match c {
            vt100::Color::Default => None,
            vt100::Color::Idx(i) if i < 16 => self.ansi.get(i as usize).copied(),
            vt100::Color::Idx(i) if i < 232 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Some(Color32::from_rgb(
                    level(i / 36),
                    level((i / 6) % 6),
                    level(i % 6),
                ))
            }
            vt100::Color::Idx(i) => {
                let v = 8 + (i - 232).saturating_mul(10);
                Some(Color32::from_gray(v))
            }
            vt100::Color::Rgb(r, g, b) => Some(Color32::from_rgb(r, g, b)),
        }
    }

    /// (foreground, background-if-not-default) of a cell.
    fn colors(&self, cell: &vt100::Cell) -> (Color32, Option<Color32>) {
        let mut fg = self.color(cell.fgcolor()).unwrap_or(self.fg);
        let mut bg = self.color(cell.bgcolor());
        if cell.bold()
            && let vt100::Color::Idx(i) = cell.fgcolor()
            && i < 8
        {
            fg = self.ansi.get(i as usize + 8).copied().unwrap_or(fg);
        }
        if cell.dim() {
            fg = fg.gamma_multiply(0.6);
        }
        if cell.inverse() {
            let old_fg = fg;
            fg = bg.unwrap_or(self.default_bg);
            bg = Some(old_fg);
        }
        (fg, bg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_to_bytes() {
        assert_eq!(
            key_bytes(Key::Enter, Modifiers::NONE, false),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            key_bytes(Key::ArrowUp, Modifiers::NONE, false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_bytes(Key::ArrowUp, Modifiers::NONE, true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(key_bytes(Key::D, Modifiers::CTRL, false), Some(vec![4]));
        assert_eq!(key_bytes(Key::A, Modifiers::NONE, false), None);
    }

    #[test]
    fn indexed_colours() {
        let p = Palette::new(&Tokens::for_kind(crate::theme::ThemeKind::Dark));
        assert_eq!(
            p.color(vt100::Color::Idx(16)),
            Some(Color32::from_rgb(0, 0, 0))
        );
        assert_eq!(
            p.color(vt100::Color::Idx(231)),
            Some(Color32::from_rgb(255, 255, 255))
        );
        assert_eq!(
            p.color(vt100::Color::Idx(255)),
            Some(Color32::from_gray(238))
        );
    }

    #[test]
    fn selection_copy_is_safe_after_terminal_shrinks() {
        let mut parser = vt100::Parser::new(nz(20), nz(80), 0);
        parser.process(b"hello world");
        parser.set_size(nz(4), nz(10));
        // Previously out-of-bounds coordinates after shrink clamp safely instead of panicking.
        assert_eq!(selection_text_clamped(parser.screen(), (18, 70), (19, 75)), None);
        assert_eq!(
            selection_text_clamped(parser.screen(), (0, 0), (0, 50)).as_deref(),
            Some("hello worl")
        );
    }

    #[test]
    fn bracketed_paste_strips_escape_sequences_and_sanitizes_font() {
        let bytes = format_paste("echo ok\x1b[201~; rm -rf /\n", true);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("\x1b[200~"));
        assert!(text.ends_with("\x1b[201~"));
        let middle = &text["\x1b[200~".len()..text.len() - "\x1b[201~".len()];
        assert!(!middle.contains('\x1b'));
        assert_eq!(sanitized_terminal_font(f32::NAN), 12.5);
        assert_eq!(sanitized_terminal_font(2.0), 9.0);
        assert_eq!(sanitized_terminal_font(99.0), 20.0);
    }
}
