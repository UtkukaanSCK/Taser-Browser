//! The overlay: a third child webview, made on first use, that floats over
//! the page for the address-bar suggestions and the Ctrl+H history list
//! (design doc §1). Same origin, CSP and IPC as the UI webview.
//!
//! Suggestions never take focus: the address input keeps it and the UI
//! forwards ↑/↓/Enter/Esc here. The history list takes focus (its search box).
//! Either closes on Esc, a focus move elsewhere (the pages' DOM blur), window
//! deactivation, resize, move, scale change, tab switch, navigation and a UI
//! renderer failure.

use crate::app::{App, UI_HEIGHT, UserEvent};
use crate::commands::{MAX_NAV_INPUT, UiCommand};
use crate::history::Entry;
use crate::native;
use crate::ui;
use serde::Serialize;
use std::time::{Duration, Instant};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::window::Theme;
use wry::{Rect, WebView};

/// Longest history-list search text (bytes).
pub const MAX_FILTER: usize = 512;
/// Row height and padding of the suggestion list (logical px); must match
/// overlay.html.
const ROW_H: f64 = 30.0;
const LIST_PAD: f64 = 10.0;
/// The suggestion list is at least this wide, the history panel at most.
const SUGGEST_MIN_W: f64 = 360.0;
const PANEL_W: f64 = 520.0;
const PANEL_H: f64 = 560.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
}

/// Messages from the UI's address bar and from the overlay page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayCmd {
    /// The address text changed: `x`, `w` are the address box's left edge and
    /// width (CSS px of the UI webview, which is never zoomed).
    Suggest { x: u16, w: u16, text: String },
    /// A key the address input forwarded while suggestions are open.
    Key(Key),
    /// Esc, an emptied input or the input's blur.
    SuggestClose,
    /// The overlay page has loaded.
    Ready,
    /// Row `i` of the list drawn as `serial` was picked.
    Open { serial: u32, i: usize },
    /// The history list lost focus.
    Close,
    /// Esc in the history list.
    Esc,
    Filter(String),
    Delete { serial: u32, i: usize },
    DeleteAll,
    ClearData,
    /// Internal: ClearBrowsingData finished (true: it succeeded). Never parsed.
    DataCleared(bool),
}

impl OverlayCmd {
    pub const NAMES: &[&str] = &[
        "suggest",
        "suggest-key",
        "suggest-close",
        "ov-ready",
        "ov-open",
        "ov-close",
        "ov-esc",
        "ov-filter",
        "ov-delete",
        "ov-delete-all",
        "ov-clear-data",
    ];

    pub fn parse(name: &str, arg: Option<&str>) -> Option<Self> {
        let row = |a: &str| {
            let (g, i) = a.split_once(':')?;
            Some((g.parse::<u32>().ok()?, i.parse::<usize>().ok()?))
        };
        Some(match (name, arg) {
            ("suggest", Some(a)) => {
                let (x, rest) = a.split_once(':')?;
                let (w, text) = rest.split_once(':')?;
                if text.len() > MAX_NAV_INPUT {
                    return None;
                }
                Self::Suggest { x: x.parse().ok()?, w: w.parse().ok()?, text: text.to_string() }
            }
            ("suggest-key", Some("up")) => Self::Key(Key::Up),
            ("suggest-key", Some("down")) => Self::Key(Key::Down),
            ("suggest-key", Some("enter")) => Self::Key(Key::Enter),
            ("suggest-close", None) => Self::SuggestClose,
            ("ov-ready", None) => Self::Ready,
            ("ov-open", Some(a)) => {
                let (serial, i) = row(a)?;
                Self::Open { serial, i }
            }
            ("ov-close", None) => Self::Close,
            ("ov-esc", None) => Self::Esc,
            ("ov-filter", None) => Self::Filter(String::new()),
            ("ov-filter", Some(a)) if a.len() <= MAX_FILTER => Self::Filter(a.to_string()),
            ("ov-delete", Some(a)) => {
                let (serial, i) = row(a)?;
                Self::Delete { serial, i }
            }
            ("ov-delete-all", None) => Self::DeleteAll,
            ("ov-clear-data", None) => Self::ClearData,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Hidden,
    Suggest,
    History,
    /// The bookmark editor or list (see `bookmarks`).
    Bookmarks,
    /// The main menu (see `menu`).
    Menu,
    /// The settings page (see `settings`).
    Settings,
}

impl Mode {
    /// The list takes the keyboard (a search box, arrow keys), as opposed to
    /// the suggestions, which the address bar keeps typing into.
    fn owns_keyboard(self) -> bool {
        matches!(self, Mode::History | Mode::Bookmarks | Mode::Menu | Mode::Settings)
    }
}

#[derive(Default)]
pub struct Overlay {
    view: Option<WebView>,
    ready: bool,
    pub mode: Mode,
    /// Bumped whenever a new list is drawn; picks name the list they were on.
    serial: u32,
    /// The addresses of the list drawn as `serial`.
    items: Vec<String>,
    /// Suggestion row chosen with ↑/↓.
    sel: Option<usize>,
    /// The text typed in the address bar.
    query: String,
    /// The address box the suggestions hang under: x and width.
    anchor: (u16, u16),
    filter: String,
    /// The window's place (position, size, scale) when it was last shown.
    placed: Option<Place>,
    /// Shown once the page is ready: script and bounds.
    pending: Option<(String, Rect)>,
    built_at: Option<Instant>,
    /// From the start of the build to the first show.
    pub first_show: Option<Duration>,
    /// The webview was kept up and focused for the next page (`hand_over`).
    handed: bool,
    /// Result of the last ClearBrowsingData.
    #[cfg(feature = "selftest")]
    pub cleared: Option<bool>,
}

impl Overlay {
    #[cfg(feature = "selftest")]
    pub fn view(&self) -> Option<&WebView> {
        self.view.as_ref()
    }
}

#[derive(Serialize)]
struct SuggestView<'a> {
    serial: u32,
    items: &'a [crate::bookmarks::Marked],
}

#[derive(Serialize)]
struct HistoryView<'a> {
    serial: u32,
    items: &'a [Entry],
    total: usize,
    filter: &'a str,
}

/// Background before the first paint: the panel's surface colour.
fn surface(theme: Theme) -> (u8, u8, u8, u8) {
    match theme {
        Theme::Dark => (0x2c, 0x2c, 0x2e, 0xff),
        _ => (0xff, 0xff, 0xff, 0xff),
    }
}

pub(crate) fn win_size(app: &App) -> LogicalSize<f64> {
    app.window.inner_size().to_logical(app.window.scale_factor())
}

pub(crate) fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { position: LogicalPosition::new(x, y).into(), size: LogicalSize::new(w.max(1.0), h.max(1.0)).into() }
}

/// Where `rows` suggestions go, under an address box at `x` of width `w`.
fn suggest_rect(size: LogicalSize<f64>, x: f64, w: f64, rows: usize) -> Rect {
    let w = w.max(SUGGEST_MIN_W).min(size.width);
    let x = x.min(size.width - w).max(0.0);
    let y = UI_HEIGHT - 4.0;
    let h = (rows as f64 * ROW_H + LIST_PAD).min((size.height - y).max(1.0));
    rect(x, y, w, h)
}

/// The history panel: at the right, under the strip.
pub(crate) fn panel_rect(size: LogicalSize<f64>) -> Rect {
    let w = PANEL_W.min(size.width - 16.0);
    let y = UI_HEIGHT + 4.0;
    rect((size.width - w - 8.0).max(0.0), y, w, PANEL_H.min(size.height - y - 8.0))
}

/// Makes the overlay webview if there is none. False if it cannot be made.
fn ensure_view(app: &mut App) -> bool {
    if app.overlay.view.is_some() {
        return true;
    }
    let proxy = app.proxy().clone();
    let started = Instant::now();
    let bg = surface(app.window.theme());
    let one = rect(0.0, 0.0, 1.0, 1.0);
    #[cfg(windows)]
    let built = ui::build_overlay(&app.window, &mut app.context, &app.environment, &proxy, one, bg);
    #[cfg(not(windows))]
    let built = ui::build_overlay(&app.window, &mut app.context, &proxy, one, bg);
    let view = match built {
        Ok(v) => v,
        Err(e) => {
            eprintln!("x-browser: cannot create the overlay: {e}");
            return false;
        }
    };
    // Its renderer is the UI's kind: a failure reloads the UI (and drops it).
    let _ = native::attach_accelerators(&view, &proxy);
    let _ = native::attach_process_failed(&view, None, &proxy);
    let o = &mut app.overlay;
    o.view = Some(view);
    o.ready = false;
    o.built_at = Some(started);
    true
}

/// The window's outer position, inner size and scale factor.
type Place = (Option<(i32, i32)>, (u32, u32), u64);

fn place(app: &App) -> Place {
    let pos = app.window.outer_position().ok().map(|p| (p.x, p.y));
    let size = app.window.inner_size();
    (pos, (size.width, size.height), app.window.scale_factor().to_bits())
}

/// Draws `js` in the overlay at `bounds` in `mode` (once the page is ready).
pub(crate) fn show(app: &mut App, mode: Mode, js: String, bounds: Rect) {
    if !ensure_view(app) {
        return;
    }
    app.overlay.placed = Some(place(app));
    let was = app.overlay.mode;
    app.overlay.mode = mode;
    if !app.overlay.ready {
        app.overlay.pending = Some((js, bounds));
        return;
    }
    present(&mut app.overlay, was, &js, bounds);
}

fn present(o: &mut Overlay, was: Mode, js: &str, bounds: Rect) {
    let Some(view) = &o.view else { return };
    let _ = view.evaluate_script(js);
    let _ = view.set_bounds(bounds);
    native::raise(view);
    if was == Mode::Hidden {
        let _ = view.set_visible(true);
        if o.first_show.is_none() {
            o.first_show = o.built_at.map(|t| t.elapsed());
            crate::timing::mark("overlay-shown");
        }
    }
    // (A handed-over webview has the focus already; moving it there again
    // would blur the page, and the blur would close it.)
    let handed = std::mem::take(&mut o.handed);
    if o.mode.owns_keyboard() && was != o.mode && !handed {
        let _ = view.focus();
    }
}

/// Runs `js` in the overlay while it shows `mode` (a menu redrawing a number).
pub(crate) fn run(app: &App, mode: Mode, js: &str) {
    if app.overlay.mode == mode
        && let Some(view) = &app.overlay.view
    {
        let _ = view.evaluate_script(js);
    }
}

/// Hides the overlay. Its last list stays known, so a pick that races the
/// close (a click blurs the address input first) still lands.
pub fn close(app: &mut App) {
    reset(app, true);
}

/// Clears the overlay's page and mode but leaves its webview shown and
/// focused, for a menu item that opens another page in it right away
/// (hiding and showing the webview blurs its page, and that blur, arriving
/// after the new page is up, would close it again). The caller must show
/// something.
pub(crate) fn hand_over(app: &mut App) {
    // Only a page that owns the keyboard is handed over; Suggest leaves the
    // keyboard in the address input, so the next page must take it.
    let was_up = app.overlay.mode.owns_keyboard() && app.overlay.view.is_some();
    reset(app, false);
    // (`|=`: a second hand-over of the same page, now Hidden, keeps the first.)
    app.overlay.handed |= was_up;
}

fn reset(app: &mut App, hide: bool) {
    let o = &mut app.overlay;
    let was = std::mem::take(&mut o.mode);
    o.pending = None;
    o.sel = None;
    if was == Mode::Hidden {
        return;
    }
    if let Some(view) = &o.view {
        let _ = view.evaluate_script("xbo.clear();xbBm.hide()");
        if hide {
            let _ = view.set_visible(false);
        }
    }
    if was == Mode::Suggest {
        let _ = app.ui.evaluate_script("xbSuggest.set(false,null)");
    }
}

/// Closes the overlay for a reason of its own (a page started loading, the
/// window moved or was resized, the UI failed). The history list held the
/// keyboard, so it goes back where Esc would put it, as long as X-Browser is
/// still the foreground window.
pub fn dismiss(app: &mut App) {
    let had_keyboard = app.overlay.mode.owns_keyboard();
    close(app);
    if had_keyboard && native::is_foreground(&app.window) {
        app.focus_after_switch();
    }
}

/// The window was resized, moved or rescaled. Closes the overlay only if the
/// window is no longer where it was when the overlay was drawn: leaving F11
/// for Ctrl+H resizes it before the list is drawn, and those events arrive
/// after it.
pub fn on_window_changed(app: &mut App) {
    if app.overlay.mode != Mode::Hidden && app.overlay.placed != Some(place(app)) {
        dismiss(app);
    }
}

/// The window lost activation. Clicks inside the window keep it active, but
/// tao also reports a child webview taking focus; so only close when another
/// window really is in front.
pub fn on_window_unfocused(app: &mut App) {
    if app.overlay.mode != Mode::Hidden && !native::is_foreground(&app.window) {
        close(app);
    }
}

/// The UI (or overlay) renderer failed: drop the overlay; it is made again
/// on next use.
pub fn on_ui_failed(app: &mut App) {
    dismiss(app);
    let first_show = app.overlay.first_show;
    if let Some(view) = app.overlay.view.take() {
        let _ = view.set_visible(false);
    }
    app.overlay = Overlay { first_show, ..Overlay::default() };
}

/// Ctrl+H: opens the history list, or closes it.
pub fn toggle_history(app: &mut App) {
    if app.overlay.mode == Mode::History {
        close(app);
        app.focus_after_switch();
        return;
    }
    close(app);
    // Karar: the list belongs to the browser frame, so fullscreen gives way
    // first (as Ctrl+L does), and Esc never has two meanings.
    if app.fullscreen.on() {
        crate::page_tools::leave_fullscreen(app);
    }
    app.overlay.filter.clear();
    render_history(app, false);
}

fn render_history(app: &mut App, keep: bool) {
    let (items, total) = app.history.list(&app.overlay.filter);
    let o = &mut app.overlay;
    o.serial = o.serial.wrapping_add(1);
    o.items = items.iter().map(|e| e.u.clone()).collect();
    let view = HistoryView { serial: o.serial, items: &items, total, filter: &o.filter };
    let Ok(json) = serde_json::to_string(&view) else { return };
    let js = format!("xbo.history({json},{keep})");
    let bounds = panel_rect(win_size(app));
    show(app, Mode::History, js, bounds);
}

fn suggest(app: &mut App, x: u16, w: u16, text: String) {
    if app.fullscreen.on() || app.overlay.mode.owns_keyboard() {
        return;
    }
    let found = crate::bookmarks::suggest(app, &text);
    app.overlay.anchor = (x, w);
    app.overlay.query = text;
    app.overlay.sel = None;
    if found.is_empty() {
        close(app);
        return;
    }
    let o = &mut app.overlay;
    o.serial = o.serial.wrapping_add(1);
    o.items = found.iter().map(|s| s.u.clone()).collect();
    let Ok(json) = serde_json::to_string(&SuggestView { serial: o.serial, items: &found }) else { return };
    let bounds = suggest_rect(win_size(app), f64::from(x), f64::from(w), found.len());
    show(app, Mode::Suggest, format!("xbo.suggest({json})"), bounds);
    if app.overlay.mode == Mode::Suggest {
        let _ = app.ui.evaluate_script("xbSuggest.set(true,null)");
    }
}

fn on_key(app: &mut App, key: Key) {
    let o = &mut app.overlay;
    if o.mode != Mode::Suggest {
        // The UI forwarded Enter just before learning the list closed: it
        // kept the key from navigating, so do it here.
        if key == Key::Enter && !o.query.is_empty() {
            let q = std::mem::take(&mut o.query);
            app.navigate(&q);
        }
        return;
    }
    let n = o.items.len();
    if key == Key::Enter {
        // Karar: Enter goes through here, so a ↓ just before it is never lost.
        let target = o.sel.and_then(|i| o.items.get(i).cloned()).unwrap_or_else(|| o.query.clone());
        o.query.clear();
        close(app);
        app.navigate(&target);
        return;
    }
    o.sel = match (key, o.sel) {
        (Key::Down, None) => Some(0),
        (Key::Down, Some(i)) if i + 1 < n => Some(i + 1),
        (Key::Up, None) => n.checked_sub(1),
        (Key::Up, Some(i)) if i > 0 => Some(i - 1),
        _ => None,
    };
    let shown = o.sel.and_then(|i| o.items.get(i)).unwrap_or(&o.query);
    let text = serde_json::to_string(shown).unwrap_or_else(|_| "null".into());
    let sel = o.sel.map_or(-1, |i| i as i64);
    if let Some(view) = &o.view {
        let _ = view.evaluate_script(&format!("xbo.select({sel})"));
    }
    let _ = app.ui.evaluate_script(&format!("xbSuggest.set(true,{text})"));
}

pub fn on_command(app: &mut App, cmd: OverlayCmd) {
    match cmd {
        OverlayCmd::Suggest { x, w, text } => suggest(app, x, w, text),
        OverlayCmd::Key(key) => on_key(app, key),
        OverlayCmd::SuggestClose => {
            if app.overlay.mode == Mode::Suggest {
                close(app);
            }
        }
        OverlayCmd::Ready => {
            let o = &mut app.overlay;
            o.ready = true;
            if let Some((js, bounds)) = o.pending.take() {
                present(o, Mode::Hidden, &js, bounds);
            }
        }
        OverlayCmd::Open { serial, i } => {
            let o = &mut app.overlay;
            if serial == o.serial
                && let Some(url) = o.items.get(i).cloned()
            {
                o.query.clear();
                close(app);
                app.navigate(&url);
            }
        }
        OverlayCmd::Close => {
            if app.overlay.mode.owns_keyboard() {
                close(app);
            }
        }
        OverlayCmd::Esc => {
            close(app);
            app.focus_after_switch();
        }
        OverlayCmd::Filter(text) => {
            if app.overlay.mode == Mode::History {
                app.overlay.filter = text;
                render_history(app, true);
            }
        }
        OverlayCmd::Delete { serial, i } => {
            let o = &app.overlay;
            if o.mode == Mode::History
                && serial == o.serial
                && let Some(url) = o.items.get(i).cloned()
            {
                app.history.delete_url(&url);
                // Nor may Ctrl+Shift+T bring back the address just deleted.
                app.closed.forget(&url);
                render_history(app, true);
            }
        }
        OverlayCmd::DeleteAll => {
            if app.overlay.mode == Mode::History {
                app.history.clear();
                // Ctrl+Shift+T must not bring back what was just deleted.
                app.closed.clear();
                render_history(app, true);
            }
        }
        OverlayCmd::ClearData => match app.overlay.mode {
            Mode::History => {
                clear_data(app);
                render_history(app, true);
            }
            // The settings page asks for it too; it words the outcome itself.
            Mode::Settings => clear_data(app),
            _ => {}
        },
        OverlayCmd::DataCleared(ok) => {
            #[cfg(feature = "selftest")]
            {
                app.overlay.cleared = Some(ok);
            }
            if let Some(view) = &app.overlay.view {
                let _ = view.evaluate_script(&format!("xbo.cleared({ok})"));
            }
        }
    }
}

/// Redraws the open list or suggestions from the history as it is now
/// (the file finished loading after they were drawn).
pub fn refresh(app: &mut App) {
    match app.overlay.mode {
        Mode::History => render_history(app, true),
        // Not while ↑/↓ has a row chosen: the list would move under it.
        Mode::Suggest if app.overlay.sel.is_none() => {
            let (x, w) = app.overlay.anchor;
            let text = app.overlay.query.clone();
            suggest(app, x, w, text);
        }
        // The bookmark list is redrawn by `bookmarks` itself on every change.
        Mode::Suggest | Mode::Hidden | Mode::Bookmarks | Mode::Menu | Mode::Settings => {}
    }
}

/// "Tarama verilerini temizle": the history file, and the profile's cookies
/// and site data plus the disk cache (see `native::clear_browsing_data`).
pub fn clear_data(app: &mut App) {
    app.history.clear();
    app.closed.clear();
    let proxy = app.proxy().clone();
    native::clear_browsing_data(&app.ui, move |ok| {
        let _ = proxy.send_event(UserEvent::Ui(UiCommand::Overlay(OverlayCmd::DataCleared(ok))));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pages_that_own_the_keyboard_are_handed_over() {
        // Suggest keeps the keyboard in the address input: the page that
        // replaces it must take the focus itself.
        assert!(!Mode::Suggest.owns_keyboard() && !Mode::Hidden.owns_keyboard());
        for m in [Mode::History, Mode::Bookmarks, Mode::Menu, Mode::Settings] {
            assert!(m.owns_keyboard());
        }
    }

    fn parts(r: Rect) -> (f64, f64, f64, f64) {
        let (p, s) = (r.position.to_logical::<f64>(1.0), r.size.to_logical::<f64>(1.0));
        (p.x, p.y, s.width, s.height)
    }

    #[test]
    fn parses_overlay_messages() {
        let p = OverlayCmd::parse;
        assert_eq!(p("suggest-key", Some("down")), Some(OverlayCmd::Key(Key::Down)));
        assert_eq!(p("ov-open", Some("3:7")), Some(OverlayCmd::Open { serial: 3, i: 7 }));
        assert_eq!(p("ov-delete", Some("3:0")), Some(OverlayCmd::Delete { serial: 3, i: 0 }));
        assert_eq!(p("ov-filter", None), Some(OverlayCmd::Filter(String::new())));
        assert_eq!(p("ov-filter", Some("a:b")), Some(OverlayCmd::Filter("a:b".into())));
        assert_eq!(p("ov-filter", Some(&"a".repeat(MAX_FILTER + 1))), None);
        assert_eq!(p("suggest", Some(&format!("0:1:{}", "a".repeat(MAX_NAV_INPUT + 1)))), None);
        for (n, a) in [
            ("suggest", Some("-1:5:a")),
            ("suggest", Some("1:70000:a")),
            ("ov-open", Some("1:-1")),
            ("ov-open", Some("x:1")),
            ("ov-delete", None),
            ("ov-delete-all", Some("")),
            ("ov-clear-data", Some("1")),
            ("suggest-close", Some("")),
            ("ov-ready", Some("x")),
        ] {
            assert_eq!(p(n, a), None, "{n} {a:?}");
        }
    }

    #[test]
    fn suggestion_list_sits_under_the_address_box_inside_the_window() {
        let size = LogicalSize::new(1200.0, 800.0);
        assert_eq!(parts(suggest_rect(size, 120.0, 600.0, 8)), (120.0, 36.0, 600.0, 250.0));
        // Never narrower than the minimum, never past the right edge.
        assert_eq!(parts(suggest_rect(size, 1000.0, 100.0, 1)), (840.0, 36.0, 360.0, 40.0));
        let small = LogicalSize::new(480.0, 240.0);
        assert_eq!(parts(suggest_rect(small, 300.0, 600.0, 8)), (0.0, 36.0, 480.0, 204.0));
    }

    #[test]
    fn history_panel_fits_the_window() {
        assert_eq!(parts(panel_rect(LogicalSize::new(1200.0, 800.0))), (672.0, 44.0, 520.0, 560.0));
        assert_eq!(parts(panel_rect(LogicalSize::new(480.0, 240.0))), (8.0, 44.0, 464.0, 188.0));
    }
}

