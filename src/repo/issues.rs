//! Issue links: `#123`, `ABC-123` or `PROJ-123` in commit messages become links to the issue
//! tracker. Rules are kept in the repository's git config (`gitcrafted.issuelink`, one per value);
//! without any, GitHub and GitLab remotes link `#123` to their issues.

use std::ops::Range;
use std::sync::{Arc, mpsc};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Response, RichText, Sense, Ui};

use super::dialogs::{Outcome, buttons, title};
use super::ext::{DialogBody, ExtAction, dialog};
use super::sync::{pick, row};
use super::{Action, Dialog, RepoTab};
use crate::git;
use crate::theme::Tokens;

const KEY: &str = "gitcrafted.issuelink";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `#123`
    Hash,
    /// `ABC-123`
    Jira,
    /// A fixed prefix then a number: `PROJ-` → `PROJ-123`.
    Prefix(String),
}

/// A pattern and where it links: `{id}` in `url` is the number (the whole key for Jira).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub kind: Kind,
    pub url: String,
}

impl Rule {
    fn encode(&self) -> String {
        let kind = match &self.kind {
            Kind::Hash => "hash".to_owned(),
            Kind::Jira => "jira".to_owned(),
            Kind::Prefix(p) => format!("prefix:{p}"),
        };
        format!("{kind}\t{}", self.url)
    }

    fn decode(line: &str) -> Option<Rule> {
        let (kind, url) = line.split_once('\t')?;
        let kind = match kind {
            "hash" => Kind::Hash,
            "jira" => Kind::Jira,
            other => Kind::Prefix(other.strip_prefix("prefix:")?.to_owned()),
        };
        (!url.trim().is_empty()).then(|| Rule {
            kind,
            url: url.trim().to_owned(),
        })
    }

    fn describe(&self) -> String {
        match &self.kind {
            Kind::Hash => format!("#123 → {}", self.url),
            Kind::Jira => format!("ABC-123 → {}", self.url),
            Kind::Prefix(p) => format!("{p}123 → {}", self.url),
        }
    }
}

/// A link found in a text; `range` is in characters.
#[derive(Debug, PartialEq, Eq)]
pub struct Link {
    pub range: Range<usize>,
    pub url: String,
}

/// Upper-case words that come before `-<number>` without being issue keys (`UTF-8`, `SHA-256`,
/// `ISO-8601`, `CVE-2024-…`): never linked by the Jira-style rule.
const NOT_KEYS: &[&str] = &[
    "AES", "ANSI", "ARM", "ASCII", "AVX", "BASE", "CP", "CRC", "CVE", "DDR", "DIN", "ECMA", "EN",
    "ES", "GMT", "GPT", "HDMI", "HTTP", "IEC", "IEEE", "IPV", "ISO", "MD", "PCI", "PDF", "PEP",
    "RFC", "RSA", "SHA", "SSE", "SSL", "TLS", "UCS", "USB", "UTC", "UTF", "WIN", "X",
];

fn alnum(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The digits starting at `i`, as an end index (`i` when there are none).
fn digits_end(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while chars.get(j).is_some_and(char::is_ascii_digit) {
        j += 1;
    }
    j
}

/// Where each rule's pattern occurs in `text`, leftmost first, without overlaps.
pub fn find_links(text: &str, rules: &[Rule]) -> Vec<Link> {
    let chars: Vec<char> = text.chars().collect();
    let before_ok = |i: usize| i == 0 || !alnum(chars[i - 1]);
    let after_ok = |j: usize| chars.get(j).is_none_or(|c| !alnum(*c));
    let mut found: Vec<Link> = Vec::new();
    for rule in rules {
        let prefix: Option<Vec<char>> = match &rule.kind {
            Kind::Prefix(p) if !p.is_empty() => Some(p.chars().collect()),
            _ => None,
        };
        let mut i = 0;
        while i < chars.len() {
            // (end, id) of a match starting at i.
            let hit: Option<(usize, String)> = match &rule.kind {
                Kind::Hash if chars[i] == '#' && before_ok(i) => {
                    let end = digits_end(&chars, i + 1);
                    (end > i + 1 && after_ok(end))
                        .then(|| (end, chars[i + 1..end].iter().collect()))
                }
                Kind::Jira if chars[i].is_ascii_uppercase() && before_ok(i) => {
                    let mut j = i;
                    while chars
                        .get(j)
                        .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                    {
                        j += 1;
                    }
                    let key: String = chars[i..j].iter().collect();
                    if j > i + 1 && chars.get(j) == Some(&'-') && !NOT_KEYS.contains(&key.as_str())
                    {
                        let end = digits_end(&chars, j + 1);
                        (end > j + 1 && after_ok(end))
                            .then(|| (end, chars[i..end].iter().collect()))
                    } else {
                        None
                    }
                }
                Kind::Prefix(_) => {
                    if let Some(prefix) = &prefix {
                        let start_ok = !alnum(prefix[0]) || before_ok(i);
                        if start_ok && chars[i..].starts_with(prefix) {
                            let from = i + prefix.len();
                            let end = digits_end(&chars, from);
                            (end > from && after_ok(end))
                                .then(|| (end, chars[from..end].iter().collect()))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            };
            match hit {
                Some((end, id)) => {
                    found.push(Link {
                        range: i..end,
                        url: rule.url.replace("{id}", &id),
                    });
                    i = end;
                }
                None => i += 1,
            }
        }
    }
    found.sort_by_key(|l| l.range.start);
    let mut links: Vec<Link> = Vec::new();
    for l in found {
        if links
            .last()
            .is_none_or(|last| l.range.start >= last.range.end)
        {
            links.push(l);
        }
    }
    links
}

/// `git@github.com:org/repo.git`, `ssh://git@host/org/repo`, `https://user@host/org/repo.git`
/// → `https://host/org/repo`.
pub fn web_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let (host, path) = if let Some((_, rest)) = remote.split_once("://") {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next()?.to_owned(), path.to_owned())
    } else {
        let rest = remote.rsplit_once('@').map_or(remote, |(_, r)| r);
        let (host, path) = rest.split_once(':')?;
        (host.to_owned(), path.to_owned())
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && !path.is_empty()).then(|| format!("https://{host}/{path}"))
}

/// What a GitHub or GitLab remote gets without rules of its own.
fn default_rules(remote_url: &str) -> Vec<Rule> {
    let Some(web) = web_url(remote_url) else {
        return Vec::new();
    };
    let url = if web.contains("gitlab") {
        format!("{web}/-/issues/{{id}}")
    } else if web.contains("github") {
        format!("{web}/issues/{{id}}")
    } else {
        return Vec::new();
    };
    vec![Rule {
        kind: Kind::Hash,
        url,
    }]
}

/// The rules in the repository's git config.
pub fn read_rules(repo: &std::path::Path) -> Vec<Rule> {
    git::run(Some(repo), &["config", "--get-all", KEY])
        .map(|out| out.lines().filter_map(Rule::decode).collect())
        .unwrap_or_default()
}

/// Replaces the repository's rules.
pub fn write_rules(repo: &std::path::Path, rules: &[Rule]) {
    // Rewritten whole: `--unset-all` fails (harmlessly) when there are none.
    let _ = git::run(Some(repo), &["config", "--unset-all", KEY]);
    for r in rules {
        let _ = git::run(Some(repo), &["config", "--add", KEY, &r.encode()]);
    }
}

/// Whether `url` is safe to pass to `open::that_detached`: `http://` or `https://` with a
/// non-empty host and no whitespace or control characters.
pub(crate) fn is_safe_web_url(url: &str) -> bool {
    let u = url.trim();
    if u.is_empty() || u.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some(rest) = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")) else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    !host.is_empty()
}

impl RepoTab {
    fn issue_cache_id(&self) -> egui::Id {
        egui::Id::new(("gitcrafted-issue-rules", &self.path))
    }

    /// The tab's issue rules (cached from background reloads; never blocks UI thread).
    pub(super) fn issue_rules(&self, ctx: &egui::Context) -> Arc<Vec<Rule>> {
        let id = self.issue_cache_id();
        if let Some(rules) = ctx.data(|d| d.get_temp::<Arc<Vec<Rule>>>(id)) {
            return rules;
        }
        let rules = Arc::new(if self.stored_issue_rules.is_empty() {
            self.default_remote()
                .and_then(|r| self.remotes.iter().find(|x| x.name == r))
                .map(|r| default_rules(&r.url))
                .unwrap_or_default()
        } else {
            self.stored_issue_rules.clone()
        });
        ctx.data_mut(|d| d.insert_temp(id, rules.clone()));
        rules
    }

    pub(super) fn save_issue_rules(&mut self, rules: Vec<Rule>, ctx: &egui::Context) {
        self.stored_issue_rules = rules.clone();
        self.forget_issue_rules(ctx);
        let repo = self.path.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            write_rules(&repo, &rules);
            wake.request_repaint();
        });
    }

    /// Forget the remembered rules (after saving them or reloading).
    pub(super) fn forget_issue_rules(&self, ctx: &egui::Context) {
        let id = self.issue_cache_id();
        ctx.data_mut(|d| d.remove::<Arc<Vec<Rule>>>(id));
    }

    pub(super) fn issue_dialog(&self, ctx: &egui::Context) -> Dialog {
        let (tx, rx) = mpsc::channel();
        let repo = self.path.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_rules(&repo));
            wake.request_repaint();
        });
        dialog(IssueDialog {
            rules: self.stored_issue_rules.clone(),
            kind: 0,
            prefix: String::new(),
            url: String::new(),
            defaults: self
                .default_remote()
                .and_then(|r| self.remotes.iter().find(|x| x.name == r))
                .map(|r| default_rules(&r.url))
                .unwrap_or_default(),
            edited: false,
            rx: Some(rx),
        })
    }
}

fn pointer_in_link(
    galley: &egui::Galley,
    pos: egui::Pos2,
    pointer: egui::Pos2,
    range: &Range<usize>,
) -> bool {
    let rel = (pointer - pos).to_pos2();
    let on_row = galley
        .rows
        .iter()
        .find(|r| r.rect().contains(rel));
    let Some(hovered_row) = on_row else {
        return false;
    };
    for i in range.clone() {
        let a = galley.pos_from_cursor(egui::text::CCursor::new(i));
        let b = galley.pos_from_cursor(egui::text::CCursor::new(i + 1));
        if (a.min.y - b.min.y).abs() < 1.0 {
            let char_rect = egui::Rect::from_min_max(a.min, egui::pos2(b.max.x, a.max.y.max(b.max.y)));
            if char_rect.contains(rel) {
                return true;
            }
        } else if a.y_range().contains(rel.y) {
            let row_right = hovered_row.rect().right();
            if rel.x >= a.min.x && rel.x <= row_right {
                return true;
            }
        }
    }
    false
}

/// Text whose `#123`-like parts are links; `sense` is what the whole text reacts to. A click on
/// a link opens it in the browser. Returns `(response, clicked_link)`.
pub(super) fn linked_text_hit(
    ui: &mut Ui,
    text: &str,
    font: FontId,
    color: Color32,
    rules: &[Rule],
    sense: Sense,
) -> (Response, bool) {
    let links = find_links(text, rules);
    let link_color = Tokens::get(ui.ctx()).accent;
    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let chars: Vec<char> = text.chars().collect();
    let plain = TextFormat::simple(font.clone(), color);
    let mut at = 0;
    for l in &links {
        if l.range.start > at {
            job.append(
                &chars[at..l.range.start].iter().collect::<String>(),
                0.0,
                plain.clone(),
            );
        }
        let mut fmt = TextFormat::simple(font.clone(), link_color);
        fmt.underline = egui::Stroke::new(1.0, link_color);
        job.append(&chars[l.range.clone()].iter().collect::<String>(), 0.0, fmt);
        at = l.range.end;
    }
    if at < chars.len() {
        job.append(&chars[at..].iter().collect::<String>(), 0.0, plain);
    }
    let sense = if links.is_empty() {
        sense
    } else {
        sense | Sense::click()
    };
    // A real label, so the message stays selectable and copyable (and readable by screen
    // readers); its galley is what the pointer is hit-tested against.
    let (pos, galley, resp) = egui::Label::new(job).wrap().sense(sense).layout_in_ui(ui);
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), galley.text())
    });
    if ui.is_rect_visible(resp.rect) {
        if ui.style().interaction.selectable_labels {
            egui::text_selection::LabelSelectionState::label_text_selection(
                ui,
                &resp,
                pos,
                galley.clone(),
                color,
                egui::Stroke::NONE,
            );
        } else {
            ui.painter()
                .add(egui::epaint::TextShape::new(pos, galley.clone(), color));
        }
    }
    let mut hit_link = false;
    if let Some(pointer) = resp.hover_pos()
        && let Some(link) = links
            .iter()
            .find(|l| pointer_in_link(&galley, pos, pointer, &l.range))
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        hit_link = true;
        if resp.clicked() {
            open_link(&link.url);
        }
    }
    (resp, hit_link)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn linked_text(
    ui: &mut Ui,
    text: &str,
    font: FontId,
    color: Color32,
    rules: &[Rule],
    sense: Sense,
) -> Response {
    linked_text_hit(ui, text, font, color, rules, sense).0
}

/// Opens a link in the browser (in tests, remembers it instead). Only safe `http://` or
/// `https://` URLs are opened.
fn open_link(url: &str) {
    if !is_safe_web_url(url) {
        return;
    }
    #[cfg(not(test))]
    {
        let _ = open::that_detached(url.trim());
    }
    #[cfg(test)]
    OPENED.with(|o| o.borrow_mut().push(url.trim().to_owned()));
}

#[cfg(test)]
thread_local! {
    static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct IssueDialog {
    rules: Vec<Rule>,
    /// 0: `#123`, 1: `ABC-123`, 2: a prefix.
    kind: usize,
    prefix: String,
    url: String,
    defaults: Vec<Rule>,
    edited: bool,
    rx: Option<mpsc::Receiver<Vec<Rule>>>,
}

const KINDS: [&str; 3] = ["#123", "ABC-123 (Jira style)", "Prefix + number"];

impl DialogBody for IssueDialog {
    fn ui(&mut self, ui: &mut Ui, t: &Tokens, _enter: bool) -> Outcome {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(rules) => {
                    self.rx = None;
                    if !self.edited {
                        self.rules = rules;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        title(
            ui,
            "Issue links",
            "Turn issue numbers in commit messages into links to your tracker.",
            t,
        );
        if self.rules.is_empty() {
            let text = match self.defaults.first() {
                Some(d) => format!("No rules: using {}", d.describe()),
                None => "No rules yet".to_owned(),
            };
            ui.label(RichText::new(text).color(t.text_faint));
        } else {
            let mut remove = None;
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, r) in self.rules.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(r.describe()).font(crate::theme::mono(11.5)),
                                )
                                .truncate(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Remove").clicked() {
                                        remove = Some(i);
                                    }
                                },
                            );
                        });
                    }
                });
            if let Some(i) = remove {
                self.rules.remove(i);
                self.edited = true;
            }
        }
        ui.add_space(6.0);
        ui.separator();
        row(ui, t, "Pattern", |ui| {
            pick(ui, "issue-kind", KINDS[self.kind], |ui| {
                let mut changed = false;
                for (i, label) in KINDS.iter().enumerate() {
                    if ui.selectable_label(self.kind == i, *label).clicked() {
                        self.kind = i;
                        changed = true;
                    }
                }
                changed
            })
        });
        if self.kind == 2 {
            row(ui, t, "Prefix", |ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.prefix)
                        .hint_text("PROJ-")
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(8, 4)),
                );
                super::widgets::text_edit_menu(&edit, &mut self.prefix);
                edit
            });
        }
        row(ui, t, "Link to", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .hint_text("https://tracker.example.com/browse/{id}")
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(8, 4)),
            );
            super::widgets::text_edit_menu(&edit, &mut self.url);
            edit
        });
        let url_trimmed = self.url.trim();
        let url_ok = is_safe_web_url(&url_trimmed.replace("{id}", "1"));
        if !url_trimmed.is_empty() && !url_ok {
            ui.label(
                RichText::new("URL must start with http:// or https:// with no spaces.")
                    .color(t.danger)
                    .small(),
            );
        }
        let can_add = url_ok && (self.kind != 2 || !self.prefix.trim().is_empty());
        if ui
            .add_enabled(can_add, egui::Button::new("Add rule"))
            .clicked()
        {
            let kind = match self.kind {
                0 => Kind::Hash,
                1 => Kind::Jira,
                _ => Kind::Prefix(self.prefix.trim().to_owned()),
            };
            self.rules.push(Rule {
                kind,
                url: url_trimmed.to_owned(),
            });
            self.url.clear();
            self.prefix.clear();
            self.edited = true;
        }
        ui.add_space(8.0);
        buttons(ui, "Save", true, false, || {
            Action::Ext(ExtAction::SaveIssueRules(self.rules.clone()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: Kind, url: &str) -> Rule {
        Rule {
            kind,
            url: url.to_owned(),
        }
    }

    fn urls(text: &str, rules: &[Rule]) -> Vec<String> {
        find_links(text, rules).into_iter().map(|l| l.url).collect()
    }

    #[test]
    fn hash_links_need_digits_and_a_boundary() {
        let r = [rule(Kind::Hash, "https://x/issues/{id}")];
        assert_eq!(
            urls("fix(D5): resolve #53578 and (#12), not C#5x or #", &r),
            ["https://x/issues/53578", "https://x/issues/12"]
        );
        assert!(urls("color #fff and a#1", &r).is_empty());
    }

    #[test]
    fn jira_keys_and_prefixes() {
        let jira = [rule(Kind::Jira, "https://j/browse/{id}")];
        assert_eq!(
            urls("ABC-123: done, see AB2-9.", &jira),
            ["https://j/browse/ABC-123", "https://j/browse/AB2-9"]
        );
        assert!(urls("abc-123 and A-1", &jira).is_empty());
        let prefix = [rule(Kind::Prefix("D5-".into()), "https://t/{id}")];
        assert_eq!(urls("see D5-77 and XD5-8", &prefix), ["https://t/77"]);
    }

    #[test]
    fn ranges_are_in_characters_and_do_not_overlap() {
        let rules = [
            rule(Kind::Hash, "h/{id}"),
            rule(Kind::Prefix("#1".into()), "p/{id}"),
        ];
        let links = find_links("é #12 end", &rules);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].range, 2..5);
    }

    #[test]
    fn rules_round_trip_through_git_config_values() {
        for r in [
            rule(Kind::Hash, "https://x/{id}"),
            rule(Kind::Jira, "https://j/{id}"),
            rule(Kind::Prefix("PROJ-".into()), "https://p/{id}"),
        ] {
            assert_eq!(Rule::decode(&r.encode()), Some(r));
        }
        assert_eq!(Rule::decode("nonsense"), None);
    }

    #[test]
    fn web_urls_from_remotes() {
        for url in [
            "git@github.com:org/repo.git",
            "ssh://git@github.com/org/repo",
            "https://user@github.com/org/repo.git",
            "https://github.com/org/repo/",
        ] {
            assert_eq!(
                web_url(url).as_deref(),
                Some("https://github.com/org/repo"),
                "{url}"
            );
        }
        assert_eq!(
            default_rules("git@gitlab.com:g/p.git")[0].url,
            "https://gitlab.com/g/p/-/issues/{id}"
        );
        assert!(default_rules("git@example.org:g/p.git").is_empty());
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::git::Remote;
    use crate::repo::ext::ExtAction;
    use crate::repo::test_support::*;

    fn rules() -> Vec<Rule> {
        vec![
            Rule {
                kind: Kind::Hash,
                url: "https://t.example/issues?id={id}&src=gitcrafted".to_owned(),
            },
            Rule {
                kind: Kind::Jira,
                url: "https://j.example/browse/{id}".to_owned(),
            },
            Rule {
                kind: Kind::Prefix("Fix (D5) ".to_owned()),
                url: "https://p.example/{id}".to_owned(),
            },
        ]
    }

    #[test]
    fn rules_survive_a_round_trip_through_git_config() {
        let dir = repo("is-config");
        assert!(read_rules(&dir).is_empty());
        write_rules(&dir, &rules());
        assert_eq!(read_rules(&dir), rules());

        // Saving replaces what was there, in the order given.
        write_rules(&dir, &rules()[1..]);
        assert_eq!(read_rules(&dir), rules()[1..]);
        write_rules(&dir, &[]);
        assert!(read_rules(&dir).is_empty());
        assert!(!git_ok(&dir, &["config", "--get", KEY]));
    }

    #[test]
    fn unreadable_config_values_are_skipped() {
        let dir = repo("is-garbage");
        git(&dir, &["config", "--add", KEY, "garbage without a tab"]);
        git(&dir, &["config", "--add", KEY, "hash\t"]);
        write_rules(&dir, &rules()[..1]);
        assert_eq!(read_rules(&dir), rules()[..1]);
        git(&dir, &["config", "--add", KEY, "wat\thttps://x/{id}"]);
        assert_eq!(read_rules(&dir), rules()[..1], "an unknown kind is ignored");
    }

    #[test]
    fn a_tab_uses_saved_rules_else_its_hosts_defaults() {
        let dir = repo("is-tab");
        let mut f = Fixture::open(&dir);

        // No remote, no rules.
        assert!(f.tab.issue_rules(&f.ctx).is_empty());
        f.tab.forget_issue_rules(&f.ctx);

        // A GitHub remote gives #123 → its issues, without any rules saved.
        f.tab.remotes = vec![Remote {
            name: "origin".to_owned(),
            url: "git@github.com:org/repo.git".to_owned(),
        }];
        let defaults = f.tab.issue_rules(&f.ctx);
        assert_eq!(
            *defaults,
            vec![Rule {
                kind: Kind::Hash,
                url: "https://github.com/org/repo/issues/{id}".to_owned()
            }]
        );

        // Saving rules through the action replaces them at once (the remembered ones are dropped).
        f.tab.ext_action(ExtAction::SaveIssueRules(rules()), &f.ctx);
        assert!(
            f.status()
                .is_some_and(|(m, e)| !e && m.contains("Issue links saved"))
        );
        assert_eq!(*f.tab.issue_rules(&f.ctx), rules());
        assert!(wait_for(|| read_rules(&dir) == rules()));

        // Another host with no rules of its own: nothing.
        f.tab.ext_action(ExtAction::SaveIssueRules(vec![]), &f.ctx);
        assert!(wait_for(|| read_rules(&dir).is_empty()));
        f.tab.remotes[0].url = "git@example.org:org/repo.git".to_owned();
        f.tab.forget_issue_rules(&f.ctx);
        assert!(f.tab.issue_rules(&f.ctx).is_empty());
    }

    #[test]
    fn the_remembered_rules_stay_until_forgotten() {
        let dir = repo("is-cache");
        write_rules(&dir, &rules()[..1]);
        let mut f = Fixture::open(&dir);
        assert_eq!(f.tab.issue_rules(&f.ctx).len(), 1);
        // Changed behind the tab's back: still the remembered ones until reloaded.
        write_rules(&dir, &rules());
        assert_eq!(f.tab.issue_rules(&f.ctx).len(), 1);
        f.reload();
        assert_eq!(f.tab.issue_rules(&f.ctx).len(), 3);
        assert!(matches!(f.tab.issue_dialog(&f.ctx), Dialog::Ext(_)));
    }

    #[test]
    fn each_kind_links_what_it_should_in_a_real_message() {
        let r = rules();
        let text = "Fix (D5) 77: crash on #12 (see ABC-9) and #x, not xABC-1";
        let links = find_links(text, &r);
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://p.example/77",
                "https://t.example/issues?id=12&src=gitcrafted",
                "https://j.example/browse/ABC-9"
            ]
        );
        // The ranges cut the text exactly at the matches.
        let chars: Vec<char> = text.chars().collect();
        let cut: Vec<String> = links
            .iter()
            .map(|l| chars[l.range.clone()].iter().collect())
            .collect();
        assert_eq!(cut, ["Fix (D5) 77", "#12", "ABC-9"]);
    }

    /// Lays the text out in a real egui frame and measures what it took.
    fn measure(text: &str, width: f32, rules: &[Rule]) -> egui::Vec2 {
        use std::cell::Cell;
        use std::rc::Rc;
        let size = Rc::new(Cell::new(egui::Vec2::ZERO));
        let out = size.clone();
        let rules = rules.to_vec();
        let text = text.to_owned();
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            ui.set_max_width(width);
            let resp = linked_text(
                ui,
                &text,
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
                &rules,
                Sense::hover(),
            );
            out.set(resp.rect.size());
        });
        harness.run();
        size.get()
    }

    #[test]
    fn linked_text_lays_out_wraps_and_handles_empty_text() {
        let r = rules();
        let one_line = measure("fix #12", 600.0, &r);
        assert!(one_line.x > 10.0 && one_line.y > 5.0, "{one_line:?}");

        // The same text in a narrow column wraps onto more lines.
        let long =
            "fix the crash that happens when the cache is cold, see #12 and ABC-9 for the story";
        let wide = measure(long, 900.0, &r);
        let narrow = measure(long, 120.0, &r);
        assert!(narrow.y > wide.y * 2.0, "{narrow:?} vs {wide:?}");
        assert!(narrow.x <= 125.0, "{narrow:?}");

        // Links don't change the size of the text, and no rules is plain text.
        let plain = measure("fix #12", 600.0, &[]);
        assert!((plain.x - one_line.x).abs() < 1.0 && (plain.y - one_line.y).abs() < 1.0);
        let empty = measure("", 600.0, &r);
        assert!(empty.x < 1.0);
    }
}

/// Regression tests (review round 2).
#[cfg(test)]
mod review_tests {
    use super::*;

    fn jira() -> Vec<Rule> {
        vec![Rule {
            kind: Kind::Jira,
            url: "https://j/browse/{id}".to_owned(),
        }]
    }

    #[test]
    fn common_upper_case_words_are_not_issue_keys() {
        let text = "Use UTF-8 and SHA-256, ISO-8601 dates; fixes CVE-2024-1234 (RFC-9110, HTTP-2). See PROJ-42.";
        let urls: Vec<String> = find_links(text, &jira())
            .into_iter()
            .map(|l| l.url)
            .collect();
        assert_eq!(urls, ["https://j/browse/PROJ-42"]);
        for word in NOT_KEYS {
            assert!(
                find_links(&format!("{word}-1"), &jira()).is_empty(),
                "{word}"
            );
        }
        assert_eq!(
            find_links("ABC-1 DEV2-7", &jira()).len(),
            2,
            "real keys still link"
        );
    }

    fn rules() -> Vec<Rule> {
        vec![Rule {
            kind: Kind::Hash,
            url: "https://t/issues/{id}".to_owned(),
        }]
    }

    #[test]
    fn the_message_text_can_be_selected_and_copied() {
        use egui_kittest::kittest::Queryable;
        let text = "fix #12 now";
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            linked_text(
                ui,
                text,
                FontId::proportional(14.0),
                Color32::WHITE,
                &rules(),
                Sense::hover(),
            );
        });
        harness.run();
        let rect = harness.get_by_label(text).rect();
        let y = rect.center().y;
        let (start, end) = (
            egui::pos2(rect.left() + 1.0, y),
            egui::pos2(rect.right() - 1.0, y),
        );
        // Drag across the whole line, then copy.
        harness.event(egui::Event::PointerMoved(start));
        harness.step();
        harness.event(egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
        for t in [0.3, 0.6, 1.0] {
            harness.event(egui::Event::PointerMoved(start + (end - start) * t));
            harness.step();
        }
        harness.event(egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
        harness.event(egui::Event::Copy);
        harness.step();
        let copied = harness
            .output()
            .platform_output
            .commands
            .iter()
            .find_map(|c| match c {
                egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            });
        assert!(
            copied.as_deref().is_some_and(|c| c.contains("#12")),
            "{copied:?}"
        );
        assert!(
            OPENED.with(|o| o.borrow().is_empty()),
            "selecting is not clicking a link"
        );
    }

    #[test]
    fn the_message_is_a_real_selectable_label_and_links_still_click() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::{NodeT, Queryable};
        let text = "fix #12 now";
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            linked_text(
                ui,
                text,
                FontId::proportional(14.0),
                Color32::WHITE,
                &rules(),
                Sense::hover(),
            );
        });
        harness.run();
        // An accessible text node: the label is a real one (selectable, readable).
        let node = harness.get_by_label(text);
        assert_eq!(node.accesskit_node().role(), Role::Label);
        let rect = node.rect();

        // Click in the middle of "#12": the link opens; a click on "fix" doesn't.
        let width = |s: &str| {
            harness.ctx.fonts_mut(|f| {
                f.layout_no_wrap(s.to_owned(), FontId::proportional(14.0), Color32::WHITE)
                    .size()
                    .x
            })
        };
        let link_x = rect.left() + width("fix ") + width("#12") / 2.0;
        let plain_x = rect.left() + width("f") / 2.0;
        for x in [plain_x, link_x] {
            let pos = egui::pos2(x, rect.center().y);
            harness.event(egui::Event::PointerMoved(pos));
            harness.step();
            for pressed in [true, false] {
                harness.event(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                harness.step();
            }
        }
        let opened = OPENED.with(|o| o.borrow().clone());
        assert_eq!(opened, ["https://t/issues/12"]);
    }

    #[test]
    fn safe_web_url_rejects_dangerous_schemes_and_control_chars() {
        assert!(is_safe_web_url("https://github.com/org/repo/issues/1"));
        assert!(is_safe_web_url("http://tracker.local:8080/browse/ABC-1"));
        assert!(!is_safe_web_url("file:///etc/passwd"));
        assert!(!is_safe_web_url("javascript:alert(1)"));
        assert!(!is_safe_web_url("https://github.com/a b"));
        assert!(!is_safe_web_url("https://"));
    }
}
