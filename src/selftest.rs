//! Scripted end-to-end scenario, compiled only with `--features selftest`.
//! Drives the real code paths (UI DOM events -> IPC, shortcut handling,
//! page events) and logs PASS/FAIL lines plus PNG captures of the webviews
//! to `$XB_SELFTEST_DIR` (default: current dir).
//!
//! Shortcuts are injected one level above the native hook: a virtual-key code
//! plus modifiers goes through `Shortcut::from_key` (the real mapping) and the
//! result through `App::handle`, exactly as `AcceleratorKeyPressed` does.
//!
//! Not covered here (needs a human / real input; tested by hand): physical key
//! presses reaching the native AcceleratorKeyPressed hook, dragging the window
//! by the strip, dragging the top edge to resize, and live switching of the
//! system light/dark theme.

use crate::app::{App, UserEvent};
use crate::commands::{Mods, Shortcut, UiCommand, vk};
use crate::native::Proxy;
use crate::tabs::TabId;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tao::dpi::LogicalSize;
use wry::WebViewExtWindows;

static FAILS: AtomicU32 = AtomicU32::new(0);
static PASSES: AtomicU32 = AtomicU32::new(0);

/// PASS lines of a complete, passing run. Fewer means a check was skipped
/// (e.g. a script callback that never came). Update it with the checks.
const EXPECTED_CHECKS: u32 = 225;

fn dir() -> PathBuf {
    std::env::var_os("XB_SELFTEST_DIR").map(PathBuf::from).unwrap_or_else(|| ".".into())
}

fn log(line: &str) {
    eprintln!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("selftest.log")) {
        let _ = writeln!(f, "{line}");
    }
}

fn check(name: &str, ok: bool, detail: impl std::fmt::Display) {
    let counter = if ok { &PASSES } else { &FAILS };
    counter.fetch_add(1, Ordering::Relaxed);
    log(&format!("{} {name}{}", if ok { "PASS" } else { "FAIL" }, if ok { String::new() } else { format!(" -- {detail}") }));
}

/// Delay before each step, in ms. Network steps get more time.
const DELAYS: &[u64] = &[
    1500, // 0 initial state
    300,  // 1 UI "+" button
    600,  // 2 address bar navigate (schemeless)
    3500, // 3 check page loaded, open second page in same tab
    3500, // 4 back
    2000, // 5 forward
    2000, // 6 pushState + hash
    800,  // 7 blocked schemes
    600,  // 8 untrusted IPC from content + script popup
    1200, // 9 load link page
    1500, // 10 click target=_blank (CDP mouse, user-initiated)
    3000, // 11 check new tab
    500,  // 12 cycle tabs
    500,  // 13 resize
    800,  // 14 check resize, capture
    500,  // 15 search
    3000, // 16 check search, start slow load
    600,  // 17 stop
    1500, // 18 check stop
    500,  // 19 close via UI x
    500,  // 20 shortcuts close down to 1 tab; window back to 1200x800
    1000, // 21 static UI facts (layout, chrome, theme); navigate to example.com
    3500, // 22 check load; CSP probe in UI + page; page theme; set reload marker
    800,  // 23 read CSP probe + marker setup
    300,  // 24 UI reload button
    2500, // 25 check reload; set marker again
    500,  // 26 Ctrl+R
    2500, // 27 check reload; navigate to example.org
    3500, // 28 check; Alt+Left
    2500, // 29 check back; Alt+Right
    2500, // 30 check forward; type in address bar + pushState
    800,  // 31 typed text survives; Escape
    500,  // 32 Escape restores URL; refused navigation with a page update right after
    500,  // 33 refused text kept; unfocus address; Ctrl+T twice
    600,  // 34 address focused; Ctrl+Shift+Tab / Ctrl+Tab cycle
    600,  // 35 Ctrl+L, then the page changes its URL
    500,  // 36 selection survives the URL change; AltGr ignored; close extras; load window.open page
    1200, // 37 click: window.open(file:)
    1500, // 38 check blocked; click: window.open(https:)
    3000, // 39 check new tab
    900,  // 40 load page with hostile title
    1200, // 41 title shown as text; UI navigate-away / window.open attempts
    1500, // 42 check; open 29 more tabs
    1500, // 43 tab strip at 1200px; shrink window
    2000, // 44 tab strip at 820px (UI needs a moment to see the new width); UI tab click
    1500, // 45 check switch; tab keys, tab strip at the minimum width; keyboard + middle-click close
    600,  // 46 check closes; Delete key on two inactive tabs (one repeated, one real)
    600,  // 47 check those; Delete key on the active tab
    600,  // 48 check it; scroll the list to its far end (still at the minimum width)
    600,  // 49 real click on the far tab
    600,  // 50 check far tab active; wheel over the list
    800,  // 51 check wheel scroll; close extras; window back to 1200x800
    1200, // 52 check closed down; click UI maximize
    1000, // 53 check maximized; click UI restore
    1000, // 54 check restored; double-click strip
    1000, // 55 check maximized; restore
    1000, // 56 check restored; click UI minimize
    1000, // 57 check minimized; restore window
    1000, // 58 check restored; minimum window size
    1500, // 59 chrome at minimum size; window back to 1200x800
    700,  // 60 window title of a blank tab; fresh tab plays audio without a gesture
    1200, // 61 check autoplay refused; window.open (script, gesture)
    3000, // 62 check popup; window title follows tab switch; page renames itself
    700,  // 63 check title; popup calls window.close()
    1000, // 64 check tab closed cleanly; address bar to http://xb.ui/
    1500, // 65 check refused; page frames / navigates to xb.ui itself
    1500, // 66 check refused; crash a content renderer
    1500, // 67 check tab survives; crash the UI renderer
    3000, // 68 check UI reloaded and re-rendered; close extras
    3500, // 69 four tabs; Ctrl+1/3/8/9; last tab loads a page
    1500, // 70 check load; page icon; Ctrl+Shift+PgUp/PgDn; UI move message
    800,  // 71 check moves, favicon in state and UI; measure drag points
    800,  // 72 real drag of a tab in the UI
    3500, // 73 check drag; 820 px window, two more tabs (narrow, the icon tab inactive)
    1200, // 74 measure a drag that starts on the icon tab's icon
    800,  // 75 real drag from the icon
    800,  // 76 check it moved; measure a drag to cancel
    800,  // 77 real drag, held
    800,  // 78 check it is being dragged; Escape, then the release
    800,  // 79 check order kept; close the two tabs; the icon tab loads a page without an icon
    3500, // 80 check its icon is gone; close that tab, Ctrl+Shift+T reopens it
    900,  // 81 check restored URL; load link page
    1500, // 82 Ctrl+click a link
    1500, // 83 check background tab; middle-click a link
    900,  // 84 check background tab; input window notifies the host; load audio page; mute
    1500, // 85 check muted; click starts a tone
    900,  // 86 check audio indicator; 30 tabs at the minimum width
    1500, // 87 check the playing tab is marked there (inactive, then active); close extras
    900,  // 88 last tab close quits
];

pub fn start(proxy: Proxy) {
    let _ = std::fs::remove_file(dir().join("selftest.log"));
    std::thread::spawn(move || {
        for (i, d) in DELAYS.iter().enumerate() {
            std::thread::sleep(Duration::from_millis(*d));
            if proxy.send_event(UserEvent::Test(i as u32)).is_err() {
                return;
            }
        }
    });
}

fn capture(app: &App, name: &str) {
    let path = dir().join(format!("{name}.png"));
    let _ = crate::native::capture_png(&app.ui, &dir().join(format!("{name}-ui.png")));
    if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
        let _ = crate::native::capture_png(v, &path);
    }
}

fn visible(v: &wry::WebView) -> bool {
    let mut b = windows::core::BOOL(0);
    unsafe {
        let _ = v.controller().IsVisible(&mut b);
    }
    b.as_bool()
}

fn active_url(app: &App) -> String {
    app.tabs.active().and_then(|id| app.tabs.get(id)).map(|t| t.url.clone()).unwrap_or_default()
}

fn active_title(app: &App) -> String {
    app.tabs.active().and_then(|id| app.tabs.get(id)).map(|t| t.title.clone()).unwrap_or_default()
}

fn ui_js(app: &App, js: &str) {
    let _ = app.ui.evaluate_script(js);
}

fn page_js(app: &App, js: &str) {
    if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
        let _ = v.evaluate_script(js);
    }
}

/// Exactly one content view visible, and it is the active tab's.
fn only_active_visible(app: &App) -> bool {
    let active = app.tabs.active();
    app.views.iter().all(|(id, v)| visible(v) == (Some(*id) == active))
}

fn type_in_address(app: &App, text: &str) {
    let mut lit = String::new();
    crate::tabs::push_json_str(&mut lit, text);
    ui_js(
        app,
        &format!(
            "(() => {{ const a = document.getElementById('address'); a.focus(); a.value = {lit}; \
             a.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Enter', bubbles: true }})); }})()"
        ),
    );
}

thread_local! {
    static SAVED: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Values carried from one step to a later one.
    static MEM: std::cell::RefCell<Vec<(&'static str, i64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn remember(key: &'static str, value: i64) {
    MEM.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|(k, _)| *k != key);
        m.push((key, value));
    });
}

fn recall(key: &'static str) -> i64 {
    MEM.with(|m| m.borrow().iter().find(|(k, _)| *k == key).map_or(-1, |(_, v)| *v))
}

/// URLs the gesture page tries to open; only the last is a web URL.
const GESTURE_URLS: [&str; 6] = [
    "file:///C:/Windows/win.ini",
    "about:blank",
    "data:text/html,hi",
    "xb://ui/index.html",
    "ftp://example.com/",
    "https://example.com/?from=window-open",
];

/// Vertical centre (page coordinates) of gesture-page button `row`.
fn row_y(row: usize) -> f64 {
    (row as f64 + 0.5) * (800.0 - crate::app::UI_HEIGHT) / 6.0
}

const NONE: Mods = Mods { ctrl: false, shift: false, alt: false };
const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false };
const ALT: Mods = Mods { ctrl: false, shift: false, alt: true };
const ALTGR: Mods = Mods { ctrl: true, shift: false, alt: true };

/// A key-down as the native hook sees it: key + modifiers -> `Shortcut::from_key`
/// -> the app's event handler. Returns what the key mapped to.
fn press(app: &mut App, key: u32, mods: Mods) -> Option<Shortcut> {
    let sc = Shortcut::from_key(key, mods)?;
    app.handle(UserEvent::Shortcut(sc));
    Some(sc)
}

/// Runs `js` in a webview and passes its (string) result to `then` when it
/// completes. Scripts run in submission order, so a read queued before an
/// action sees the state from before it. When there is no view, the script
/// cannot be started or it throws, `then` never runs and check `name` FAILs
/// instead. (A callback WebView2 never calls is caught by `EXPECTED_CHECKS`.)
fn eval(v: Option<&wry::WebView>, name: &'static str, js: &str, then: impl Fn(String) + Send + 'static) {
    let Some(v) = v else { return check(name, false, "no webview to run the script in") };
    let started = v.evaluate_script_with_callback(js, move |r| {
        // Every real result is JSON; wry passes "" when the script threw.
        if r.is_empty() {
            check(name, false, "script threw");
        } else {
            then(decode(&r));
        }
    });
    if let Err(e) = started {
        check(name, false, format!("script not started: {e}"));
    }
}

fn ui_eval(app: &App, name: &'static str, js: &str, then: impl Fn(String) + Send + 'static) {
    eval(Some(&app.ui), name, js, then);
}

fn page_eval(app: &App, name: &'static str, js: &str, then: impl Fn(String) + Send + 'static) {
    eval(app.tabs.active().and_then(|id| app.view(id)), name, js, then);
}

/// Undoes the JSON string encoding of a script result (WebView2 also escapes
/// `<` and `>` as `<`; surrogate pairs are not needed here).
fn decode(raw: &str) -> String {
    let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return raw.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
            }
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Value of `key` in a `a=1;b=2` result string.
fn field<'a>(s: &'a str, key: &str) -> &'a str {
    s.split(';').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')).unwrap_or("<missing>")
}

fn num(s: &str, key: &str) -> f64 {
    field(s, key).parse().unwrap_or(f64::NAN)
}

/// Whether the system's app theme is dark, from the same registry value
/// Windows itself uses (`None` if it cannot be read).
fn system_apps_dark() -> Option<bool> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("reg")
        .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "/v", "AppsUseLightTheme"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let value = text.split_whitespace().last()?;
    Some(u32::from_str_radix(value.strip_prefix("0x")?, 16).ok()? == 0)
}

fn logical_size(app: &App) -> (f64, f64) {
    let s = app.window.inner_size().to_logical::<f64>(app.window.scale_factor());
    (s.width, s.height)
}

/// Narrowest width (logical px) the OS lets the user drag the window to, i.e.
/// what `with_min_inner_size` set up, read by asking the window procedure.
fn min_track_width(app: &App) -> f64 {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{MINMAXINFO, SendMessageW, WM_GETMINMAXINFO};
    let mut mmi = MINMAXINFO::default();
    unsafe {
        SendMessageW(HWND(app.window.hwnd() as _), WM_GETMINMAXINFO, Some(WPARAM(0)), Some(LPARAM(&mut mmi as *mut MINMAXINFO as isize)));
    }
    f64::from(mmi.ptMinTrackSize.x) / app.window.scale_factor()
}

/// Closes every tab but the first.
fn close_extra_tabs(app: &mut App) {
    let extra: Vec<TabId> = app.tabs.ids().skip(1).collect();
    for id in extra {
        app.close_tab(id, true);
    }
}

/// Narrowest width the window may have (`with_min_inner_size` in main.rs).
const MIN_W: f64 = 480.0;

/// Shared JS helpers: `visEl` is true when an element has a box fully inside
/// the viewport and is the topmost thing at its centre (not clipped, not covered).
const VIS_JS: &str = "const q = (id) => document.getElementById(id); \
  const rc = (id) => q(id).getBoundingClientRect(); \
  const visEl = (el) => { if (!el) return false; const b = el.getBoundingClientRect(); \
  if (b.width <= 0 || b.height <= 0 || b.left < 0 || b.right > innerWidth || b.top < 0 || b.bottom > innerHeight) return false; \
  const hit = document.elementFromPoint(b.left + b.width / 2, b.top + b.height / 2); \
  return hit === el || el.contains(hit); }; \
  const vis = (id) => visEl(q(id)); \
  const chromeIds = ['back','forward','reload','address','new-tab','min','max','close']; \
  const selTab = document.querySelector('[role=tab][aria-selected=true]'); \
  const nt = rc('new-tab'); \
  const drag = document.elementFromPoint(nt.right + 20, 20);";

/// Static facts about the UI strip (layout, chrome, theme).
fn ui_static_js() -> String {
    format!(
        "(() => {{ {VIS_JS} const res = rc('resizer'); \
  return [`strip_h=${{rc('strip').height}}`, `doc_h=${{document.documentElement.clientHeight}}`, \
  `res_h=${{res.height}}`, `res_top=${{res.top}}`, `res_w=${{res.width}}`, `iw=${{innerWidth}}`, \
  `res_hidden=${{q('resizer').hidden}}`, `res_n=${{q('resizer').children.length}}`, \
  `drag_el=${{drag && drag.id}}`, \
  `chrome=${{chromeIds.filter((i) => !vis(i)).join(',') || 'ok'}}`, \
  `sel_vis=${{visEl(selTab)}}`, `sel_w=${{selTab ? selTab.getBoundingClientRect().width : 0}}`, \
  `addr_w=${{rc('address-box').width}}`, \
  `dark=${{matchMedia('(prefers-color-scheme: dark)').matches}}`, \
  `bg=${{getComputedStyle(document.body).backgroundColor}}`].join(';'); }})()"
    )
}

/// Tab strip geometry: overflow of the tab list (it may scroll), the tab zone
/// and the whole strip (they may not), per-tab boxes, the scroll arrows, the
/// "+" button, the active tab and the window chrome. `leak` counts tabs whose
/// middle lies outside the tab list's box yet can be hit there (the list must
/// clip what it scrolls; a tab cut by the list's edge is hit only inside it).
fn tab_strip_js() -> String {
    format!(
        "(() => {{ {VIS_JS} \
  const strip = q('tablist'), zone = q('tabs'), sr = strip.getBoundingClientRect(), zr = zone.getBoundingClientRect(); \
  const tabs = [...strip.querySelectorAll('[role=tab]')]; \
  const rects = tabs.map((e) => e.getBoundingClientRect()); \
  const inside = (a, b) => a.left >= b.left - 0.5 && a.right <= b.right + 0.5; \
  const hitOut = (e, r) => {{ const x = r.left + r.width / 2; if (x >= sr.left && x <= sr.right) return false;   const h = document.elementFromPoint(x, r.top + r.height / 2); return !!h && e.contains(h); }}; \
  const sel = tabs.filter((e) => e.getAttribute('aria-selected') === 'true'); \
  return [`n=${{tabs.length}}`, `cw=${{strip.clientWidth}}`, `over=${{strip.scrollWidth - strip.clientWidth}}`, \
  `zover=${{zone.scrollWidth - zone.clientWidth}}`, `sover=${{q('strip').scrollWidth - q('strip').clientWidth}}`, \
  `minw=${{Math.min(...rects.map((r) => r.width)).toFixed(1)}}`, \
  `leak=${{tabs.filter((e, i) => hitOut(e, rects[i])).length}}`, \
  `ovf=${{zone.hasAttribute('data-overflow')}}`, `arrows=${{vis('tab-left') && vis('tab-right')}}`, \
  `newtab=${{vis('new-tab') && inside(nt, zr)}}`, \
  `win=${{['min', 'max', 'close'].every(vis)}}`, `selected=${{sel.length}}`, \
  `sel_id=${{sel[0] ? sel[0].dataset.id : 'none'}}`, `sel_vis=${{visEl(selTab)}}`, \
  `sel_w=${{selTab ? selTab.getBoundingClientRect().width.toFixed(1) : 0}}`, \
  `drag_el=${{drag && drag.id}}`, `addr_w=${{rc('address-box').width.toFixed(1)}}`, \
  `chrome=${{chromeIds.filter((i) => !vis(i)).join(',') || 'ok'}}`, `iw=${{innerWidth}}`].join(';'); }})()"
    )
}

/// Roving-tabindex, ARIA structure and key handling of the tab list. Keys are
/// dispatched on the tab elements themselves, as a keyboard would.
const TAB_KEYS_JS: &str = "(() => { \
  const list = document.getElementById('tablist'), nt = document.getElementById('new-tab'); \
  const tabs = [...list.querySelectorAll('[role=tab]')]; \
  const sel = tabs.filter((t) => t.getAttribute('aria-selected') === 'true'); \
  const key = (el, k) => { el.focus(); el.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true })); }; \
  const last = tabs.length - 1; \
  const o = [`n=${tabs.length}`, `list_role=${list.getAttribute('role')}`, \
  `tab0=${tabs.filter((t) => t.tabIndex === 0).length}`, \
  `sel_is_tab0=${sel.length === 1 && sel[0].tabIndex === 0}`, \
  `others_m1=${tabs.filter((t) => !sel.includes(t)).every((t) => t.tabIndex === -1)}`, \
  `only_tabs=${[...list.children].every((c) => c.getAttribute('role') === 'tab')}`, \
  `plus_outside=${!list.contains(nt) && !nt.closest('[role=tablist]')}`, \
  `close_btns=${[...list.querySelectorAll('button')].every((b) => b.tabIndex === -1 && b.getAttribute('aria-label') && b.getAttribute('aria-hidden') === 'true')}`, \
  `named=${tabs.every((t) => t.getAttribute('aria-label'))}`]; \
  key(tabs[3], 'ArrowRight'); o.push(`right=${document.activeElement === tabs[4]}`); \
  key(tabs[3], 'ArrowLeft'); o.push(`left=${document.activeElement === tabs[2]}`); \
  key(tabs[5], 'Home'); o.push(`home=${document.activeElement === tabs[0]}`); \
  key(tabs[5], 'End'); o.push(`end=${document.activeElement === tabs[last]}`); \
  key(tabs[last], 'ArrowRight'); o.push(`wrap=${document.activeElement === tabs[0]}`); \
  const sel2 = tabs.filter((t) => t.getAttribute('aria-selected') === 'true'); \
  o.push(`sel_kept=${sel2.length === 1 && sel2[0] === sel[0]}`); \
  return o.join(';'); })()";

/// Mouse reachability of every tab: back to the start with the left arrow,
/// then right-arrow clicks until the end, noting each tab that was visible and
/// on top (`visEl`) at some point. Arrows are clicked as a mouse would (a
/// disabled one ignores the click).
fn reach_js() -> String {
    format!(
        "(() => {{ {VIS_JS} \
  const list = q('tablist'), L = q('tab-left'), R = q('tab-right'); \
  const tabs = [...list.querySelectorAll('[role=tab]')]; \
  const arrows = visEl(L) && visEl(R); \
  for (let i = 0; i < 100 && !L.disabled; i++) L.click(); \
  const start = list.scrollLeft === 0; \
  const seen = new Set(); let clicks = 0; \
  for (;;) {{ tabs.forEach((t, i) => {{ if (visEl(t)) seen.add(i); }}); if (R.disabled || clicks >= 100) break; R.click(); clicks++; }} \
  const end = list.scrollLeft >= list.scrollWidth - list.clientWidth - 1; \
  return `n=${{tabs.length}};seen=${{seen.size}};clicks=${{clicks}};arrows=${{arrows}};start=${{start}};end=${{end}}`; }})()"
    )
}

/// Scrolls the tab list to its end with the right arrow and reports where the
/// last tab and the middle of the list are (for real CDP mouse input).
const SCROLL_END_JS: &str = "(() => { const list = document.getElementById('tablist'), R = document.getElementById('tab-right'); \
  for (let i = 0; i < 100 && !R.disabled; i++) R.click(); \
  const t = list.lastElementChild.getBoundingClientRect(), l = list.getBoundingClientRect(); \
  return `x=${t.left + t.width / 2};y=${t.top + t.height / 2};wx=${l.left + l.width / 2};wy=${l.top + l.height / 2};id=${list.lastElementChild.dataset.id}`; })()";

/// Screen points carried from a script result to a later step (CDP input).
static SPOT: std::sync::Mutex<[f64; 4]> = std::sync::Mutex::new([f64::NAN; 4]);

fn set_spot(p: [f64; 4]) {
    if let Ok(mut s) = SPOT.lock() {
        *s = p;
    }
}

fn spot() -> [f64; 4] {
    SPOT.lock().map(|s| *s).unwrap_or([f64::NAN; 4])
}

/// The tab ids in strip order as the UI shows them.
const UI_ORDER_JS: &str = "[...document.querySelectorAll('#tablist [role=tab]')].map((t) => t.dataset.id).join(',')";

/// A 16x16 PNG icon the favicon test page links to.
const ICON_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGUlEQVR42mM4oaHxnxLMMGrAqAGjBgwXAwCpMhcfTAD1DwAAAABJRU5ErkJggg==";

/// Two links filling the top of the page: Ctrl+click goes to the first, a
/// middle click to the second.
const LINKS_HTML: &str = "<!doctype html><title>Links</title><body style=\"margin:0\">\
  <a href=\"https://example.com/?bg=ctrl\" style=\"display:block;height:200px\">ctrl</a>\
  <a href=\"https://example.com/?bg=middle\" style=\"display:block;height:200px\">middle</a></body>";

/// A page that starts a quiet tone on its first click (a user gesture).
const AUDIO_HTML: &str = "<!doctype html><title>Audio</title><body style=\"margin:0;height:100vh\"><script>\
  window.__tone = 'idle'; \
  document.addEventListener('mousedown', () => { const c = new AudioContext(); const o = c.createOscillator(), g = c.createGain(); \
  g.gain.value = 0.1; o.connect(g).connect(c.destination); o.start(); window.__ctx = c; \
  c.resume().then(() => { window.__tone = c.state; }); }, { once: true });</script></body>";

/// Tries every inline-code route the CSP must refuse, in whatever document it
/// runs in. Results are read back by `CSP_READ_JS`.
const CSP_PROBE_JS: &str = "(() => { \
  const r = { attr: 'no', script: 'no', evalr: 'no', fetchr: 'pending', viol: [] }; \
  window.__cspProbe = r; \
  document.addEventListener('securitypolicyviolation', (e) => r.viol.push(e.violatedDirective)); \
  const d = document.createElement('div'); d.id = '__csp_probe'; document.body.appendChild(d); \
  d.innerHTML = '<img src=\"data:image/png;base64,AAAA\" onerror=\"window.__cspProbe.attr = \\'ran\\'\">'; \
  const s = document.createElement('script'); s.textContent = 'window.__cspProbe.script = \"ran\"'; d.appendChild(s); \
  try { new Function('window.__cspProbe.evalr = \"ran\"')(); } catch (e) { r.evalr = 'blocked'; } \
  fetch(location.origin + '/js/core.js').then(() => { r.fetchr = 'allowed'; }, () => { r.fetchr = 'blocked'; }); })()";

const CSP_READ_JS: &str = "(() => { const r = window.__cspProbe; const d = document.getElementById('__csp_probe'); \
  if (d) d.remove(); \
  return `attr=${r.attr};script=${r.script};eval=${r.evalr};fetch=${r.fetchr};viol=${r.viol.join(',')}`; })()";

pub fn step(app: &mut App, n: u32) {
    match n {
        0 => {
            log(&format!("state {}", app.tabs.to_json()));
            check("ui-ready", app.ui_ready, "UI never sent 'ready'");
            check("one-tab-at-start", app.tabs.len() == 1, app.tabs.len());
            check("first-tab-visible", only_active_visible(app), "visibility mismatch");
            capture(app, "00-start");
        }
        1 => {
            // Real UI button -> IPC -> Rust.
            ui_js(app, "document.getElementById('new-tab').click()");
        }
        2 => {
            check("ui-plus-button-opens-tab", app.tabs.len() == 2, app.tabs.len());
            check("new-tab-is-active-and-visible", only_active_visible(app), "visibility mismatch");
            type_in_address(app, "example.com");
        }
        3 => {
            log(&format!("state {}", app.tabs.to_json()));
            check("schemeless-gets-https", active_url(app).starts_with("https://example.com"), active_url(app));
            check("title-updates", active_title(app) == "Example Domain", active_title(app));
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("loading-finished", !t.loading, "still loading");
            capture(app, "01-example");
            type_in_address(app, "https://example.org/");
        }
        4 => {
            check("second-nav", active_url(app).starts_with("https://example.org"), active_url(app));
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("can-go-back", t.can_back && !t.can_forward, format!("{t:?}"));
            ui_js(app, "document.getElementById('back').click()");
        }
        5 => {
            check("back", active_url(app).starts_with("https://example.com"), active_url(app));
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("can-go-forward", t.can_forward, format!("{t:?}"));
            app.handle(UserEvent::Shortcut(Shortcut::Forward));
        }
        6 => {
            check("forward-shortcut", active_url(app).starts_with("https://example.org"), active_url(app));
            page_js(app, "history.pushState({}, '', '/pushed'); document.title = 'Pushed';");
        }
        7 => {
            check("pushstate-updates-url", active_url(app) == "https://example.org/pushed", active_url(app));
            check("script-title-updates", active_title(app) == "Pushed", active_title(app));
            page_js(app, "location.hash = 'frag';");
            let before = active_url(app);
            SAVED.with(|s| s.borrow_mut().push(app.tabs.len() as u32));
            app.handle(UserEvent::Ui(UiCommand::Navigate("javascript:document.title='pwned'".into())));
            app.handle(UserEvent::Ui(UiCommand::Navigate("file:///C:/Windows/win.ini".into())));
            app.handle(UserEvent::Ui(UiCommand::Navigate(r"C:\Windows\win.ini".into())));
            check("blocked-schemes-ignored", active_url(app) == before, active_url(app));
        }
        8 => {
            check("hash-updates-url", active_url(app) == "https://example.org/pushed#frag", active_url(app));
            check("javascript-did-not-run", active_title(app) == "Pushed", active_title(app));
            // An untrusted page tries every IPC path, and opens a popup from
            // its own load-time script (no user gesture). Note: host-side
            // ExecuteScript counts as a gesture, so the popup must come from
            // the page itself.
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                let _ = v.load_html(
                    "<!doctype html><title>Hostile</title><script>\
                     try { window.chrome.webview.postMessage('new'); } catch (e) {}\
                     try { window.chrome.webview.postMessage('quit'); } catch (e) {}\
                     try { window.ipc.postMessage('new'); } catch (e) {}\
                     window.open('https://example.net/');\
                     setTimeout(() => window.open('https://example.net/?late'), 100);\
                     </script>",
                );
            }
        }
        9 => {
            let before = SAVED.with(|s| s.borrow()[0]);
            check("content-ipc-ignored", app.tabs.len() as u32 == before && !app.quit, app.tabs.len());
            check("script-popup-without-gesture-blocked", app.tabs.len() as u32 == before, app.tabs.len());
            page_js(
                app,
                "document.open(); document.write('<a id=l target=_blank href=\"https://example.com/?from=blank\" \
                 style=\"display:block;position:fixed;inset:0;font-size:40px\">big link</a>'); document.close();",
            );
        }
        10 => {
            SAVED.with(|s| s.borrow_mut().push(app.tabs.len() as u32));
            // Trusted (user-gesture) click inside our own webview via DevTools protocol.
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                cdp_click(v, 200.0, 200.0);
            }
        }
        11 => {
            let before = SAVED.with(|s| s.borrow()[1]);
            log(&format!("state {}", app.tabs.to_json()));
            check("target-blank-opens-tab", app.tabs.len() as u32 == before + 1, app.tabs.len());
            check("target-blank-url", active_url(app).contains("from=blank"), active_url(app));
            check("only-new-tab-visible", only_active_visible(app), "visibility mismatch");
            capture(app, "02-blank-target");
        }
        12 => {
            let a = app.tabs.active();
            app.handle(UserEvent::Shortcut(Shortcut::NextTab));
            let b = app.tabs.active();
            app.handle(UserEvent::Shortcut(Shortcut::PrevTab));
            app.flush(); // what the event loop does after every pass
            check("ctrl-tab-cycles", a != b && app.tabs.active() == a, format!("{a:?} {b:?}"));
            check("cycle-visibility", only_active_visible(app), "visibility mismatch");
            // Switch latency on the real path: activate() puts the new tab on
            // screen (raise + show + focus); flush() then pushes the UI state
            // and does the deferred hide of the old view.
            let n = app.tabs.ids().count() as u32;
            let rounds = 10 * n;
            let (mut on_screen, mut total) = (Duration::ZERO, Duration::ZERO);
            for _ in 0..rounds {
                let next = app.tabs.cycle(1).unwrap();
                let t0 = std::time::Instant::now();
                app.activate(next);
                let t1 = std::time::Instant::now();
                app.flush();
                on_screen += t1 - t0;
                total += t0.elapsed();
            }
            let ms = |d: Duration| (d / rounds).as_secs_f64() * 1000.0;
            log(&format!(
                "INFO tab-switch {:.2} ms until the new tab is on screen, {:.2} ms incl. deferred hide ({n} tabs, {rounds} switches)",
                ms(on_screen),
                ms(total)
            ));
            check("tab-switch-fast", on_screen / rounds < Duration::from_millis(16), format!("{:.2} ms", ms(on_screen)));
            check("switch-loop-visibility", only_active_visible(app) && app.tabs.active() == a, "visibility mismatch");
        }
        13 => {
            // Resizing a minimized window does nothing, and every later layout
            // check would measure the 144x19 minimized frame. Something outside
            // the app can minimize it (observed once), so say so and restore.
            if app.window.is_minimized() {
                log("INFO window was minimized by something outside the selftest; restoring it");
                app.window.set_minimized(false);
            }
            app.window.set_inner_size(LogicalSize::new(820.0, 520.0));
        }
        14 => {
            let scale = app.window.scale_factor();
            let ui_b = app.ui.bounds().unwrap();
            let v = app.view(app.tabs.active().unwrap()).unwrap();
            let c = v.bounds().unwrap();
            let size = |s: wry::dpi::Size| s.to_logical::<f64>(scale);
            let pos = |p: wry::dpi::Position| p.to_logical::<f64>(scale);
            let (us, cs, cp) = (size(ui_b.size), size(c.size), pos(c.position));
            log(&format!("INFO after resize: ui {}x{}, content at y={} {}x{}", us.width, us.height, cp.y, cs.width, cs.height));
            let close = |a: f64, b: f64| (a - b).abs() <= 1.0;
            check("ui-resized", close(us.width, 820.0) && close(us.height, 40.0), format!("{us:?}"));
            check(
                "content-resized",
                close(cs.width, 820.0) && close(cs.height, 480.0) && close(cp.y, 40.0),
                format!("{cs:?} {cp:?}"),
            );
            capture(app, "03-resized");
            // Hidden tabs get correct bounds when shown.
            app.handle(UserEvent::Shortcut(Shortcut::NextTab));
            let v = app.view(app.tabs.active().unwrap()).unwrap();
            let cs = size(v.bounds().unwrap().size);
            check("hidden-tab-resized-on-show", close(cs.width, 820.0) && close(cs.height, 480.0), format!("{cs:?}"));
            app.handle(UserEvent::Shortcut(Shortcut::PrevTab));
        }
        15 => {
            type_in_address(app, "rust programming language");
        }
        16 => {
            check(
                "search-duckduckgo",
                active_url(app).starts_with("https://duckduckgo.com/") && active_url(app).contains("rust"),
                active_url(app),
            );
            capture(app, "04-search");
            type_in_address(app, "https://httpbin.org/delay/8");
        }
        17 => {
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("loading-flag-during-load", t.loading, "not loading");
            capture(app, "05-loading");
            ui_eval(
                app,
                "ui-shows-loading",
                "JSON.stringify({ reload: document.getElementById('reload').hasAttribute('data-loading'), \
                 bar: document.getElementById('loadbar').hasAttribute('data-on'), \
                 barOpacity: getComputedStyle(document.getElementById('loadbar')).opacity, \
                 stopShown: getComputedStyle(document.querySelector('#reload svg:last-child')).display })",
                |r| {
                    log(&format!("INFO ui-dom {r}"));
                    let ok = r.contains(r#""reload":true"#) && r.contains(r#""bar":true"#);
                    check("ui-shows-loading", ok, &r);
                },
            );
            ui_eval(
                app,
                "ui-tab-shows-loading-dot",
                "(() => { const t = document.querySelector('[role=tab][aria-selected=true]'); \
                 return `tab=${t.hasAttribute('data-loading')};dot=${getComputedStyle(t.querySelector('.dot')).display}`; })()",
                |r| check("ui-tab-shows-loading-dot", field(&r, "tab") == "true" && field(&r, "dot") == "block", &r),
            );
            ui_eval(app, "ui-stop-tooltip-promises-no-missing-shortcut", "document.getElementById('reload').title", |r| {
                check("ui-stop-tooltip-promises-no-missing-shortcut", !r.contains("Esc"), &r);
            });
            ui_js(app, "document.getElementById('reload').click()"); // reload button acts as stop while loading
        }
        18 => {
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("stop", !t.loading, "still loading after stop");
        }
        19 => {
            let n = app.tabs.len();
            let id = app.tabs.active().unwrap();
            ui_js(app, &format!("document.querySelector('[data-id=\"{id}\"] .close').click()"));
            SAVED.with(|s| s.borrow_mut().push(n as u32));
        }
        20 => {
            let before = SAVED.with(|s| s.borrow()[2]);
            check("ui-close-button", app.tabs.len() as u32 == before - 1, app.tabs.len());
            while app.tabs.len() > 1 {
                press(app, vk::W, CTRL);
            }
            check("ctrl-w-closes", app.tabs.len() == 1 && !app.quit, app.tabs.len());
            app.window.set_inner_size(LogicalSize::new(1200.0, 800.0));
        }
        21 => {
            check("window-frameless", !app.window.is_decorated(), "window has native decorations");
            let registry = system_apps_dark();
            let dark = registry.unwrap_or(app.window.theme() == tao::window::Theme::Dark);
            match registry {
                Some(r) => check(
                    "window-theme-matches-system",
                    (app.window.theme() == tao::window::Theme::Dark) == r,
                    format!("tao={:?} registry dark={r}", app.window.theme()),
                ),
                // Not a skip: the check count must not depend on the machine.
                None => check("window-theme-matches-system", false, "system theme registry value unreadable"),
            }
            ui_eval(app, "ui-strip-height-matches-native", &ui_static_js(), move |r| {
                log(&format!("INFO ui-static {r}"));
                let h = crate::app::UI_HEIGHT;
                check("ui-strip-height-matches-native", num(&r, "strip_h") == h && num(&r, "doc_h") == h, &r);
                check("ui-chrome-controls-visible", field(&r, "chrome") == "ok", format!("not visible: {}", field(&r, "chrome")));
                check(
                    "top-resize-handles-present",
                    field(&r, "res_n") == "3"
                        && num(&r, "res_top") == 0.0
                        && num(&r, "res_h") == 3.0
                        && num(&r, "res_w") == num(&r, "iw")
                        && field(&r, "res_hidden") == "false",
                    &r,
                );
                check("drag-region-present", ["tabs", "strip"].contains(&field(&r, "drag_el")), &r);
                check("ui-prefers-color-scheme-matches-system", field(&r, "dark") == dark.to_string(), format!("{r} system dark={dark}"));
                let want_bg = if dark { "rgb(28, 28, 30)" } else { "rgb(244, 244, 245)" };
                check("ui-palette-matches-theme", field(&r, "bg") == want_bg, format!("{r} want {want_bg}"));
            });
            type_in_address(app, "example.com");
        }
        22 => {
            check("reload-page-loaded", active_url(app).starts_with("https://example.com") && !app.tabs.get(app.tabs.active().unwrap()).unwrap().loading, active_url(app));
            let url = active_url(app);
            ui_eval(
                app,
                "ui-tab-title-live",
                "(() => { const t = document.querySelector('[role=tab][aria-selected=true]'); \
                 return `title=${t.querySelector('.title').textContent};addr=${document.getElementById('address').value}`; })()",
                move |r| {
                    check("ui-tab-title-live", field(&r, "title") == "Example Domain", &r);
                    check("ui-address-shows-live-url", field(&r, "addr") == url, format!("{r} want {url}"));
                },
            );
            let dark = system_apps_dark().unwrap_or(app.window.theme() == tao::window::Theme::Dark);
            page_eval(app, "content-prefers-color-scheme-matches-system", "String(matchMedia('(prefers-color-scheme: dark)').matches)", move |r| {
                check("content-prefers-color-scheme-matches-system", r == dark.to_string(), format!("page dark={r} system dark={dark}"));
            });
            ui_js(app, CSP_PROBE_JS);
            page_js(app, CSP_PROBE_JS);
            page_js(app, "window.__m = 1;");
        }
        23 => {
            ui_eval(app, "csp-blocks-inline-event-handler", CSP_READ_JS, |r| {
                log(&format!("INFO csp-ui {r}"));
                check("csp-blocks-inline-event-handler", field(&r, "attr") == "no", &r);
                check("csp-blocks-inline-script", field(&r, "script") == "no", &r);
                // No eval check: code injected through ExecuteScript is exempt
                // from the page CSP (`eval=ran` above), so it cannot prove
                // anything. The absence of 'unsafe-eval' is a unit test in ui.rs.
                check("csp-blocks-network-from-ui", field(&r, "fetch") == "blocked", &r);
                check("csp-violations-reported", field(&r, "viol").contains("script-src"), &r);
            });
            // Control: the very same probe must succeed where no CSP applies,
            // otherwise "blocked" above would prove nothing.
            page_eval(app, "csp-probe-runs-in-unrestricted-page", CSP_READ_JS, |r| {
                log(&format!("INFO csp-page {r}"));
                check(
                    "csp-probe-runs-in-unrestricted-page",
                    field(&r, "attr") == "ran" && field(&r, "script") == "ran" && field(&r, "fetch") == "allowed",
                    &r,
                );
            });
            page_eval(app, "reload-setup-marker-and-fresh-navigation", "`m=${window.__m};nav=${performance.getEntriesByType('navigation')[0].type}`", |r| {
                check("reload-setup-marker-and-fresh-navigation", field(&r, "m") == "1" && field(&r, "nav") == "navigate", &r);
            });
        }
        24 => {
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("reload-setup-page-idle", !t.loading, "still loading, the button would act as stop");
            ui_js(app, "document.getElementById('reload').click()");
        }
        25 => {
            page_eval(app, "reload-button-reloads-page", "`m=${window.__m};nav=${performance.getEntriesByType('navigation')[0].type}`", |r| {
                check("reload-button-reloads-page", field(&r, "m") == "undefined" && field(&r, "nav") == "reload", &r);
            });
            ui_eval(
                app,
                "ui-loading-indicator-clears-after-reload",
                "`reload=${document.getElementById('reload').hasAttribute('data-loading')};\
                 bar=${document.getElementById('loadbar').hasAttribute('data-on')}`",
                |r| check("ui-loading-indicator-clears-after-reload", field(&r, "reload") == "false" && field(&r, "bar") == "false", &r),
            );
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("loading-finished-after-reload", !t.loading, "still loading");
            page_js(app, "window.__m = 2;");
        }
        26 => {
            check("ctrl-r-maps-to-reload", press(app, vk::R, CTRL) == Some(Shortcut::Reload), "no mapping");
        }
        27 => {
            page_eval(app, "reload-shortcut-reloads-page", "`m=${window.__m};nav=${performance.getEntriesByType('navigation')[0].type}`", |r| {
                check("reload-shortcut-reloads-page", field(&r, "m") == "undefined" && field(&r, "nav") == "reload", &r);
            });
            type_in_address(app, "https://example.org/");
        }
        28 => {
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("history-setup-on-example-org", active_url(app).starts_with("https://example.org") && t.can_back, format!("{t:?}"));
            ui_eval(
                app,
                "ui-back-button-enabled-fwd-disabled",
                "`back=${document.getElementById('back').disabled};fwd=${document.getElementById('forward').disabled}`",
                |r| check("ui-back-button-enabled-fwd-disabled", field(&r, "back") == "false" && field(&r, "fwd") == "true", &r),
            );
            press(app, vk::LEFT, ALT);
        }
        29 => {
            check("alt-left-goes-back", active_url(app).starts_with("https://example.com"), active_url(app));
            ui_eval(
                app,
                "ui-forward-button-enabled-after-back",
                "`back=${document.getElementById('back').disabled};fwd=${document.getElementById('forward').disabled}`",
                |r| check("ui-forward-button-enabled-after-back", field(&r, "fwd") == "false", &r),
            );
            press(app, vk::RIGHT, ALT);
        }
        30 => {
            check("alt-right-goes-forward", active_url(app).starts_with("https://example.org"), active_url(app));
            // Typing in the address bar must not be overwritten by page URL changes.
            ui_js(
                app,
                "(() => { const a = document.getElementById('address'); a.focus(); a.value = 'typed-text'; \
                 a.dispatchEvent(new Event('input', { bubbles: true })); })()",
            );
            page_js(app, "history.pushState({}, '', '/p2');");
        }
        31 => {
            check("pushstate-url-reached-app", active_url(app) == "https://example.org/p2", active_url(app));
            ui_eval(app, "typed-address-survives-url-update", "document.getElementById('address').value", |r| {
                check("typed-address-survives-url-update", r == "typed-text", r);
            });
            ui_js(
                app,
                "document.getElementById('address').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))",
            );
        }
        32 => {
            let url = active_url(app);
            ui_eval(app, "escape-restores-address-url", "document.getElementById('address').value", move |r| {
                check("escape-restores-address-url", r == url, format!("address={r} want {url}"));
            });
            // A refused navigation (Rust answers with xb.blocked()) must leave the
            // text for the user to fix, even when the page updates right after.
            // `data-blocked` (set by xb.blocked()) fades after 900 ms, so a latch
            // records the moment it appears; step 33 reads the latch, not the timer.
            ui_js(
                app,
                "(() => { const a = document.getElementById('address'); a.focus(); a.value = 'javascript:refused'; \
                 const box = document.getElementById('address-box'); window.__blocked = false; \
                 new MutationObserver(() => { if (box.hasAttribute('data-blocked')) window.__blocked = true; }) \
                 .observe(box, { attributes: true, attributeFilter: ['data-blocked'] }); \
                 a.dispatchEvent(new Event('input', { bubbles: true })); \
                 a.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); })()",
            );
            page_js(app, "history.pushState({}, '', '/p2b');");
        }
        33 => {
            check("pushstate-after-refusal-reached-app", active_url(app) == "https://example.org/p2b", active_url(app));
            // The latch proves Enter reached Rust and was refused (xb.blocked()),
            // not that Enter did nothing.
            ui_eval(
                app,
                "refused-navigation-keeps-typed-text",
                "`v=${document.getElementById('address').value};f=${document.activeElement.id};b=${window.__blocked}`",
                |r| {
                    check(
                        "refused-navigation-keeps-typed-text",
                        field(&r, "v") == "javascript:refused" && field(&r, "f") == "address" && field(&r, "b") == "true",
                        &r,
                    );
                },
            );
            ui_js(app, "document.activeElement && document.activeElement.blur()");
            ui_eval(app, "ctrl-t-setup-address-unfocused", "document.activeElement ? document.activeElement.id : 'none'", |r| {
                check("ctrl-t-setup-address-unfocused", r != "address", r);
            });
            remember("t1", app.tabs.active().unwrap() as i64);
            press(app, vk::T, CTRL);
            app.flush(); // the deferred hide of the old view, as at the end of a loop pass
            let b1 = app.tabs.active().unwrap();
            let blank = app.tabs.get(b1).map(|t| t.url.as_str() == "about:blank").unwrap_or(false);
            check(
                "ctrl-t-opens-blank-tab",
                app.tabs.len() == 2 && b1 as i64 != recall("t1") && blank && only_active_visible(app),
                app.tabs.to_json(),
            );
            remember("b1", b1 as i64);
            press(app, vk::T, CTRL);
            remember("b2", app.tabs.active().unwrap() as i64);
            check("ctrl-t-opens-another-tab", app.tabs.len() == 3, app.tabs.len());
        }
        34 => {
            ui_eval(app, "ctrl-t-focuses-address-bar", "document.activeElement ? document.activeElement.id : 'none'", |r| {
                check("ctrl-t-focuses-address-bar", r == "address", format!("activeElement={r}"));
            });
            // Order is [t1, b1, b2] with b2 active. Three tabs, so previous
            // and next are different tabs.
            let (t1, b1, b2) = (recall("t1") as TabId, recall("b1") as TabId, recall("b2") as TabId);
            check("shift-tab-setup-b2-active", app.tabs.active() == Some(b2), format!("{:?}", app.tabs.active()));
            press(app, vk::TAB, CTRL_SHIFT);
            let a1 = app.tabs.active();
            press(app, vk::TAB, CTRL_SHIFT);
            let a2 = app.tabs.active();
            press(app, vk::TAB, CTRL_SHIFT);
            let a3 = app.tabs.active();
            press(app, vk::TAB, CTRL);
            let a4 = app.tabs.active();
            check("ctrl-shift-tab-goes-to-previous-tab", a1 == Some(b1) && a2 == Some(t1), format!("{a1:?} {a2:?} want {b1} {t1}"));
            check("ctrl-shift-tab-wraps-to-last-tab", a3 == Some(b2), format!("{a3:?} want {b2}"));
            check("ctrl-tab-wraps-to-first-tab", a4 == Some(t1), format!("{a4:?} want {t1}"));
            app.flush();
            check("shortcut-cycle-visibility", only_active_visible(app), "visibility mismatch");
        }
        35 => {
            ui_js(app, "document.activeElement && document.activeElement.blur()");
            ui_eval(app, "ctrl-l-setup-address-unfocused", "document.activeElement ? document.activeElement.id : 'none'", |r| {
                check("ctrl-l-setup-address-unfocused", r != "address", r);
            });
            check("ctrl-l-maps-to-focus-address", press(app, vk::L, CTRL) == Some(Shortcut::FocusAddress), "no mapping");
            ui_eval(
                app,
                "ctrl-l-focuses-and-selects-address",
                "(() => { const a = document.getElementById('address'); \
                 return `id=${document.activeElement.id};len=${a.value.length};s=${a.selectionStart};e=${a.selectionEnd}`; })()",
                |r| {
                    let len = num(&r, "len");
                    check(
                        "ctrl-l-focuses-and-selects-address",
                        field(&r, "id") == "address" && len > 0.0 && num(&r, "s") == 0.0 && num(&r, "e") == len,
                        &r,
                    );
                },
            );
            page_js(app, "history.pushState({}, '', '/p3');");
        }
        36 => {
            // The page changed its URL (pushState) after Ctrl+L selected the
            // address: the selection must still be intact and typing replaces it.
            let url = active_url(app);
            check("selection-setup-pushstate-reached-app", url.ends_with("/p3"), &url);
            ui_eval(
                app,
                "focused-address-not-overwritten-by-url-update",
                "(() => { const a = document.getElementById('address'); \
                 const o = `v=${a.value};len=${a.value.length};s=${a.selectionStart};e=${a.selectionEnd};f=${document.activeElement.id}`; \
                 document.execCommand('insertText', false, 'z'); return `${o};after=${a.value}`; })()",
                |r| {
                    let len = num(&r, "len");
                    check(
                        "focused-address-not-overwritten-by-url-update",
                        field(&r, "f") == "address" && len > 0.0 && num(&r, "s") == 0.0 && num(&r, "e") == len && !field(&r, "v").ends_with("/p3"),
                        &r,
                    );
                    check("typing-replaces-selected-address", field(&r, "after") == "z", &r);
                },
            );
            // AltGr (= Ctrl+Alt) must never trigger a shortcut. First prove the
            // keys are live: each does map with its proper modifier.
            let live: &[(u32, Mods)] = &[
                (vk::T, CTRL),
                (vk::W, CTRL),
                (vk::L, CTRL),
                (vk::R, CTRL),
                (vk::TAB, CTRL),
                (vk::NEXT, CTRL),
                (vk::PRIOR, CTRL),
                (vk::F5, NONE),
                (vk::LEFT, ALT),
                (vk::RIGHT, ALT),
            ];
            let dead: Vec<u32> = live.iter().filter(|(k, m)| Shortcut::from_key(*k, *m).is_none()).map(|(k, _)| *k).collect();
            check("altgr-setup-keys-map-without-altgr", dead.is_empty(), format!("unmapped: {dead:x?}"));
            let state = |a: &App| (a.tabs.len(), a.tabs.active(), active_url(a));
            let before = state(app);
            let fired: Vec<u32> = live.iter().map(|(k, _)| *k).filter(|k| press(app, *k, ALTGR).is_some()).collect();
            check("altgr-triggers-no-shortcut", fired.is_empty() && state(app) == before, format!("fired {fired:x?}"));
            close_extra_tabs(app);
            check("extra-tabs-closed", app.tabs.len() == 1, app.tabs.len());
            let t1 = app.tabs.active().unwrap();
            remember("t1", t1 as i64);
            // A tab after the opener, so "placed right after the opener" differs
            // from "appended at the end".
            press(app, vk::T, CTRL);
            remember("b1", app.tabs.active().unwrap() as i64);
            app.activate(t1);
            // One full-width button per URL; each click calls window.open from
            // a trusted (CDP mouse) gesture. Six rows: five non-web, one https.
            let mut html = String::from("<!doctype html><title>Gesture</title><body style=\"margin:0\">");
            for (i, url) in GESTURE_URLS.iter().enumerate() {
                html.push_str(&format!(
                    "<button style=\"position:fixed;left:0;width:100%;top:{:.4}%;height:{:.4}%\" onclick=\"window.__c=(window.__c||0)+1; window.open('{url}')\">{i}</button>",
                    i as f64 * 100.0 / 6.0,
                    100.0 / 6.0,
                ));
            }
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                let _ = v.load_html(&html);
            }
        }
        37 => {
            remember("len", app.tabs.len() as i64);
            page_eval(app, "window-open-setup-page-loaded", "String(document.querySelectorAll('button').length)", |r| {
                check("window-open-setup-page-loaded", r == "6", r);
            });
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                for row in 0..5 {
                    cdp_click(v, 200.0, row_y(row)); // non-web schemes
                }
            }
        }
        38 => {
            eval(app.view(recall("t1") as TabId), "window-open-setup-clicks-reached-page", "String(window.__c)", |r| {
                check("window-open-setup-clicks-reached-page", r == "5", r)
            });
            check(
                "gesture-window-open-non-web-schemes-blocked",
                app.tabs.len() as i64 == recall("len"),
                format!("{} ({:?})", app.tabs.to_json(), &GESTURE_URLS[..5]),
            );
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                cdp_click(v, 200.0, row_y(5)); // https
            }
        }
        39 => {
            let t1 = recall("t1") as TabId;
            // The new tab is active now; the click counter lives in the opener.
            eval(app.view(t1), "window-open-setup-last-click-reached-page", "String(window.__c)", |r| {
                check("window-open-setup-last-click-reached-page", r == "6", r)
            });
            let order: Vec<TabId> = app.tabs.ids().collect();
            check("window-open-with-gesture-opens-tab", app.tabs.len() as i64 == recall("len") + 1, app.tabs.to_json());
            check("window-open-tab-has-requested-url", active_url(app).contains("from=window-open"), active_url(app));
            check(
                "window-open-tab-placed-after-opener",
                order.len() == 3 && order[0] == t1 && Some(order[1]) == app.tabs.active() && order[2] == recall("b1") as TabId,
                format!("{order:?} active {:?} (want [{t1}, new, {}])", app.tabs.active(), recall("b1")),
            );
            check("window-open-tab-active-and-visible", only_active_visible(app), "visibility mismatch");
            close_extra_tabs(app);
        }
        40 => {
            check("window-open-extra-tab-closed", app.tabs.len() == 1, app.tabs.len());
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                let _ = v.load_html("<!doctype html><title><img src=x onerror=window.__pwn=1></title><body>hostile title</body>");
            }
        }
        41 => {
            check("hostile-title-reached-app", active_title(app).starts_with("<img"), active_title(app));
            ui_eval(
                app,
                "hostile-title-rendered-as-text-not-html",
                "(() => { const t = document.querySelector('[role=tab][aria-selected=true] .title'); \
                 return `imgs=${document.querySelectorAll('#tabs img:not(.icon)').length};pwn=${typeof window.__pwn};text=${t.textContent}`; })()",
                |r| {
                    check(
                        "hostile-title-rendered-as-text-not-html",
                        field(&r, "imgs") == "0" && field(&r, "pwn") == "undefined" && field(&r, "text").contains("<img"),
                        &r,
                    );
                },
            );
            // The UI webview must not be steerable away from its own origin or
            // into popups, even by script running inside it.
            remember("len", app.tabs.len() as i64);
            ui_js(app, "window.__w = window.open('https://example.org/'); location.href = 'https://example.com/';");
        }
        42 => {
            ui_eval(app, "ui-cannot-navigate-away", "`href=${location.href};xb=${typeof window.xb}`", |r| {
                check(
                    "ui-cannot-navigate-away",
                    crate::ui::is_ui_origin(field(&r, "href")) && field(&r, "xb") == "object",
                    &r,
                );
            });
            // Denied popups return null; an allowed one would be a real window
            // (which is not a tab, so the tab count alone would not notice).
            ui_eval(app, "ui-window-open-denied", "String(window.__w)", |r| check("ui-window-open-denied", r == "null", format!("window.open returned {r}")));
            check("ui-window-open-opens-no-tab", app.tabs.len() as i64 == recall("len"), app.tabs.to_json());
            remember("base", app.tabs.len() as i64);
            for _ in 0..29 {
                app.handle(UserEvent::Ui(UiCommand::NewTab));
            }
            app.flush();
            check("many-tabs-opened", app.tabs.len() == 30 && app.views.len() == 30, format!("{} tabs {} views", app.tabs.len(), app.views.len()));
        }
        43 => {
            let (n, active) = (app.tabs.len(), app.tabs.active().unwrap_or(0));
            capture(app, "06-30-tabs-1200");
            ui_eval(app, "many-tabs-ui-lists-every-tab", &tab_strip_js(), move |r| {
                log(&format!("INFO tab-strip-1200 {r}"));
                check("many-tabs-ui-lists-every-tab", num(&r, "n") == n as f64 && num(&r, "selected") == 1.0 && field(&r, "sel_id") == active.to_string(), &r);
                // 30 tabs do not fit at 1200 px either: the list scrolls (with
                // its arrows), the zone and the strip never overflow.
                check(
                    "many-tabs-list-scrolls-strip-does-not",
                    num(&r, "over") > 0.0 && field(&r, "ovf") == "true" && field(&r, "arrows") == "true"
                        && num(&r, "zover") <= 0.0 && num(&r, "sover") <= 0.0 && num(&r, "leak") == 0.0,
                    &r,
                );
                check("many-tabs-keep-minimum-width", num(&r, "minw") >= 36.0, &r);
                check("many-tabs-active-tab-scrolled-into-view", field(&r, "sel_vis") == "true", &r);
                check("many-tabs-new-tab-button-visible", field(&r, "newtab") == "true", &r);
                check("many-tabs-window-buttons-visible", field(&r, "win") == "true", &r);
            });
            app.window.set_inner_size(LogicalSize::new(820.0, 520.0));
        }
        44 => {
            capture(app, "06-30-tabs-820");
            ui_eval(app, "many-tabs-820px-window-buttons-visible", &tab_strip_js(), |r| {
                log(&format!("INFO tab-strip-820 {r}"));
                // iw proves the resize reached the UI; otherwise this measured 1200 px.
                check("many-tabs-820px-window-buttons-visible", num(&r, "iw") == 820.0 && field(&r, "win") == "true", &r);
                check(
                    "many-tabs-820px-no-overflow",
                    field(&r, "arrows") == "true" && num(&r, "minw") >= 36.0
                        && num(&r, "zover") <= 0.0 && num(&r, "sover") <= 0.0 && num(&r, "leak") == 0.0,
                    &r,
                );
                check("many-tabs-820px-new-tab-visible", field(&r, "newtab") == "true", &r);
                check("many-tabs-820px-active-tab-identifiable", field(&r, "sel_vis") == "true" && num(&r, "sel_w") >= 24.0, &r);
                check("many-tabs-820px-drag-area", ["tabs", "strip"].contains(&field(&r, "drag_el")), &r);
            });
            let target = app.tabs.ids().nth(4).unwrap();
            remember("click", target as i64);
            ui_js(
                app,
                &format!(
                    "document.querySelector('[data-id=\"{target}\"]').dispatchEvent(new MouseEvent('mousedown', {{ button: 0, bubbles: true }}))"
                ),
            );
            // The narrowest window.
            app.window.set_inner_size(LogicalSize::new(MIN_W, 240.0));
        }
        45 => {
            let target = recall("click") as TabId;
            check("ui-tab-click-activates", app.tabs.active() == Some(target), format!("{:?} want {target}", app.tabs.active()));
            check("ui-tab-click-shows-only-that-view", only_active_visible(app), "visibility mismatch");
            // 30 tabs at the narrowest window.
            capture(app, "07-30-tabs-min-width");
            ui_eval(app, "many-tabs-min-width-window-is-minimum", &tab_strip_js(), |r| {
                log(&format!("INFO tab-strip-min {r}"));
                check("many-tabs-min-width-window-is-minimum", num(&r, "iw") == MIN_W, &r);
                check(
                    "many-tabs-min-width-no-overflow",
                    field(&r, "arrows") == "true" && num(&r, "minw") >= 36.0
                        && num(&r, "zover") <= 0.0 && num(&r, "sover") <= 0.0 && num(&r, "leak") == 0.0,
                    &r,
                );
                check("many-tabs-min-width-chrome-visible", field(&r, "chrome") == "ok", &r);
                check("many-tabs-min-width-new-tab-visible", field(&r, "newtab") == "true", &r);
                check("many-tabs-min-width-active-tab-identifiable", field(&r, "sel_vis") == "true" && num(&r, "sel_w") >= 24.0, &r);
                check("many-tabs-min-width-drag-area", ["tabs", "strip"].contains(&field(&r, "drag_el")), &r);
            });
            // Every one of the 30 tabs can be brought into view with the mouse.
            ui_eval(app, "many-tabs-min-width-every-tab-reachable", &reach_js(), |r| {
                log(&format!("INFO tab-reach-min {r}"));
                check(
                    "many-tabs-min-width-every-tab-reachable",
                    num(&r, "n") == 30.0 && num(&r, "seen") == 30.0 && field(&r, "arrows") == "true"
                        && field(&r, "start") == "true" && field(&r, "end") == "true",
                    &r,
                );
            });
            ui_eval(app, "tabs-aria-structure", TAB_KEYS_JS, |r| {
                log(&format!("INFO tab-keys {r}"));
                check(
                    "tabs-aria-structure",
                    field(&r, "list_role") == "tablist"
                        && field(&r, "only_tabs") == "true"
                        && field(&r, "plus_outside") == "true"
                        && field(&r, "close_btns") == "true"
                        && field(&r, "named") == "true",
                    &r,
                );
                check(
                    "tabs-roving-tabindex",
                    field(&r, "tab0") == "1" && field(&r, "sel_is_tab0") == "true" && field(&r, "others_m1") == "true",
                    &r,
                );
                check(
                    "tab-arrow-home-end-move-focus-only",
                    ["right", "left", "home", "end", "wrap", "sel_kept"].iter().all(|k| field(&r, k) == "true"),
                    &r,
                );
            });
            // Keyboard: Enter on a focused tab activates it, Delete closes one.
            let (enter, del) = (app.tabs.ids().nth(6).unwrap(), app.tabs.ids().nth(8).unwrap());
            remember("enter", enter as i64);
            remember("del", del as i64);
            let key = |id: TabId, k: &str| {
                format!(
                    "(() => {{ const t = document.querySelector('[data-id=\"{id}\"]'); t.focus(); \
                     t.dispatchEvent(new KeyboardEvent('keydown', {{ key: '{k}', bubbles: true, cancelable: true }})); }})()"
                )
            };
            ui_js(
                app,
                &format!(
                    "document.querySelector('[data-id=\"{target}\"]').dispatchEvent(new MouseEvent('auxclick', {{ button: 1, bubbles: true }}))"
                ),
            );
            ui_js(app, &key(enter, "Enter"));
            ui_js(app, &key(del, "Delete"));
        }
        46 => {
            let target = recall("click") as TabId;
            check(
                "middle-click-closes-tab",
                app.tabs.get(target).is_none(),
                format!("{} tabs {} views", app.tabs.len(), app.views.len()),
            );
            check(
                "keyboard-enter-activates-tab",
                app.tabs.active() == Some(recall("enter") as TabId),
                format!("{:?} want {}", app.tabs.active(), recall("enter")),
            );
            check("keyboard-delete-closes-tab", app.tabs.get(recall("del") as TabId).is_none(), app.tabs.to_json());
            check(
                "tab-closes-settled",
                app.tabs.len() == 28 && app.views.len() == 28,
                format!("{} tabs {} views", app.tabs.len(), app.views.len()),
            );
            // The Delete key on two inactive tabs of the settled strip (DOM order is
            // the tab order):
            //  - `held`: a repeated keydown (key held down) must close nothing;
            //  - `inactive`: closed, and keyboard focus must still be on its next
            //    neighbour once the new state has rendered (step 47 reads it), with
            //    no tab element moved: a moved element loses focus.
            // The UI messages are handled before the next step, which checks the state.
            let ids: Vec<TabId> = app.tabs.ids().collect();
            let (Some(&held), Some(&inactive), Some(&inactive_next)) = (ids.get(12), ids.get(1), ids.get(2)) else {
                return check("keyboard-delete-has-tabs-to-use", false, app.tabs.to_json());
            };
            remember("del-held", held as i64);
            remember("del-inactive", inactive as i64);
            ui_js(
                app,
                &format!(
                    "(() => {{ const q = (id) => document.querySelector('[data-id=\"' + id + '\"]'); \
                     const key = (id, repeat) => {{ const t = q(id); t.focus(); \
                     t.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Delete', repeat, bubbles: true, cancelable: true }})); }}; \
                     window.__nbr = q({inactive_next}); window.__added = 0; \
                     window.__obs = new MutationObserver((ms) => {{ for (const m of ms) window.__added += m.addedNodes.length; }}); \
                     window.__obs.observe(document.getElementById('tablist'), {{ childList: true }}); \
                     key({held}, true); \
                     key({inactive}, false); }})()"
                ),
            );
        }
        47 => {
            let (held, inactive) = (recall("del-held") as TabId, recall("del-inactive") as TabId);
            check(
                "keyboard-delete-repeat-closes-nothing",
                app.tabs.get(held).is_some() && app.views.iter().any(|(id, _)| *id == held),
                app.tabs.to_json(),
            );
            check("keyboard-delete-inactive-tab-closes-it", app.tabs.get(inactive).is_none(), app.tabs.to_json());
            // Read after the state has rendered. The neighbour keeps focus only if
            // the render leaves every remaining tab element where it is.
            ui_eval(
                app,
                "keyboard-delete-inactive-tab-keeps-neighbour-focus",
                "(() => { window.__obs.takeRecords(); window.__obs.disconnect(); \
                 return 'focus=' + (document.activeElement === window.__nbr && window.__nbr.isConnected) \
                 + ';added=' + window.__added + ';n=' + document.querySelectorAll('[role=tab]').length; })()",
                |r| {
                    check("keyboard-delete-inactive-tab-keeps-neighbour-focus", field(&r, "focus") == "true", &r);
                    check("closing-a-tab-moves-no-other-tab-element", field(&r, "n") == "27" && field(&r, "added") == "0", &r);
                },
            );
            // The active tab: closed, and the next tab becomes active. Checked on
            // state only (page focus goes to the content webview), in the next step.
            let ids: Vec<TabId> = app.tabs.ids().collect();
            let active = app.tabs.active();
            let next = active.and_then(|a| ids.iter().position(|&i| i == a)).and_then(|at| ids.get(at + 1)).copied();
            let (Some(active), Some(next)) = (active, next) else {
                return check("keyboard-delete-active-has-next-tab", false, app.tabs.to_json());
            };
            remember("del-active", active as i64);
            remember("del-next", next as i64);
            ui_js(
                app,
                &format!(
                    "(() => {{ const t = document.querySelector('[data-id=\"{active}\"]'); t.focus(); \
                     t.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Delete', bubbles: true, cancelable: true }})); }})()"
                ),
            );
        }
        48 => {
            let (active, next) = (recall("del-active") as TabId, recall("del-next") as TabId);
            check(
                "keyboard-delete-active-tab-activates-neighbour",
                app.tabs.get(active).is_none() && app.tabs.active() == Some(next) && app.tabs.len() == 26,
                format!("active {:?} want {next}: {}", app.tabs.active(), app.tabs.to_json()),
            );
            // Real (CDP) mouse input on the far end of the strip, still at the
            // minimum width: scroll there with the arrow, note where it is.
            set_spot([f64::NAN; 4]);
            let far = order(app).last().map_or(-1, |&id| id as i64);
            remember("far", far);
            ui_eval(app, "strip-scrolled-to-the-end", SCROLL_END_JS, move |r| {
                log(&format!("INFO strip-end {r}"));
                set_spot([num(&r, "x"), num(&r, "y"), num(&r, "wx"), num(&r, "wy")]);
                check("strip-scrolled-to-the-end", field(&r, "id") == far.to_string() && num(&r, "x") < MIN_W, &r);
            });
        }
        49 => {
            let [x, y, ..] = spot();
            check("strip-far-tab-has-a-point", x.is_finite() && y.is_finite(), format!("{x},{y}"));
            cdp_click(&app.ui, x, y);
        }
        50 => {
            let far = recall("far") as TabId;
            check("strip-real-click-activates-far-tab", app.tabs.active() == Some(far), format!("{:?} want {far}", app.tabs.active()));
            // Wheel up over the list scrolls it back toward the start.
            let [_, _, wx, wy] = spot();
            cdp_wheel(&app.ui, wx, wy, -400.0);
        }
        51 => {
            ui_eval(
                app,
                "strip-wheel-scrolls-tabs",
                "(() => { const l = document.getElementById('tablist'); return `sl=${l.scrollLeft};max=${l.scrollWidth - l.clientWidth}`; })()",
                |r| check("strip-wheel-scrolls-tabs", num(&r, "max") > 0.0 && num(&r, "sl") < num(&r, "max") - 100.0, &r),
            );
            close_extra_tabs(app);
            app.window.set_inner_size(LogicalSize::new(1200.0, 800.0));
        }
        52 => {
            check(
                "many-tabs-closed-back-down",
                app.tabs.len() == 1 && app.views.len() == 1 && only_active_visible(app),
                format!("{} tabs {} views", app.tabs.len(), app.views.len()),
            );
            ui_eval(app, "ui-tab-strip-follows-closes", &tab_strip_js(), |r| {
                check("ui-tab-strip-follows-closes", num(&r, "n") == 1.0 && num(&r, "selected") == 1.0 && num(&r, "over") <= 0.0, &r);
            });
            let size = app.window.inner_size();
            remember("w", size.width as i64);
            remember("h", size.height as i64);
            check("window-starts-unmaximized", !app.window.is_maximized() && !app.window.is_minimized(), "unexpected window state");
            ui_js(app, "document.getElementById('max').click()");
        }
        53 => {
            check("ui-max-button-maximizes", app.window.is_maximized(), "not maximized");
            ui_eval(
                app,
                "ui-max-button-shows-restore-state",
                "`max=${document.getElementById('max').hasAttribute('data-max')};res_hidden=${document.getElementById('resizer').hidden}`",
                |r| {
                    check("ui-max-button-shows-restore-state", field(&r, "max") == "true", &r);
                    check("top-resize-handles-hidden-when-maximized", field(&r, "res_hidden") == "true", &r);
                },
            );
            let (w, h) = logical_size(app);
            let scale = app.window.scale_factor();
            let c = app.view(app.tabs.active().unwrap()).unwrap().bounds().unwrap();
            let cs = c.size.to_logical::<f64>(scale);
            check(
                "content-follows-maximize",
                (cs.width - w).abs() <= 1.0 && (cs.height - (h - crate::app::UI_HEIGHT)).abs() <= 1.0,
                format!("content {cs:?} window {w}x{h}"),
            );
            ui_js(app, "document.getElementById('max').click()");
        }
        54 => {
            let size = app.window.inner_size();
            check("ui-max-button-restores", !app.window.is_maximized(), "still maximized");
            check(
                "restore-returns-original-size",
                (size.width as i64 - recall("w")).abs() <= 2 && (size.height as i64 - recall("h")).abs() <= 2,
                format!("{}x{} want {}x{}", size.width, size.height, recall("w"), recall("h")),
            );
            ui_eval(
                app,
                "ui-max-button-resets-state",
                "`max=${document.getElementById('max').hasAttribute('data-max')};res_hidden=${document.getElementById('resizer').hidden}`",
                |r| check("ui-max-button-resets-state", field(&r, "max") == "false" && field(&r, "res_hidden") == "false", &r),
            );
            // Double-click on empty strip space maximizes (a single mousedown
            // there would start an OS drag loop, which needs a real mouse).
            ui_js(
                app,
                "document.getElementById('tabs').dispatchEvent(new MouseEvent('mousedown', \
                 { button: 0, detail: 2, bubbles: true, cancelable: true }))",
            );
        }
        55 => {
            check("strip-double-click-maximizes", app.window.is_maximized(), "not maximized");
            ui_js(app, "document.getElementById('max').click()");
        }
        56 => {
            check("window-restored-before-minimize", !app.window.is_maximized() && !app.window.is_minimized(), "unexpected window state");
            ui_js(app, "document.getElementById('min').click()");
        }
        57 => {
            check("ui-min-button-minimizes", app.window.is_minimized(), "not minimized");
            app.window.set_minimized(false);
        }
        58 => {
            check("window-restores-from-minimize", !app.window.is_minimized(), "still minimized");
            app.window.set_inner_size(LogicalSize::new(MIN_W, 240.0));
        }
        59 => {
            let (w, h) = logical_size(app);
            log(&format!("INFO minimum window {w}x{h}"));
            // set_inner_size is not clamped, so ask the OS what `with_min_inner_size`
            // configured: the narrowest width a user can drag the window to.
            let track = min_track_width(app);
            check("window-min-width-is-configured", (MIN_W..=MIN_W + 40.0).contains(&track), format!("OS minimum track width {track}, want {MIN_W}"));
            ui_eval(app, "min-size-iw-is-minimum", &ui_static_js(), |r| {
                log(&format!("INFO ui-static-min {r}"));
                check("min-size-iw-is-minimum", num(&r, "iw") == MIN_W, &r);
                check("min-size-chrome-complete", field(&r, "chrome") == "ok", format!("not visible: {}", field(&r, "chrome")));
                check("min-size-drag-area", ["tabs", "strip"].contains(&field(&r, "drag_el")), &r);
                check("min-size-active-tab-visible", field(&r, "sel_vis") == "true" && num(&r, "sel_w") >= 24.0, &r);
                check("min-size-address-box-usable", num(&r, "addr_w") >= 100.0, &r);
            });
            app.window.set_inner_size(LogicalSize::new(1200.0, 800.0));
        }
        60 => {
            press(app, vk::T, CTRL);
            app.flush();
            check("window-title-blank-tab", app.window.title() == "X-Browser", app.window.title());
            let sets = app.title_sets;
            app.mark_dirty();
            app.flush();
            check("window-title-not-reset-when-unchanged", app.title_sets == sets, format!("{} -> {}", sets, app.title_sets));
            // A fresh tab (no input ever) whose own script starts unmuted audio:
            // Chromium's default policy refuses it without a user gesture.
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                let _ = v.load_html(AUTOPLAY_HTML);
            }
        }
        61 => {
            page_eval(app, "autoplay-without-gesture-refused", "String(window.__ap)", |r| {
                check("autoplay-without-gesture-refused", r == "NotAllowedError", format!("play() -> {r}"));
            });
            remember("len", app.tabs.len() as i64);
            remember("ap", app.tabs.active().unwrap() as i64);
            // ExecuteScript counts as a user gesture, so this opens a tab.
            page_js(app, "window.open('https://example.com/?from=close')");
        }
        62 => {
            let (ap, popup) = (recall("ap") as TabId, app.tabs.active().unwrap());
            remember("popup", popup as i64);
            check(
                "close-test-popup-opened",
                app.tabs.len() as i64 == recall("len") + 1 && popup != ap && active_url(app).contains("from=close"),
                app.tabs.to_json(),
            );
            let want = format!("{} - X-Browser", active_title(app));
            check("window-title-follows-active-tab", app.window.title() == want && active_title(app) == "Example Domain", app.window.title());
            let sets = app.title_sets;
            app.activate(ap);
            app.flush();
            let on_blank = app.window.title();
            app.activate(popup);
            app.flush();
            let back = app.window.title();
            app.mark_dirty();
            app.flush();
            check("window-title-follows-tab-switch", on_blank == "X-Browser" && back == want, format!("{on_blank:?} then {back:?}"));
            check("window-title-set-only-on-change", app.title_sets == sets + 2, format!("{} sets for 2 changes", app.title_sets - sets));
            page_js(app, "document.title = 'Renamed page'");
        }
        63 => {
            check("window-title-follows-page-title", app.window.title() == "Renamed page - X-Browser", app.window.title());
            remember("len", app.tabs.len() as i64);
            remember("closed", app.closed.len() as i64);
            page_js(app, "window.close()");
        }
        64 => {
            let popup = recall("popup") as TabId;
            check(
                "window-close-removes-tab",
                app.tabs.get(popup).is_none() && app.view(popup).is_none() && app.tabs.len() as i64 == recall("len") - 1,
                app.tabs.to_json(),
            );
            // Not something Ctrl+Shift+T brings back (only the user's own closes are).
            check(
                "page-window-close-not-remembered",
                app.closed.len() as i64 == recall("closed") && !app.closed.newest().is_some_and(|u| u.contains("from=close")),
                format!("{} remembered, newest {:?}", app.closed.len(), app.closed.newest()),
            );
            check(
                "window-close-app-healthy",
                !app.quit && app.tabs.active() == Some(recall("ap") as TabId) && only_active_visible(app),
                format!("quit {} active {:?}", app.quit, app.tabs.active()),
            );
            let hosts = host_windows(app);
            check("window-close-leaves-no-stray-host-window", hosts == app.views.len() + 1, format!("{hosts} child windows, {} views + UI", app.views.len()));
            app.handle(UserEvent::Ui(UiCommand::Navigate("http://xb.ui/index.html".into())));
        }
        65 => {
            // Refused before it starts: the old page stays (no network error page).
            page_eval(app, "content-cannot-load-ui-origin", "`href=${location.href};xb=${typeof window.xb};title=${document.title}`", |r| {
                check(
                    "content-cannot-load-ui-origin",
                    !crate::ui::is_ui_url(field(&r, "href")) && field(&r, "xb") == "undefined" && field(&r, "title") == "Autoplay",
                    &r,
                );
            });
            check("address-bar-drops-refused-ui-url", !crate::ui::is_ui_url(&active_url(app)), active_url(app));
            check(
                "refused-ui-url-leaves-tab-not-loading",
                app.tabs.active().and_then(|id| app.tabs.get(id)).is_some_and(|t| !t.loading),
                app.tabs.to_json(),
            );
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                let _ = v.load_html(
                    "<!doctype html><title>Frames</title><iframe id=f src=\"http://xb.ui/index.html\"></iframe><script>\
                     window.__alive = 1; setTimeout(() => { location.href = 'https://xb.ui/js/core.js'; }, 300);</script>",
                );
            }
        }
        66 => {
            page_eval(
                app,
                "content-script-cannot-navigate-to-ui-host",
                "`alive=${window.__alive};href=${location.href};frame=${(() => { \
                 try { return document.getElementById('f').contentWindow.location.href; } catch (e) { return 'cross-origin'; } })()}`",
                |r| {
                    check("content-script-cannot-navigate-to-ui-host", field(&r, "alive") == "1" && !crate::ui::is_ui_url(field(&r, "href")), &r);
                    check("content-frame-cannot-load-ui-host", field(&r, "frame") == "about:blank", &r);
                },
            );
            // A renderer dies mid-load: the tab stays and stops spinning.
            let id = app.tabs.active().unwrap();
            remember("crashed", id as i64);
            if let Some(t) = app.tabs.get_mut(id) {
                t.loading = true;
            }
            if let Some(v) = app.view(id) {
                cdp(v, "Page.crash", "{}");
            }
        }
        67 => {
            let id = recall("crashed") as TabId;
            check(
                "content-crash-keeps-tab-not-loading",
                app.tabs.get(id).is_some_and(|t| !t.loading) && app.view(id).is_some(),
                app.tabs.to_json(),
            );
            check("content-crash-app-healthy", !app.quit && app.fatal.is_none() && app.ui_ready, format!("quit {} ui_ready {}", app.quit, app.ui_ready));
            cdp(&app.ui, "Page.crash", "{}");
        }
        68 => {
            check("ui-crash-reloads-ui", app.ui_ready && !app.quit && app.fatal.is_none(), format!("ui_ready {} quit {}", app.ui_ready, app.quit));
            let (n, active) = (app.tabs.len(), app.tabs.active().unwrap_or(0));
            ui_eval(app, "ui-crash-rerenders-tabs", &tab_strip_js(), move |r| {
                check("ui-crash-rerenders-tabs", num(&r, "n") == n as f64 && field(&r, "sel_id") == active.to_string(), &r);
            });
            close_extra_tabs(app);
        }
        // Strip features on a fresh row of four tabs [a, b, c, d].
        69 => {
            // Earlier steps closed tabs too; start from an empty reopen stack.
            app.closed = Default::default();
            for _ in 0..3 {
                press(app, vk::T, CTRL);
            }
            app.flush();
            let ids = order(app);
            check("strip-test-tabs-opened", ids.len() == 4 && app.tabs.active() == ids.last().copied(), app.tabs.to_json());
            let [a, b, c, d] = ids[..] else { return };
            for (k, id) in [("sa", a), ("sb", b), ("sc", c), ("sd", d)] {
                remember(k, id as i64);
            }
            // d gets a web page (for the icon and for Ctrl+Shift+T).
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?reopen=1".into())));
            let mut jump = |key: u32| {
                press(app, key, CTRL);
                app.tabs.active()
            };
            let (one, three, eight, nine) = (jump(0x31), jump(0x33), jump(0x38), jump(0x39));
            let got = format!("{one:?} {three:?} {eight:?} {nine:?} for [{a}, {b}, {c}, {d}]");
            check("ctrl-1-selects-first-tab", one == Some(a), &got);
            check("ctrl-3-selects-third-tab", three == Some(c), &got);
            check("ctrl-8-past-the-last-tab-does-nothing", eight == Some(c), &got);
            check("ctrl-9-selects-last-tab", nine == Some(d), &got);
        }
        70 => {
            let [a, b, c, d] = ["sa", "sb", "sc", "sd"].map(|k| recall(k) as TabId);
            check("strip-test-page-loaded", tab_url(app, d).contains("reopen=1") && app.tabs.active() == Some(d), tab_url(app, d));
            page_js(
                app,
                &format!(
                    "(() => {{ const l = document.createElement('link'); l.rel = 'icon'; l.href = '{ICON_PNG}'; document.head.appendChild(l); }})()"
                ),
            );
            press(app, vk::PRIOR, CTRL_SHIFT);
            let moved_left = order(app);
            press(app, vk::NEXT, CTRL_SHIFT);
            // Already the last tab: nothing.
            press(app, vk::NEXT, CTRL_SHIFT);
            let moved_back = order(app);
            check("ctrl-shift-pgup-moves-tab-left", moved_left == [a, b, d, c], format!("{moved_left:?}"));
            check(
                "ctrl-shift-pgdn-moves-tab-right-up-to-the-end",
                moved_back == [a, b, c, d] && app.tabs.active() == Some(d),
                format!("{moved_back:?} active {:?}", app.tabs.active()),
            );
            // A drop in the UI arrives as a `move` message on the UI's own IPC channel.
            ui_js(app, &format!("window.ipc.postMessage('move:{a}:3')"));
        }
        71 => {
            let [a, b, c, d] = ["sa", "sb", "sc", "sd"].map(|k| recall(k) as TabId);
            let ids = order(app);
            check("ui-move-command-reorders", ids == [b, c, d, a], format!("{ids:?}"));
            let want = ids_csv(&ids);
            ui_eval(app, "ui-strip-follows-reorder", UI_ORDER_JS, move |r| {
                check("ui-strip-follows-reorder", r == want, format!("{r} want {want}"));
            });
            let icon = app.tabs.get(d).and_then(|t| t.favicon.clone()).unwrap_or_default();
            check(
                "favicon-reaches-tab-as-png-data-url",
                icon.starts_with("data:image/png;base64,iVBORw0KGgo"),
                icon.chars().take(60).collect::<String>(),
            );
            let icons: Vec<String> = ids
                .iter()
                .map(|&id| {
                    let f = app.tabs.get(id).and_then(|t| t.favicon.clone()).unwrap_or_default();
                    format!("{id}:{}:{}", f.len(), f.chars().skip(22).take(40).collect::<String>())
                })
                .collect();
            check(
                "favicon-only-on-its-tab",
                [a, b, c].iter().all(|&id| app.tabs.get(id).is_some_and(|t| t.favicon.is_none())),
                icons.join(" "),
            );
            ui_eval(
                app,
                "favicon-shown-in-ui",
                &format!(
                    "(() => {{ const t = (id) => document.querySelector('[data-id=\"' + id + '\"]'); \
                     const i = t({d}).querySelector('img.icon'); \
                     return [`src=${{i.getAttribute('src').startsWith('data:image/png;base64,')}}`, \
                     `shown=${{i.getBoundingClientRect().width === 16 && i.naturalWidth > 0}}`, \
                     `others=${{[{a}, {b}, {c}].every((id) => !t(id).hasAttribute('data-icon') && !t(id).querySelector('img.icon').hasAttribute('src'))}}`].join(';'); }})()"
                ),
                |r| check("favicon-shown-in-ui", ["src", "shown", "others"].iter().all(|k| field(&r, k) == "true"), &r),
            );
            // Where b (first) and a (last) are, for a real drag of b past a.
            set_spot([f64::NAN; 4]);
            ui_eval(
                app,
                "drag-points-measured",
                &format!(
                    "(() => {{ const r = (id) => document.querySelector('[data-id=\"' + id + '\"]').getBoundingClientRect(); \
                     const b = r({b}), a = r({a}); return `x0=${{b.left + b.width / 2}};y=${{b.top + b.height / 2}};x1=${{a.right - 4}}`; }})()"
                ),
                |r| {
                    set_spot([num(&r, "x0"), num(&r, "y"), num(&r, "x1"), 0.0]);
                    check("drag-points-measured", num(&r, "x1") > num(&r, "x0"), &r);
                },
            );
        }
        72 => {
            let [x0, y, x1, _] = spot();
            if x0.is_finite() && y.is_finite() && x1.is_finite() {
                cdp_drag(&app.ui, x0, x1, y);
            }
        }
        73 => {
            let [a, b, c, d] = ["sa", "sb", "sc", "sd"].map(|k| recall(k) as TabId);
            let ids = order(app);
            check("ui-drag-moves-tab", ids == [c, d, a, b], format!("{ids:?}"));
            // Narrow tabs, d inactive (two new blank tabs, the last one
            // active): a drag that starts on d's page icon.
            app.window.set_inner_size(LogicalSize::new(820.0, 520.0));
            for key in ["se", "sf"] {
                app.handle(UserEvent::Ui(UiCommand::NewTab));
                remember(key, app.tabs.active().map_or(-1, |id| id as i64));
            }
            app.flush();
        }
        74 => {
            let [b, d] = ["sb", "sd"].map(|k| recall(k) as TabId);
            check("drag-on-icon-tab-inactive", app.tabs.active() != Some(d) && app.tabs.len() == 6, app.tabs.to_json());
            set_spot([f64::NAN; 4]);
            ui_eval(
                app,
                "drag-on-icon-points-measured",
                &format!(
                    "(() => {{ const t = (id) => document.querySelector('[data-id=\"' + id + '\"]'); \
                     const d = t({d}), i = d.querySelector('img.icon').getBoundingClientRect(), b = t({b}).getBoundingClientRect(); \
                     const x = i.left + i.width / 2, y = i.top + i.height / 2, l = document.getElementById('tablist'); \
                     return [`x0=${{x}}`, `y=${{y}}`, `x1=${{b.right - 4}}`, `w=${{d.getBoundingClientRect().width}}`, `iw=${{i.width}}`, \
                     `sel=${{d.getAttribute('aria-selected')}}`, `icon=${{d.hasAttribute('data-icon')}}`, \
                     `on_icon=${{d.querySelector('.fav').contains(document.elementFromPoint(x, y))}}`, \
                     `fits=${{l.scrollWidth <= l.clientWidth}}`,                      `guard=${{(() => {{ const img = d.querySelector('img.icon'); return img.draggable === false && getComputedStyle(img).pointerEvents === 'none'                      && getComputedStyle(d).getPropertyValue('-webkit-user-drag') === 'none'; }})()}}`].join(';'); }})()"
                ),
                |r| {
                    set_spot([num(&r, "x0"), num(&r, "y"), num(&r, "x1"), 0.0]);
                    // Narrow (no title, icon only), inactive, the press point on its icon.
                    check(
                        "drag-on-icon-points-measured",
                        num(&r, "w") < 52.0 && num(&r, "iw") == 16.0 && field(&r, "sel") == "false" && field(&r, "icon") == "true"
                            && field(&r, "on_icon") == "true" && field(&r, "fits") == "true" && num(&r, "x1") > num(&r, "x0"),
                        &r,
                    );
                    // CDP input never starts a native drag, so the drag below passes
                    // without these guards too; they are checked directly.
                    check("tab-icon-never-starts-native-drag", field(&r, "guard") == "true", &r);
                },
            );
        }
        75 => {
            let [x0, y, x1, _] = spot();
            if x0.is_finite() && y.is_finite() && x1.is_finite() {
                cdp_drag(&app.ui, x0, x1, y);
            }
        }
        76 => {
            let [a, b, c, d, e, f] = ["sa", "sb", "sc", "sd", "se", "sf"].map(|k| recall(k) as TabId);
            let ids = order(app);
            check("ui-drag-from-page-icon-moves-tab", ids == [c, a, b, d, e, f], format!("{ids:?}"));
            // Next: a drag of a to the far left, cancelled with Escape.
            set_spot([f64::NAN; 4]);
            ui_eval(
                app,
                "escape-drag-points-measured",
                &format!(
                    "(() => {{ const r = (id) => document.querySelector('[data-id=\"' + id + '\"]').getBoundingClientRect(); \
                     const a = r({a}), c = r({c}); return `x0=${{a.left + a.width / 2}};y=${{a.top + a.height / 2}};x1=${{c.left + 4}}`; }})()"
                ),
                |r| {
                    set_spot([num(&r, "x0"), num(&r, "y"), num(&r, "x1"), 0.0]);
                    check("escape-drag-points-measured", num(&r, "x1") < num(&r, "x0"), &r);
                },
            );
        }
        77 => {
            let [x0, y, x1, _] = spot();
            if x0.is_finite() && y.is_finite() && x1.is_finite() {
                cdp_drag_hold(&app.ui, x0, x1, y);
            }
        }
        78 => {
            // Still held: the tab is being dragged and already shown first.
            let [a, c] = ["sa", "sc"].map(|k| recall(k) as TabId);
            let want = format!("{a},{c},");
            ui_eval(
                app,
                "escape-test-drag-in-progress",
                &format!("`order=${{{UI_ORDER_JS}}};dragging=${{document.querySelectorAll('[data-dragging]').length}}`"),
                move |r| check("escape-test-drag-in-progress", field(&r, "order").starts_with(&want) && field(&r, "dragging") == "1", &r),
            );
            let [_, y, x1, _] = spot();
            if y.is_finite() && x1.is_finite() {
                cdp_escape_release(&app.ui, x1, y);
            }
        }
        79 => {
            let [a, b, c, d, e, f] = ["sa", "sb", "sc", "sd", "se", "sf"].map(|k| recall(k) as TabId);
            let ids = order(app);
            let want = ids_csv(&[c, a, b, d, e, f]);
            check("escape-cancels-drag", ids == [c, a, b, d, e, f], format!("{ids:?}"));
            ui_eval(
                app,
                "escape-restores-ui-order",
                &format!("`order=${{{UI_ORDER_JS}}};dragging=${{document.querySelectorAll('[data-dragging]').length}}`"),
                move |r| check("escape-restores-ui-order", field(&r, "order") == want && field(&r, "dragging") == "0", &r),
            );
            for id in [e, f] {
                app.close_tab(id, true);
            }
            app.window.set_inner_size(LogicalSize::new(1200.0, 800.0));
            // d's next document declares no icon: the previous one must go.
            if let Some(v) = app.view(d) {
                let _ = v.load_url("https://example.com/?reopen=1&icon=none");
            }
        }
        80 => {
            let d = recall("sd") as TabId;
            check(
                "favicon-dropped-by-next-page",
                tab_url(app, d).contains("icon=none") && app.tabs.get(d).is_some_and(|t| t.favicon.is_none()),
                format!("{} icon {:?}", tab_url(app, d), app.tabs.get(d).and_then(|t| t.favicon.as_ref()).map(|f| f.len())),
            );
            ui_eval(
                app,
                "ui-drops-favicon-with-the-page",
                &format!(
                    "(() => {{ const t = document.querySelector('[data-id=\"{d}\"]'); \
                     return `attr=${{t.hasAttribute('data-icon')}};src=${{t.querySelector('img.icon').hasAttribute('src')}}`; }})()"
                ),
                |r| check("ui-drops-favicon-with-the-page", field(&r, "attr") == "false" && field(&r, "src") == "false", &r),
            );
            // Ctrl+Shift+T: close d (a web page) and bring it back in its place.
            app.activate(d);
            let at = app.tabs.index_of(d);
            press(app, vk::W, CTRL);
            check("closed-tab-remembered", app.tabs.get(d).is_none() && app.closed.len() == 1, app.tabs.to_json());
            press(app, vk::T, CTRL_SHIFT);
            let back = app.tabs.active();
            check(
                "ctrl-shift-t-reopens-in-place",
                back.is_some_and(|id| id != d) && back.and_then(|id| app.tabs.index_of(id)) == at && app.tabs.len() == 4 && app.closed.len() == 0,
                format!("active {back:?} at {:?} want {at:?}: {}", back.and_then(|id| app.tabs.index_of(id)), app.tabs.to_json()),
            );
            remember("reopened", back.map_or(-1, |id| id as i64));
            press(app, vk::T, CTRL_SHIFT);
            check("ctrl-shift-t-with-nothing-closed-does-nothing", app.tabs.len() == 4 && app.tabs.active() == back, app.tabs.to_json());
        }
        81 => {
            let back = recall("reopened") as TabId;
            check("ctrl-shift-t-tab-is-active", app.tabs.active() == Some(back), format!("{:?} want {back}", app.tabs.active()));
            page_eval(app, "ctrl-shift-t-restores-url", "location.href", |r| {
                check("ctrl-shift-t-restores-url", r.starts_with("https://example.com/") && r.contains("reopen=1"), &r);
            });
            remember("opener", back as i64);
            if let Some(v) = app.view(back) {
                let _ = v.load_html(LINKS_HTML);
            }
        }
        82 => {
            remember("len", app.tabs.len() as i64);
            // Ctrl held (as the new-window handler reads it) and a Ctrl+click.
            set_ctrl_state(true);
            if let Some(v) = app.view(recall("opener") as TabId) {
                cdp_click_with(v, 100.0, 100.0, "left", 2);
            }
        }
        83 => {
            set_ctrl_state(false);
            let opener = recall("opener") as TabId;
            let bg: Vec<TabId> = order(app).into_iter().filter(|&id| tab_url(app, id).contains("bg=ctrl")).collect();
            check(
                "ctrl-click-opens-background-tab",
                app.tabs.len() as i64 == recall("len") + 1 && bg.len() == 1 && app.tabs.active() == Some(opener),
                format!("active {:?} opener {opener}: {}", app.tabs.active(), app.tabs.to_json()),
            );
            check("background-tab-stays-hidden", only_active_visible(app), "visibility mismatch");
            // Middle click. CDP input goes straight to the renderer, so the host
            // window hears of no press: it is told (the WM_PARENTNOTIFY a real
            // press causes) and must have recorded it. A real middle press is on
            // the manual checklist.
            let recorded = app.view(opener).and_then(|v| crate::native::simulate_press(v, true));
            check("simulated-middle-press-recorded-by-host", recorded == Some(true), format!("{recorded:?}"));
            if let Some(v) = app.view(opener) {
                cdp_click_with(v, 100.0, 300.0, "middle", 0);
            }
        }
        84 => {
            let opener = recall("opener") as TabId;
            let mid: Vec<TabId> = order(app).into_iter().filter(|&id| tab_url(app, id).contains("bg=middle")).collect();
            check(
                "middle-click-opens-background-tab",
                app.tabs.len() as i64 == recall("len") + 2 && mid.len() == 1 && app.tabs.active() == Some(opener),
                format!("active {:?} opener {opener}: {}", app.tabs.active(), app.tabs.to_json()),
            );
            // The audio test runs on the opener alone (a full-width tab).
            for id in order(app) {
                if id != opener {
                    app.close_tab(id, true);
                }
            }
            if let Some(v) = app.view(opener) {
                // Whether Chromium's windows under the host would tell it about
                // real presses: WS_EX_NOPARENTNOTIFY on the window under the
                // pointer, or on one between it and the host, would not.
                let probe = crate::native::parent_notify_probe(v);
                let detail = probe.iter().map(|(c, own, path)| format!("{c}:{}{}", u8::from(*own), u8::from(*path))).collect::<Vec<_>>().join(" ");
                log(&format!("INFO parent-notify {detail}"));
                // Chromium's input window, and every window between it and the host,
                // must let presses through (a compositor helper window may opt out).
                let input: Vec<_> = probe.iter().filter(|(c, _, _)| c == "Chrome_RenderWidgetHostHWND").collect();
                check("page-input-window-notifies-host-of-presses", !input.is_empty() && input.iter().all(|(_, _, path)| !path), &detail);
                let _ = v.load_html(AUDIO_HTML);
            }
            // Mute first (the tab's sound button), so the test tone is never heard.
            ui_js(app, &format!("document.querySelector('[data-id=\"{opener}\"] .audio').click()"));
        }
        85 => {
            let opener = recall("opener") as TabId;
            let native_muted = app.view(opener).is_some_and(is_muted);
            check(
                "sound-button-mutes-tab",
                app.tabs.get(opener).is_some_and(|t| t.muted) && native_muted,
                format!("native {native_muted}: {}", app.tabs.to_json()),
            );
            if let Some(v) = app.view(opener) {
                cdp_click(v, 100.0, 100.0);
            }
        }
        86 => {
            let opener = recall("opener") as TabId;
            page_eval(app, "audio-test-tone-state", "String(window.__tone)", |r| log(&format!("INFO tone {r}")));
            check(
                "audio-indicator-follows-playback",
                app.tabs.get(opener).is_some_and(|t| t.audible && t.muted),
                app.tabs.to_json(),
            );
            ui_eval(
                app,
                "ui-shows-muted-sound-button",
                &format!(
                    "(() => {{ const t = document.querySelector('[data-id=\"{opener}\"]'), b = t.querySelector('.audio'), \
                     m = getComputedStyle(t.querySelector('.tab-body'), '::after'); \
                     return `audio=${{t.hasAttribute('data-audio')}};muted=${{t.hasAttribute('data-muted')}};w=${{b.getBoundingClientRect().width}};\
                     tw=${{t.getBoundingClientRect().width}};content=${{m.content}};mw=${{parseFloat(m.width) || 0}};label=${{b.getAttribute('aria-label')}}`; }})()"
                ),
                |r| {
                    check(
                        "ui-shows-muted-sound-button",
                        field(&r, "audio") == "true" && field(&r, "muted") == "true" && num(&r, "w") > 0.0 && field(&r, "label") == "Sesi aç",
                        &r,
                    );
                    // One sound marker: with the button shown, no corner dot. (`content:
                    // none` = not generated; its declared width is reported regardless.)
                    check(
                        "wide-tab-shows-no-audio-dot",
                        num(&r, "tw") >= 76.0 && num(&r, "w") > 0.0 && field(&r, "content") == "none",
                        &r,
                    );
                },
            );
            // The narrowest strip: 30 tabs at the minimum width, the sound button
            // hidden on every tab. The playing tab must still be marked.
            app.window.set_inner_size(LogicalSize::new(MIN_W, 240.0));
            for _ in 0..29 {
                app.handle(UserEvent::Ui(UiCommand::NewTab));
            }
            app.flush();
        }
        87 => {
            let opener = recall("opener") as TabId;
            let inactive = app.tabs.active() != Some(opener) && app.tabs.len() == 30;
            check("narrow-audio-test-setup", inactive && app.tabs.get(opener).is_some_and(|t| t.audible), app.tabs.to_json());
            let js = format!(
                "(() => {{ const t = document.querySelector('[data-id=\"{opener}\"]'), m = getComputedStyle(t.querySelector('.tab-body'), '::after'); \
                 return [`w=${{t.getBoundingClientRect().width}}`, `sel=${{t.getAttribute('aria-selected')}}`, \
                 `speaker=${{getComputedStyle(t.querySelector('.audio')).display}}`, `content=${{m.content}}`, `display=${{m.display}}`, \
                 `mw=${{parseFloat(m.width)}}`, `mh=${{parseFloat(m.height)}}`, `bg=${{m.backgroundColor}}`, \
                 `title=${{t.title}}`, `label=${{t.getAttribute('aria-label')}}`].join(';'); }})()"
            );
            // Marked: the dot is generated, shown and sized, and the name says so.
            let marked = |r: &str| {
                num(r, "w") < 52.0 && field(r, "speaker") == "none" && field(r, "content") != "none" && field(r, "display") != "none"
                    && num(r, "mw") >= 4.0 && num(r, "mh") >= 4.0 && !field(r, "bg").contains("rgba(0, 0, 0, 0)")
                    && field(r, "title").contains("(ses çalıyor") && field(r, "label").contains("(ses çalıyor")
            };
            ui_eval(app, "narrow-inactive-tab-shows-audio-mark", &js, move |r| {
                log(&format!("INFO narrow-audio-inactive {r}"));
                check("narrow-inactive-tab-shows-audio-mark", marked(&r) && field(&r, "sel") == "false", &r);
            });
            // The active tab at this width shows only its close button.
            app.activate(opener);
            app.flush();
            ui_eval(app, "narrow-active-tab-shows-audio-mark", &js, move |r| {
                log(&format!("INFO narrow-audio-active {r}"));
                check("narrow-active-tab-shows-audio-mark", marked(&r) && field(&r, "sel") == "true", &r);
            });
            page_js(app, "window.__ctx && window.__ctx.close()");
            close_extra_tabs(app);
            app.window.set_inner_size(LogicalSize::new(1200.0, 800.0));
        }
        88 => {
            app.handle(UserEvent::Shortcut(Shortcut::CloseTab));
            check("last-tab-close-quits", app.quit, "app still running");
            let passes = PASSES.load(Ordering::Relaxed);
            if passes != EXPECTED_CHECKS {
                check(
                    "all-expected-checks-ran",
                    false,
                    format!("{passes} PASS, want {EXPECTED_CHECKS} (a check failed, never ran, or was added)"),
                );
            }
            log(&format!("DONE failures={} checks={passes}", FAILS.load(Ordering::Relaxed)));
        }
        _ => {}
    }
}

fn cdp(v: &wry::WebView, method: &str, params: &str) {
    use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
    use windows::core::HSTRING;
    unsafe {
        let _ = v.webview().CallDevToolsProtocolMethod(
            &HSTRING::from(method),
            &HSTRING::from(params),
            &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(()))),
        );
    }
}

fn cdp_click(v: &wry::WebView, x: f64, y: f64) {
    cdp_click_with(v, x, y, "left", 0);
}

/// A CDP click with `button` ("left", "middle") and CDP `modifiers` (2 = Ctrl).
fn cdp_click_with(v: &wry::WebView, x: f64, y: f64, button: &str, modifiers: u32) {
    for kind in ["mousePressed", "mouseReleased"] {
        let params = format!(r#"{{"type":"{kind}","x":{x},"y":{y},"button":"{button}","clickCount":1,"modifiers":{modifiers}}}"#);
        cdp(v, "Input.dispatchMouseEvent", &params);
    }
}

/// A left-button drag from (x0, y) to (x1, y) in a few moves.
fn cdp_drag(v: &wry::WebView, x0: f64, x1: f64, y: f64) {
    let ev = |kind: &str, x: f64, buttons: u32| {
        let params = format!(r#"{{"type":"{kind}","x":{x},"y":{y},"button":"left","buttons":{buttons},"clickCount":1}}"#);
        cdp(v, "Input.dispatchMouseEvent", &params);
    };
    ev("mousePressed", x0, 1);
    for i in 1..=6 {
        ev("mouseMoved", x0 + (x1 - x0) * f64::from(i) / 6.0, 1);
    }
    ev("mouseReleased", x1, 0);
}

/// The first half of `cdp_drag`: pressed and moved, not released.
fn cdp_drag_hold(v: &wry::WebView, x0: f64, x1: f64, y: f64) {
    let ev = |kind: &str, x: f64| {
        let params = format!(r#"{{"type":"{kind}","x":{x},"y":{y},"button":"left","buttons":1,"clickCount":1}}"#);
        cdp(v, "Input.dispatchMouseEvent", &params);
    };
    ev("mousePressed", x0);
    for i in 1..=6 {
        ev("mouseMoved", x0 + (x1 - x0) * f64::from(i) / 6.0);
    }
}

/// Escape, then the release that ends a `cdp_drag_hold`.
fn cdp_escape_release(v: &wry::WebView, x: f64, y: f64) {
    for kind in ["rawKeyDown", "keyUp"] {
        let params = format!(r#"{{"type":"{kind}","key":"Escape","code":"Escape","windowsVirtualKeyCode":27,"nativeVirtualKeyCode":27}}"#);
        cdp(v, "Input.dispatchKeyEvent", &params);
    }
    let params = format!(r#"{{"type":"mouseReleased","x":{x},"y":{y},"button":"left","buttons":0,"clickCount":1}}"#);
    cdp(v, "Input.dispatchMouseEvent", &params);
}

fn cdp_wheel(v: &wry::WebView, x: f64, y: f64, dy: f64) {
    let params = format!(r#"{{"type":"mouseWheel","x":{x},"y":{y},"deltaX":0,"deltaY":{dy}}}"#);
    cdp(v, "Input.dispatchMouseEvent", &params);
}

/// Sets or clears the Ctrl key in this thread's keyboard state, which is what
/// `GetKeyState` in the new-window handler reads (no input is sent anywhere).
fn set_ctrl_state(down: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState, VK_CONTROL, VK_LCONTROL};
    let mut keys = [0u8; 256];
    unsafe {
        if GetKeyboardState(&mut keys).is_err() {
            return;
        }
        for k in [VK_CONTROL, VK_LCONTROL] {
            keys[usize::from(k.0)] = if down { 0x80 } else { 0 };
        }
        let _ = SetKeyboardState(&keys);
    }
}

/// WebView2's own mute state for this view (ICoreWebView2_8::IsMuted).
fn is_muted(v: &wry::WebView) -> bool {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_8;
    use windows::core::Interface;
    use wry::WebViewExtWindows;
    let Ok(wv) = v.webview().cast::<ICoreWebView2_8>() else { return false };
    let mut muted = windows::core::BOOL(0);
    unsafe { wv.IsMuted(&mut muted).is_ok() && muted.as_bool() }
}

fn tab_url(app: &App, id: TabId) -> String {
    app.tabs.get(id).map(|t| t.url.clone()).unwrap_or_default()
}

fn order(app: &App) -> Vec<TabId> {
    app.tabs.ids().collect()
}

fn ids_csv(ids: &[TabId]) -> String {
    ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
}

/// Direct child windows of the main window (wry's host per webview).
fn host_windows(app: &App) -> usize {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GW_CHILD, GW_HWNDNEXT, GetWindow};
    let mut n = 0;
    let mut next = unsafe { GetWindow(HWND(app.window.hwnd() as _), GW_CHILD) };
    while let Ok(child) = next {
        if child.is_invalid() {
            break;
        }
        n += 1;
        next = unsafe { GetWindow(child, GW_HWNDNEXT) };
    }
    n
}

/// Unmuted (silent) audio started by the page's own load-time script.
const AUTOPLAY_HTML: &str = r#"<!doctype html><title>Autoplay</title><script>
window.__ap = 'pending';
const n = 800, b = new ArrayBuffer(44 + n), d = new DataView(b);
const s = (o, t) => { for (let i = 0; i < t.length; i++) d.setUint8(o + i, t.charCodeAt(i)); };
s(0, 'RIFF'); d.setUint32(4, 36 + n, true); s(8, 'WAVE'); s(12, 'fmt ');
d.setUint32(16, 16, true); d.setUint16(20, 1, true); d.setUint16(22, 1, true);
d.setUint32(24, 8000, true); d.setUint32(28, 8000, true); d.setUint16(32, 1, true); d.setUint16(34, 8, true);
s(36, 'data'); d.setUint32(40, n, true);
for (let i = 0; i < n; i++) d.setUint8(44 + i, 128);
const a = new Audio(URL.createObjectURL(new Blob([b], { type: 'audio/wav' })));
a.play().then(() => { window.__ap = 'played'; }, (e) => { window.__ap = e.name; });
</script>"#;
