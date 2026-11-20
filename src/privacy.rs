//! Three privacy settings, each on by default (`settings`):
//!
//! - HTTPS-only: an `http://` address is tried as `https://` first
//!   (`url_clean::https_upgrade`; never for local or private hosts or an
//!   explicit port). If that fails, the tab's error document gets the
//!   HTTPS-only page (`error_page::https_only_script`), whose "Yine de http
//!   ile aç" opens the http address and lets that host use http for the rest
//!   of the session (nothing is saved).
//! - Tracking parameters (`utm_*`, `fbclid`, ...) are dropped from the
//!   address before a page loads (`url_clean::strip_tracking`).
//! - Global Privacy Control: every request carries `Sec-GPC: 1` and pages
//!   see `navigator.globalPrivacyControl === true` (`native::privacy`).
//!
//! The first two rewrite a main navigation as it starts: the tab's hook
//! (`native::content`) cancels it and the app loads the new address
//! (`PageEvent::Rewrite`). Only new documents are rewritten: Back, Forward
//! and Reload go where they were. A chain of rewrites is cut off after
//! `MAX_REWRITES`, and a redirect back to an address just cleaned is let
//! through, so a server that insists cannot loop the tab.

use crate::tabs::TabId;
use crate::url::host_of;
use crate::url_clean::{https_upgrade, strip_tracking};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

static HTTPS_ONLY: AtomicBool = AtomicBool::new(true);
static STRIP: AtomicBool = AtomicBool::new(true);
static GPC: AtomicBool = AtomicBool::new(true);

/// Puts the settings' values in force (read on every navigation and request).
pub fn set(https_only: bool, strip: bool, gpc: bool) {
    HTTPS_ONLY.store(https_only, Ordering::Relaxed);
    STRIP.store(strip, Ordering::Relaxed);
    GPC.store(gpc, Ordering::Relaxed);
}

/// Whether requests carry `Sec-GPC: 1` right now.
pub fn gpc() -> bool {
    GPC.load(Ordering::Relaxed)
}

/// Where the HTTPS-only page's "Yine de http ile aç" goes. Never loaded
/// (`.invalid` resolves nowhere): the tab's hook stops it and, only from
/// that tab's failed https page, opens the http address instead.
pub const HTTP_SENTINEL: &str = "https://xb-http.invalid/";

/// Makes `navigator.globalPrivacyControl` true in every document.
pub const GPC_SCRIPT: &str = "(() => { try { Object.defineProperty(Navigator.prototype, 'globalPrivacyControl', \
     { get() { return true; }, configurable: true, enumerable: true }); } catch (e) {} })();";

/// Rewrites in a row (redirects included) before the rest are let through.
const MAX_REWRITES: u8 = 4;

/// What becomes of a main navigation that is starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// It goes ahead as it is.
    Go,
    /// It is stopped, and this address loaded in its place.
    Load(String),
    /// It is stopped (a sentinel no failed https page sent).
    Refuse,
}

/// An `http://` address being tried as https.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Upgrade {
    https: String,
    http: String,
    /// The https address failed and its error document shows the
    /// HTTPS-only page: only now may the sentinel open `http`.
    failed: bool,
}

#[derive(Debug, Default)]
struct TabState {
    upgrade: Option<Upgrade>,
    /// The address last loaded in place of another (what to expect next).
    expect: Option<String>,
    /// Rewrites since the user (or a page) last started a navigation.
    rewrites: u8,
    /// The address last cleaned of tracking: a redirect back to it goes
    /// through as it is.
    unstripped: Option<String>,
}

/// The rewrites of every tab, and the hosts allowed http this session.
#[derive(Debug, Default)]
pub struct Rewrites {
    tabs: HashMap<TabId, TabState>,
    http_hosts: HashSet<String>,
}

/// What a starting navigation is, as the tab's hook reads it.
#[derive(Debug, Clone, Copy)]
pub struct Nav<'a> {
    pub uri: &'a str,
    /// A server redirect of a navigation already started.
    pub redirected: bool,
    /// A new document (not Back, Forward or Reload).
    pub new_document: bool,
    /// The address of the document on screen now.
    pub source: &'a str,
}

impl Rewrites {
    pub fn start(&mut self, tab: TabId, nav: Nav, https_only: bool, strip: bool) -> Start {
        let t = self.tabs.entry(tab).or_default();
        if nav.uri == HTTP_SENTINEL {
            return match t.upgrade.take() {
                Some(u) if u.failed && nav.source == u.https => {
                    if let Some(host) = host_of(&u.http) {
                        self.http_hosts.insert(host);
                    }
                    t.expect = Some(u.http.clone());
                    Start::Load(u.http)
                }
                other => {
                    t.upgrade = other;
                    Start::Refuse
                }
            };
        }
        if !nav.redirected {
            // Not ours: a new chain starts.
            if t.expect.take().as_deref() != Some(nav.uri) {
                t.rewrites = 0;
                t.unstripped = None;
            }
            // (Kept for its own https address: "Yeniden dene" tries again.)
            match &mut t.upgrade {
                Some(u) if u.https == nav.uri => u.failed = false,
                _ => t.upgrade = None,
            }
        }
        if !crate::url::is_web_url(nav.uri) {
            *t = TabState::default();
            return Start::Go;
        }
        if !nav.new_document || t.rewrites >= MAX_REWRITES {
            return Start::Go;
        }
        let mut target = nav.uri.to_string();
        if strip && t.unstripped.as_deref() != Some(nav.uri)
            && let Some(clean) = strip_tracking(nav.uri)
        {
            t.unstripped = Some(nav.uri.to_string());
            target = clean;
        }
        if https_only && let Some(https) = https_upgrade(&target) {
            let host = host_of(&target);
            let downgrade = nav.redirected && t.upgrade.as_ref().is_some_and(|u| host_of(&u.https) == host);
            if downgrade {
                // The https site sent us back to http: its choice, taken.
                t.upgrade = None;
            } else if !host.is_some_and(|h| self.http_hosts.contains(&h)) {
                t.upgrade = Some(Upgrade { https: https.clone(), http: target, failed: false });
                target = https;
            }
        }
        if target == nav.uri {
            return Start::Go;
        }
        t.rewrites += 1;
        t.expect = Some(target.clone());
        Start::Load(target)
    }

    /// See `before_load`. Its navigation then reaches `start` as the
    /// address expected, and goes ahead with the upgrade it made.
    pub fn before_load(&mut self, tab: TabId, url: &str, https_only: bool, strip: bool) -> String {
        if url == HTTP_SENTINEL {
            return url.to_string();
        }
        let nav = Nav { uri: url, redirected: false, new_document: true, source: "" };
        match self.start(tab, nav, https_only, strip) {
            Start::Load(target) => target,
            Start::Go | Start::Refuse => url.to_string(),
        }
    }

    /// The tab's navigation failed and left an error document at `source`.
    /// If that was an upgrade's https address (and the failure is not one
    /// http would share: no network, no such host), the http address to offer.
    pub fn failed(&mut self, tab: TabId, source: &str, status: i32) -> Option<String> {
        use crate::error_page::{Kind, kind, status::HOST_NAME_NOT_RESOLVED};
        let u = self.tabs.get_mut(&tab)?.upgrade.as_mut().filter(|u| u.https == source)?;
        if status == HOST_NAME_NOT_RESOLVED || matches!(kind(status), None | Some(Kind::Offline)) {
            return None;
        }
        u.failed = true;
        Some(u.http.clone())
    }

    /// The tab's navigation loaded a page: the chain is over.
    pub fn loaded(&mut self, tab: TabId) {
        if let Some(t) = self.tabs.get_mut(&tab) {
            t.upgrade = None;
            t.rewrites = 0;
        }
    }

    pub fn forget(&mut self, tab: TabId) {
        self.tabs.remove(&tab);
    }
}

thread_local! {
    /// Only the UI thread, where WebView2 raises its events, uses it.
    static REWRITES: RefCell<Rewrites> = RefCell::new(Rewrites::default());
}

/// A main navigation of `tab` is starting (see `Rewrites::start`).
pub fn on_start(tab: TabId, nav: Nav) -> Start {
    let (https_only, strip) = (HTTPS_ONLY.load(Ordering::Relaxed), STRIP.load(Ordering::Relaxed));
    REWRITES.with(|r| r.borrow_mut().start(tab, nav, https_only, strip))
}

/// The address to load when the app itself starts a navigation of `tab`
/// to `url` (address bar, restored tab). Rewritten here, before the load:
/// a navigation stopped in its hook has already sent its request.
pub fn before_load(tab: TabId, url: &str) -> String {
    let (https_only, strip) = (HTTPS_ONLY.load(Ordering::Relaxed), STRIP.load(Ordering::Relaxed));
    REWRITES.with(|r| r.borrow_mut().before_load(tab, url, https_only, strip))
}

/// See `Rewrites::failed`.
pub fn on_failed(tab: TabId, source: &str, status: i32) -> Option<String> {
    REWRITES.with(|r| r.borrow_mut().failed(tab, source, status))
}

pub fn on_loaded(tab: TabId) {
    REWRITES.with(|r| r.borrow_mut().loaded(tab));
}

/// The tab closed.
pub fn forget(tab: TabId) {
    REWRITES.with(|r| r.borrow_mut().forget(tab));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_page::status;

    const T: TabId = 1;

    fn nav<'a>(uri: &'a str, source: &'a str) -> Nav<'a> {
        Nav { uri, redirected: false, new_document: true, source }
    }

    fn redirect<'a>(uri: &'a str, source: &'a str) -> Nav<'a> {
        Nav { redirected: true, ..nav(uri, source) }
    }

    fn load(s: &str) -> Start {
        Start::Load(s.into())
    }

    #[test]
    fn http_is_tried_as_https_and_the_failure_offers_http_once() {
        let mut r = Rewrites::default();
        assert_eq!(r.start(T, nav("http://a.example.com/x", ""), true, true), load("https://a.example.com/x"));
        assert_eq!(r.start(T, nav("https://a.example.com/x", ""), true, true), Start::Go, "the rewrite itself goes");
        let page = "https://a.example.com/x";
        assert_eq!(r.start(T, nav(HTTP_SENTINEL, page), true, true), Start::Refuse, "nothing failed yet");
        assert_eq!(r.failed(T, page, status::CERTIFICATE_IS_INVALID).as_deref(), Some("http://a.example.com/x"));
        assert_eq!(r.start(T, nav(HTTP_SENTINEL, "https://other.example/"), true, true), Start::Refuse, "only from that page");
        assert_eq!(r.start(T, nav(HTTP_SENTINEL, page), true, true), load("http://a.example.com/x"));
        assert_eq!(r.start(T, nav("http://a.example.com/x", page), true, true), Start::Go, "the host may use http now");
        assert_eq!(r.start(T, nav(HTTP_SENTINEL, page), true, true), Start::Refuse, "the offer was used up");
        assert_eq!(r.start(T, nav("http://a.example.com/other", ""), true, true), Start::Go, "for the whole host");
        assert_eq!(r.start(2, nav("http://a.example.com/", ""), true, true), Start::Go, "and in every tab");
        assert_eq!(r.start(T, nav("http://b.example.com/", ""), true, true), load("https://b.example.com/"), "not other hosts");
    }

    #[test]
    fn failures_http_would_share_offer_nothing() {
        for s in [status::HOST_NAME_NOT_RESOLVED, status::DISCONNECTED, status::OPERATION_CANCELED] {
            let mut r = Rewrites::default();
            r.start(T, nav("http://a.example.com/", ""), true, true);
            r.start(T, nav("https://a.example.com/", ""), true, true);
            assert_eq!(r.failed(T, "https://a.example.com/", s), None, "{s}");
            assert_eq!(r.start(T, nav(HTTP_SENTINEL, "https://a.example.com/"), true, true), Start::Refuse);
        }
        let mut r = Rewrites::default();
        r.start(T, nav("http://a.example.com/", ""), true, true);
        assert_eq!(r.failed(T, "https://elsewhere.example/", status::TIMEOUT), None, "another address failed");
        assert_eq!(r.failed(2, "https://a.example.com/", status::TIMEOUT), None, "in another tab");
        assert!(r.failed(T, "https://a.example.com/", status::CONNECTION_RESET).is_some());
    }

    #[test]
    fn retrying_keeps_the_offer_and_leaving_drops_it() {
        let mut r = Rewrites::default();
        let page = "https://a.example.com/";
        r.start(T, nav("http://a.example.com/", ""), true, true);
        r.start(T, nav(page, ""), true, true);
        r.failed(T, page, status::TIMEOUT);
        // "Yeniden dene" reloads the https address.
        assert_eq!(r.start(T, Nav { new_document: false, ..nav(page, page) }, true, true), Start::Go);
        assert_eq!(r.start(T, nav(HTTP_SENTINEL, page), true, true), Start::Refuse, "not until it fails again");
        assert!(r.failed(T, page, status::TIMEOUT).is_some());
        assert_eq!(r.start(T, nav("https://b.example/", page), true, true), Start::Go);
        assert_eq!(r.failed(T, page, status::TIMEOUT), None, "another navigation ended the try");
        r.start(T, nav("http://c.example.com/", ""), true, true);
        r.loaded(T);
        assert_eq!(r.failed(T, "https://c.example.com/", status::TIMEOUT), None, "a page loaded");
    }

    #[test]
    fn local_hosts_ports_and_settings_off_are_left_alone() {
        let mut r = Rewrites::default();
        for u in ["http://localhost/", "http://127.0.0.1:8080/", "http://192.168.1.1/", "http://router/", "http://a.example.com:81/"] {
            assert_eq!(r.start(T, nav(u, ""), true, true), Start::Go, "{u}");
        }
        assert_eq!(r.start(T, nav("http://a.example.com/", ""), false, true), Start::Go);
        assert_eq!(r.start(T, nav("http://a.example.com/?utm_source=x", ""), false, false), Start::Go);
        assert_eq!(r.start(T, nav("about:blank", ""), true, true), Start::Go);
    }

    #[test]
    fn back_forward_and_reload_are_not_rewritten() {
        let mut r = Rewrites::default();
        let n = Nav { new_document: false, ..nav("http://a.example.com/?fbclid=1", "") };
        assert_eq!(r.start(T, n, true, true), Start::Go);
    }

    #[test]
    fn tracking_is_dropped_and_combined_with_the_upgrade() {
        let mut r = Rewrites::default();
        assert_eq!(r.start(T, nav("https://a.example/?utm_source=x&id=1", ""), true, true), load("https://a.example/?id=1"));
        assert_eq!(r.start(T, nav("https://a.example/?id=1", ""), true, true), Start::Go);
        assert_eq!(r.start(T, nav("http://b.example.com/?gclid=1", ""), true, true), load("https://b.example.com/"));
        r.start(T, nav("https://b.example.com/", ""), true, true);
        assert_eq!(r.failed(T, "https://b.example.com/", status::TIMEOUT).as_deref(), Some("http://b.example.com/"), "http, cleaned");
        assert_eq!(r.start(T, nav("http://b.example.com/?gclid=1", ""), true, false), load("https://b.example.com/?gclid=1"), "strip off");
    }

    #[test]
    fn redirects_are_rewritten_but_cannot_loop() {
        let mut r = Rewrites::default();
        // A redirect to tracking is cleaned; one back to it goes through.
        assert_eq!(r.start(T, nav("https://a.example/", ""), true, true), Start::Go);
        assert_eq!(r.start(T, redirect("https://a.example/?utm_source=x", ""), true, true), load("https://a.example/"));
        assert_eq!(r.start(T, nav("https://a.example/", ""), true, true), Start::Go);
        assert_eq!(r.start(T, redirect("https://a.example/?utm_source=x", ""), true, true), Start::Go, "the server insists");
        // An https site sending us back to http is let through.
        let mut r = Rewrites::default();
        r.start(T, nav("http://b.example.com/", ""), true, true);
        r.start(T, nav("https://b.example.com/", ""), true, true);
        assert_eq!(r.start(T, redirect("http://b.example.com/", ""), true, true), Start::Go);
        // Sites bouncing between each other stop after a few rounds.
        let mut r = Rewrites::default();
        let mut rewrites = 0;
        for i in 0..20 {
            let host = if i % 2 == 0 { "http://c.example.com/" } else { "http://d.example.com/" };
            if let Start::Load(to) = r.start(T, redirect(host, ""), true, true) {
                rewrites += 1;
                assert_eq!(r.start(T, nav(&to, ""), true, true), Start::Go);
            }
        }
        assert_eq!(rewrites, MAX_REWRITES);
        r.loaded(T);
        assert_eq!(r.start(T, nav("http://c.example.com/", ""), true, true), load("https://c.example.com/"), "a load ends the chain");
    }

    #[test]
    fn a_new_navigation_starts_a_new_chain() {
        let mut r = Rewrites::default();
        for _ in 0..MAX_REWRITES {
            r.start(T, nav("https://a.example/?fbclid=1", ""), true, true);
        }
        assert_eq!(r.start(T, nav("https://a.example/?fbclid=2", ""), true, true), load("https://a.example/"));
        r.forget(T);
        assert!(r.tabs.is_empty());
    }

    #[test]
    fn the_apps_own_loads_are_rewritten_before_they_start() {
        let mut r = Rewrites::default();
        let to = r.before_load(T, "http://a.example.com/x?utm_source=1&id=2", true, true);
        assert_eq!(to, "https://a.example.com/x?id=2");
        assert_eq!(r.start(T, nav(&to, ""), true, true), Start::Go, "its navigation goes as it is");
        assert!(r.failed(T, &to, status::TIMEOUT).is_some(), "and still offers http");
        assert_eq!(r.before_load(T, "https://a.example/", true, true), "https://a.example/");
        assert_eq!(r.before_load(T, "http://127.0.0.1:1/?fbclid=1", true, true), "http://127.0.0.1:1/");
        assert_eq!(r.before_load(T, "http://a.example.com/?fbclid=1", false, false), "http://a.example.com/?fbclid=1");
        assert_eq!(r.before_load(T, HTTP_SENTINEL, true, true), HTTP_SENTINEL);
    }

    #[test]
    fn the_gpc_script_only_defines_the_property() {
        assert!(GPC_SCRIPT.contains("'globalPrivacyControl'") && GPC_SCRIPT.contains("return true"));
        assert!(!GPC_SCRIPT.contains("fetch") && !GPC_SCRIPT.contains("postMessage"));
    }
}
