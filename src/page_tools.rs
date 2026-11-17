//! Page tools: zoom and fullscreen (F11 and the page's own). Find (Ctrl+F)
//! and print (Ctrl+P) are WebView2's built-in find bar and print preview:
//! their keys are deliberately not in `Shortcut`, so they reach the browser.
//! DevTools (F12) stay off in release builds: content is untrusted, and
//! DevTools on the UI webview would expose its IPC channel.
//!
//! Fullscreen hides the strip, so every entry (F11 or a page's own) shows a
//! notice for `NOTICE_TIME` over the top centre: which site took the screen
//! and how to leave. It is the UI webview, shrunk to a band above the page
//! (`app::notice_rect`): a separate window the page cannot paint over.
//! Esc always leaves fullscreen through the host (known trade-off: in
//! fullscreen a page never sees Esc itself, e.g. a game's pause key).
//!
//! Both vocabularies here are closed sets nested into the ones in
//! `commands`: `PageCmd` comes from the trusted UI (a tab id only),
//! `PageKey` from a native key event (no data at all).

use crate::app::{App, UserEvent};
use crate::commands::Mods;
use crate::native;
use crate::tabs::TabId;
use std::cell::Cell;

/// UI commands of the page tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageCmd {
    /// `zoom-reset:<id>`: the zoom indicator in the address bar was clicked.
    ZoomReset(TabId),
}

impl PageCmd {
    /// `name` and `arg` of a `name:arg` message. A tab id only.
    pub fn parse(name: &str, arg: &str) -> Option<Self> {
        match name {
            "zoom-reset" => Some(Self::ZoomReset(arg.parse().ok()?)),
            _ => None,
        }
    }
}

/// Keyboard shortcuts of the page tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKey {
    /// Ctrl+= / Ctrl++ / Ctrl+NumPad+.
    ZoomIn,
    /// Ctrl+- / Ctrl+NumPad-.
    ZoomOut,
    /// Ctrl+0 / Ctrl+NumPad0.
    ZoomReset,
    /// F11.
    ToggleFullscreen,
    /// Esc, only while fullscreen (F11 or a page's own): the host leaves it,
    /// whatever the page does.
    ExitFullscreen,
}

/// Virtual-key codes of the page tools (same on every keyboard layout).
pub mod keys {
    pub const ZERO: u32 = 0x30;
    /// Not ours (find, print, DevTools); named for the tests.
    #[cfg(any(test, feature = "selftest"))]
    pub const F: u32 = 0x46;
    /// Not ours (find, print, DevTools); named for the tests.
    #[cfg(any(test, feature = "selftest"))]
    pub const P: u32 = 0x50;
    pub const NUMPAD0: u32 = 0x60;
    pub const ADD: u32 = 0x6B;
    pub const SUBTRACT: u32 = 0x6D;
    pub const F11: u32 = 0x7A;
    /// Not ours (find, print, DevTools); named for the tests.
    #[cfg(any(test, feature = "selftest"))]
    pub const F12: u32 = 0x7B;
    pub const OEM_PLUS: u32 = 0xBB;
    pub const OEM_MINUS: u32 = 0xBD;
    pub const ESCAPE: u32 = 0x1B;
}

impl PageKey {
    /// The same keys as Chrome's zoom accelerators. Ctrl without Alt only, so
    /// AltGr (Ctrl+Alt) never matches. Shift is allowed with + and -, since
    /// `+` needs Shift on many layouts. `esc_exits`: Esc means "leave F11
    /// fullscreen" right now (see `esc_exits_fullscreen`); otherwise it is
    /// the page's.
    pub fn from_key(key: u32, m: Mods, esc_exits: bool) -> Option<Self> {
        use keys::*;
        let plain = !m.ctrl && !m.alt && !m.shift;
        let ctrl = m.ctrl && !m.alt;
        Some(match key {
            F11 if plain => Self::ToggleFullscreen,
            ESCAPE if plain && esc_exits => Self::ExitFullscreen,
            OEM_PLUS | ADD if ctrl => Self::ZoomIn,
            OEM_MINUS | SUBTRACT if ctrl => Self::ZoomOut,
            ZERO | NUMPAD0 if ctrl && !m.shift => Self::ZoomReset,
            _ => return None,
        })
    }

    /// Holding Ctrl+= / Ctrl+- keeps zooming; the rest act once.
    pub fn repeats(self) -> bool {
        matches!(self, Self::ZoomIn | Self::ZoomOut)
    }
}

/// Chromium's zoom levels (as in Chrome's own zoom steps), as factors.
pub const ZOOM_STEPS: [f64; 17] =
    [0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0];

/// The next zoom step above (`up`) or below `current`, or None at either end.
/// A factor between two steps (set by Ctrl+wheel, say) goes to the nearest
/// step in that direction.
pub fn next_zoom(current: f64, up: bool) -> Option<f64> {
    const EPS: f64 = 0.001;
    if up {
        ZOOM_STEPS.iter().copied().find(|&s| s > current + EPS)
    } else {
        ZOOM_STEPS.iter().rev().copied().find(|&s| s < current - EPS)
    }
}

/// A zoom factor as the whole percent the UI shows (100 = no zoom). Values
/// WebView2 should never report (not finite, out of its range) read as 100.
pub fn zoom_percent(factor: f64) -> u16 {
    if factor.is_finite() && (0.1..=10.0).contains(&factor) { (factor * 100.0).round() as u16 } else { 100 }
}

/// Run in every content frame before the page's own scripts: removes
/// Keyboard Lock (`navigator.keyboard.lock`), which in fullscreen would let
/// a page keep Esc and F11 from the app. Child frames cannot lock at all
/// (Chromium refuses them); they lose the method so they cannot lend it to
/// the top frame; WebView2 runs it in a fresh `about:blank` iframe too (the
/// self-test checks both). See `ui::BROWSER_ARGS` for the system keys.
pub const NO_KEYBOARD_LOCK: &str =
    "(() => { try { if (typeof Keyboard === 'function') delete Keyboard.prototype.lock; } catch (_) {} })();";

/// How long the fullscreen notice stays up.
pub const NOTICE_TIME: std::time::Duration = std::time::Duration::from_millis(3500);

/// Longest host the notice shows in full; a longer one keeps its end (the
/// part that names the site), e.g. `…login.example.com`.
const NOTICE_HOST_MAX: usize = 48;

/// Fullscreen state. The window is fullscreen (borderless, strip hidden,
/// the page over the whole window) while either reason holds.
#[derive(Debug, Default)]
pub struct Fullscreen {
    /// F11: the user put the window in fullscreen.
    pub window: bool,
    /// A page element (video, game) is fullscreen in this tab.
    pub page: Option<TabId>,
    /// The notice is on screen.
    notice: bool,
    /// Bumped by every notice shown or hidden, so only the latest notice's
    /// timer may hide it.
    notice_gen: u32,
}

impl Fullscreen {
    /// Whether the window should be (and, after `apply`, is) fullscreen.
    pub fn on(&self) -> bool {
        self.window || self.page.is_some()
    }

    /// Whether the fullscreen notice is on screen.
    pub fn notice_up(&self) -> bool {
        self.notice
    }

    /// Tab `id` is gone or dead: it no longer holds page fullscreen. True if
    /// it did.
    fn drop_page(&mut self, id: TabId) -> bool {
        if self.page == Some(id) {
            self.page = None;
            true
        } else {
            false
        }
    }
}

/// The notice's host for a tab URL, as the address box shows it (punycode),
/// so a look-alike Unicode name cannot pose as another site here either.
fn notice_host(url: &str) -> Option<String> {
    crate::url::host_of(&crate::policy::ascii_host(url))
}

/// The notice's two parts: the site (shown bold; None for F11 and for pages
/// without a host) and the rest. `host` comes from the tab's URL as the host
/// tracks it, never from the page's script.
pub fn notice_parts(page: bool, host: Option<&str>) -> (Option<String>, &'static str) {
    if !page {
        return (None, "Tam ekran — çıkmak için F11 ya da Esc");
    }
    match host.filter(|h| !h.is_empty()) {
        Some(h) => {
            let n = h.chars().count();
            let shown = if n > NOTICE_HOST_MAX {
                let tail: String = h.chars().skip(n - (NOTICE_HOST_MAX - 1)).collect();
                format!("…{tail}")
            } else {
                h.to_string()
            };
            (Some(shown), " tam ekran — çıkmak için Esc")
        }
        None => (None, "Bu sayfa tam ekran — çıkmak için Esc"),
    }
}

thread_local! {
    /// Read by the native key hook (same thread): Esc leaves fullscreen.
    static ESC_EXITS: Cell<bool> = const { Cell::new(false) };
}

/// Whether Esc now leaves fullscreen: whenever the window is fullscreen, for
/// either reason. The host restores the window itself, so a page cannot keep
/// the screen by ignoring Esc. Outside fullscreen Esc is the page's.
pub fn esc_exits_fullscreen() -> bool {
    ESC_EXITS.get()
}

pub fn on_command(app: &mut App, cmd: PageCmd) {
    match cmd {
        PageCmd::ZoomReset(id) => {
            set_zoom(app, id, 1.0);
            // The click took focus into the strip; the page gets its keys back.
            if app.tabs.active() == Some(id)
                && let Some(v) = app.view(id)
            {
                let _ = v.focus();
            }
        }
    }
}

pub fn on_key(app: &mut App, key: PageKey) {
    let active = app.tabs.active();
    match key {
        PageKey::ZoomIn | PageKey::ZoomOut => {
            let Some(id) = active else { return };
            let Some(current) = app.view(id).and_then(native::zoom_factor) else { return };
            if let Some(next) = next_zoom(current, key == PageKey::ZoomIn) {
                set_zoom(app, id, next);
            }
        }
        PageKey::ZoomReset => {
            if let Some(id) = active {
                set_zoom(app, id, 1.0);
            }
        }
        PageKey::ToggleFullscreen => {
            if app.fullscreen.on() {
                leave_fullscreen(app);
            } else {
                app.fullscreen.window = true;
                apply(app);
                show_notice(app, false);
            }
        }
        PageKey::ExitFullscreen => leave_fullscreen(app),
    }
}

/// Sets tab `id`'s zoom. WebView2 raises no ZoomFactorChanged for a factor
/// the host sets (only for Ctrl+wheel and per-site zoom on navigation), so
/// the tab's state follows here, from the factor WebView2 actually took.
pub fn set_zoom(app: &mut App, id: TabId, factor: f64) {
    let Some(v) = app.view(id) else { return };
    native::set_zoom_factor(v, factor);
    let now = native::zoom_factor(v).unwrap_or(factor);
    on_zoom(app, id, now);
}

/// Tab `id`'s zoom factor is now `factor` (set here, or reported by WebView2).
pub fn on_zoom(app: &mut App, id: TabId, factor: f64) {
    let percent = zoom_percent(factor);
    if let Some(t) = app.tabs.get_mut(id)
        && t.zoom != percent
    {
        t.zoom = percent;
        app.mark_dirty();
    }
}

/// A page entered (`on`) or left element fullscreen.
pub fn on_page_fullscreen(app: &mut App, id: TabId, on: bool) {
    if on {
        // Only the tab on screen may take the screen; a background tab that
        // got there anyway (a request racing a tab switch) is asked to leave.
        if app.tabs.active() != Some(id) {
            exit_page(app, id);
            return;
        }
        app.fullscreen.page = Some(id);
        apply(app);
        show_notice(app, true);
        return;
    }
    if app.fullscreen.drop_page(id) {
        hide_notice(app);
        apply(app);
    }
}

/// Tab `id`'s content process died or hung: it no longer holds the screen.
pub fn on_page_failed(app: &mut App, id: TabId) {
    if app.fullscreen.drop_page(id) {
        exit_page(app, id);
        hide_notice(app);
        apply(app);
    }
}

/// The tab on screen is about to become `next` (switch, close, new tab). A
/// page's fullscreen belongs to its tab: it ends with the switch. F11
/// fullscreen stays.
pub fn on_switch(app: &mut App, next: Option<TabId>) {
    if let Some(page) = app.fullscreen.page
        && Some(page) != next
    {
        exit_page(app, page);
        app.fullscreen.page = None;
        hide_notice(app);
        apply(app);
    }
}

/// F11 or Esc while fullscreen for either reason, or anything that needs the
/// strip (Ctrl+T, Ctrl+L): back to the normal window.
pub fn leave_fullscreen(app: &mut App) {
    if let Some(page) = app.fullscreen.page.take() {
        exit_page(app, page);
    }
    app.fullscreen.window = false;
    apply(app);
}

/// Asks tab `id`'s page to leave element fullscreen. Best effort: the
/// request runs in the page's own world, so its script can ignore or undo
/// it. The window is restored regardless (the page then only fills its own
/// content area below the strip, as before this feature).
fn exit_page(app: &App, id: TabId) {
    if let Some(v) = app.view(id) {
        let _ = v.evaluate_script("document.fullscreenElement && document.exitFullscreen().catch(() => {})");
    }
}

/// Puts the window, the strip and the page bounds in line with `fullscreen`.
fn apply(app: &mut App) {
    let on = app.fullscreen.on();
    ESC_EXITS.set(on);
    if !on {
        hide_notice(app);
    }
    if on != app.window.fullscreen().is_some() {
        // tao keeps the window's placement (size, position, maximized) while
        // fullscreen and puts it back on the way out.
        if on {
            let _ = app.ui.set_visible(false);
            app.window.set_fullscreen(Some(tao::window::Fullscreen::Borderless(None)));
        } else {
            app.window.set_fullscreen(None);
            let _ = app.ui.set_visible(true);
        }
        // The strip may have had keyboard focus; it is hidden now (or
        // back, but the page is what the user was looking at).
        focus_page(app);
    }
    app.layout();
    app.mark_dirty();
}

fn focus_page(app: &App) {
    if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
        let _ = v.focus();
    }
}

/// Shows the fullscreen notice (again) for `NOTICE_TIME`: the UI webview as
/// a band over the top centre of the page, above it in z-order. The page
/// keeps keyboard focus; clicks on the band go to the band, not the page.
fn show_notice(app: &mut App, page: bool) {
    if !app.fullscreen.on() {
        return;
    }
    let host = app.tabs.active().and_then(|id| app.tabs.get(id)).and_then(|t| notice_host(&t.url));
    let (site, rest) = notice_parts(page, host.as_deref());
    let mut js = String::from("xb.notice(");
    match &site {
        Some(s) => crate::tabs::push_json_str(&mut js, s),
        None => js.push_str("null"),
    }
    js.push(',');
    crate::tabs::push_json_str(&mut js, rest);
    js.push(')');
    let _ = app.ui.evaluate_script(&js);
    app.fullscreen.notice = true;
    app.fullscreen.notice_gen = app.fullscreen.notice_gen.wrapping_add(1);
    app.layout();
    native::raise(&app.ui);
    let _ = app.ui.set_visible(true);
    focus_page(app);
    let generation = app.fullscreen.notice_gen;
    let proxy = app.proxy().clone();
    std::thread::spawn(move || {
        std::thread::sleep(NOTICE_TIME);
        let _ = proxy.send_event(UserEvent::NoticeTimeout(generation));
    });
}

/// The timer of notice `generation` ran out.
pub fn on_notice_timeout(app: &mut App, generation: u32) {
    if generation == app.fullscreen.notice_gen {
        hide_notice(app);
    }
}

/// Takes the notice down (the strip stays hidden while fullscreen).
fn hide_notice(app: &mut App) {
    if !app.fullscreen.notice {
        return;
    }
    app.fullscreen.notice = false;
    app.fullscreen.notice_gen = app.fullscreen.notice_gen.wrapping_add(1);
    let on = app.fullscreen.on();
    if on {
        let _ = app.ui.set_visible(false);
    }
    let _ = app.ui.evaluate_script("xb.notice(null)");
    app.layout();
    if on {
        // A click on the band gave the UI webview the keyboard; hiding it
        // does not hand that back, so the page would silently lose its keys.
        focus_page(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false };
    const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false };
    const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };
    const ALTGR: Mods = Mods { ctrl: true, shift: false, alt: true };

    #[test]
    fn parses_zoom_reset_only_with_a_tab_id() {
        assert_eq!(PageCmd::parse("zoom-reset", "3"), Some(PageCmd::ZoomReset(3)));
        for bad in ["", "x", "-1", "1.5", "3:4", " 3", "99999999999"] {
            assert_eq!(PageCmd::parse("zoom-reset", bad), None, "{bad:?}");
        }
        assert_eq!(PageCmd::parse("zoom", "3"), None);
    }

    #[test]
    fn zoom_keys_match_chrome() {
        use keys::*;
        for k in [OEM_PLUS, ADD] {
            assert_eq!(PageKey::from_key(k, CTRL, false), Some(PageKey::ZoomIn));
            assert_eq!(PageKey::from_key(k, CTRL_SHIFT, false), Some(PageKey::ZoomIn), "Ctrl++ with Shift");
            assert_eq!(PageKey::from_key(k, NONE, false), None, "plain + types");
        }
        for k in [OEM_MINUS, SUBTRACT] {
            assert_eq!(PageKey::from_key(k, CTRL, false), Some(PageKey::ZoomOut));
            assert_eq!(PageKey::from_key(k, SHIFT, false), None, "Shift+- types _");
        }
        for k in [ZERO, NUMPAD0] {
            assert_eq!(PageKey::from_key(k, CTRL, false), Some(PageKey::ZoomReset));
            assert_eq!(PageKey::from_key(k, CTRL_SHIFT, false), None);
        }
        assert!(PageKey::ZoomIn.repeats() && PageKey::ZoomOut.repeats());
        assert!(!PageKey::ZoomReset.repeats() && !PageKey::ToggleFullscreen.repeats());
    }

    #[test]
    fn f11_toggles_and_esc_only_exits_when_armed() {
        use keys::*;
        assert_eq!(PageKey::from_key(F11, NONE, false), Some(PageKey::ToggleFullscreen));
        assert_eq!(PageKey::from_key(F11, CTRL, false), None);
        assert_eq!(PageKey::from_key(F11, SHIFT, false), None);
        assert_eq!(PageKey::from_key(ESCAPE, NONE, false), None, "Esc is the page's outside fullscreen");
        assert_eq!(PageKey::from_key(ESCAPE, NONE, true), Some(PageKey::ExitFullscreen));
        assert_eq!(PageKey::from_key(ESCAPE, SHIFT, true), None);
        assert_eq!(PageKey::from_key(ESCAPE, CTRL, true), None);
    }

    #[test]
    fn find_print_and_devtools_keys_are_not_ours() {
        use keys::*;
        for esc in [false, true] {
            assert_eq!(PageKey::from_key(F, CTRL, esc), None, "Ctrl+F: WebView2's find bar");
            assert_eq!(PageKey::from_key(P, CTRL, esc), None, "Ctrl+P: WebView2's print preview");
            assert_eq!(PageKey::from_key(F12, NONE, esc), None, "F12: DevTools stay off");
        }
    }

    #[test]
    fn altgr_never_matches() {
        for key in 0..=0xFF_u32 {
            for shift in [false, true] {
                for esc in [false, true] {
                    assert_eq!(PageKey::from_key(key, Mods { shift, ..ALTGR }, esc), None, "vk {key:#x}");
                }
            }
        }
    }

    #[test]
    fn zoom_steps_walk_chromes_levels() {
        assert_eq!(next_zoom(1.0, true), Some(1.1));
        assert_eq!(next_zoom(1.1, true), Some(1.25));
        assert_eq!(next_zoom(1.0, false), Some(0.9));
        assert_eq!(next_zoom(0.25, false), None, "smallest");
        assert_eq!(next_zoom(5.0, true), None, "largest");
        // Between two steps (Ctrl+wheel, float noise): the nearest one that way.
        assert_eq!(next_zoom(1.17, true), Some(1.25));
        assert_eq!(next_zoom(1.17, false), Some(1.1));
        assert_eq!(next_zoom(1.0000001, true), Some(1.1));
        assert_eq!(next_zoom(0.3333, true), Some(0.5), "1/3 as WebView2 may round it");
        // Every step goes up to the next and back.
        for w in ZOOM_STEPS.windows(2) {
            assert_eq!(next_zoom(w[0], true), Some(w[1]));
            assert_eq!(next_zoom(w[1], false), Some(w[0]));
        }
    }

    #[test]
    fn zoom_percent_rounds_and_rejects_nonsense() {
        assert_eq!(zoom_percent(1.0), 100);
        assert_eq!(zoom_percent(1.1), 110);
        assert_eq!(zoom_percent(1.0 / 3.0), 33);
        assert_eq!(zoom_percent(2.0 / 3.0), 67);
        assert_eq!(zoom_percent(5.0), 500);
        assert_eq!(zoom_percent(f64::NAN), 100);
        assert_eq!(zoom_percent(f64::INFINITY), 100);
        assert_eq!(zoom_percent(0.0), 100);
        assert_eq!(zoom_percent(1e9), 100);
    }

    #[test]
    fn fullscreen_is_on_for_either_reason() {
        let mut f = Fullscreen::default();
        assert!(!f.on());
        f.window = true;
        assert!(f.on());
        f.window = false;
        f.page = Some(2);
        assert!(f.on());
    }

    #[test]
    fn a_failed_tab_drops_only_its_own_page_fullscreen() {
        let mut f = Fullscreen { page: Some(2), ..Fullscreen::default() };
        assert!(!f.drop_page(3), "another tab");
        assert_eq!(f.page, Some(2));
        assert!(f.drop_page(2));
        assert!(!f.on());
        assert!(!f.drop_page(2), "already dropped");
        let mut f = Fullscreen { window: true, page: Some(2), ..Fullscreen::default() };
        assert!(f.drop_page(2));
        assert!(f.on(), "F11 stays");
    }

    #[test]
    fn notice_names_the_site_and_the_way_out() {
        assert_eq!(notice_parts(false, Some("example.com")), (None, "Tam ekran — çıkmak için F11 ya da Esc"));
        assert_eq!(notice_parts(true, Some("example.com")), (Some("example.com".into()), " tam ekran — çıkmak için Esc"));
        assert_eq!(notice_parts(true, None), (None, "Bu sayfa tam ekran — çıkmak için Esc"));
        assert_eq!(notice_parts(true, Some("")), (None, "Bu sayfa tam ekran — çıkmak için Esc"));
        // A long host keeps its end, which names the site.
        let long = format!("{}evil.example", "accounts.google.com.".repeat(4));
        let site = notice_parts(true, Some(&long)).0.unwrap();
        assert_eq!(site.chars().count(), NOTICE_HOST_MAX);
        assert!(site.starts_with('…') && site.ends_with(".evil.example"), "{site}");
        let exact = "a".repeat(NOTICE_HOST_MAX);
        assert_eq!(notice_parts(true, Some(&exact)).0.as_deref(), Some(exact.as_str()));
    }

    #[test]
    fn notice_names_a_unicode_host_in_punycode() {
        assert_eq!(notice_host("https://bücher.example/a").as_deref(), Some("xn--bcher-kva.example"));
        assert_eq!(notice_host("https://аррӏе.com/").as_deref(), Some("xn--80ak6aa92e.com"));
        assert_eq!(notice_host("https://Example.COM./x").as_deref(), Some("example.com"));
        assert_eq!(notice_host("about:blank"), None);
    }
}
