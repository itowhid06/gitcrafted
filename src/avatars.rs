//! Author avatars, fetched in the background, cached on disk, painted round. A repository on
//! GitHub or GitLab tells us (through [`hint`]) where its authors' account pictures are; other
//! addresses go to Gravatar (or GitHub for its noreply ones). Initials stand in until (or unless)
//! one loads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use egui::{Rect, Vec2, load::Bytes};

/// A missing avatar is asked for again after this long.
const RETRY_MISSING: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_AVATAR_BYTES: usize = 512 * 1024;
const MAX_MEMORY_AVATARS: usize = 512;

fn extract_host(authority: &str) -> Option<&str> {
    let trimmed = authority.trim();
    if let Some(rest) = trimmed.strip_prefix('[') {
        let (addr, after) = rest.split_once(']')?;
        if after.is_empty() {
            return Some(addr);
        }
        let port = after.strip_prefix(':')?;
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            return Some(addr);
        }
        return None;
    }
    if let Some((host, port)) = trimmed.split_once(':') {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            Some(host)
        } else {
            None
        }
    } else {
        Some(trimmed)
    }
}

fn is_safe_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_link_local()
                || v4.is_private()
                || v4.is_broadcast()
                || v4.is_documentation())
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_safe_ip(std::net::IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            let is_link_local = (seg0 & 0xffc0) == 0xfe80;
            let is_unique_local = (seg0 & 0xfe00) == 0xfc00;
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_link_local
                || is_unique_local)
        }
    }
}

fn is_safe_host(host: &str) -> bool {
    let Some(bare) = extract_host(host) else {
        return false;
    };
    let lower = bare.to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    if let Ok(ip) = lower.parse::<std::net::IpAddr>() {
        return is_safe_ip(ip);
    }
    lower != "localhost"
        && !lower.ends_with(".localhost")
        && !lower.ends_with(".local")
        && !lower.starts_with('.')
        && !lower.ends_with('.')
        && !lower.starts_with('-')
        && !lower.ends_with('-')
        && lower
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

fn is_safe_https_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    is_safe_host(host)
}

#[derive(Clone)]
enum State {
    Pending,
    /// The picture, and its cache key (which says where it came from).
    Ready(Arc<[u8]>, String),
    Missing,
}

struct Avatars {
    map: Mutex<HashMap<String, State>>,
    queue: Mutex<Sender<String>>,
}

/// Where an address's picture is, when a forge knows better than Gravatar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hint {
    /// The account's avatar URL (GitHub).
    Url(String),
    /// Ask this GitLab host's avatar API.
    GitLab(String),
}

static AVATARS: OnceLock<Avatars> = OnceLock::new();

fn hints() -> &'static Mutex<HashMap<String, Hint>> {
    static HINTS: OnceLock<Mutex<HashMap<String, Hint>>> = OnceLock::new();
    HINTS.get_or_init(Default::default)
}

/// Records where `email`'s picture is; one already shown from elsewhere is fetched again.
pub fn hint(email: &str, hint: Hint) {
    let k = email.trim().to_lowercase();
    if k.is_empty() {
        return;
    }
    match &hint {
        Hint::Url(u) if !is_safe_https_url(u) => return,
        Hint::GitLab(h) if !is_safe_host(h) => return,
        _ => {}
    }
    let mut h = lock(hints());
    if h.get(&k) == Some(&hint) {
        return;
    }
    h.insert(k.clone(), hint);
    drop(h);
    if let Some(av) = AVATARS.get() {
        lock(&av.map).remove(&k);
    }
}

fn cache_dir() -> Option<PathBuf> {
    eframe::storage_dir(crate::APP_ID).map(|d| d.join("avatars"))
}

fn avatars(ctx: &egui::Context) -> &'static Avatars {
    AVATARS.get_or_init(|| {
        let (tx, rx) = channel::<String>();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(8))
                .user_agent("gitcrafted")
                .build();
            while let Ok(email) = rx.recv() {
                let hint = lock(hints()).get(&email).cloned();
                let state = fetch(&agent, &email, hint.as_ref());
                // Do not overwrite a newer hint that arrived while this fetch was in flight.
                if lock(hints()).get(&email).cloned() != hint {
                    continue;
                }
                if let Some(av) = AVATARS.get() {
                    let mut map = lock(&av.map);
                    if map.len() >= MAX_MEMORY_AVATARS {
                        map.clear();
                    }
                    map.insert(email, state);
                }
                ctx.request_repaint();
            }
        });
        Avatars {
            map: Mutex::new(HashMap::new()),
            queue: Mutex::new(tx),
        }
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn key(email: &str) -> String {
    format!("{:x}", md5::compute(email.trim().to_lowercase()))
}

/// Where to ask: GitHub knows its noreply addresses, Gravatar the rest.
fn url(email: &str) -> String {
    let email = email.trim().to_lowercase();
    if let Some(local) = email.strip_suffix("@users.noreply.github.com") {
        let valid_user = |u: &str| {
            !u.is_empty() && u.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        };
        match local.split_once('+') {
            Some((id, _)) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
                return format!("https://avatars.githubusercontent.com/u/{id}?s=80");
            }
            None if valid_user(local) => {
                return format!("https://github.com/{local}.png?size=80");
            }
            _ => {}
        }
    }
    format!("https://www.gravatar.com/avatar/{}?s=80&d=404", key(&email))
}

/// Cache file name: Gravatar's hash, plus where the picture came from when a forge said.
fn cache_key(email: &str, hint: Option<&Hint>) -> String {
    match hint {
        None => key(email),
        Some(Hint::Url(_)) => format!("{}-gh", key(email)),
        Some(Hint::GitLab(host)) => format!("{}-gl-{}", key(email), key(host)),
    }
}

/// The picture's URL: a forge's, else [`url`].
fn source(agent: &ureq::Agent, email: &str, hint: Option<&Hint>) -> String {
    match hint {
        Some(Hint::Url(u)) if is_safe_https_url(u) => {
            format!("{u}{}s=80", if u.contains('?') { '&' } else { '?' })
        }
        Some(Hint::GitLab(host)) if is_safe_host(host) => {
            let api = format!(
                "https://{host}/api/v4/avatar?email={}&size=80",
                query_escape(email)
            );
            let found = agent
                .get(&api)
                .call()
                .ok()
                .and_then(|r| r.into_string().ok())
                .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
                .and_then(|v| v.get("avatar_url")?.as_str().map(str::to_owned));
            match found {
                // GitLab falls back to a generated Gravatar; initials read better.
                Some(u) if is_safe_https_url(&u) && !u.contains("gravatar.com") => u,
                _ => url(email),
            }
        }
        _ => url(email),
    }
}

fn query_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'@' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn is_supported_image(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.len() <= MAX_AVATAR_BYTES && image::guess_format(bytes).is_ok()
}

fn atomic_write(dir: &std::path::Path, filename: &str, bytes: &[u8]) {
    let _ = std::fs::create_dir_all(dir);
    let tmp = dir.join(format!("{filename}.tmp-{}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, dir.join(filename)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn fetch(agent: &ureq::Agent, email: &str, hint: Option<&Hint>) -> State {
    let dir = cache_dir();
    let k = cache_key(email, hint);
    if let Some(dir) = &dir {
        let img_path = dir.join(format!("{k}.img"));
        if let Ok(bytes) = std::fs::read(&img_path) {
            if is_supported_image(&bytes) {
                return State::Ready(Arc::from(bytes.into_boxed_slice()), k);
            }
            let _ = std::fs::remove_file(&img_path);
        }
        let missing = dir.join(format!("{k}.none"));
        let recent = std::fs::metadata(&missing)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < RETRY_MISSING);
        if recent {
            return State::Missing;
        }
    }
    let target = source(agent, email, hint);
    let mut definitively_missing = false;
    let bytes = match agent.get(&target).call() {
        Ok(resp) => {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = resp
                .into_reader()
                .take((MAX_AVATAR_BYTES + 1) as u64)
                .read_to_end(&mut buf);
            is_supported_image(&buf).then_some(buf)
        }
        Err(ureq::Error::Status(404 | 410, _)) => {
            definitively_missing = true;
            None
        }
        Err(_) => None,
    };
    match bytes {
        Some(bytes) => {
            if let Some(dir) = &dir {
                atomic_write(dir, &format!("{k}.img"), &bytes);
            }
            State::Ready(Arc::from(bytes.into_boxed_slice()), k)
        }
        None => {
            if definitively_missing && let Some(dir) = &dir {
                atomic_write(dir, &format!("{k}.none"), b"");
            }
            State::Missing
        }
    }
}

/// Paints `email`'s avatar into `rect` if it is loaded; returns false (and queues a download)
/// otherwise, so the caller keeps its initials.
pub fn paint(ui: &egui::Ui, rect: Rect, email: &str) -> bool {
    let k = email.trim().to_lowercase();
    if k.is_empty() || !crate::settings::Settings::get(ui.ctx()).avatars {
        return false;
    }
    let av = avatars(ui.ctx());
    let state = {
        let mut map = lock(&av.map);
        match map.get(&k) {
            Some(s) => s.clone(),
            None => {
                if map.len() >= MAX_MEMORY_AVATARS {
                    map.clear();
                }
                map.insert(k.clone(), State::Pending);
                let _ = lock(&av.queue).send(k.clone());
                State::Pending
            }
        }
    };
    let State::Ready(bytes, source) = state else {
        return false;
    };
    // egui keeps an image per URI: one per source, so a forge's picture replaces Gravatar's.
    let uri = format!("bytes://avatar/{source}.png");
    egui::Image::from_bytes(uri, Bytes::Shared(bytes))
        .fit_to_exact_size(Vec2::splat(rect.width()))
        .corner_radius(rect.width() / 2.0)
        .show_loading_spinner(false)
        .paint_at(ui, rect);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hinted_sources() {
        let agent = ureq::AgentBuilder::new().build();
        assert_eq!(
            source(
                &agent,
                "a@b.c",
                Some(&Hint::Url("https://av/u/1?v=4".into()))
            ),
            "https://av/u/1?v=4&s=80"
        );
        assert_eq!(query_escape("me+git@x.org"), "me%2Bgit@x.org");
        assert_ne!(
            cache_key("a@b.c", None),
            cache_key("a@b.c", Some(&Hint::Url(String::new())))
        );
    }

    #[test]
    fn avatar_urls() {
        assert_eq!(
            url("12345+octo@users.noreply.github.com"),
            "https://avatars.githubusercontent.com/u/12345?s=80"
        );
        assert_eq!(
            url("octo@users.noreply.github.com"),
            "https://github.com/octo.png?size=80"
        );
        // Gravatar hashes the trimmed, lowercased address.
        assert_eq!(
            url(" MyEmailAddress@example.com "),
            "https://www.gravatar.com/avatar/0bc83cb571cd1c50ba6f3e8a78ef1346?s=80&d=404"
        );
    }

    #[test]
    fn rejects_private_loopback_and_non_https_avatar_targets() {
        for bad in [
            "localhost",
            "sub.localhost",
            "printer.local",
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "[::1]:443",
            "[::ffff:127.0.0.1]",
            "fe80::1",
        ] {
            assert!(!is_safe_host(bad), "expected {bad} to be rejected");
        }
        assert!(is_safe_host("avatars.githubusercontent.com"));
        assert!(!is_safe_https_url("http://avatars.githubusercontent.com/u/1"));
        assert!(!is_safe_https_url("https://127.0.0.1/avatar.png"));
        assert!(!is_safe_https_url("https://avatars.githubusercontent.com/u/1\n"));
        assert!(is_safe_https_url("https://avatars.githubusercontent.com/u/1?v=4"));
    }
}
