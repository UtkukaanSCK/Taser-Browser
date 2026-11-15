//! Security indicators and policy of content tabs.
//!
//! - **Connection indicator**: `security_of` derives what the address box
//!   shows for the active tab from its committed URL (`render_js`, drawn by
//!   `ui/js/security.js`).
//! - **Certificate errors**: WebView2 keeps its own interstitial; the host is
//!   remembered here (`native::attach_policy` reports it), so the indicator
//!   says "certificate error" for that host until the app exits, also after
//!   the user chose to proceed.
//! - **External URI schemes** (`mailto:` and the like): WebView2 never starts
//!   the application (`native::attach_policy` cancels every launch). A launch
//!   the user asked for becomes a `PendingExternalLaunch`, which only
//!   `ExternalLaunch::confirm` can start. Nothing calls `confirm` yet: the
//!   confirmation popup is the overlay's job (see "For the overlay" below).
//! - **`file:`**: a content tab never loads it (`refuses_navigation`).
//! - **IDN**: the address box shows hosts in ASCII only (`ascii_host`).
//! - **Crashed tab**: remembered until the tab loads a page again, shown in
//!   the indicator, which then is a reload button.
//!
//! # For the overlay (external-launch confirmation)
//!
//! `app.policy.launch.pending(now)` is the request to show: `scheme`,
//! `origin` (the asking page, may be empty) and `uri`. It always belongs to
//! the active tab. The popup's buttons call
//! `app.policy.launch.confirm(tab, uri, now)` with the tab and URI the popup
//! was showing (anything else is refused) or `dismiss()`. A request goes
//! away by itself on a tab switch, a navigation or close of its tab and
//! after `LAUNCH_TIMEOUT`; the places are marked "overlay:" below.

use crate::app::{App, Failure, PageEvent};
use crate::tabs::TabId;
use std::borrow::Cow;
use std::time::{Duration, Instant};

/// What the address box says about the connection of the shown page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// Internal and blank pages: no mark.
    None,
    /// https with a certificate WebView2 accepted.
    Secure,
    /// Plain http.
    NotSecure,
    /// https to a host whose certificate failed in this run.
    CertError,
}

fn scheme_of(url: &str) -> Option<&str> {
    let (scheme, _) = url.split_once(':')?;
    let mut bytes = scheme.bytes();
    let ok = bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    ok.then_some(scheme)
}

/// The indicator for a committed URL. `bad_cert` says whether a host's
/// certificate failed.
pub fn security_of(url: &str, bad_cert: impl Fn(&str) -> bool) -> Security {
    let Some(scheme) = scheme_of(url) else { return Security::None };
    if scheme.eq_ignore_ascii_case("http") {
        Security::NotSecure
    } else if scheme.eq_ignore_ascii_case("https") {
        match crate::url::host_of(url) {
            Some(host) if bad_cert(&host) => Security::CertError,
            _ => Security::Secure,
        }
    } else {
        Security::None
    }
}

/// URLs a content tab never loads as its page: the UI's own host, and local
/// files. The address bar and a second start refuse `file:` already
/// (`url::resolve_input`, `url::is_tab_url`); this is the path left over: a
/// file dropped on the page, which WebView2 would open in place of the page.
pub fn refuses_navigation(url: &str) -> bool {
    crate::ui::is_ui_url(url) || scheme_of(url).is_some_and(|s| s.eq_ignore_ascii_case("file"))
}

// ---- IDN ----------------------------------------------------------------

/// Byte range of the host in an absolute `scheme://` URL (without userinfo
/// and port).
fn host_span(url: &str) -> Option<std::ops::Range<usize>> {
    let start = scheme_of(url)?.len() + 1;
    let start = start + url[start..].starts_with("//").then_some(2)?;
    let end = start + url[start..].find(['/', '?', '#', '\\']).unwrap_or(url.len() - start);
    let start = url[start..end].rfind('@').map_or(start, |at| start + at + 1);
    let host = &url[start..end];
    let len = match host.find(']') {
        Some(close) if host.starts_with('[') => close + 1,
        _ => host.find(':').unwrap_or(host.len()),
    };
    Some(start..start + len)
}

/// Punycode (RFC 3492) of one label, without the `xn--` prefix.
fn punycode(label: &str) -> String {
    const BASE: u64 = 36;
    const T_MIN: u64 = 1;
    const T_MAX: u64 = 26;
    const SKEW: u64 = 38;
    const DAMP: u64 = 700;
    let digit = |d: u64| (if d < 26 { b'a' + d as u8 } else { b'0' + (d - 26) as u8 }) as char;
    let chars: Vec<u64> = label.chars().map(|c| u64::from(u32::from(c))).collect();
    let mut out: String = label.chars().filter(char::is_ascii).collect();
    let basic = out.len() as u64;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias, mut done) = (0x80u64, 0u64, 72u64, basic);
    while done < chars.len() as u64 {
        // The smallest code point not yet written. 64-bit sums cannot
        // overflow for any URL short enough to be a tab's address.
        let next = chars.iter().copied().filter(|&c| c >= n).min().unwrap_or(n);
        delta += (next - n) * (done + 1);
        n = next;
        for &c in &chars {
            if c < n {
                delta += 1;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = k.saturating_sub(bias).clamp(T_MIN, T_MAX);
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                // Bias adaptation.
                let mut d = if done == basic { delta / DAMP } else { delta / 2 };
                d += d / (done + 1);
                let mut k = 0;
                while d > ((BASE - T_MIN) * T_MAX) / 2 {
                    d /= BASE - T_MIN;
                    k += BASE;
                }
                bias = k + (BASE - T_MIN + 1) * d / (d + SKEW);
                delta = 0;
                done += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    out
}

/// `url` with an ASCII-only host, for the address box.
///
/// A Unicode host can look exactly like another site's (Cyrillic "а" for
/// Latin "a"), and telling a deceptive name from an honest one needs script
/// tables this app does not carry. So no host is ever shown in Unicode: each
/// non-ASCII label is lowercased and written in its `xn--` form, which cannot
/// be mistaken for a plain ASCII name. WebView2 reports a page's address in
/// that form already; this covers what comes from elsewhere (the text the
/// user typed, until the page reports its own address, and restored tabs).
/// It is for display only: unlike the network stack it applies no IDNA
/// mapping, so for unusual input the label may differ from the one that is
/// requested, but it is always ASCII.
pub fn ascii_host(url: &str) -> Cow<'_, str> {
    let Some(span) = host_span(url).filter(|s| !url[s.clone()].is_ascii()) else { return Cow::Borrowed(url) };
    let host = url[span.clone()]
        .split('.')
        .map(|label| if label.is_ascii() { label.to_string() } else { format!("xn--{}", punycode(&label.to_lowercase())) })
        .collect::<Vec<_>>()
        .join(".");
    Cow::Owned(format!("{}{host}{}", &url[..span.start], &url[span.end..]))
}

// ---- External URI schemes ------------------------------------------------

/// Longest URI a launch request may carry, in bytes. Longer ones are
/// refused, not cut: a cut URI is a different URI.
pub const MAX_LAUNCH_URI: usize = 2048;

/// How long a request waits for the user's answer.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Schemes that never go to the system, confirmed or not.
///
/// - Not external at all: the browser's own and script schemes. Handing them
///   to the shell would open a local file or run script in whatever program
///   is registered for them.
/// - Windows shell and legacy help/search handlers. They open Explorer
///   views, local help files or search results chosen by the page, and have
///   been used for exploits (`search-ms` remote search folders, `res`/`mk`/
///   `its`/`mhtml` local resource and help-file loading, `hcp` Help Center).
const DENIED_SCHEMES: &[&str] = &[
    "http", "https", "file", "javascript", "data", "vbscript", "about", "blob", "view-source", // not external
    "shell", "search", "search-ms", "res", "mk", "its", "mhtml", "hcp", // shell and legacy handlers
];

/// Every `ms-` scheme is refused as well. That namespace is Windows' own:
/// settings pages (`ms-settings:`), installers (`ms-appinstaller:`, abused
/// to install malware, CVE-2021-43890), diagnostic tools (`ms-msdt:`,
/// "Follina", CVE-2022-30190), Office command lines (`ms-officecmd:`,
/// `ms-word:` and the like, which open remote documents). A web page has no
/// need for any of them, and new ones appear with Windows updates, so the
/// prefix is refused rather than a list kept. The cost: a Microsoft Store
/// link (`ms-windows-store:`) does not open from a page.
const DENIED_PREFIX: &str = "ms-";

/// The scheme of a URI the system may be asked to open, or None.
///
/// The launcher is `ShellExecuteW`, which also runs programs and opens
/// paths. So the text must be a URI and nothing else: a syntactically valid
/// scheme of at least two letters (one letter is a drive, `C:\...`), not on
/// the deny list, and only visible ASCII without quotes. WebView2 reports
/// URIs escaped that way; a control character, a space or a quote could
/// split the command line of the program that handles the scheme.
pub fn launchable_scheme(uri: &str) -> Option<String> {
    let scheme = scheme_of(uri)?.to_ascii_lowercase();
    let ok = scheme.len() >= 2
        && uri.len() <= MAX_LAUNCH_URI
        && uri.bytes().all(|b| b.is_ascii_graphic() && b != b'"')
        && !DENIED_SCHEMES.contains(&scheme.as_str())
        && !scheme.starts_with(DENIED_PREFIX);
    ok.then_some(scheme)
}

/// A page's request, made with a user gesture, to open a URI in another
/// application. Waiting for the user's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingExternalLaunch {
    pub tab: TabId,
    /// Lowercase, e.g. `mailto`.
    pub scheme: String,
    /// The whole URI, at most `MAX_LAUNCH_URI` bytes of visible ASCII.
    pub uri: String,
    /// Origin of the asking page as WebView2 reports it; may be empty.
    pub origin: String,
    asked: Instant,
}

/// Why `ExternalLaunch::confirm` did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing is waiting (never asked, answered, dropped or timed out).
    NoRequest,
    /// The tab or URI is not the one waiting. The request is dropped.
    Mismatch,
    /// The system could not open it.
    LaunchFailed,
}

/// Starts the application for a URI; false if it could not.
pub type Launcher = fn(&str) -> bool;

/// Tests and the self-test never reach the real launcher: until they set a
/// recording one, nothing is launched.
#[cfg(any(test, feature = "selftest"))]
const DEFAULT_LAUNCHER: Launcher = |_| false;
#[cfg(not(any(test, feature = "selftest")))]
const DEFAULT_LAUNCHER: Launcher = crate::native::open_external;

/// At most one external launch waiting for the user's answer.
pub struct ExternalLaunch {
    pending: Option<PendingExternalLaunch>,
    launcher: Launcher,
}

impl Default for ExternalLaunch {
    fn default() -> Self {
        Self { pending: None, launcher: DEFAULT_LAUNCHER }
    }
}

// Until the overlay exists nothing outside tests answers a request.
#[cfg_attr(not(any(test, feature = "selftest")), allow(dead_code))]
impl ExternalLaunch {
    #[cfg(any(test, feature = "selftest"))]
    pub fn set_launcher(&mut self, launcher: Launcher) {
        self.launcher = launcher;
    }

    /// A page in `tab` asked to open `uri`. Replaces an older request; true
    /// if this one now waits (false: refused by `launchable_scheme`).
    pub fn request(&mut self, tab: TabId, uri: &str, origin: &str, now: Instant) -> bool {
        self.pending = launchable_scheme(uri).map(|scheme| PendingExternalLaunch {
            tab,
            scheme,
            uri: uri.to_string(),
            origin: origin.chars().filter(|c| !c.is_control()).take(256).collect(),
            asked: now,
        });
        self.pending.is_some()
    }

    /// The request waiting at `now`, if any.
    pub fn pending(&self, now: Instant) -> Option<&PendingExternalLaunch> {
        self.pending.as_ref().filter(|p| now.saturating_duration_since(p.asked) < LAUNCH_TIMEOUT)
    }

    /// The user agreed to open `uri` for `tab`. Launches only the request
    /// that is waiting, exactly as it was asked, and only once: whatever the
    /// outcome, nothing waits afterwards.
    pub fn confirm(&mut self, tab: TabId, uri: &str, now: Instant) -> Result<(), Refused> {
        let waiting = self.pending(now).is_some();
        let Some(p) = self.pending.take().filter(|_| waiting) else { return Err(Refused::NoRequest) };
        // Checked again here, so that no later change to `request` can make
        // this the place an unchecked URI reaches the system.
        if p.tab != tab || p.uri != uri || launchable_scheme(&p.uri).is_none() {
            return Err(Refused::Mismatch);
        }
        if (self.launcher)(&p.uri) { Ok(()) } else { Err(Refused::LaunchFailed) }
    }

    /// The user said no (or closed the popup).
    pub fn dismiss(&mut self) {
        self.pending = None;
    }

    /// `tab` navigated or closed: its request is void.
    pub fn drop_for(&mut self, tab: TabId) {
        if self.pending.as_ref().is_some_and(|p| p.tab == tab) {
            self.pending = None;
        }
    }
}

// ---- App state and bindings ------------------------------------------------

/// From a content webview (`native::attach_policy`). Untrusted.
#[derive(Debug)]
pub enum Event {
    /// The certificate of this request's server failed verification.
    CertError(String),
    /// A page finished loading without error.
    Loaded,
    /// WebView2 was about to open `uri` in another application (and was
    /// stopped). `user`: the page had a user gesture for it.
    ExternalLaunch { uri: String, origin: String, user: bool },
}

/// Hosts remembered for a bad certificate; the oldest are forgotten.
const MAX_BAD_CERT_HOSTS: usize = 64;

#[derive(Default)]
pub struct State {
    /// Hosts whose certificate failed in this run. Not per tab and not
    /// cleared: once the user proceeds through WebView2's interstitial, the
    /// host loads without another error for the rest of the run, in every
    /// tab, and must keep its mark.
    bad_cert: Vec<String>,
    /// Tabs whose renderer exited and that have not loaded a page since.
    crashed: Vec<TabId>,
    pub launch: ExternalLaunch,
    /// Launches without a user gesture, dropped unseen.
    #[cfg(feature = "selftest")]
    pub silent_drops: u32,
}

impl State {
    pub fn security(&self, url: &str) -> Security {
        security_of(url, |host| self.bad_cert.iter().any(|h| h == host))
    }

    pub fn is_crashed(&self, tab: TabId) -> bool {
        self.crashed.contains(&tab)
    }
}

pub fn on_event(app: &mut App, tab: TabId, ev: Event) {
    if app.tabs.get(tab).is_none() {
        return;
    }
    match ev {
        Event::CertError(uri) => {
            let Some(host) = crate::url::host_of(&uri).filter(|h| !h.is_empty() && h.len() <= 255) else { return };
            let hosts = &mut app.policy.bad_cert;
            hosts.retain(|h| *h != host);
            hosts.push(host);
            if hosts.len() > MAX_BAD_CERT_HOSTS {
                hosts.remove(0);
            }
        }
        Event::Loaded => app.policy.crashed.retain(|t| *t != tab),
        Event::ExternalLaunch { user: false, .. } => {
            #[cfg(feature = "selftest")]
            {
                app.policy.silent_drops += 1;
            }
            return;
        }
        Event::ExternalLaunch { uri, origin, user: true } => {
            // Only the page the user is looking at may ask.
            if app.tabs.active() == Some(tab) {
                // overlay: when this returns true, show the popup.
                app.policy.launch.request(tab, &uri, &origin, Instant::now());
            }
            return;
        }
    }
    app.mark_dirty();
}

/// Called for every event of a content tab, before the app handles it.
pub fn on_page(app: &mut App, tab: TabId, ev: &PageEvent) {
    // A request belongs to the page that made it.
    if matches!(ev, PageEvent::Loading(true) | PageEvent::CloseRequested) {
        // overlay: close the popup if it shows this tab's request.
        app.policy.launch.drop_for(tab);
    }
}

/// The shown tab changes (switch, close, new tab).
pub fn on_switch(app: &mut App) {
    // overlay: close the popup.
    app.policy.launch.dismiss();
}

/// A content tab's renderer failed. A hung renderer may still answer again;
/// only one that exited leaves a dead page behind.
pub fn on_page_failed(app: &mut App, tab: TabId, failure: Failure) {
    app.policy.launch.drop_for(tab);
    if failure == Failure::RendererExited && !app.policy.crashed.contains(&tab) {
        app.policy.crashed.push(tab);
    }
}

/// The indicator's state for the active tab, as the script `App::flush`
/// appends to its render call. Also forgets tabs that are gone.
pub fn render_js(app: &mut App) -> String {
    let tabs = &app.tabs;
    app.policy.crashed.retain(|t| tabs.get(*t).is_some());
    let active = tabs.active().and_then(|id| tabs.get(id));
    let state = match active {
        Some(t) if app.policy.is_crashed(t.id) => "crashed",
        Some(t) => match app.policy.security(&t.url) {
            Security::None => "",
            Security::Secure => "secure",
            Security::NotSecure => "insecure",
            Security::CertError => "cert",
        },
        None => "",
    };
    format!("window.xbSecurity&&xbSecurity.render(\"{state}\")")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn indicator_follows_scheme_and_certificate() {
        let none = |_: &str| false;
        assert_eq!(security_of("https://example.com/", none), Security::Secure);
        assert_eq!(security_of("HTTPS://Example.com/a?b#c", none), Security::Secure);
        assert_eq!(security_of("http://example.com/", none), Security::NotSecure);
        assert_eq!(security_of("http://localhost:8080/", none), Security::NotSecure);
        for internal in ["", "about:blank", "data:text/html,x", "blob:https://a.com/1", "edge://gpu", "not a url", "file:///C:/a"] {
            assert_eq!(security_of(internal, none), Security::None, "{internal}");
        }
        // "https" somewhere else in the text is not the scheme.
        assert_eq!(security_of("about:https://example.com/", none), Security::None);
        assert_eq!(security_of(" https://example.com/", none), Security::None);
    }

    #[test]
    fn bad_certificate_marks_exactly_that_host() {
        let state = State { bad_cert: vec!["bad.example".into()], ..Default::default() };
        assert_eq!(state.security("https://bad.example/"), Security::CertError);
        assert_eq!(state.security("https://user@BAD.example.:8443/path"), Security::CertError);
        assert_eq!(state.security("https://good.example/?u=https://bad.example/"), Security::Secure);
        assert_eq!(state.security("https://sub.bad.example/"), Security::Secure);
        assert_eq!(state.security("https://bad.example.evil.com/"), Security::Secure);
        // Plain http has no certificate to be wrong; it is not secure anyway.
        assert_eq!(state.security("http://bad.example/"), Security::NotSecure);
    }

    #[test]
    fn content_never_loads_files_or_the_ui_host() {
        assert!(refuses_navigation("file:///C:/Windows/win.ini"));
        assert!(refuses_navigation("FILE://server/share/a.html"));
        assert!(refuses_navigation("file:C:/a"));
        assert!(refuses_navigation("http://xb.ui/index.html"));
        assert!(!refuses_navigation("https://example.com/file:///C:/a"));
        assert!(!refuses_navigation("https://example.com/"));
        assert!(!refuses_navigation("about:blank"));
        assert!(!refuses_navigation("filesystem:https://a.com/x"));
    }

    #[test]
    fn punycode_matches_known_names() {
        assert_eq!(punycode("bücher"), "bcher-kva");
        assert_eq!(punycode("münchen"), "mnchen-3ya");
        assert_eq!(punycode("例え"), "r8jz45g");
        assert_eq!(punycode("türkçe"), "trke-2oa7j");
        assert_eq!(punycode("аррӏе"), "80ak6aa92e");
        // RFC 3492, 7.1 (L): 3<nen>B<gumi><kinpachi><sensei>.
        assert_eq!(punycode("3年b組金八先生"), "3b-ww4c5e180e575a65lsy2b");
    }

    #[test]
    fn address_box_host_is_always_ascii() {
        assert_eq!(ascii_host("https://example.com/yol?q=ü"), "https://example.com/yol?q=ü", "only the host matters");
        assert!(matches!(ascii_host("https://example.com/ü"), Cow::Borrowed(_)));
        assert_eq!(ascii_host("https://bücher.example/a?b#c"), "https://xn--bcher-kva.example/a?b#c");
        assert_eq!(ascii_host("https://BÜCHER.example"), "https://xn--bcher-kva.example");
        // The lookalike of apple.com, in Cyrillic.
        assert_eq!(ascii_host("https://аррӏе.com/"), "https://xn--80ak6aa92e.com/");
        assert_eq!(ascii_host("http://user:pw@www.münchen.de:8080/x"), "http://user:pw@www.xn--mnchen-3ya.de:8080/x");
        assert_eq!(ascii_host("https://例え.jp"), "https://xn--r8jz45g.jp");
        // An ideographic full stop is not ASCII either: it ends up inside a label.
        assert!(ascii_host("https://a。b/").is_ascii());
        for other in ["", "about:blank", "ü", "mailto:ü@example.com", "https://[::1]:8080/ü"] {
            assert_eq!(ascii_host(other), other);
        }
        for url in ["https://ü", "https://ü.ö/", "https://@ü:1", "x://üü.üü?ü", "https://a.ü\\ü"] {
            let shown = ascii_host(url);
            let host = host_span(&shown).map(|s| shown[s].to_string()).unwrap();
            assert!(host.is_ascii() && host.contains("xn--"), "{url} -> {shown}");
        }
    }

    #[test]
    fn only_real_uris_of_harmless_schemes_may_launch() {
        assert_eq!(launchable_scheme("mailto:someone@example.com?subject=Hi%20there").as_deref(), Some("mailto"));
        assert_eq!(launchable_scheme("TEL:+901234567").as_deref(), Some("tel"));
        assert_eq!(launchable_scheme("msteams:/l/meetup-join/1").as_deref(), Some("msteams"), "not the ms- namespace");
        for denied in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "data:text/html,x",
            "vbscript:msgbox(1)",
            "https://example.com/",
            "about:blank",
            "ms-settings:privacy",
            "MS-MSDT:/id%20PCWDiagnostic",
            "ms-appinstaller:?source=https://example.com/a.msix",
            "ms-word:ofe|u|https://example.com/a.docx",
            "search-ms:query=x&crumb=location:%5C%5Cexample.com%5Cshare",
            "shell:startup",
        ] {
            assert_eq!(launchable_scheme(denied), None, "{denied}");
        }
        // Not URIs: the shell would run or open these.
        for not_uri in ["", "calc.exe", r"C:\Windows\System32\calc.exe", "c:/x", r"\\server\share\a.exe", ":x", "1a:x", "a b:x"] {
            assert_eq!(launchable_scheme(not_uri), None, "{not_uri}");
        }
        for unsafe_text in ["mailto:a@b.c\n", "mailto:a\u{0}b", "mailto:a b", "mailto:a\"b", "mailto:a\u{7f}", "mailto:ü@b.c", "mailto:\u{202e}a"] {
            assert_eq!(launchable_scheme(unsafe_text), None, "{unsafe_text:?}");
        }
        let at_cap = format!("mailto:{}", "a".repeat(MAX_LAUNCH_URI - 7));
        assert!(launchable_scheme(&at_cap).is_some());
        assert!(launchable_scheme(&format!("{at_cap}a")).is_none(), "one byte over is refused");
    }

    thread_local! {
        static LAUNCHED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    fn recording() -> ExternalLaunch {
        LAUNCHED.with_borrow_mut(Vec::clear);
        let mut launch = ExternalLaunch::default();
        launch.set_launcher(|uri| {
            LAUNCHED.with_borrow_mut(|l| l.push(uri.to_string()));
            true
        });
        launch
    }

    fn launched() -> Vec<String> {
        LAUNCHED.with_borrow(Clone::clone)
    }

    const MAIL: &str = "mailto:someone@example.com";

    #[test]
    fn default_launcher_in_tests_launches_nothing() {
        let now = Instant::now();
        let mut launch = ExternalLaunch::default();
        assert!(launch.request(1, MAIL, "https://example.com", now));
        assert_eq!(launch.confirm(1, MAIL, now), Err(Refused::LaunchFailed));
    }

    #[test]
    fn confirm_launches_the_waiting_request_once() {
        let now = Instant::now();
        let mut launch = recording();
        assert_eq!(launch.confirm(1, MAIL, now), Err(Refused::NoRequest));
        assert!(launch.request(1, MAIL, "https://example.com\n", now));
        let p = launch.pending(now).unwrap();
        assert_eq!((p.tab, p.scheme.as_str(), p.uri.as_str(), p.origin.as_str()), (1, "mailto", MAIL, "https://example.com"));
        assert!(launched().is_empty(), "asking launches nothing");
        assert_eq!(launch.confirm(1, MAIL, now), Ok(()));
        assert_eq!(launched(), [MAIL]);
        assert_eq!(launch.confirm(1, MAIL, now), Err(Refused::NoRequest), "one answer per request");
        assert_eq!(launched().len(), 1);
    }

    #[test]
    fn confirm_refuses_anything_but_the_waiting_request() {
        let now = Instant::now();
        let mut launch = recording();
        for (tab, uri) in [(1, "mailto:other@example.com"), (1, "mailto:someone@example.com "), (1, "calc.exe"), (2, MAIL)] {
            assert!(launch.request(1, MAIL, "", now));
            assert_eq!(launch.confirm(tab, uri, now), Err(Refused::Mismatch), "{tab} {uri}");
            assert!(launch.pending(now).is_none(), "a wrong answer drops the request");
        }
        assert!(launched().is_empty());
    }

    #[test]
    fn refused_request_leaves_nothing_waiting() {
        let now = Instant::now();
        let mut launch = recording();
        assert!(launch.request(1, MAIL, "", now));
        assert!(!launch.request(1, "ms-settings:privacy", "", now));
        assert!(launch.pending(now).is_none(), "the older request is gone too");
        assert_eq!(launch.confirm(1, "ms-settings:privacy", now), Err(Refused::NoRequest));
        assert_eq!(launch.confirm(1, MAIL, now), Err(Refused::NoRequest));
        assert!(launched().is_empty());
    }

    #[test]
    fn request_is_dropped_by_dismiss_navigation_and_time() {
        let now = Instant::now();
        let mut launch = recording();
        assert!(launch.request(1, MAIL, "", now));
        launch.dismiss();
        assert_eq!(launch.confirm(1, MAIL, now), Err(Refused::NoRequest));

        assert!(launch.request(1, MAIL, "", now));
        launch.drop_for(2);
        assert!(launch.pending(now).is_some(), "another tab's navigation does not matter");
        launch.drop_for(1);
        assert_eq!(launch.confirm(1, MAIL, now), Err(Refused::NoRequest));

        assert!(launch.request(1, MAIL, "", now));
        let late = now + LAUNCH_TIMEOUT;
        assert!(launch.pending(late - Duration::from_millis(1)).is_some());
        assert!(launch.pending(late).is_none());
        assert_eq!(launch.confirm(1, MAIL, late), Err(Refused::NoRequest));
        assert!(launched().is_empty());
    }
}
