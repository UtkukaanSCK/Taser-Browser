//! The site information panel: a click on the address box's connection mark
//! (the lock, "Güvenli değil", a certificate error) opens it under the mark,
//! in the overlay. It names the site and its connection, lists the site's
//! permissions with Sor / İzin ver / Engelle for each, and deletes what the
//! site stored (its cookies and storage, nothing else).
//!
//! A permission chosen in a page's own prompt is kept in the profile, and
//! until now nothing in X-Browser could take one back; this is where it is
//! taken back. The panel is about one origin (`https://host[:port]`), the
//! one WebView2 keeps permissions under; only http(s) pages have one.
//!
//! The overlay page only draws what Rust sends (`xbSite.show`) and sends the
//! user's choices back (`si-*`), each acted on only while the panel is up.

use crate::app::{App, UI_HEIGHT, UserEvent};
use crate::commands::UiCommand;
use crate::native;
use crate::overlay::{self, Mode};
use serde::Serialize;

/// The panel's width and the heights its height is made of (logical px);
/// must match overlay.html (`#site`). The list scrolls if they are off.
const PANEL_W: f64 = 340.0;
const HEAD_H: f64 = 82.0;
const LIST_HEAD_H: f64 = 40.0;
const ROW_H: f64 = 36.0;
const FOOT_H: f64 = 104.0;
/// The overlay page's own 1px border, above and below.
const BORDER: f64 = 2.0;
const EDGE: f64 = 8.0;

/// A permission kind: its `COREWEBVIEW2_PERMISSION_KIND`, its name, and
/// whether its default is to ask (all but autoplay, whose default is the
/// browser's rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    pub id: i32,
    pub name: &'static str,
    ask: bool,
}

const fn kind(id: i32, name: &'static str) -> Kind {
    Kind { id, name, ask: true }
}

/// Offered for every site, in this order.
pub const KINDS: [Kind; 6] = [
    kind(3, "Konum"),
    kind(2, "Kamera"),
    kind(1, "Mikrofon"),
    kind(4, "Bildirimler"),
    kind(6, "Pano okuma"),
    Kind { id: 9, name: "Otomatik oynatma", ask: false },
];

/// Shown only for a site that has one of them set.
const OTHER_KINDS: [Kind; 6] = [
    kind(7, "Birden çok indirme"),
    kind(8, "Dosya düzenleme"),
    kind(10, "Yerel yazı tipleri"),
    kind(11, "MIDI cihazları"),
    kind(12, "Pencere yönetimi"),
    kind(5, "Diğer sensörler"),
];

fn kind_of(id: i32) -> Option<Kind> {
    KINDS.iter().chain(OTHER_KINDS.iter()).copied().find(|k| k.id == id)
}

/// `COREWEBVIEW2_PERMISSION_STATE`: 0 the default (ask), 1 allow, 2 block.
pub const DEFAULT: i32 = 0;
pub const ALLOW: i32 = 1;
pub const BLOCK: i32 = 2;

/// What the panel says about the connection, by the indicator's state.
pub const CONN_SECURE: &str = "Bağlantı güvenli (HTTPS).";
pub const CONN_INSECURE: &str = "Bağlantı güvenli değil: bu siteye yazdıklarınızı başkaları görebilir.";
pub const CONN_CERT: &str = "Sertifika hatası: bu sitenin kimliği doğrulanamadı.";
pub const CONN_CRASHED: &str = "Sayfa çöktü.";

/// The footer's messages.
pub const SAID_SAVED: &str = "Kaydedildi. Bazı değişiklikler sayfa yeniden yüklenince geçerli olur.";
pub const SAID_SAVE_FAILED: &str = "İzin değiştirilemedi.";
pub const SAID_LIST_FAILED: &str = "Bu sitenin izinleri okunamadı.";
pub const SAID_CLEARING: &str = "Siliniyor…";
pub const SAID_CLEARED: &str = "Bu sitenin çerezleri ve verileri silindi.";
pub const SAID_CLEAR_FAILED: &str = "Bu sitenin verileri silinemedi.";

/// Messages from the indicator and from the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteCmd {
    /// The mark was clicked; its left edge (CSS px of the UI webview).
    Open(u16),
    /// Permission `kind` of the panel's site set to `state`.
    Permission { kind: i32, state: i32 },
    /// Delete the panel's site's data (the panel asked first).
    Clear,
    /// Internal: the profile's permission settings (`None`: unreadable),
    /// asked for by open number `serial`. Never parsed.
    Listed { serial: u32, list: Option<Vec<native::PermissionSetting>> },
    /// Internal: a permission change finished. Never parsed.
    Set { serial: u32, kind: i32, state: i32, ok: bool },
    /// Internal: deleting the site's data finished. Never parsed.
    Cleared { serial: u32, ok: bool },
}

impl SiteCmd {
    pub const NAMES: &[&str] = &["site-info", "si-perm", "si-clear"];

    pub fn parse(name: &str, arg: Option<&str>) -> Option<Self> {
        Some(match (name, arg) {
            ("site-info", Some(x)) => Self::Open(x.parse().ok()?),
            ("si-perm", Some(a)) => {
                let (k, s) = a.split_once(':')?;
                let (kind, state) = (k.parse::<i32>().ok()?, s.parse::<i32>().ok()?);
                kind_of(kind)?;
                if ![DEFAULT, ALLOW, BLOCK].contains(&state) {
                    return None;
                }
                Self::Permission { kind, state }
            }
            ("si-clear", None) => Self::Clear,
            _ => return None,
        })
    }
}

/// The panel's state, kept between its opening and its next one.
#[derive(Default)]
pub struct SiteInfo {
    /// The site (`https://host[:port]`) the panel is open, or opening, for.
    origin: String,
    /// Left edge of the mark it hangs under.
    anchor: f64,
    /// Bumped on every opening; an answer for an earlier one is dropped.
    serial: u32,
    /// Asked to open, waiting for the permission list.
    opening: bool,
    /// The site's permissions that are not the default: (kind, state).
    perms: Vec<(i32, i32)>,
    status: &'static str,
    /// Result of the last deletion of a site's data.
    #[cfg(feature = "selftest")]
    pub cleared: Option<bool>,
}

impl SiteInfo {
    #[cfg(feature = "selftest")]
    pub fn origin(&self) -> &str {
        &self.origin
    }
}

/// The origin WebView2 keeps an http(s) page's permissions under:
/// `scheme://host`, with the port only when it is not the scheme's default.
/// `None` for any other address.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "https" => "443",
        "http" => "80",
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#', '\\']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = match host_port.find(']') {
        Some(end) if host_port.starts_with('[') => (&host_port[..=end], host_port[end + 1..].strip_prefix(':')),
        _ => match host_port.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (host_port, None),
        },
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    match port.filter(|p| !p.is_empty() && *p != default_port) {
        Some(p) if p.bytes().all(|b| b.is_ascii_digit()) => Some(format!("{scheme}://{host}:{p}")),
        Some(_) => None,
        None => Some(format!("{scheme}://{host}")),
    }
}

/// Whether two origins name the same site (WebView2 may write one with a
/// trailing slash).
fn same_origin(a: &str, b: &str) -> bool {
    a.trim_end_matches('/').eq_ignore_ascii_case(b.trim_end_matches('/'))
}

/// The non-default permissions of `origin` in `list`, as (kind, state).
pub fn perms_of(list: &[native::PermissionSetting], origin: &str) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = Vec::new();
    for (kind, o, state) in list {
        if same_origin(o, origin) && *state != DEFAULT && kind_of(*kind).is_some() && !out.iter().any(|(k, _)| k == kind) {
            out.push((*kind, *state));
        }
    }
    out
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: i32,
    pub name: &'static str,
    pub state: i32,
    /// The default option's label: "Sor", or "Varsayılan" for autoplay.
    pub default: &'static str,
}

/// The panel's rows: every kind of `KINDS`, then any other kind the site
/// has set.
pub fn rows(perms: &[(i32, i32)]) -> Vec<Row> {
    let state = |id| perms.iter().find(|(k, _)| *k == id).map_or(DEFAULT, |(_, s)| *s);
    let row = |k: &Kind| Row { kind: k.id, name: k.name, state: state(k.id), default: if k.ask { "Sor" } else { "Varsayılan" } };
    let mut out: Vec<Row> = KINDS.iter().map(row).collect();
    out.extend(OTHER_KINDS.iter().filter(|k| perms.iter().any(|(id, _)| *id == k.id)).map(row));
    out
}

#[derive(Serialize)]
struct View<'a> {
    host: &'a str,
    origin: &'a str,
    conn: &'static str,
    rows: Vec<Row>,
    status: &'static str,
}

/// Where the panel goes: under the strip, its left edge near the mark's.
fn panel_rect(size: tao::dpi::LogicalSize<f64>, left: f64, rows: usize) -> wry::Rect {
    let w = PANEL_W.min((size.width - 2.0 * EDGE).max(1.0));
    let x = (left - 4.0).min(size.width - w - EDGE).max(0.0);
    let y = UI_HEIGHT + 2.0;
    let h = (BORDER + HEAD_H + LIST_HEAD_H + rows as f64 * ROW_H + FOOT_H).min((size.height - y - EDGE).max(1.0));
    overlay::rect(x, y, w, h)
}

/// The active tab's site, if it has one.
fn active_origin(app: &App) -> Option<String> {
    let id = app.tabs.active()?;
    origin_of(&app.tabs.get(id)?.url)
}

/// What the indicator shows for the active tab, as one sentence.
fn connection(app: &App) -> &'static str {
    use crate::policy::Security;
    let Some(t) = app.tabs.active().and_then(|id| app.tabs.get(id)) else { return "" };
    if app.policy.is_crashed(t.id) {
        return CONN_CRASHED;
    }
    match app.policy.indicator(t.id, &t.url, &t.page_url) {
        Security::Secure => CONN_SECURE,
        Security::NotSecure => CONN_INSECURE,
        Security::CertError => CONN_CERT,
        Security::None => "",
    }
}

/// The mark: opens the panel for the active tab's site, or closes it.
fn open(app: &mut App, left: f64) {
    if app.overlay.mode == Mode::SiteInfo {
        overlay::close(app);
        app.focus_after_switch();
        return;
    }
    let Some(origin) = active_origin(app) else { return };
    let s = &mut app.site;
    s.serial = s.serial.wrapping_add(1);
    s.origin = origin;
    s.anchor = left;
    s.opening = true;
    s.perms.clear();
    s.status = "";
    let serial = s.serial;
    let proxy = app.proxy().clone();
    let done = move |list| {
        let _ = proxy.send_event(UserEvent::Ui(UiCommand::Site(SiteCmd::Listed { serial, list })));
    };
    match app.content_view() {
        Some(v) => native::site_permissions(v, done),
        None => done(None),
    }
}

/// Draws the panel; `keep`: an update of the panel already up.
fn render(app: &mut App, keep: bool) {
    let host = crate::url::host_of(&app.site.origin).unwrap_or_default();
    let view = View {
        host: &host,
        origin: &app.site.origin,
        conn: connection(app),
        rows: rows(&app.site.perms),
        status: app.site.status,
    };
    let n = view.rows.len();
    let Ok(json) = serde_json::to_string(&view) else { return };
    if !keep {
        crate::popup::close(app, false);
        overlay::hand_over_from_strip(app);
        if app.fullscreen.on() {
            crate::page_tools::leave_fullscreen(app);
        }
    }
    let bounds = panel_rect(overlay::win_size(app), app.site.anchor, n);
    overlay::show(app, Mode::SiteInfo, format!("xbSite.show({json},{keep})"), bounds);
    if !keep && app.overlay.mode == Mode::SiteInfo {
        let _ = app.ui.evaluate_script("xbSecurity.expanded(true)");
    }
}

pub fn on_command(app: &mut App, cmd: SiteCmd) {
    match cmd {
        SiteCmd::Open(left) => open(app, f64::from(left)),
        SiteCmd::Listed { serial, list } => {
            let s = &app.site;
            let up = app.overlay.mode == Mode::SiteInfo;
            if serial != s.serial || !(s.opening || up) {
                return;
            }
            // Opened for a page that is no longer the one shown: not now.
            if s.opening && active_origin(app).as_deref() != Some(s.origin.as_str()) {
                app.site.opening = false;
                return;
            }
            let s = &mut app.site;
            match list {
                Some(list) => s.perms = perms_of(&list, &s.origin),
                None => s.status = SAID_LIST_FAILED,
            }
            let keep = !s.opening;
            s.opening = false;
            render(app, keep);
        }
        SiteCmd::Permission { kind, state } => {
            if app.overlay.mode != Mode::SiteInfo {
                return;
            }
            let (serial, origin) = (app.site.serial, app.site.origin.clone());
            let proxy = app.proxy().clone();
            let done = move |ok| {
                let _ = proxy.send_event(UserEvent::Ui(UiCommand::Site(SiteCmd::Set { serial, kind, state, ok })));
            };
            match app.content_view() {
                Some(v) => native::set_site_permission(v, kind, &origin, state, done),
                None => done(false),
            }
        }
        SiteCmd::Set { serial, kind, state, ok } => {
            if serial != app.site.serial {
                return;
            }
            let s = &mut app.site;
            if ok {
                s.perms.retain(|(k, _)| *k != kind);
                if state != DEFAULT {
                    s.perms.push((kind, state));
                }
                s.status = SAID_SAVED;
            } else {
                s.status = SAID_SAVE_FAILED;
            }
            // Redrawn either way: a failed choice goes back to what it was.
            if app.overlay.mode == Mode::SiteInfo {
                render(app, true);
            }
        }
        SiteCmd::Clear => {
            if app.overlay.mode != Mode::SiteInfo {
                return;
            }
            let (serial, origin) = (app.site.serial, app.site.origin.clone());
            app.site.status = SAID_CLEARING;
            render(app, true);
            let proxy = app.proxy().clone();
            let done = move |ok| {
                let _ = proxy.send_event(UserEvent::Ui(UiCommand::Site(SiteCmd::Cleared { serial, ok })));
            };
            match app.content_view() {
                Some(v) => native::clear_site_data(v, &origin, done),
                None => done(false),
            }
        }
        SiteCmd::Cleared { serial, ok } => {
            #[cfg(feature = "selftest")]
            {
                app.site.cleared = Some(ok);
            }
            if serial != app.site.serial {
                return;
            }
            app.site.status = if ok { SAID_CLEARED } else { SAID_CLEAR_FAILED };
            if app.overlay.mode == Mode::SiteInfo {
                render(app, true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_scheme_host_and_a_port_other_than_the_default() {
        assert_eq!(origin_of("https://Example.COM/a?b#c").as_deref(), Some("https://example.com"));
        assert_eq!(origin_of("https://example.com:443/").as_deref(), Some("https://example.com"));
        assert_eq!(origin_of("http://example.com:80").as_deref(), Some("http://example.com"));
        assert_eq!(origin_of("http://127.0.0.1:8080/x").as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(origin_of("https://user:pw@example.com./").as_deref(), Some("https://example.com"));
        assert_eq!(origin_of("https://[::1]:8443/").as_deref(), Some("https://[::1]:8443"));
        assert_eq!(origin_of("HTTPS://example.com").as_deref(), Some("https://example.com"));
        for bad in [
            "about:blank",
            "file:///C:/x",
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop/p.html",
            "javascript:alert(1)",
            "xb://ui/",
            "https:///x",
            "https://example.com:12a/",
        ] {
            assert_eq!(origin_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_the_sites_own_known_settings_are_listed() {
        let list = vec![
            (3, "https://example.com".to_string(), BLOCK),
            (2, "https://example.com/".to_string(), ALLOW),
            (4, "https://other.example".to_string(), BLOCK),
            (1, "https://example.com:8443".to_string(), BLOCK),
            (99, "https://example.com".to_string(), BLOCK),
            (6, "https://example.com".to_string(), DEFAULT),
            (12, "https://EXAMPLE.com".to_string(), ALLOW),
        ];
        assert_eq!(perms_of(&list, "https://example.com"), vec![(3, BLOCK), (2, ALLOW), (12, ALLOW)]);
    }

    #[test]
    fn rows_offer_the_common_kinds_and_any_other_the_site_set() {
        let r = rows(&[(3, BLOCK), (12, ALLOW)]);
        let names: Vec<_> = r.iter().map(|r| r.name).collect();
        assert_eq!(names, ["Konum", "Kamera", "Mikrofon", "Bildirimler", "Pano okuma", "Otomatik oynatma", "Pencere yönetimi"]);
        assert_eq!((r[0].kind, r[0].state, r[0].default), (3, BLOCK, "Sor"));
        assert_eq!((r[1].state, r[5].default), (DEFAULT, "Varsayılan"));
        assert_eq!((r[6].kind, r[6].state), (12, ALLOW));
        assert_eq!(rows(&[]).len(), KINDS.len());
    }

    #[test]
    fn parses_panel_messages() {
        let p = SiteCmd::parse;
        assert_eq!(p("site-info", Some("212")), Some(SiteCmd::Open(212)));
        assert_eq!(p("si-perm", Some("3:2")), Some(SiteCmd::Permission { kind: 3, state: BLOCK }));
        assert_eq!(p("si-perm", Some("12:0")), Some(SiteCmd::Permission { kind: 12, state: DEFAULT }));
        assert_eq!(p("si-clear", None), Some(SiteCmd::Clear));
        for (n, a) in [
            ("site-info", None),
            ("site-info", Some("-1")),
            ("site-info", Some("70000")),
            ("si-perm", Some("3:3")),
            ("si-perm", Some("3:-1")),
            ("si-perm", Some("0:1")),
            ("si-perm", Some("99:1")),
            ("si-perm", Some("3")),
            ("si-perm", None),
            ("si-clear", Some("https://example.com")),
        ] {
            assert_eq!(p(n, a), None, "{n} {a:?}");
        }
    }

    #[test]
    fn panel_hangs_under_the_mark_inside_the_window() {
        let parts = |r: wry::Rect| {
            let (p, s) = (r.position.to_logical::<f64>(1.0), r.size.to_logical::<f64>(1.0));
            (p.x, p.y, s.width, s.height)
        };
        let size = tao::dpi::LogicalSize::new(1200.0, 800.0);
        let y = UI_HEIGHT + 2.0;
        let h = BORDER + HEAD_H + LIST_HEAD_H + 6.0 * ROW_H + FOOT_H;
        assert_eq!(parts(panel_rect(size, 212.0, 6)), (208.0, y, PANEL_W, h));
        // Never past the right edge, never taller than the room under the strip.
        assert_eq!(parts(panel_rect(size, 1100.0, 6)).0, 1200.0 - PANEL_W - EDGE);
        let small = tao::dpi::LogicalSize::new(300.0, 240.0);
        assert_eq!(parts(panel_rect(small, 100.0, 6)), (EDGE, y, 300.0 - 2.0 * EDGE, 240.0 - y - EDGE));
    }
}
