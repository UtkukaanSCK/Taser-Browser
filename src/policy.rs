//! Security indicators and policy of content tabs.
//!
//! - **Connection indicator**: `State::indicator` derives what the address
//!   box shows for the active tab from the page that is on screen (`render_js`,
//!   drawn by `ui/js/security.js`). An address that was typed but has not
//!   loaded yet gets no mark, and neither does an error page.
//! - **Certificate errors**: WebView2 keeps its own interstitial. When the
//!   failing request is the tab's own navigation (`is_main_navigation`), its
//!   host (every port, `cert_host`) is remembered here, so the indicator
//!   says "certificate error" for it until the app exits, also after the user
//!   chose to proceed. The list is never shortened: when it is full, no https
//!   page gets a lock any more (`State::cert_error`).
//! - **External URI schemes** (`mailto:` and the like): WebView2 never starts
//!   the application (`native::attach_policy` cancels every launch). A launch
//!   the user asked for becomes a `PendingExternalLaunch`, which only
//!   `ExternalLaunch::confirm` can start, after the user said yes to the
//!   overlay's question (see "The question" below).
//! - **`file:`**: a content tab never loads it (`refuses_navigation`).
//! - **Address box**: hosts in ASCII only and no `user:password@`
//!   (`display`).
//! - **Crashed tab**: remembered until the tab navigates again, shown in the
//!   indicator together with a reload button.
//!
//! # The question (external-launch confirmation)
//!
//! A waiting request and the overlay in `Mode::Launch` go together. The
//! overlay shows the request's scheme, asking origin and whole URI
//! (`show_question`); its page answers with `LaunchCmd` only, which carries
//! no data: "open" starts the request held here, never anything the page
//! sends. Whatever closes the overlay (Esc, a click elsewhere, a tab switch,
//! a navigation, a resize, the timeout) answers "no" (`overlay::close`), and
//! a request dropped here closes the overlay (`sync_question`).
//! A second request while one is shown answers the shown one "no" and is
//! dropped itself. "Aç" counts only after a pause in pressing of
//! `ANSWER_DELAY`: since the question came on screen, and since the last
//! press inside it that came sooner than that (`ExternalLaunch::may_answer`,
//! `ExternalLaunch::pressed`); and only from the overlay's own page
//! (`ui::ui_builder`). If the application then does not start, the question
//! stays to say so.

use crate::app::{App, Failure, PageEvent};
use crate::overlay::Mode;
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

/// The indicator for a committed URL. `bad_cert` says whether a certificate
/// of a host (`cert_host`) failed.
pub fn security_of(url: &str, bad_cert: impl Fn(&str) -> bool) -> Security {
    let Some(scheme) = scheme_of(url) else { return Security::None };
    if scheme.eq_ignore_ascii_case("http") {
        // This machine talking to itself crosses no network: no warning.
        if crate::url::host_of(url).is_some_and(|h| is_loopback(&h)) { Security::None } else { Security::NotSecure }
    } else if scheme.eq_ignore_ascii_case("https") {
        match cert_host(url) {
            Some(host) if bad_cert(&host) => Security::CertError,
            Some(_) => Security::Secure,
            // No server can be named: nothing to vouch for.
            None => Security::None,
        }
    } else {
        Security::None
    }
}

/// `localhost`, `*.localhost`, `127.x.x.x` and `[::1]`.
fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
        || host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .is_some_and(|h| h.parse::<std::net::Ipv6Addr>().is_ok_and(|ip| ip.is_loopback()))
}

/// `host:port` of an http(s) or ws(s) URL, lowercase, the scheme's port when
/// none is written: the server a request goes to (`is_main_navigation`).
fn site_key(url: &str) -> Option<String> {
    let default = match scheme_of(url)?.to_ascii_lowercase().as_str() {
        "https" | "wss" => 443,
        "http" | "ws" => 80,
        _ => return None,
    };
    let span = host_span(url)?;
    let host = url[span.clone()].trim_end_matches('.').to_ascii_lowercase();
    let rest = &url[span.end..];
    let rest = &rest[..rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len())];
    let port = match rest.strip_prefix(':') {
        Some("") => default,
        Some(port) => port.parse::<u16>().ok()?,
        None if rest.is_empty() => default,
        None => return None,
    };
    (!host.is_empty() && host.len() <= 255).then(|| format!("{host}:{port}"))
}

/// The host of an http(s) or ws(s) URL (`site_key` without the port): what
/// a certificate error is remembered for. Chromium keeps the user's "proceed
/// anyway" per host, not per port: a warning passed on `host:8443` lets the
/// same certificate through on `host:443` without another one, so the mark
/// must cover every port of the host.
fn cert_host(url: &str) -> Option<String> {
    let mut key = site_key(url)?;
    key.truncate(key.rfind(':')?);
    Some(key)
}

/// Whether the failed request `request` is the page itself: the tab's main
/// navigation is to that server. An image, script or frame from a server
/// with a bad certificate is refused by WebView2 and says nothing about the
/// page's own connection. (A navigation that started as http may have been
/// upgraded to https before the request was made.)
fn is_main_navigation(request: &str, nav: &str) -> bool {
    let https = |u: &str| scheme_of(u).is_some_and(|s| s.eq_ignore_ascii_case("https"));
    let (Some(req), Some(to)) = (site_key(request), site_key(nav)) else { return false };
    https(request) && (req == to && https(nav) || req.strip_suffix(":443").is_some_and(|h| to.strip_suffix(":80") == Some(h)) && !https(nav))
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
///
/// No real name is longer than 253 bytes or has a label over 63 characters.
/// Such a host is not converted (the conversion's cost grows with the square
/// of the label) and not shown either: `UNSHOWN_HOST` stands in its place.
pub fn ascii_host(url: &str) -> Cow<'_, str> {
    let Some(span) = host_span(url).filter(|s| !url[s.clone()].is_ascii()) else { return Cow::Borrowed(url) };
    let name = &url[span.clone()];
    let host = if name.len() > 253 || name.split('.').any(|label| label.chars().count() > 63) {
        UNSHOWN_HOST.to_string()
    } else {
        name.split('.')
            .map(|label| if label.is_ascii() { label.to_string() } else { format!("xn--{}", punycode(&label.to_lowercase())) })
            .collect::<Vec<_>>()
            .join(".")
    };
    Cow::Owned(format!("{}{host}{}", &url[..span.start], &url[span.end..]))
}

/// Shown in place of a non-ASCII host too long to be a real name. `.invalid`
/// is reserved (RFC 2606): it is no site, whoever reads or submits it.
const UNSHOWN_HOST: &str = "gecersiz-ad.invalid";

/// What the address box shows for a URL: an ASCII host (`ascii_host`), and no
/// `user:password@`. Text before the `@` is not where the page comes from
/// (`https://bank.example@evil.example/`), and a password has no place on
/// screen.
pub fn display(url: &str) -> Cow<'_, str> {
    let shown = ascii_host(url);
    let Some(host) = host_span(&shown) else { return shown };
    let Some(start) = scheme_of(&shown).map(|s| s.len() + 3).filter(|s| *s < host.start) else { return shown };
    Cow::Owned(format!("{}{}", &shown[..start], &shown[host.start..]))
}

// ---- External URI schemes ------------------------------------------------

/// Longest URI a launch request may carry, in bytes. Longer ones are
/// refused, not cut: a cut URI is a different URI.
pub const MAX_LAUNCH_URI: usize = 2048;

/// How long a request waits for the user's answer.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);

/// "Aç" does nothing until the question has been on screen this long, and
/// until this long after the last press inside it that came too early (see
/// `ExternalLaunch::may_answer`). The page shows the button as off meanwhile
/// and reports those presses (`LaunchCmd::Press`); the rule is kept here.
pub const ANSWER_DELAY: Duration = Duration::from_millis(500);

/// Schemes that never go to the system, confirmed or not.
///
/// - Not external at all: the browser's own and script schemes. Handing them
///   to the shell would open a local file or run script in whatever program
///   is registered for them.
/// - Windows shell and legacy help/search handlers. They open Explorer
///   views, local help files or search results chosen by the page, and have
///   been used for exploits (`search-ms` remote search folders, `res`/`mk`/
///   `its`/`mhtml` local resource and help-file loading, `hcp` Help Center).
/// - Handlers that fetch and open or run what the page names: OneNote
///   (`onenote`, `onenote-cmd`), Edge and Internet Explorer taking a URL from
///   the page (`microsoft-edge`, `ie.http`), Java Web Start (`jnlp`, `jnlps`:
///   downloads and runs an application), network shares and disk images
///   (`afp`, `disk`, `disks`), and old news and radio handlers (`nntp`,
///   `vnd.ms.radio`).
///
/// A deny list on purpose: an allow list would stop every handler nobody
/// thought of (`steam`, `spotify`, `vscode`), and each launch is still asked.
const DENIED_SCHEMES: &[&str] = &[
    "http", "https", "file", "javascript", "data", "vbscript", "about", "blob", "view-source", // not external
    "shell", "search", "search-ms", "res", "mk", "its", "mhtml", "hcp", // shell and legacy handlers
    "onenote", "onenote-cmd", "microsoft-edge", "ie.http", "jnlp", "jnlps", "afp", "disk", "disks", "nntp", "vnd.ms.radio", // fetch-and-open handlers
];

/// Every `ms-` scheme is refused as well. That namespace is Windows' own:
/// settings pages (`ms-settings:`), installers (`ms-appinstaller:`, abused
/// to install malware, CVE-2021-43890), diagnostic tools (`ms-msdt:`,
/// "Follina", CVE-2022-30190), Office command lines (`ms-officecmd:`,
/// `ms-word:` and the like, which open remote documents). A web page has no
/// need for any of them, and new ones appear with Windows updates, so the
/// prefix is refused rather than a list kept. The one exception is the
/// Microsoft Store (`ms-windows-store:`): sites link to their Store page, and
/// it only shows a page of the Store, after the user's yes like any other.
const DENIED_PREFIX: &str = "ms-";
const ALLOWED_MS: &str = "ms-windows-store";

/// The scheme of a URI the system may be asked to open, or None.
///
/// The launcher is the shell, which also runs programs and opens paths, and
/// which works out what the text is from the text itself. So it must be a
/// URI and nothing else, and this is checked before a request is even kept:
/// a syntactically valid scheme (a letter, then letters, digits, `+`, `-`,
/// `.`) of at least two characters (one letter is a drive, `C:\...`), not on
/// the deny list, and only visible ASCII without quotes. WebView2 reports
/// URIs escaped that way; a control character, a space or a quote could
/// split the command line of the program that handles the scheme.
pub fn launchable_scheme(uri: &str) -> Option<String> {
    let scheme = scheme_of(uri)?.to_ascii_lowercase();
    let ok = scheme.len() >= 2
        && uri.len() <= MAX_LAUNCH_URI
        && uri.bytes().all(|b| b.is_ascii_graphic() && b != b'"')
        && !DENIED_SCHEMES.contains(&scheme.as_str())
        && (!scheme.starts_with(DENIED_PREFIX) || scheme == ALLOWED_MS);
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
    /// When the question came on screen (`ExternalLaunch::shown`), or the
    /// last press or "open" that came too early (`ExternalLaunch::pressed`,
    /// `ExternalLaunch::may_answer`).
    guard: Option<Instant>,
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
    /// The question stays open to say that this tab's launch failed.
    failed: Option<TabId>,
    launcher: Launcher,
}

impl Default for ExternalLaunch {
    fn default() -> Self {
        Self { pending: None, failed: None, launcher: DEFAULT_LAUNCHER }
    }
}

impl ExternalLaunch {
    #[cfg(any(test, feature = "selftest"))]
    pub fn set_launcher(&mut self, launcher: Launcher) {
        self.launcher = launcher;
    }

    /// A page in `tab` asked to open `uri`. True if this request now waits.
    /// False: refused by `launchable_scheme`, or another request was waiting.
    /// Then nothing waits any more: a question must not change under the
    /// user's hand, so the one shown is answered "no" and the new one dropped.
    pub fn request(&mut self, tab: TabId, uri: &str, origin: &str, now: Instant) -> bool {
        let free = self.pending(now).is_none();
        self.failed = None;
        self.pending = launchable_scheme(uri).filter(|_| free).map(|scheme| PendingExternalLaunch {
            tab,
            scheme,
            uri: uri.to_string(),
            // Visible ASCII only (WebView2 reports origins that way): no
            // lookalike letters, no direction marks, no line breaks.
            origin: origin.chars().filter(char::is_ascii_graphic).take(256).collect(),
            asked: now,
            guard: None,
        });
        self.pending.is_some()
    }

    /// The question for the waiting request came on screen at `now`.
    pub fn shown(&mut self, now: Instant) {
        if let Some(p) = &mut self.pending {
            p.guard = Some(now);
        }
    }

    /// Whether "open" may be taken at `now`: the question has been on screen
    /// for `ANSWER_DELAY`, and so long ago was the last "open" that came too
    /// early. A click aimed at the page (the second of a double click, a
    /// game's fast clicking) must not answer; one that comes too early
    /// starts the wait again.
    pub fn may_answer(&mut self, now: Instant) -> bool {
        let ok = self.pending.as_ref().and_then(|p| p.guard).is_some_and(|since| now.saturating_duration_since(since) >= ANSWER_DELAY);
        if !ok {
            self.pressed(now);
        }
        ok
    }

    /// The user pressed inside the shown question (a mouse button, Enter or
    /// Space) at `now`, sooner than `ANSWER_DELAY` after it came on screen or
    /// after the press before. The wait starts again from this press, as
    /// for an "open" that comes too early: a page that makes the user click
    /// one spot again and again, where "Aç" will appear, gets no answer
    /// however long the clicking goes on. Only "open" after a pause counts.
    /// Before the question is on screen, there is no wait to restart.
    pub fn pressed(&mut self, now: Instant) {
        if let Some(p) = &mut self.pending
            && p.guard.is_some()
        {
            p.guard = Some(now);
        }
    }

    /// The question is still shown: for a request, or to say it failed.
    pub fn asking(&self, now: Instant) -> bool {
        self.pending(now).is_some() || self.failed.is_some()
    }

    /// Keeps the question open to tell the user that `tab`'s launch failed.
    fn fail(&mut self, tab: TabId) {
        self.pending = None;
        self.failed = Some(tab);
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
        self.failed = None;
    }

    /// `tab` navigated or closed: its request is void.
    pub fn drop_for(&mut self, tab: TabId) {
        if self.pending.as_ref().is_some_and(|p| p.tab == tab) {
            self.pending = None;
        }
        if self.failed == Some(tab) {
            self.failed = None;
        }
    }
}

// ---- App state and bindings ------------------------------------------------

/// From a content webview (`native::attach_policy`). Untrusted.
#[derive(Debug)]
pub enum Event {
    /// The certificate of the server of request `uri` failed verification,
    /// while the tab's main navigations started and not completed were to
    /// `navs` (`native::attach_policy`).
    CertError { uri: String, navs: Vec<String> },
    /// The tab began to show a new document; `error`: WebView2's own error
    /// page (no connection, name not found, ...), not the site's.
    Page { error: bool },
    /// A navigation ended in failure after its document was shown.
    Failed,
    /// A page finished loading without error.
    Loaded,
    /// WebView2 was about to open `uri` in another application (and was
    /// stopped). `user`: the page had a user gesture for it.
    ExternalLaunch { uri: String, origin: String, user: bool },
}

/// Hosts remembered for a bad certificate. None is ever forgotten: see
/// `State::cert_error` for what happens when the list is full.
const MAX_BAD_CERT_SITES: usize = 64;

#[derive(Default)]
pub struct State {
    /// Hosts (`cert_host`) whose certificate failed in this run. Not per
    /// tab: once the user proceeds through WebView2's interstitial, the host
    /// loads without another error for the rest of the run, in every tab and
    /// on every port, and must keep its mark.
    bad_cert: Vec<String>,
    /// A certificate error could not be remembered (see `cert_error`): from
    /// then on no https page gets a lock, each counts as a certificate error.
    distrust: bool,
    /// Tabs showing an error page or a document whose load failed.
    failed: Vec<TabId>,
    /// Tabs whose renderer exited and that have not navigated since.
    crashed: Vec<TabId>,
    pub launch: ExternalLaunch,
    /// Launches without a user gesture, dropped unseen.
    #[cfg(feature = "selftest")]
    pub silent_drops: u32,
}

impl State {
    /// What the indicator says for `tab`, whose address box holds `shown`
    /// while the page it shows was committed as `page`.
    ///
    /// The mark is about the page on screen, never about an address that was
    /// only typed: until that navigation commits (or ends, and the box goes
    /// back to the page's address) there is no mark at all. And a lock is
    /// never put on an error page: there is no site behind it to vouch for.
    pub fn indicator(&self, tab: TabId, shown: &str, page: &str) -> Security {
        if shown != page {
            return Security::None;
        }
        match security_of(page, |site| self.distrust || self.bad_cert.iter().any(|s| s == site)) {
            Security::Secure if self.failed.contains(&tab) => Security::None,
            other => other,
        }
    }

    pub fn is_crashed(&self, tab: TabId) -> bool {
        self.crashed.contains(&tab)
    }

    /// The certificate of the server of `request` failed while the tab's
    /// main navigations not completed yet were to `navs`. Only a page's own
    /// server is remembered (`is_main_navigation`, which compares host and
    /// port, against each of them: a newer navigation does not make an
    /// older one's error a subresource's); a page cannot fill the list with
    /// requests to other servers. The mark is for the host (`cert_host`).
    ///
    /// A full list forgets nothing: a forgotten host the user had proceeded
    /// to would load without error and show a lock. Nor is WebView2 asked to
    /// forget the "proceed"s instead: that is asynchronous, and a page from
    /// the back/forward cache or over a TLS connection still open shows
    /// without a new handshake, so without a new error. A host that does not
    /// fit is one that may load without error from now on and that nothing
    /// here remembers: then no https page is called secure for the rest of
    /// the run (`distrust`). A warning too many, never a lock too many.
    pub fn cert_error(&mut self, request: &str, navs: &[String]) {
        let Some(host) = cert_host(request).filter(|_| navs.iter().any(|nav| is_main_navigation(request, nav))) else { return };
        if self.bad_cert.contains(&host) {
            return;
        }
        if self.bad_cert.len() >= MAX_BAD_CERT_SITES {
            self.distrust = true;
            return;
        }
        self.bad_cert.push(host);
    }

    fn set_failed(&mut self, tab: TabId, failed: bool) {
        self.failed.retain(|t| *t != tab);
        if failed {
            self.failed.push(tab);
        }
    }
}

pub fn on_event(app: &mut App, tab: TabId, ev: Event) {
    if app.tabs.get(tab).is_none() {
        return;
    }
    match ev {
        Event::CertError { uri, navs } => app.policy.cert_error(&uri, &navs),
        Event::Page { error } => app.policy.set_failed(tab, error),
        Event::Failed => app.policy.set_failed(tab, true),
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
                if app.policy.launch.request(tab, &uri, &origin, Instant::now()) {
                    show_question(app);
                } else {
                    // Refused, or one was waiting: then nothing waits now.
                    sync_question(app);
                }
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
        app.policy.launch.drop_for(tab);
        sync_question(app);
    }
    // Any navigation ends "crashed": the tab is doing something again, and
    // the mark must not sit on whatever comes next.
    if matches!(ev, PageEvent::Navigating { .. }) && app.policy.is_crashed(tab) {
        app.policy.crashed.retain(|t| *t != tab);
        app.mark_dirty();
    }
}

/// The shown tab changes (switch, close, new tab).
pub fn on_switch(app: &mut App) {
    app.policy.launch.dismiss();
    sync_question(app);
}

/// A content tab's renderer failed. A hung renderer may still answer again;
/// only one that exited leaves a dead page behind.
pub fn on_page_failed(app: &mut App, tab: TabId, failure: Failure) {
    app.policy.launch.drop_for(tab);
    sync_question(app);
    if failure == Failure::RendererExited && !app.policy.crashed.contains(&tab) {
        app.policy.crashed.push(tab);
    }
}

/// The overlay page's answer to the question. It names no tab and no URI:
/// the request it answers is the one held in `ExternalLaunch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchCmd {
    /// "Aç".
    Open,
    /// "İptal", the page's own timeout, or the popup lost focus.
    Cancel,
    /// A press inside the question that came too early: no answer, the
    /// wait for "Aç" starts again (`ExternalLaunch::pressed`).
    Press,
}

impl LaunchCmd {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "launch-open" => Some(Self::Open),
            "launch-cancel" => Some(Self::Cancel),
            "launch-press" => Some(Self::Press),
            _ => None,
        }
    }
}

/// Shows the question for the waiting request.
fn show_question(app: &mut App) {
    let Some(p) = app.policy.launch.pending(Instant::now()) else { return };
    let view = serde_json::json!({
        "scheme": p.scheme,
        "origin": p.origin,
        "uri": p.uri,
        "ms": LAUNCH_TIMEOUT.as_millis() as u64,
        "guard": ANSWER_DELAY.as_millis() as u64,
    });
    crate::overlay::show_launch(app, format!("xbo.launch({view})"));
    // No overlay, no question: then the answer is no.
    sync_question(app);
}

/// The overlay put the question on screen (`overlay::present`): "Aç" counts
/// from now, not from when the page asked. The overlay may be made, loaded
/// and shown long after.
pub fn on_question_shown(app: &mut App) {
    app.policy.launch.shown(Instant::now());
}

/// The overlay stopped showing the question, whatever the reason: that is
/// the answer "no". The one place the overlay tells this module so.
pub fn on_question_closed(app: &mut App) {
    app.policy.launch.dismiss();
}

/// Keeps "a request waits" and "the question is shown" the same thing.
fn sync_question(app: &mut App) {
    let shown = app.overlay.mode == Mode::Launch;
    let asking = app.policy.launch.asking(Instant::now());
    if shown && !asking {
        crate::overlay::dismiss(app);
    } else if asking && !shown {
        app.policy.launch.dismiss();
    }
}

/// An answer from the overlay page (`ui::ui_builder` takes it from no other
/// webview).
pub fn on_command(app: &mut App, cmd: LaunchCmd) {
    // An answer to a question that is not shown is no answer.
    if app.overlay.mode != Mode::Launch {
        return;
    }
    let now = Instant::now();
    if cmd == LaunchCmd::Press {
        // Not an answer: the question stays, and "Aç" waits again.
        return app.policy.launch.pressed(now);
    }
    // The request as it waits now, from here and not from the page.
    if cmd == LaunchCmd::Open
        && let Some(p) = app.policy.launch.pending(now).cloned()
    {
        if !app.policy.launch.may_answer(now) {
            // Too early: the page shows "Aç" as off (aria-disabled) again for
            // as long.
            return crate::overlay::tell_launch(app, &format!("xbo.launchGuard({})", ANSWER_DELAY.as_millis()));
        }
        let done = if app.tabs.active() == Some(p.tab) { app.policy.launch.confirm(p.tab, &p.uri, now) } else { Err(Refused::Mismatch) };
        if let Err(e) = done {
            // Not closed in silence: the user is told that nothing opened.
            eprintln!("x-browser: could not open the {} address: {e:?}", p.scheme);
            app.policy.launch.fail(p.tab);
            return crate::overlay::tell_launch(app, "xbo.launchFailed()");
        }
    }
    // Answered; closing also drops a request not confirmed.
    crate::overlay::dismiss(app);
}

/// The indicator's state for the active tab, as the script `App::flush`
/// appends to its render call. Also forgets tabs that are gone.
pub fn render_js(app: &mut App) -> String {
    let tabs = &app.tabs;
    let policy = &mut app.policy;
    for list in [&mut policy.crashed, &mut policy.failed] {
        list.retain(|t| tabs.get(*t).is_some());
    }
    let active = tabs.active().and_then(|id| tabs.get(id));
    let state = match active {
        Some(t) if policy.is_crashed(t.id) => "crashed",
        Some(t) => match policy.indicator(t.id, &t.url, &t.page_url) {
            Security::None => "",
            Security::Secure => "secure",
            Security::NotSecure => "insecure",
            Security::CertError => "cert",
        },
        None => "",
    };
    // `distrust`: a certificate error is shown with why every https page has
    // one now; the page says so once, unasked (`security.js`).
    format!("window.xbSecurity&&xbSecurity.render(\"{state}\",{})", policy.distrust)
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
        assert_eq!(security_of("http://localhost.example.com/", none), Security::NotSecure);
        assert_eq!(security_of("http://127.0.0.1.example.com/", none), Security::NotSecure);
        assert_eq!(security_of("http://128.0.0.1/", none), Security::NotSecure);
        // This machine itself: no warning (and no lock either).
        for own in ["http://localhost:8080/", "http://LOCALHOST/", "http://app.localhost/x", "http://127.0.0.1:3000/", "http://127.9.9.9/", "http://[::1]:8080/a"] {
            assert_eq!(security_of(own, none), Security::None, "{own}");
        }
        for internal in ["", "about:blank", "data:text/html,x", "blob:https://a.com/1", "edge://gpu", "not a url", "file:///C:/a"] {
            assert_eq!(security_of(internal, none), Security::None, "{internal}");
        }
        // "https" somewhere else in the text is not the scheme.
        assert_eq!(security_of("about:https://example.com/", none), Security::None);
        assert_eq!(security_of(" https://example.com/", none), Security::None);
    }

    #[test]
    fn bad_certificate_marks_that_host_on_every_port() {
        let state = State { bad_cert: vec!["bad.example".into(), "[::1]".into()], ..Default::default() };
        let of = |url: &str| state.indicator(1, url, url);
        assert_eq!(of("https://bad.example/"), Security::CertError);
        assert_eq!(of("https://bad.example:443/"), Security::CertError);
        assert_eq!(of("https://user@BAD.example.:8443/path"), Security::CertError);
        assert_eq!(of("https://bad.example:9443/"), Security::CertError, "a proceed holds for the host, not the port");
        assert_eq!(of("https://[::1]:8443/"), Security::CertError);
        assert_eq!(of("https://good.example/?u=https://bad.example/"), Security::Secure);
        assert_eq!(of("https://sub.bad.example/"), Security::Secure);
        assert_eq!(of("https://bad.example.evil.com/"), Security::Secure);
        // Plain http has no certificate to be wrong; it is not secure anyway.
        assert_eq!(of("http://bad.example/"), Security::NotSecure);
        // No lock for an address no server can be read from.
        assert_eq!(of("https://bad.example:99999/"), Security::None);
        assert_eq!(of("https:///x"), Security::None);
    }

    #[test]
    fn server_keys_carry_the_port() {
        assert_eq!(site_key("https://Example.com./a").as_deref(), Some("example.com:443"));
        assert_eq!(site_key("http://example.com").as_deref(), Some("example.com:80"));
        assert_eq!(site_key("https://u:p@example.com:8443?x").as_deref(), Some("example.com:8443"));
        assert_eq!(site_key("wss://example.com:/").as_deref(), Some("example.com:443"));
        assert_eq!(site_key("https://[::1]:8443/").as_deref(), Some("[::1]:8443"));
        for none in ["", "about:blank", "mailto:a@example.com", "https://", "https://example.com:x/", "https://[::1]x/", "ftp://example.com/"] {
            assert_eq!(site_key(none), None, "{none}");
            assert_eq!(cert_host(none), None, "{none}");
        }
        // Certificate marks drop the port.
        assert_eq!(cert_host("https://u:p@Example.com.:8443?x").as_deref(), Some("example.com"));
        assert_eq!(cert_host("https://[::1]:8443/").as_deref(), Some("[::1]"));
        assert_eq!(cert_host("https://example.com:99999/"), None, "no port, no server");
    }

    fn mark(state: &mut State, request: &str, nav: &str) {
        state.cert_error(request, &[nav.to_string()]);
    }

    #[test]
    fn an_older_navigation_still_under_way_keeps_its_certificate_error() {
        let mut state = State::default();
        // A navigation to a bad server, then another one started before the
        // first completed: the first one's error is still the page's own.
        let navs = ["https://bad.example/".to_string(), "https://good.example/".to_string()];
        state.cert_error("https://bad.example/", &navs);
        assert_eq!(state.bad_cert, ["bad.example"]);
        // Neither of them: a subresource.
        state.cert_error("https://ads.example/a.png", &navs);
        state.cert_error("https://ads.example/", &[]);
        assert_eq!(state.bad_cert, ["bad.example"]);
    }

    #[test]
    fn a_proceed_on_one_port_marks_the_host_on_every_port() {
        let mut state = State::default();
        let page = "https://host.example:8443/admin";
        mark(&mut state, page, page);
        assert_eq!(state.bad_cert, ["host.example"]);
        for url in [page, "https://host.example/", "https://host.example:443/x", "https://HOST.example.:9443/"] {
            assert_eq!(state.indicator(1, url, url), Security::CertError, "{url}");
        }
        assert_eq!(state.indicator(1, "https://other.example/", "https://other.example/"), Security::Secure);
    }

    #[test]
    fn subresource_certificate_error_does_not_mark() {
        let mut state = State::default();
        let page = "https://news.example/story";
        // An image, a script, a frame and a socket from servers with bad
        // certificates, while the page itself loaded fine.
        for request in ["https://ads.example/a.png", "https://news.example:8443/x.js", "https://cdn.example/frame.html", "wss://news.example/live"] {
            mark(&mut state, request, page);
        }
        assert!(state.bad_cert.is_empty());
        assert_eq!(state.indicator(1, page, page), Security::Secure);
        assert_eq!(state.indicator(1, "https://ads.example/", "https://ads.example/"), Security::Secure);
        // Nothing to compare with is not a main navigation either.
        mark(&mut state, "https://ads.example/", "");
        mark(&mut state, "https://ads.example/", "about:blank");
        assert!(state.bad_cert.is_empty());

        // The page's own request, also after a redirect or an http address
        // that was upgraded.
        mark(&mut state, "https://news.example/story?x=1", page);
        mark(&mut state, "https://Upgraded.example/", "http://upgraded.example/");
        assert_eq!(state.bad_cert, ["news.example", "upgraded.example"]);
        // Not the page's own server: another port than the one navigated to.
        mark(&mut state, "https://other.example:8443/", "http://other.example/");
        mark(&mut state, "https://news.example/", page);
        assert_eq!(state.bad_cert.len(), 2, "no other server, and no host twice");
        assert_eq!(state.indicator(2, page, page), Security::CertError, "in every tab");
    }

    #[test]
    fn a_full_list_never_turns_an_earlier_server_secure() {
        let site = |i: usize| format!("https://bad{i}.example/");
        let mut state = State::default();
        for i in 0..MAX_BAD_CERT_SITES {
            mark(&mut state, &site(i), &site(i));
        }
        assert_eq!(state.bad_cert.len(), MAX_BAD_CERT_SITES);
        // A host already marked takes no new place, on any port.
        mark(&mut state, &site(3), &site(3));
        mark(&mut state, "https://bad3.example:8443/", "https://bad3.example:8443/");
        assert!(!state.distrust);
        assert_eq!(state.indicator(2, "https://good.example/", "https://good.example/"), Security::Secure);

        // The 65th host can be remembered nowhere, and nothing is forgotten
        // for it: no https page is called secure for the rest of the run.
        mark(&mut state, &site(64), &site(64));
        assert!(state.distrust);
        assert_eq!(state.bad_cert.len(), MAX_BAD_CERT_SITES, "the list stays as it was");
        for i in 0..=MAX_BAD_CERT_SITES {
            assert_eq!(state.indicator(1, &site(i), &site(i)), Security::CertError, "{i}");
        }
        assert_eq!(state.indicator(2, "https://good.example/", "https://good.example/"), Security::CertError);
        assert_eq!(state.indicator(2, "http://good.example/", "http://good.example/"), Security::NotSecure);
        assert_eq!(state.indicator(2, "about:blank", "about:blank"), Security::None);
    }

    #[test]
    fn mark_is_for_the_committed_page_only() {
        let mut state = State { bad_cert: vec!["bad.example".into()], ..Default::default() };
        let (page, typed) = ("http://plain.example/", "https://bank.example/");
        // A typed address that has not committed: no lock for it, and the
        // old page's mark does not stand beside the new address either.
        assert_eq!(state.indicator(1, typed, page), Security::None);
        assert_eq!(state.indicator(1, "https://bad.example/", page), Security::None);
        assert_eq!(state.indicator(1, typed, ""), Security::None, "a new tab that has not loaded");
        assert_eq!(state.indicator(1, page, page), Security::NotSecure);
        assert_eq!(state.indicator(1, typed, typed), Security::Secure);

        // An error page, or a load that failed, never has a lock.
        state.set_failed(1, true);
        assert_eq!(state.indicator(1, typed, typed), Security::None);
        assert_eq!(state.indicator(2, typed, typed), Security::Secure, "another tab");
        assert_eq!(state.indicator(1, page, page), Security::NotSecure, "warnings stay");
        assert_eq!(state.indicator(1, "https://bad.example/", "https://bad.example/"), Security::CertError);
        state.set_failed(1, false);
        assert_eq!(state.indicator(1, typed, typed), Security::Secure);
    }

    #[test]
    fn address_box_shows_no_credentials() {
        assert_eq!(display("https://user:secret@example.com/a@b"), "https://example.com/a@b");
        assert_eq!(display("https://bank.example@evil.example/"), "https://evil.example/");
        assert_eq!(display("http://a@b@c.example:8080/x?y#z"), "http://c.example:8080/x?y#z");
        assert_eq!(display("http://user:pw@www.münchen.de:8080/x"), "http://www.xn--mnchen-3ya.de:8080/x");
        assert_eq!(display("https://@example.com"), "https://example.com");
        for same in ["", "about:blank", "https://example.com/?to=a@b", "https://example.com/a@b", "mailto:a@example.com", "https://[::1]/"] {
            assert!(matches!(display(same), Cow::Borrowed(s) if s == same), "{same}");
        }
    }

    #[test]
    fn overlong_unicode_hosts_are_not_shown() {
        let label = "ü".repeat(64);
        assert_eq!(ascii_host(&format!("https://{label}.example/a")), "https://gecersiz-ad.invalid/a");
        assert!(ascii_host(&format!("https://{}.example/", "ü".repeat(63))).starts_with("https://xn--"));
        let long = format!("https://{}.aaaa/", ["ü"; 83].join("."));
        assert_eq!(long.len(), "https://".len() + 253 + 1);
        assert!(ascii_host(&long).starts_with("https://xn--"), "253 bytes");
        let longer = format!("https://{}/x", ["ü"; 85].join("."));
        assert_eq!(longer.len(), "https://".len() + 254 + 2);
        assert_eq!(ascii_host(&longer), "https://gecersiz-ad.invalid/x");
        assert_eq!(display(&format!("https://user@{}:8080/x", "ü".repeat(5000))), "https://gecersiz-ad.invalid:8080/x");
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
        assert_eq!(launchable_scheme("ms-windows-store://pdp/?productid=9WZDNCRFJ3TJ").as_deref(), Some("ms-windows-store"));
        assert_eq!(launchable_scheme("MS-Windows-Store://home").as_deref(), Some("ms-windows-store"));
        for denied in [
            "file:///C:/Windows/System32/calc.exe",
            r"c:\windows\notepad.exe",
            "c:foo",
            "C:/Windows/notepad.exe",
            r"\\server\share\a.exe",
            "notepad.exe",
            "1a:x",
            "a_b:x",
            "javascript:alert(1)",
            "data:text/html,x",
            "vbscript:msgbox(1)",
            "https://example.com/",
            "about:blank",
            "ms-settings:privacy",
            "ms-windows-store2:x",
            "ms-windows-store-evil://x",
            "MS-MSDT:/id%20PCWDiagnostic",
            "ms-appinstaller:?source=https://example.com/a.msix",
            "ms-word:ofe|u|https://example.com/a.docx",
            "search-ms:query=x&crumb=location:%5C%5Cexample.com%5Cshare",
            "shell:startup",
            "onenote:https://example.com/a.one",
            "onenote-cmd:x",
            "microsoft-edge:https://example.com/",
            "jnlp:https://example.com/a.jnlp",
            "jnlps://example.com/a.jnlp",
            "ie.http://example.com/",
            "afp://example.com/share",
            "disk:x",
            "disks:x",
            "nntp://example.com/group",
            "VND.MS.RADIO:x",
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

    #[test]
    fn a_second_request_answers_the_first_no() {
        let now = Instant::now();
        let mut launch = recording();
        assert!(launch.request(1, MAIL, "", now));
        assert!(!launch.request(1, "tel:+901234567", "", now), "the new one is dropped");
        assert!(launch.pending(now).is_none() && !launch.asking(now), "and the shown one answered no");
        assert_eq!(launch.confirm(1, MAIL, now), Err(Refused::NoRequest));
        assert_eq!(launch.confirm(1, "tel:+901234567", now), Err(Refused::NoRequest));
        // The same address again changes nothing about that.
        assert!(launch.request(1, MAIL, "", now));
        assert!(!launch.request(1, MAIL, "", now));
        assert!(launch.pending(now).is_none());
        // Afterwards, and after a request that timed out, asking works again.
        assert!(launch.request(1, MAIL, "", now));
        assert!(launch.request(2, "tel:+901234567", "", now + LAUNCH_TIMEOUT));
        assert_eq!(launch.pending(now + LAUNCH_TIMEOUT).map(|p| p.tab), Some(2));
        assert!(launched().is_empty());
    }

    #[test]
    fn open_counts_from_when_the_question_is_shown() {
        let ms = Duration::from_millis;
        let asked = Instant::now();
        let mut launch = recording();
        assert!(!launch.may_answer(asked), "nothing waits");
        assert!(launch.request(1, MAIL, "", asked));
        // Asked long ago, but not on screen yet (the overlay was being made).
        assert!(!launch.may_answer(asked + ms(5000)));
        let shown = asked + ms(6000);
        launch.shown(shown);
        assert!(!launch.may_answer(shown));
        // Too early: the wait starts again from that click, every time.
        assert!(!launch.may_answer(shown + ms(400)));
        assert!(!launch.may_answer(shown + ms(800)));
        assert!(!launch.may_answer(shown + ms(1299)));
        assert!(launch.pending(shown + ms(1299)).is_some(), "an early click drops nothing");
        assert!(launch.may_answer(shown + ms(1799)));
        assert!(launch.may_answer(shown + ms(1800)), "an accepted one does not start it again");
        assert!(launched().is_empty());
    }

    #[test]
    fn every_press_too_early_starts_the_wait_again() {
        let ms = Duration::from_millis;
        let asked = Instant::now();
        let mut launch = recording();
        launch.pressed(asked);
        assert!(launch.request(1, MAIL, "", asked));
        // Presses before the question is on screen restart nothing: the
        // wait has not begun.
        launch.pressed(asked + ms(100));
        let shown = asked + ms(200);
        launch.shown(shown);
        // A burst of clicks where "Aç" is, every 150 ms for three seconds,
        // reported as presses: never an answer, at any moment of it.
        let mut t = shown;
        for _ in 0..20 {
            t += ms(150);
            launch.pressed(t);
            assert!(!launch.may_answer(t + ms(149)), "at {:?}", t - shown);
        }
        // The last of them restarted the wait (and so did each "open" just
        // above): an answer counts only a full pause after.
        let last = t + ms(149);
        assert!(!launch.may_answer(last + ms(499)));
        assert!(launch.may_answer(last + ms(499) + ANSWER_DELAY));
        assert!(launch.pending(last).is_some(), "presses drop nothing");
        // With nothing waiting, a press is nothing.
        launch.dismiss();
        launch.pressed(last);
        assert!(!launch.may_answer(last + ms(5000)));
        assert!(launched().is_empty());
    }

    #[test]
    fn a_failed_launch_keeps_the_question_to_say_so() {
        let now = Instant::now();
        let mut launch = ExternalLaunch::default();
        assert!(launch.request(1, MAIL, "", now));
        assert_eq!(launch.confirm(1, MAIL, now), Err(Refused::LaunchFailed));
        assert!(!launch.asking(now));
        launch.fail(1);
        assert!(launch.asking(now) && launch.pending(now).is_none(), "shown, but nothing can be opened");
        launch.drop_for(2);
        assert!(launch.asking(now));
        launch.drop_for(1);
        assert!(!launch.asking(now));
        launch.fail(1);
        launch.dismiss();
        assert!(!launch.asking(now));
        launch.fail(1);
        assert!(launch.request(1, MAIL, "", now), "a new request takes its place");
        assert!(launch.pending(now).is_some());
    }

    #[test]
    fn the_question_shows_origins_in_visible_ascii_only() {
        let now = Instant::now();
        let mut launch = recording();
        assert!(launch.request(1, MAIL, "https://аррӏе.com\u{202e}\u{200b} x\n", now));
        assert_eq!(launch.pending(now).unwrap().origin, "https://.comx");
        launch.dismiss();
        assert!(launch.request(1, MAIL, &"a".repeat(1000), now));
        assert_eq!(launch.pending(now).unwrap().origin.len(), 256);
    }

    #[test]
    fn the_answer_carries_no_address() {
        use crate::commands::UiCommand;
        assert_eq!(UiCommand::parse("launch-open"), Some(UiCommand::Launch(LaunchCmd::Open)));
        assert_eq!(UiCommand::parse("launch-cancel"), Some(UiCommand::Launch(LaunchCmd::Cancel)));
        assert_eq!(UiCommand::parse("launch-press"), Some(UiCommand::Launch(LaunchCmd::Press)));
        // Nothing the page adds is taken: not a URI, not a tab.
        for msg in ["launch-open:mailto:other@example.com", "launch-open:", "launch-open:1", "launch-cancel:1", "launch-press:1", "launch", "launch-opens"] {
            assert_eq!(UiCommand::parse(msg), None, "{msg}");
        }
    }
}
