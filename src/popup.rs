//! An extension's popup (its `action.default_popup`), opened from the strip's
//! extensions button (`ext_ui`). Built when it opens and dropped when it
//! closes, so nothing of it exists at start-up.
//!
//! - A child webview of the main window in the default WebView2 profile (the
//!   one with the extensions), under the button. It loads
//!   `chrome-extension://<id>/<popup>` and may not leave that extension:
//!   other navigations, new windows and downloads are refused.
//! - Sized to its page, as Chrome sizes a popup: from `MIN_W` x `MIN_H` to
//!   `MAX_W` x `MAX_H`, and inside the window. Until its page is first
//!   measured it is `POPUP_W` x `POPUP_H` and see-through, so no empty box
//!   shows; it is measured again while it is open (`MEASURE_EVERY`), so a
//!   page that draws itself late, or changes, gets its size too.
//! - No IPC handler, no web messages, no host objects: nothing in it can
//!   reach the UI or the app. No other application is started from it,
//!   not even after a question (`native::attach_popup_policy`), and its
//!   frames keep the content tabs' navigation rules.
//! - Esc always closes it: the extension cannot receive Esc.
//! - `chrome.tabs.query` answers with WebView2's own tab object for the
//!   app's active content tab, found by its address (an init script that
//!   runs only on `chrome-extension:` pages): a popup's own window is not a
//!   browser tab, so WebView2 would not know which tab is meant. None of the
//!   app's tab ids reaches the extension.
//! - It closes on Esc and on its own `window.close()` (focus goes back to
//!   the page), and when the focus goes to another webview or program (a
//!   click outside, another window), on a tab switch, and when the window
//!   moves or is resized.
//! - It opens on the event loop's next pass after its list (the overlay)
//!   is hidden (`ext_ui::open_popup`), and takes the keyboard itself: the
//!   page is not given it in between (WebView2 would hand it over after
//!   the popup took it, and the popup would close).

use crate::app::{App, UI_HEIGHT, UserEvent};
use crate::commands::UiCommand;
use crate::ext_ui::ExtCmd;
use crate::native;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use wry::{WebView, WebViewBuilder};

/// Size of the popup before its page is measured (logical px).
pub const POPUP_W: f64 = 360.0;
pub const POPUP_H: f64 = 480.0;
/// The smallest and largest popup, as in Chrome (CSS px, which are logical
/// px here: the popup is not zoomed).
pub const MIN_W: f64 = 25.0;
pub const MIN_H: f64 = 25.0;
pub const MAX_W: f64 = 800.0;
pub const MAX_H: f64 = 600.0;
/// Gap to the window's edges.
const EDGE: f64 = 8.0;
/// How often the open popup's page is measured: often while it loads,
/// then less.
const MEASURE_FIRST: Duration = Duration::from_millis(50);
const MEASURE_EVERY: Duration = Duration::from_millis(250);
/// Measurements at `MEASURE_FIRST` before `MEASURE_EVERY`.
const FIRST_MEASURES: u32 = 20;

/// The page's size, in CSS px: its content's widest unwrapped width
/// (`max-content`, with the body's margins), and its height at that width.
/// The root's own `style` is put back as it was, before anything is drawn.
/// `null` while the page is still being parsed.
const MEASURE: &str = r#"(() => {
  const r = document.documentElement;
  if (!r || document.readyState === "loading") return null;
  const had = r.getAttribute("style");
  r.style.setProperty("width", "max-content", "important");
  const w = Math.ceil(r.getBoundingClientRect().width);
  r.style.setProperty("width", w + "px", "important");
  const h = Math.ceil(r.getBoundingClientRect().height);
  if (had === null) r.removeAttribute("style"); else r.setAttribute("style", had);
  return [w, h];
})()"#;

/// Window outer position, inner size, scale factor (as `overlay` keeps).
type Place = (Option<(i32, i32)>, (u32, u32), u64);

#[derive(Default)]
pub struct Popup {
    view: Option<WebView>,
    /// The extension whose popup is open.
    ext: Option<String>,
    /// Bumped at each open: a close event from an earlier popup is stale.
    generation: u32,
    placed: Option<Place>,
    /// The right edge it hangs from (logical px).
    anchor: f64,
    /// Its page's last measured size (CSS px); `None` until measured.
    size: Option<(u32, u32)>,
    /// Cleared when it closes: its measuring timer stops.
    alive: Option<Arc<AtomicBool>>,
}

fn place(app: &App) -> Place {
    let pos = app.window.outer_position().ok().map(|p| (p.x, p.y));
    let size = app.window.inner_size();
    (pos, (size.width, size.height), app.window.scale_factor().to_bits())
}

/// Where a popup of `w` x `h` goes in a window of `size`: under the strip,
/// its right edge at `right`, within Chrome's limits and the window.
pub fn popup_rect(size: tao::dpi::LogicalSize<f64>, right: f64, w: f64, h: f64) -> wry::Rect {
    let w = w.clamp(MIN_W, MAX_W).min((size.width - 2.0 * EDGE).max(1.0));
    let x = (right - w).min(size.width - w - EDGE).max(0.0);
    let y = UI_HEIGHT + 2.0;
    let h = h.clamp(MIN_H, MAX_H).min((size.height - y - EDGE).max(1.0));
    crate::overlay::rect(x, y, w, h)
}

/// `MEASURE`'s answer: `[w, h]` in CSS px, or `None` (still parsing, or
/// anything else).
fn parse_size(json: &str) -> Option<(u32, u32)> {
    let (w, h): (u32, u32) = serde_json::from_str(json).ok()?;
    (w <= 100_000 && h <= 100_000).then_some((w, h))
}

/// The init script: `chrome.tabs.query` answers with the browser's own tab
/// object (WebView2's, as the extension's permissions let it see it) whose
/// address is `__URL__` (the active content tab's), for a query it
/// matches; with no tab when there is none, or more than one. Nothing of
/// the app (its tab ids) goes in: the id, address and title all come from
/// WebView2. Callback and promise forms both work.
const SHIM: &str = r#"(() => {
  if (location.protocol !== "chrome-extension:") return;
  const want = __URL__;
  const esc = (s) => s.replace(/[.+?^${}()|[\]\\/]/g, "\\$&");
  const matches = (p, u) => {
    if (typeof p !== "string") return false;
    if (p === "<all_urls>") return /^(https?|file|ftp):/.test(u);
    try { return new RegExp("^" + p.split("*").map(esc).join(".*") + "$").test(u); } catch (e) { return false; }
  };
  try {
    let c = window.chrome;
    if (!c) { c = {}; Object.defineProperty(window, "chrome", { value: c, configurable: true, writable: true, enumerable: true }); }
    const t = c.tabs;
    const original = t && typeof t.query === "function" ? t.query : null;
    const all = () => new Promise((done) => {
      if (!original || want === "") { done([]); return; }
      try {
        original.call(t, {}, (tabs) => { void (c.runtime && c.runtime.lastError); done(Array.isArray(tabs) ? tabs : []); });
      } catch (e) { done([]); }
    });
    const query = function (info, cb) {
      const q = info && typeof info === "object" ? info : {};
      const out = all().then((tabs) => {
        const hit = tabs.filter((x) => x && typeof x.url === "string" && !x.url.startsWith("chrome-extension:") && x.url === want);
        if (hit.length !== 1) return [];
        const tab = hit[0];
        let ok = q.active !== false && q.currentWindow !== false && q.lastFocusedWindow !== false;
        if (ok && q.url !== undefined) ok = (Array.isArray(q.url) ? q.url : [q.url]).some((p) => matches(p, tab.url));
        if (ok && typeof q.title === "string") ok = matches(q.title, String(tab.title || ""));
        return ok ? [tab] : [];
      });
      if (typeof cb === "function") { out.then((r) => cb(r)); return undefined; }
      return out;
    };
    if (t) {
      try { Object.defineProperty(t, "query", { value: query, configurable: true, writable: true }); } catch (e) {}
      if (t.query !== query) { try { t.query = query; } catch (e) {} }
    }
    if (!t || t.query !== query) {
      const own = Object.create(t || null);
      own.query = query;
      Object.defineProperty(c, "tabs", { value: own, configurable: true, writable: true, enumerable: true });
    }
  } catch (e) {}
})();"#;

/// The active content tab's address, as a JS string: empty for the blank
/// page or a refused address (no tab an extension can act on).
fn active_url_json(app: &App) -> String {
    let url = app.tabs.active().and_then(|id| app.tabs.get(id)).map(|t| t.url.as_str()).filter(|u| crate::url::is_tab_url(u)).unwrap_or("");
    serde_json::Value::from(url).to_string()
}

/// The init script for the current active tab.
pub fn shim(app: &App) -> String {
    SHIM.replace("__URL__", &active_url_json(app))
}

/// Opens extension `id`'s popup page `path` (checked by the caller,
/// `ext_ui::page_path`), its right edge at `right`.
pub fn open(app: &mut App, id: &str, path: &str, right: f64) {
    close(app, false);
    let prefix = format!("chrome-extension://{id}/");
    let url = format!("{prefix}{path}");
    let bounds = popup_rect(crate::overlay::win_size(app), right, POPUP_W, POPUP_H);
    let script = shim(app);
    let builder = WebViewBuilder::new_with_web_context(&mut app.context)
        .with_bounds(bounds)
        .with_visible(false)
        // See-through until its page is measured (`on_size`).
        .with_background_color((255, 255, 255, 0))
        .with_initialization_script_for_main_only(script, true)
        .with_navigation_handler(move |to| to.starts_with(&prefix))
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false);
    // No profile name: the default profile, where the extensions are.
    #[cfg(windows)]
    let builder = {
        use wry::WebViewBuilderExtWindows;
        builder.with_environment(app.environment.clone())
    };
    let view = match builder.build_as_child(&app.window) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("x-browser: cannot create the extension popup: {e}");
            app.focus_after_switch();
            return;
        }
    };
    app.popup.generation = app.popup.generation.wrapping_add(1);
    let generation = app.popup.generation;
    // Not one event may be missed, nor may a page load before the doors
    // are shut: the page loads last.
    let ready = native::harden_content(&view).is_ok()
        && native::attach_popup_policy(&view).is_ok()
        && native::attach_popup_events(&view, generation, app.proxy()).is_ok()
        && view.load_url(&url).is_ok();
    if !ready {
        eprintln!("x-browser: cannot set up the extension popup");
        app.focus_after_switch();
        return;
    }
    native::raise(&view);
    let _ = view.set_visible(true);
    let _ = view.focus();
    app.popup.view = Some(view);
    app.popup.ext = Some(id.to_owned());
    app.popup.placed = Some(place(app));
    app.popup.anchor = right;
    app.popup.size = None;
    app.popup.alive = Some(start_measuring(app.proxy().clone(), generation));
}

/// A timer that asks for popup `generation`'s page to be measured, until
/// the flag it returns is cleared.
fn start_measuring(proxy: native::Proxy, generation: u32) -> Arc<AtomicBool> {
    let alive = Arc::new(AtomicBool::new(true));
    let flag = alive.clone();
    let spawned = std::thread::Builder::new().name("popup-measure".into()).spawn(move || {
        let mut n = 0u32;
        loop {
            std::thread::sleep(if n < FIRST_MEASURES { MEASURE_FIRST } else { MEASURE_EVERY });
            n = n.saturating_add(1);
            let event = UserEvent::Ui(UiCommand::Ext(ExtCmd::PopupMeasure { generation }));
            if !flag.load(Ordering::Relaxed) || proxy.send_event(event).is_err() {
                return;
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("x-browser: cannot measure the extension popup: {e}");
    }
    alive
}

/// Measures popup `generation`'s page (`ExtCmd::PopupMeasure`); the answer
/// comes back as `ExtCmd::PopupSize`.
pub fn measure(app: &mut App, generation: u32) {
    if generation != app.popup.generation {
        return;
    }
    let Some(v) = &app.popup.view else { return };
    let proxy = app.proxy().clone();
    let _ = v.evaluate_script_with_callback(MEASURE, move |r| {
        if let Some((w, h)) = parse_size(&r) {
            let _ = proxy.send_event(UserEvent::Ui(UiCommand::Ext(ExtCmd::PopupSize { generation, w, h })));
        }
    });
}

/// Popup `generation`'s page measures `w` x `h`: the popup takes that size
/// (and, the first time, its white background).
pub fn on_size(app: &mut App, generation: u32, w: u32, h: u32) {
    if generation != app.popup.generation || app.popup.view.is_none() || app.popup.size == Some((w, h)) {
        return;
    }
    let first = app.popup.size.is_none();
    app.popup.size = Some((w, h));
    let bounds = popup_rect(crate::overlay::win_size(app), app.popup.anchor, f64::from(w), f64::from(h));
    let Some(v) = &app.popup.view else { return };
    let _ = v.set_bounds(bounds);
    if first {
        let _ = v.set_background_color((255, 255, 255, 255));
    }
}

/// Closes the popup, if one is open. `refocus`: the popup had the keyboard
/// and nothing else took it; it goes back to the page (or the address bar).
#[track_caller]
pub fn close(app: &mut App, refocus: bool) {
    #[cfg(feature = "selftest")]
    if app.popup.view.is_some() {
        crate::selftest::trace(&format!("popup closed from {}", std::panic::Location::caller()));
    }
    let p = &mut app.popup;
    p.ext = None;
    p.placed = None;
    p.size = None;
    if let Some(alive) = p.alive.take() {
        alive.store(false, Ordering::Relaxed);
    }
    let Some(view) = p.view.take() else { return };
    let _ = view.set_visible(false);
    drop(view);
    if refocus {
        app.focus_after_switch();
    }
}

/// The popup asked to close or lost the focus (`ExtCmd::PopupClosed`).
pub fn on_closed(app: &mut App, generation: u32, refocus: bool) {
    if generation != app.popup.generation || app.popup.view.is_none() {
        return;
    }
    // A focus loss (`!refocus`) closes it only for a click outside: the
    // focus in another webview or program. One gone to the window itself
    // or to a hidden webview is the list it was picked from going away
    // (WebView2 hands a hidden webview's focus back to its parent, and that
    // can come after the popup took the focus): the popup takes it back.
    if !refocus && let Some(v) = &app.popup.view {
        let at = native::popup_focus(&app.window, v);
        #[cfg(feature = "selftest")]
        crate::selftest::trace(&format!(
            "popup lost the focus: {at:?} ({}, foreground {})",
            native::focus_class(),
            native::is_foreground(&app.window)
        ));
        match at {
            native::FocusAt::Popup => return,
            native::FocusAt::Nowhere => {
                let _ = v.focus();
                return;
            }
            native::FocusAt::Elsewhere => {}
        }
        // While the self-test has X-Browser count as in front
        // (`App::foreground`), a focus loss that only comes from the window
        // really being behind another program is no click outside.
        #[cfg(feature = "selftest")]
        if app.foreground == Some(true) && !native::is_foreground(&app.window) {
            return;
        }
    }
    close(app, refocus && native::is_foreground(&app.window));
}

/// The extension whose popup is open.
pub fn extension(app: &App) -> Option<&str> {
    app.popup.ext.as_deref()
}

/// The window moved, was resized or rescaled.
pub fn on_window_changed(app: &mut App) {
    if app.popup.view.is_some() && app.popup.placed != Some(place(app)) {
        close(app, native::is_foreground(&app.window));
    }
}

/// The window lost activation to another window.
pub fn on_window_unfocused(app: &mut App) {
    if app.popup.view.is_some() && !app.in_front() {
        close(app, false);
    }
}

#[cfg(feature = "selftest")]
pub fn view(app: &App) -> Option<&WebView> {
    app.popup.view.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(r: wry::Rect) -> (f64, f64, f64, f64) {
        let (p, s) = (r.position.to_logical::<f64>(1.0), r.size.to_logical::<f64>(1.0));
        (p.x, p.y, s.width, s.height)
    }

    #[test]
    fn popup_hangs_under_the_button_inside_the_window() {
        let size = tao::dpi::LogicalSize::new(1200.0, 800.0);
        assert_eq!(parts(popup_rect(size, 1100.0, POPUP_W, POPUP_H)), (740.0, 42.0, 360.0, 480.0));
        assert_eq!(parts(popup_rect(size, 1200.0, POPUP_W, POPUP_H)).0, 832.0);
        assert_eq!(parts(popup_rect(size, 10.0, POPUP_W, POPUP_H)).0, 0.0);
        for (w, h, right) in [(480.0, 240.0, 400.0), (300.0, 200.0, 290.0), (1920.0, 1080.0, 1900.0)] {
            for (pw, ph) in [(POPUP_W, POPUP_H), (800.0, 600.0), (25.0, 25.0)] {
                let (x, y, pw, ph) = parts(popup_rect(tao::dpi::LogicalSize::new(w, h), right, pw, ph));
                assert!(x >= 0.0 && y >= 0.0 && x + pw <= w && y + ph <= h, "{w}x{h}");
            }
        }
    }

    #[test]
    fn popup_takes_its_pages_size_within_chromes_limits() {
        let size = tao::dpi::LogicalSize::new(1200.0, 800.0);
        // Its right edge stays under the button whatever its width.
        assert_eq!(parts(popup_rect(size, 1100.0, 216.0, 90.0)), (884.0, 42.0, 216.0, 90.0));
        assert_eq!(parts(popup_rect(size, 1100.0, 3.0, 0.0)), (1075.0, 42.0, 25.0, 25.0));
        assert_eq!(parts(popup_rect(size, 1100.0, 5000.0, 5000.0)), (300.0, 42.0, 800.0, 600.0));
    }

    #[test]
    fn only_a_measured_pair_is_a_size() {
        assert_eq!(parse_size("[216,90]"), Some((216, 90)));
        for bad in ["null", "", "[1]", "[1,2,3]", "[-1,2]", "[1.5,2]", "\"x\"", "[1,200000]"] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
    }

    #[test]
    fn measuring_puts_the_roots_style_back() {
        assert!(MEASURE.contains(r#"document.readyState === "loading") return null"#));
        assert!(MEASURE.contains(r#"if (had === null) r.removeAttribute("style"); else r.setAttribute("style", had);"#));
        assert!(MEASURE.find("setAttribute").unwrap() < MEASURE.find("return [w, h]").unwrap());
    }

    #[test]
    fn the_shim_runs_only_on_extension_pages_and_gives_no_app_ids() {
        let js = SHIM.replace("__URL__", r#""https://example.com/""#);
        assert!(js.starts_with("(() => {\n  if (location.protocol !== \"chrome-extension:\") return;"));
        assert!(!js.contains("__URL__") && js.contains(r#"const want = "https://example.com/";"#));
        // The tab handed out is WebView2's own; the app's ids never go in.
        assert!(js.contains("original.call(t, {},") && js.contains("hit.length !== 1"));
        // A pattern's dots and the like match only themselves.
        assert!(js.contains(r#""\\$&""#));
        for bad in ["ipc", "postMessage", "eval(", "\"id\"", "windowId"] {
            assert!(!js.contains(bad), "{bad}");
        }
    }
}
