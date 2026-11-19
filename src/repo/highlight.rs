//! Syntax colouring for diffs (syntect with bat's syntaxes, pure-Rust regex engine), chosen by
//! file extension or name.

use std::sync::OnceLock;

use egui::Color32;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// Coloured pieces of one line.
pub type Spans = Vec<(Color32, String)>;

/// Bigger diffs or minified blobs stay uncoloured: highlighting is the slow part of opening them.
const MAX_LINES: usize = 8000;
const MAX_LINE_BYTES: usize = 4096;
const MAX_TOTAL_BYTES: usize = 512 * 1024;

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

/// Colours `lines` (one file's text, in order) for `path`; `None` for plain text, unknown
/// types, minified lines, or very long inputs.
pub fn highlight<'a>(
    path: &str,
    lines: impl ExactSizeIterator<Item = &'a str>,
    dark: bool,
) -> Option<Vec<Spans>> {
    if lines.len() > MAX_LINES {
        return None;
    }
    let collected: Vec<&'a str> = lines.collect();
    let mut total_bytes = 0usize;
    for &line in &collected {
        if line.len() > MAX_LINE_BYTES {
            return None;
        }
        total_bytes = total_bytes.saturating_add(line.len());
        if total_bytes > MAX_TOTAL_BYTES {
            return None;
        }
    }
    let ss = syntaxes();
    let file = std::path::Path::new(path);
    let name = file.file_name()?.to_string_lossy();
    // Match exact filenames (e.g. `Dockerfile`, `Makefile`, `Cargo.lock`) first, then extension.
    let syntax = ss
        .find_syntax_by_extension(&name)
        .or_else(|| {
            file.extension()
                .and_then(|e| ss.find_syntax_by_extension(&e.to_string_lossy().to_lowercase()))
        })?;
    if syntax.name == "Plain Text" {
        return None;
    }
    // VS Code's theme when it suits the background: a dark theme on a light gitcrafted reads badly.
    let editor = crate::vscode_theme::enabled()
        .then(crate::vscode_theme::current)
        .flatten()
        .filter(|e| e.dark == dark);
    let theme = match &editor {
        Some(e) => &e.theme,
        None => themes().themes.get(if dark {
            "base16-ocean.dark"
        } else {
            "InspiredGitHub"
        })?,
    };
    let mut h = HighlightLines::new(syntax, theme);
    let mut out = Vec::with_capacity(collected.len());
    for line in collected {
        let with_nl = format!("{line}\n");
        let ranges = h.highlight_line(&with_nl, ss).ok()?;
        out.push(
            ranges
                .into_iter()
                .map(|(style, text)| {
                    let c = style.foreground;
                    (
                        Color32::from_rgb(c.r, c.g, c.b),
                        text.trim_end_matches('\n').to_owned(),
                    )
                })
                .filter(|(_, t)| !t.is_empty())
                .collect(),
        );
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_rust_and_skips_plain_text() {
        let lines = ["fn main() {", "    let x = 1;", "}"];
        let spans = highlight("a.rs", lines.iter().copied(), true);
        let spans = spans.unwrap_or_default();
        assert_eq!(spans.len(), 3);
        let text: String = spans[1].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(text, "    let x = 1;");
        // More than one colour on a line of code.
        assert!(spans[0].len() > 1);
        assert!(highlight("notes.txt", lines.iter().copied(), true).is_none());
        assert!(highlight("Makefile-without-ext", lines.iter().copied(), true).is_none());
    }

    /// The languages of a typical web or .NET repository each get more than one colour.
    #[test]
    fn colours_common_languages() {
        let cases: &[(&str, &[&str])] = &[
            ("Program.cs", &["public class A { private int x = 1; }"]),
            ("app.js", &["const x = require('y');"]),
            ("app.ts", &["const x: number = 1;"]),
            ("App.tsx", &["export const A = (): JSX.Element => <div />;"]),
            (
                "Details.vue",
                &[
                    "<script setup lang=\"ts\">",
                    "import { ref } from 'vue'",
                    "</script>",
                ],
            ),
            ("Cargo.toml", &["[package]", "name = \"x\""]),
            ("Dockerfile", &["FROM alpine:3", "RUN echo hi"]),
            ("styles.scss", &[".a { color: red; }"]),
        ];
        let plain: Vec<&str> = cases
            .iter()
            .filter(|(path, lines)| {
                !highlight(path, lines.iter().copied(), true)
                    .is_some_and(|spans| spans.iter().any(|line| line.len() > 1))
            })
            .map(|(path, _)| *path)
            .collect();
        assert!(plain.is_empty(), "not coloured: {plain:?}");
    }
}
