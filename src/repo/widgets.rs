//! Small painted widgets shared by the repository panes.

use egui::{Align2, Color32, CornerRadius, Rect, Response, Sense, Ui, pos2, vec2};

use crate::icons;
use crate::theme::{self, Tokens};

pub const ROW_H: f32 = 24.0;

pub fn toolbar_separator(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 20.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, t.separator);
    ui.add_space(4.0);
}

/// A rounded label with a tinted background.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let font = theme::medium(11.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let size = vec2(galley.size().x + 12.0, 18.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(9), color.gamma_multiply(0.18));
    ui.painter().galley(
        pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    resp
}

/// The strip at the top of a side panel, like PhotoCraft's panel tab strips: a tab with the
/// title, and room on the right for buttons.
pub fn panel_header(ui: &mut Ui, title: &str, add_buttons: impl FnOnce(&mut Ui)) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, t.tab_strip);
    let font = theme::semibold(11.5);
    let galley = painter.layout_no_wrap(title.to_owned(), font, t.text);
    let tab = Rect::from_min_size(
        pos2(rect.left() + 4.0, rect.top() + 4.0),
        vec2(galley.size().x + 24.0, rect.height() - 4.0),
    );
    painter.rect_filled(
        tab,
        CornerRadius {
            nw: t.radius as u8,
            ne: t.radius as u8,
            sw: 0,
            se: 0,
        },
        t.card,
    );
    painter.galley(
        pos2(tab.left() + 12.0, tab.center().y - galley.size().y / 2.0),
        galley,
        t.text,
    );
    let buttons = Rect::from_min_max(
        pos2(tab.right() + 4.0, rect.top()),
        rect.max - vec2(6.0, 0.0),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(buttons)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    add_buttons(&mut child);
}

/// A collapsible section header (`▸ ICON TITLE  count`). Returns whether it is open.
pub fn section(
    ui: &mut Ui,
    id: &str,
    icon: &str,
    title: &str,
    count: impl std::fmt::Display,
    default_open: bool,
) -> bool {
    section_with_button(ui, id, icon, title, count, default_open, None).0
}

/// [`section`] with an icon button on the header, left of the count: `(icon, tooltip)`.
/// Returns (open, button clicked).
pub fn section_with_button(
    ui: &mut Ui,
    id: &str,
    icon: &str,
    title: &str,
    count: impl std::fmt::Display,
    default_open: bool,
    button: Option<(&str, &str)>,
) -> (bool, bool) {
    let t = Tokens::get(ui.ctx());
    let id = ui.id().with(("section", id));
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(default_open);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    if resp.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 0.0, t.hover);
    }
    let y = rect.center().y;
    let chevron = if open {
        "chevron-down"
    } else {
        "chevron-right"
    };
    icons::paint(
        ui,
        Rect::from_center_size(pos2(rect.left() + 14.0, y), vec2(14.0, 14.0)),
        chevron,
        12.0,
        t.text_faint,
    );
    icons::paint(
        ui,
        Rect::from_center_size(pos2(rect.left() + 32.0, y), vec2(14.0, 14.0)),
        icon,
        14.0,
        t.icon,
    );
    painter.text(
        pos2(rect.left() + 46.0, y),
        Align2::LEFT_CENTER,
        title,
        theme::semibold(11.5),
        t.text_dim,
    );
    let count = painter.text(
        pos2(rect.right() - 10.0, y),
        Align2::RIGHT_CENTER,
        count.to_string(),
        theme::medium(11.0),
        t.text_faint,
    );
    let mut clicked = false;
    if let Some((icon, tooltip)) = button {
        // Left of the count, however long it reads (`120/870` while branches are hidden).
        let x = (count.left() - 18.0).min(rect.right() - 36.0);
        let r = Rect::from_center_size(pos2(x, y), vec2(20.0, 20.0));
        let b = ui
            .interact(r, id.with("button"), Sense::click())
            .on_hover_text(tooltip);
        if b.hovered() {
            ui.painter().rect_filled(r, t.radius_sm, t.pressed);
        }
        icons::paint(ui, r, icon, 13.0, if b.hovered() { t.text } else { t.icon });
        clicked = b.clicked();
    }
    (open, clicked)
}

pub fn status_color(t: &Tokens, code: char) -> Color32 {
    match code {
        'A' | '?' => t.success,
        'D' => t.danger,
        'R' | 'C' => t.accent,
        'U' => t.danger,
        _ => t.warning,
    }
}

/// What a file row shows on its right edge.
pub enum RowExtra<'a> {
    /// `+added −removed` (`None` for binary files).
    Stats(Option<u32>, Option<u32>),
    /// Icon buttons shown while the row is hovered: `(icon, tooltip)`.
    Buttons(&'a [(&'a str, &'a str)]),
}

pub struct FileRowResponse {
    pub row: Response,
    /// Index of the clicked hover button.
    pub button: Option<usize>,
}

/// One changed file: status letter, file name, its folder dimmed, and `extra` on the right.
pub fn file_row(
    ui: &mut Ui,
    code: char,
    path: &str,
    old_path: Option<&str>,
    selected: bool,
    extra: RowExtra<'_>,
) -> FileRowResponse {
    file_row_in(ui, code, path, old_path, None, selected, extra)
}

/// [`file_row`] in a tree of folders, `indent` in: just the file's name, its folder being the
/// rows above.
pub fn file_row_in(
    ui: &mut Ui,
    code: char,
    path: &str,
    old_path: Option<&str>,
    indent: Option<f32>,
    selected: bool,
    extra: RowExtra<'_>,
) -> FileRowResponse {
    let shift = indent.unwrap_or(0.0);
    let t = Tokens::get(ui.ctx());
    let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 0.0, t.row_selected);
    } else if hovered {
        painter.rect_filled(rect, 0.0, t.hover);
    }
    let y = rect.center().y;

    // Status letter in a small rounded square.
    let color = status_color(&t, code);
    let badge = Rect::from_center_size(pos2(rect.left() + 18.0 + shift, y), vec2(16.0, 16.0));
    painter.rect_filled(badge, CornerRadius::same(3), color.gamma_multiply(0.22));
    let letter = if code == '?' { 'A' } else { code };
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        letter.to_string(),
        theme::semibold(10.5),
        color,
    );

    // Right edge first, so the name is clipped before it.
    let mut right = rect.right() - 8.0;
    let mut button = None;
    match extra {
        RowExtra::Stats(added, removed) => {
            let small = theme::mono(11.0);
            match (added, removed) {
                (Some(a), Some(d)) => {
                    let r = painter.text(
                        pos2(right, y),
                        Align2::RIGHT_CENTER,
                        format!("−{d}"),
                        small.clone(),
                        t.danger,
                    );
                    right = r.left() - 6.0;
                    let r = painter.text(
                        pos2(right, y),
                        Align2::RIGHT_CENTER,
                        format!("+{a}"),
                        small,
                        t.success,
                    );
                    right = r.left() - 8.0;
                }
                _ => {
                    let r = painter.text(
                        pos2(right, y),
                        Align2::RIGHT_CENTER,
                        "bin",
                        small,
                        t.text_faint,
                    );
                    right = r.left() - 8.0;
                }
            }
        }
        RowExtra::Buttons(buttons) => {
            if hovered || selected {
                for (i, (icon, tooltip)) in buttons.iter().enumerate().rev() {
                    let r = Rect::from_center_size(pos2(right - 10.0, y), vec2(20.0, 20.0));
                    let resp = ui
                        .interact(r, row.id.with(("btn", i)), Sense::click())
                        .on_hover_text(*tooltip);
                    if resp.hovered() {
                        painter.rect_filled(r, t.radius_sm, t.pressed);
                    }
                    icons::paint(
                        ui,
                        r,
                        icon,
                        14.0,
                        if resp.hovered() { t.text } else { t.icon },
                    );
                    if resp.clicked() {
                        button = Some(i);
                    }
                    right = r.left() - 2.0;
                }
            }
        }
    }

    let (dir, name) = match path.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", path),
    };
    let text_clip = Rect::from_min_max(
        pos2(rect.left() + 32.0 + shift, rect.top()),
        pos2(right, rect.bottom()),
    );
    let tp = painter.with_clip_rect(text_clip);
    let name_rect = tp.text(
        pos2(text_clip.left(), y),
        Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(12.5),
        t.text,
    );
    let mut faint = String::new();
    if let Some(old) = old_path {
        faint.push_str("← ");
        faint.push_str(old);
        faint.push_str("  ");
    }
    if indent.is_none() {
        faint.push_str(dir);
    }
    if !faint.is_empty() {
        tp.text(
            pos2(name_rect.right() + 8.0, y),
            Align2::LEFT_CENTER,
            faint,
            egui::FontId::proportional(11.5),
            t.text_faint,
        );
    }
    // The full path only adds something when the row shows it cut short.
    let row = match old_path {
        Some(old) => row.on_hover_text(format!("{old} → {path}")),
        None if !dir.is_empty() => row.on_hover_text(path),
        None => row,
    };
    FileRowResponse { row, button }
}

/// A primary (accent-filled) button.
pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool, width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let fill = if enabled { t.accent } else { t.hover };
    let text_color = if enabled { t.accent_text } else { t.text_faint };
    ui.add_enabled(
        enabled,
        egui::Button::new(
            egui::RichText::new(text)
                .color(text_color)
                .font(theme::medium(12.5)),
        )
        .fill(fill)
        .min_size(vec2(width, 30.0)),
    )
}

/// A round avatar: the author's picture when one is loaded (see `avatars`), their initials in
/// a colour of their own until then.
pub fn avatar(ui: &Ui, center: egui::Pos2, radius: f32, name: &str, email: &str) {
    let rect = Rect::from_center_size(center, vec2(radius * 2.0, radius * 2.0));
    if crate::avatars::paint(ui, rect, email) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let painter = ui.painter();
    painter.circle_filled(center, radius, t.lane(name_hash(name)));
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initials(name),
        theme::semibold(radius * 0.8),
        Color32::WHITE,
    );
}

fn initials(name: &str) -> String {
    let mut parts = name.split_whitespace().filter_map(|w| w.chars().next());
    let first = parts.next().unwrap_or('?');
    let last = parts.next_back();
    let mut s: String = first.to_uppercase().collect();
    if let Some(l) = last {
        s.extend(l.to_uppercase());
    }
    s
}

fn name_hash(name: &str) -> usize {
    name.bytes()
        .fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize))
}

/// The accent line under an active toolbar toggle.
pub fn active_underline(ui: &Ui, button: Rect, color: Color32) {
    ui.painter().line_segment(
        [
            button.left_bottom() + vec2(6.0, -1.0),
            button.right_bottom() + vec2(-6.0, -1.0),
        ],
        egui::Stroke::new(2.0, color),
    );
}

/// The terminal panel's strip: one tab per shell, `+` with a shell menu, and close.
pub fn terminal_tabs(
    ui: &mut Ui,
    terminals: &[crate::terminal::Terminal],
    active: usize,
    acts: &mut Vec<super::Action>,
) {
    let t = Tokens::get(ui.ctx());
    let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().rect_filled(strip, 0.0, t.tab_strip);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(strip.shrink2(vec2(4.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Max)),
    );
    let ui = &mut child;
    ui.spacing_mut().item_spacing.x = 2.0;
    for (i, term) in terminals.iter().enumerate() {
        let selected = i == active;
        let label = format!("{} {}", i + 1, term.shell);
        let font = if selected {
            theme::semibold(11.5)
        } else {
            theme::medium(11.5)
        };
        let galley =
            ui.painter()
                .layout_no_wrap(label, font, if selected { t.text } else { t.text_dim });
        let (rect, resp) =
            ui.allocate_exact_size(vec2(galley.size().x + 44.0, 26.0), Sense::click());
        let shape = CornerRadius {
            nw: t.radius as u8,
            ne: t.radius as u8,
            sw: 0,
            se: 0,
        };
        if selected {
            ui.painter().rect_filled(rect, shape, t.card);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, shape, t.hover);
        }
        icons::paint(
            ui,
            Rect::from_center_size(pos2(rect.left() + 13.0, rect.center().y), vec2(14.0, 14.0)),
            "square-terminal",
            12.0,
            t.text_faint,
        );
        ui.painter().galley(
            pos2(rect.left() + 24.0, rect.center().y - galley.size().y / 2.0),
            galley,
            t.text,
        );
        let x_rect =
            Rect::from_center_size(pos2(rect.right() - 11.0, rect.center().y), vec2(16.0, 16.0));
        let x = ui
            .interact(x_rect, resp.id.with("close"), Sense::click())
            .on_hover_text("Close (ends the shell)");
        if x.hovered() {
            ui.painter().rect_filled(x_rect, t.radius_sm, t.pressed);
        }
        icons::paint(ui, x_rect, "x", 11.0, t.text_faint);
        if x.clicked() {
            acts.push(super::Action::CloseTerminal(i));
        } else if resp.clicked() {
            acts.push(super::Action::SelectTerminal(i));
        }
    }
    ui.add_space(4.0);
    let plus = icons::small_button(ui, "plus", "New terminal");
    if plus.clicked() {
        acts.push(super::Action::NewTerminal {
            shell: None,
            cwd: None,
        });
    }
    egui::Popup::context_menu(&plus)
        .style(crate::theme::menu_style)
        .show(|ui| {
            for shell in crate::terminal::available_shells() {
                if ui.button(&shell.label).clicked() {
                    acts.push(super::Action::NewTerminal {
                        shell: Some(shell),
                        cwd: None,
                    });
                }
            }
        });
    let more = icons::small_button(ui, "chevron-down", "New terminal with…");
    egui::Popup::menu(&more)
        .style(crate::theme::menu_style)
        .show(|ui| {
            for shell in crate::terminal::available_shells() {
                if ui.button(&shell.label).clicked() {
                    acts.push(super::Action::NewTerminal {
                        shell: Some(shell),
                        cwd: None,
                    });
                }
            }
        });
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(4.0);
        if icons::small_button(ui, "x", "Hide the panel (shells keep running)").clicked() {
            acts.push(super::Action::ToggleTerminal);
        }
    });
}

/// Right-click menu for a file of the working tree or a commit: open it, its folder, copy path.
/// Right-click menu of a file. `rev`: the commit it is shown in (history and blame start there);
/// `ignorable`: a working-tree file that can be added to `.gitignore`.
pub fn file_menu(
    resp: &Response,
    path: &str,
    rev: Option<&str>,
    ignorable: bool,
    acts: &mut Vec<super::Action>,
) {
    file_menu_with(resp, path, rev, ignorable, acts, |_, _| {});
}

/// [`file_menu`] with more entries at the end, from `extra`.
pub fn file_menu_with(
    resp: &Response,
    path: &str,
    rev: Option<&str>,
    ignorable: bool,
    acts: &mut Vec<super::Action>,
    extra: impl FnOnce(&mut Ui, &mut Vec<super::Action>),
) {
    use super::Action;
    use super::file_view::FileTab;
    egui::Popup::context_menu(resp)
        .style(crate::theme::menu_style)
        .show(|ui| {
            let rel = std::path::PathBuf::from(path);
            if ui.button("Open file").clicked() {
                acts.push(Action::OpenPath(rel.clone()));
            }
            if ui.button("Open in editor").clicked() {
                acts.push(Action::OpenInEditor(rel.clone()));
            }
            if ui.button(super::reveal_label()).clicked() {
                acts.push(Action::OpenPath(
                    rel.parent().map(|p| p.to_path_buf()).unwrap_or_default(),
                ));
            }
            ui.separator();
            if ui.button("File history").clicked() {
                acts.push(Action::FileView {
                    path: path.to_owned(),
                    rev: rev.map(str::to_owned),
                    tab: FileTab::History,
                });
            }
            if ui.button("Blame").clicked() {
                acts.push(Action::FileView {
                    path: path.to_owned(),
                    rev: rev.map(str::to_owned),
                    tab: FileTab::Blame,
                });
            }
            if ignorable {
                ui.menu_button("Ignore", |ui| {
                    for (label, pattern, untrack) in ignore_patterns(path) {
                        if ui.button(label).clicked() {
                            acts.push(Action::Ignore {
                                pattern,
                                path: untrack,
                            });
                        }
                    }
                });
            }
            ui.separator();
            if ui.button("Copy path").clicked() {
                ui.ctx().copy_text(path.to_owned());
            }
            extra(ui, acts);
        });
}

/// The ways to ignore `path`: (menu label, `.gitignore` line, what to stop tracking).
fn ignore_patterns(path: &str) -> Vec<(String, String, String)> {
    let p = std::path::Path::new(path);
    let mut out = vec![("This file".to_owned(), format!("/{path}"), path.to_owned())];
    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
        out.push((
            format!("All .{ext} files"),
            format!("*.{ext}"),
            path.to_owned(),
        ));
    }
    if let Some(dir) = path.rsplit_once('/').map(|(d, _)| d) {
        out.push((format!("Folder {dir}/"), format!("/{dir}/"), dir.to_owned()));
    }
    out
}

/// How a slider's value reads in the box beside it.
#[derive(Clone, Copy)]
pub enum SliderUnit {
    /// `1.25` reads `125 %`.
    Percent,
    /// The number and a unit after it, like ` px`.
    Suffix(&'static str),
}

/// A slider: a rail filled up to a round knob in the theme's accent, and the value beside it
/// to drag or type. Returns the slider's and the box's response together.
pub fn slider(
    ui: &mut Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f32,
    unit: SliderUnit,
) -> Response {
    const KNOB: f32 = 7.0;
    let t = Tokens::get(ui.ctx());
    let (min, max) = (*range.start(), *range.end());
    ui.horizontal(|ui| {
        let size = vec2(ui.spacing().slider_width, ui.spacing().interact_size.y);
        let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let resp_hover = resp.clone();
        let track = rect.x_range().shrink(KNOB);
        if let Some(p) = resp.interact_pointer_pos() {
            let f = ((p.x - track.min) / track.span()).clamp(0.0, 1.0);
            let mut v = min + f * (max - min);
            if step > 0.0 {
                v = min + ((v - min) / step).round() * step;
            }
            let v = v.clamp(min, max);
            if (v - *value).abs() > f32::EPSILON {
                *value = v;
                resp.mark_changed();
            }
        }
        let f = if max > min {
            ((*value - min) / (max - min)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let y = rect.center().y;
        let x = track.min + f * track.span();
        let p = ui.painter();
        let rail = Rect::from_min_max(pos2(rect.left(), y - 2.0), pos2(rect.right(), y + 2.0));
        p.rect_filled(rail, 2.0, t.field_border);
        p.rect_filled(rail.with_max_x(x), 2.0, t.accent.gamma_multiply(0.6));
        let lifted = resp_hover.hovered() || resp_hover.dragged();
        p.circle_filled(pos2(x, y), if lifted { KNOB + 1.0 } else { KNOB }, t.accent);
        let resp = resp.on_hover_cursor(egui::CursorIcon::Grab);

        let mut drag = egui::DragValue::new(value)
            .range(min..=max)
            .speed(step.max(0.01));
        drag = match unit {
            SliderUnit::Percent => drag
                .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                .custom_parser(|s| {
                    let n: f64 = s.trim().trim_end_matches('%').trim().parse().ok()?;
                    Some(n / 100.0)
                }),
            SliderUnit::Suffix(suffix) => drag.suffix(suffix),
        };
        resp | ui.add(drag)
    })
    .inner
}

/// The usual right-click menu of a text field (`resp`, from adding a `TextEdit` over `text`):
/// cut, copy, paste, select all and clear. egui's text fields have none of their own.
/// Returns whether an entry changed the text here (a paste arrives as typing, next frame).
pub fn text_edit_menu(resp: &Response, text: &mut String) -> bool {
    edit_menu(resp, text, false)
}

/// The right-click menu of a password field: paste, select all and clear, but nothing that puts
/// the hidden text on the clipboard.
pub fn secret_edit_menu(resp: &Response, text: &mut String) -> bool {
    edit_menu(resp, text, true)
}

fn edit_menu(resp: &Response, text: &mut String, secret: bool) -> bool {
    use egui::text::{CCursor, CCursorRange};

    let ctx = resp.ctx.clone();
    let id = resp.id;
    // A text field moves its cursor on any press, the right button's too, which would lose the
    // selection the menu is opened for: kept from the frame before and put back.
    let kept = id.with("selection-before-menu");
    let right_press = resp.hovered()
        && ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
    if let Some(mut state) = egui::TextEdit::load_state(&ctx, id) {
        if right_press {
            if let Some(range) = ctx.data(|d| d.get_temp::<Option<CCursorRange>>(kept)).flatten() {
                state.cursor.set_char_range(Some(range));
                egui::TextEdit::store_state(&ctx, id, state);
            }
        } else {
            let range = state.cursor.char_range();
            ctx.data_mut(|d| d.insert_temp(kept, range));
        }
    }
    // The selection as it was when the menu opened: clicking an entry may take the focus away.
    let selection = || {
        egui::TextEdit::load_state(&ctx, id)
            .and_then(|s| s.cursor.char_range())
            .filter(|r| !r.is_empty())
            .map(|r| {
                let r = r.as_sorted_char_range();
                (usize::from(r.start), usize::from(r.end))
            })
    };
    let byte = |s: &str, chars: usize| s.char_indices().nth(chars).map_or(s.len(), |(b, _)| b);
    let set_cursor = |range: CCursorRange| {
        let mut state = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(range));
        egui::TextEdit::store_state(&ctx, id, state);
        ctx.memory_mut(|m| m.request_focus(id));
    };
    let mut changed = false;
    egui::Popup::context_menu(resp)
        .style(crate::theme::menu_style)
        .show(|ui| {
            let selected = selection().filter(|_| !secret);
            if !secret
                && ui
                    .add_enabled(selected.is_some(), egui::Button::new("Cut"))
                    .clicked()
                && let Some((from, to)) = selected
            {
                let (a, b) = (byte(text, from), byte(text, to));
                ui.ctx().copy_text(text[a..b].to_owned());
                text.replace_range(a..b, "");
                changed = true;
                set_cursor(CCursorRange::one(CCursor::new(from)));
            }
            if !secret
                && ui
                    .add_enabled(selected.is_some(), egui::Button::new("Copy"))
                    .clicked()
                && let Some((from, to)) = selected
            {
                ui.ctx()
                    .copy_text(text[byte(text, from)..byte(text, to)].to_owned());
            }
            if ui.button("Paste").clicked() {
                // The clipboard is read by the window, which hands it to the focused field.
                ctx.memory_mut(|m| m.request_focus(id));
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            ui.separator();
            if ui
                .add_enabled(!text.is_empty(), egui::Button::new("Select all"))
                .clicked()
            {
                set_cursor(CCursorRange::two(
                    CCursor::new(0),
                    CCursor::new(text.chars().count()),
                ));
            }
            if ui
                .add_enabled(!text.is_empty(), egui::Button::new("Clear"))
                .clicked()
            {
                text.clear();
                changed = true;
                set_cursor(CCursorRange::one(CCursor::new(0)));
            }
        });
    changed
}

/// A whole commit message split like git does: the first non-blank line, and what follows it
/// without the blank lines in between (line endings normalised, trailing space dropped).
pub fn split_message(text: &str) -> (String, String) {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
    let summary = lines.next().unwrap_or_default().trim().to_owned();
    let body = lines
        .skip_while(|l| l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned();
    (summary, body)
}

/// Call before adding the one-line field `id` that takes a password or token: a paste drops the
/// line breaks around it (copied from a terminal or a password manager, they would otherwise turn
/// into spaces the host refuses). Spaces typed or pasted inside are kept.
pub fn trim_pasted_secret(ui: &Ui, id: egui::Id) {
    if !ui.memory(|m| m.has_focus(id)) {
        return;
    }
    ui.ctx().input_mut(|i| {
        for event in &mut i.events {
            if let egui::Event::Paste(text) = event {
                let trimmed = text.trim_matches(['\r', '\n']);
                if trimmed.len() != text.len() {
                    *text = trimmed.to_owned();
                }
            }
        }
    });
}

/// Call before adding the summary (`summary_id`) and description (`body_id`) fields of a commit
/// message. A pasted message of several lines goes where it belongs: pasted into the summary,
/// its first line stays there and the rest goes in front of the description; pasted into the
/// description while both are empty, its first line becomes the summary.
pub fn route_message_paste(
    ui: &Ui,
    summary_id: egui::Id,
    body_id: egui::Id,
    summary: &mut String,
    body: &mut String,
) {
    let into_summary = ui.memory(|m| m.has_focus(summary_id));
    let into_body = !into_summary
        && ui.memory(|m| m.has_focus(body_id))
        && summary.trim().is_empty()
        && body.trim().is_empty();
    if !into_summary && !into_body {
        return;
    }
    ui.ctx().input_mut(|i| {
        for event in &mut i.events {
            let egui::Event::Paste(text) = event else {
                continue;
            };
            if !text.contains(['\n', '\r']) {
                continue;
            }
            let (first, rest) = split_message(text);
            if into_summary {
                // The field takes the first line where the cursor is, as typed.
                *text = first;
                if !rest.is_empty() {
                    *body = match body.trim() {
                        "" => rest,
                        old => format!("{rest}\n\n{old}"),
                    };
                }
            } else if !rest.is_empty() {
                *summary = first;
                *text = rest;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    fn field() -> Harness<'static, String> {
        let mut harness = Harness::new_ui_state(
            |ui, text: &mut String| {
                let resp = ui.add(egui::TextEdit::singleline(text));
                super::text_edit_menu(&resp, text);
            },
            "feature/login".to_owned(),
        );
        harness.run();
        harness
    }

    fn menu(harness: &mut Harness<'_, String>, entry: &str) -> Vec<egui::OutputCommand> {
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .click_secondary();
        harness.run();
        harness.get_by_label(entry).click();
        harness.step();
        let commands = harness.output().platform_output.commands.clone();
        harness.run();
        commands
    }

    #[test]
    fn the_text_field_menu_selects_cuts_and_clears() {
        let mut harness = field();
        // Nothing selected yet: copy and cut wait for a selection.
        let copied = menu(&mut harness, "Copy");
        assert!(
            !copied
                .iter()
                .any(|c| matches!(c, egui::OutputCommand::CopyText(_))),
            "{copied:?}"
        );
        harness.key_press(egui::Key::Escape);
        harness.run();

        menu(&mut harness, "Select all");
        let cut = menu(&mut harness, "Cut");
        assert!(
            cut.contains(&egui::OutputCommand::CopyText("feature/login".to_owned())),
            "{cut:?}"
        );
        assert_eq!(harness.state(), "");

        *harness.state_mut() = "main".to_owned();
        harness.run();
        menu(&mut harness, "Clear");
        assert_eq!(harness.state(), "");
    }

    #[test]
    fn the_text_field_menu_asks_the_window_to_paste() {
        let mut harness = field();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .click_secondary();
        harness.run();
        harness.get_by_label("Paste").click();
        harness.step();
        let asked = harness
            .output()
            .viewport_output
            .values()
            .flat_map(|v| v.commands.iter())
            .any(|c| *c == egui::ViewportCommand::RequestPaste);
        assert!(asked, "the window was not asked to paste");
        harness.run();
        // What the window answers goes into the field, which has the focus again.
        assert!(
            harness
                .get_by_role(egui::accesskit::Role::TextInput)
                .is_focused()
        );
        harness.event(egui::Event::Paste("-v2".to_owned()));
        harness.run();
        assert!(harness.state().contains("-v2"), "{}", harness.state());
    }

    /// A focused password field with the pasted-secret trimming and its menu.
    fn password_field() -> Harness<'static, String> {
        let mut harness = Harness::new_ui_state(
            |ui, text: &mut String| {
                let id = egui::Id::new("password");
                super::trim_pasted_secret(ui, id);
                let resp = ui.add(egui::TextEdit::singleline(text).id(id).password(true));
                super::secret_edit_menu(&resp, text);
                if !resp.has_focus() && !egui::Popup::is_any_open(ui.ctx()) {
                    resp.request_focus();
                }
            },
            String::new(),
        );
        harness.run();
        harness
    }

    #[test]
    fn the_text_field_menu_works_inside_a_modal_dialog() {
        // The dialogs are egui modals, which block layers under them: the menu must stay usable.
        let mut harness = Harness::new_ui_state(
            |ui, text: &mut String| {
                egui::Modal::new(egui::Id::new("dialog")).show(ui.ctx(), |ui| {
                    let resp = ui.add(egui::TextEdit::singleline(text));
                    super::text_edit_menu(&resp, text);
                });
            },
            "origin".to_owned(),
        );
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .click_secondary();
        harness.run();
        harness.get_by_label("Clear").click();
        harness.run();
        assert_eq!(harness.state(), "");
    }

    #[test]
    fn a_pasted_token_loses_its_line_breaks_but_keeps_spaces() {
        let mut harness = password_field();
        harness.event(egui::Event::Paste("ghp_abc123\r\n".to_owned()));
        harness.run();
        assert_eq!(harness.state(), "ghp_abc123");

        let mut harness = password_field();
        harness.event(egui::Event::Paste("\n pass phrase \n".to_owned()));
        harness.run();
        assert_eq!(harness.state(), " pass phrase ");
    }

    #[test]
    fn the_password_menu_never_copies() {
        let mut harness = password_field();
        harness.state_mut().push_str("secret");
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::PasswordInput)
            .click_secondary();
        harness.run();
        assert!(harness.query_by_label("Copy").is_none());
        assert!(harness.query_by_label("Cut").is_none());
        harness.get_by_label("Clear").click();
        harness.run();
        assert_eq!(harness.state(), "");
    }

    #[test]
    fn a_message_splits_into_summary_and_description() {
        let split = |s: &str| super::split_message(s);
        assert_eq!(split("Fix it"), ("Fix it".to_owned(), String::new()));
        assert_eq!(split("Fix it\n"), ("Fix it".to_owned(), String::new()));
        assert_eq!(
            split("\r\n  Fix it  \r\n\r\nBecause.\r\n\r\n  - one\r\n- two\r\n\r\n"),
            ("Fix it".to_owned(), "Because.\n\n  - one\n- two".to_owned())
        );
        assert_eq!(
            split("Fix it\nno blank line"),
            ("Fix it".to_owned(), "no blank line".to_owned())
        );
    }

    /// Two message fields (summary, description) with the paste routing, and which has focus.
    fn message_fields(focus_body: bool) -> Harness<'static, (String, String)> {
        let mut harness = Harness::new_ui_state(
            move |ui, (summary, body): &mut (String, String)| {
                let (sid, bid) = (egui::Id::new("summary"), egui::Id::new("body"));
                super::route_message_paste(ui, sid, bid, summary, body);
                let s = ui.add(egui::TextEdit::singleline(summary).id(sid));
                let b = ui.add(egui::TextEdit::multiline(body).id(bid));
                if !ui.memory(|m| m.has_focus(sid) || m.has_focus(bid)) {
                    if focus_body { b } else { s }.request_focus();
                }
            },
            (String::new(), String::new()),
        );
        harness.run();
        harness
    }

    #[test]
    fn a_whole_message_pasted_into_the_summary_fills_both_fields() {
        let mut harness = message_fields(false);
        harness.state_mut().1 = "Kept.".to_owned();
        harness.run();
        harness.event(egui::Event::Paste(
            "Add login\n\nWith a remember-me box.\n- and tests".to_owned(),
        ));
        harness.run();
        let (summary, body) = harness.state();
        assert_eq!(summary, "Add login");
        assert_eq!(body, "With a remember-me box.\n- and tests\n\nKept.");

        // One line (even copied with its line break) only goes in the summary.
        let mut harness = message_fields(false);
        harness.event(egui::Event::Paste("Fix typo\n".to_owned()));
        harness.run();
        assert_eq!(harness.state(), &("Fix typo".to_owned(), String::new()));
    }

    #[test]
    fn a_whole_message_pasted_into_empty_fields_fills_both() {
        let mut harness = message_fields(true);
        harness.event(egui::Event::Paste("Add login\r\n\r\nWhy.".to_owned()));
        harness.run();
        assert_eq!(
            harness.state(),
            &("Add login".to_owned(), "Why.".to_owned())
        );

        // With a summary written, the description takes the paste as it is.
        let mut harness = message_fields(true);
        harness.state_mut().0 = "Mine".to_owned();
        harness.run();
        harness.event(egui::Event::Paste("one\ntwo".to_owned()));
        harness.run();
        assert_eq!(harness.state(), &("Mine".to_owned(), "one\ntwo".to_owned()));
    }
}
