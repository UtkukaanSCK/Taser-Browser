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
    if !ok {
        FAILS.fetch_add(1, Ordering::Relaxed);
    }
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
    500,  // 32 Escape restores URL; unfocus address
    500,  // 33 Ctrl+T twice
    600,  // 34 address focused; Ctrl+Shift+Tab / Ctrl+Tab cycle
    600,  // 35 Ctrl+L
    500,  // 36 AltGr ignored; close extras; load window.open page
    1200, // 37 click: window.open(file:)
    1500, // 38 check blocked; click: window.open(https:)
    3000, // 39 check new tab
    900,  // 40 load page with hostile title
    1200, // 41 title shown as text; UI navigate-away / window.open attempts
    1500, // 42 check; open 29 more tabs
    1500, // 43 tab strip at 1200px; shrink window
    2000, // 44 tab strip at 820px (UI needs a moment to see the new width); UI tab click
    600,  // 45 check switch; middle-click close
    600,  // 46 check close; close extras; window back to 1200x800
    1200, // 47 check closed down; click UI maximize
    1000, // 48 check maximized; click UI restore
    1000, // 49 check restored; double-click strip
    1000, // 50 check maximized; restore
    1000, // 51 check restored; click UI minimize
    1000, // 52 check minimized; restore window
    1000, // 53 check restored; minimum window size
    1500, // 54 chrome at minimum size; window back to 1200x800
    900,  // 55 last tab close quits
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
/// action sees the state from before it.
fn eval(v: &wry::WebView, js: &str, then: impl Fn(String) + Send + 'static) {
    let _ = v.evaluate_script_with_callback(js, move |r| then(decode(&r)));
}

fn ui_eval(app: &App, js: &str, then: impl Fn(String) + Send + 'static) {
    eval(&app.ui, js, then);
}

fn page_eval(app: &App, js: &str, then: impl Fn(String) + Send + 'static) {
    if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
        eval(v, js, then);
    }
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

/// Closes every tab but the first.
fn close_extra_tabs(app: &mut App) {
    let extra: Vec<TabId> = app.tabs.ids().skip(1).collect();
    for id in extra {
        app.close_tab(id);
    }
}

/// Static facts about the UI strip (layout, chrome, theme).
const UI_STATIC_JS: &str = "(() => { \
  const q = (id) => document.getElementById(id); \
  const rc = (id) => q(id).getBoundingClientRect(); \
  const vis = (id) => { const el = q(id), b = el.getBoundingClientRect(); \
    if (b.width <= 0 || b.height <= 0 || b.left < 0 || b.right > innerWidth || b.top < 0 || b.bottom > innerHeight) return false; \
    const hit = document.elementFromPoint(b.left + b.width / 2, b.top + b.height / 2); \
    return hit === el || el.contains(hit); }; \
  const nt = rc('new-tab'), res = rc('resizer'); \
  const drag = document.elementFromPoint(nt.right + 20, 20); \
  return [`strip_h=${rc('strip').height}`, `doc_h=${document.documentElement.clientHeight}`, \
    `res_h=${res.height}`, `res_top=${res.top}`, `res_w=${res.width}`, `iw=${innerWidth}`, \
    `res_hidden=${q('resizer').hidden}`, `res_n=${q('resizer').children.length}`, \
    `drag_el=${drag && drag.id}`, \
    `chrome=${['back','forward','reload','address','new-tab','min','max','close'].filter((i) => !vis(i)).join(',') || 'ok'}`, \
    `dark=${matchMedia('(prefers-color-scheme: dark)').matches}`, \
    `bg=${getComputedStyle(document.body).backgroundColor}`].join(';'); })()";

/// Tab strip geometry: overflow, per-tab boxes, new-tab and window buttons.
const TAB_STRIP_JS: &str = "(() => { \
  const q = (id) => document.getElementById(id); \
  const strip = q('tabs'), sr = strip.getBoundingClientRect(); \
  const tabs = [...strip.querySelectorAll('[role=tab]')]; \
  const rects = tabs.map((e) => e.getBoundingClientRect()); \
  const inside = (a, b) => a.left >= b.left - 0.5 && a.right <= b.right + 0.5; \
  const vis = (id) => { const el = q(id), b = el.getBoundingClientRect(); \
    if (b.width <= 0 || b.height <= 0 || b.left < 0 || b.right > innerWidth) return false; \
    const hit = document.elementFromPoint(b.left + b.width / 2, b.top + b.height / 2); \
    return hit === el || el.contains(hit); }; \
  const sel = tabs.filter((e) => e.getAttribute('aria-selected') === 'true'); \
  return [`n=${tabs.length}`, `cw=${strip.clientWidth}`, `over=${strip.scrollWidth - strip.clientWidth}`, \
    `minw=${Math.min(...rects.map((r) => r.width)).toFixed(1)}`, \
    `outside=${rects.filter((r) => !inside(r, sr)).length}`, \
    `newtab=${vis('new-tab') && inside(q('new-tab').getBoundingClientRect(), sr)}`, \
    `win=${['min', 'max', 'close'].every(vis)}`, `selected=${sel.length}`, \
    `sel_id=${sel[0] ? sel[0].dataset.id : 'none'}`, `iw=${innerWidth}`].join(';'); })()";

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
  fetch(location.origin + '/app.js').then(() => { r.fetchr = 'allowed'; }, () => { r.fetchr = 'blocked'; }); })()";

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
            let _ = app.ui.evaluate_script_with_callback(
                "JSON.stringify({ reload: document.getElementById('reload').hasAttribute('data-loading'), \
                 bar: document.getElementById('loadbar').hasAttribute('data-on'), \
                 barOpacity: getComputedStyle(document.getElementById('loadbar')).opacity, \
                 stopShown: getComputedStyle(document.querySelector('#reload svg:last-child')).display })",
                |r| {
                    log(&format!("INFO ui-dom {r}"));
                    let ok = r.contains(r#"\"reload\":true"#) && r.contains(r#"\"bar\":true"#);
                    check("ui-shows-loading", ok, &r);
                },
            );
            ui_eval(
                app,
                "(() => { const t = document.querySelector('[role=tab][aria-selected=true]'); \
                 return `tab=${t.hasAttribute('data-loading')};dot=${getComputedStyle(t.querySelector('.dot')).display}`; })()",
                |r| check("ui-tab-shows-loading-dot", field(&r, "tab") == "true" && field(&r, "dot") == "block", &r),
            );
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
                None => log("INFO system theme registry value unreadable; using tao's theme"),
            }
            ui_eval(app, UI_STATIC_JS, move |r| {
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
                "(() => { const t = document.querySelector('[role=tab][aria-selected=true]'); \
                 return `title=${t.querySelector('.title').textContent};addr=${document.getElementById('address').value}`; })()",
                move |r| {
                    check("ui-tab-title-live", field(&r, "title") == "Example Domain", &r);
                    check("ui-address-shows-live-url", field(&r, "addr") == url, format!("{r} want {url}"));
                },
            );
            let dark = system_apps_dark().unwrap_or(app.window.theme() == tao::window::Theme::Dark);
            page_eval(app, "String(matchMedia('(prefers-color-scheme: dark)').matches)", move |r| {
                check("content-prefers-color-scheme-matches-system", r == dark.to_string(), format!("page dark={r} system dark={dark}"));
            });
            ui_js(app, CSP_PROBE_JS);
            page_js(app, CSP_PROBE_JS);
            page_js(app, "window.__m = 1;");
        }
        23 => {
            ui_eval(app, CSP_READ_JS, |r| {
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
            page_eval(app, CSP_READ_JS, |r| {
                log(&format!("INFO csp-page {r}"));
                check(
                    "csp-probe-runs-in-unrestricted-page",
                    field(&r, "attr") == "ran" && field(&r, "script") == "ran" && field(&r, "fetch") == "allowed",
                    &r,
                );
            });
            page_eval(app, "`m=${window.__m};nav=${performance.getEntriesByType('navigation')[0].type}`", |r| {
                check("reload-setup-marker-and-fresh-navigation", field(&r, "m") == "1" && field(&r, "nav") == "navigate", &r);
            });
        }
        24 => {
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("reload-setup-page-idle", !t.loading, "still loading, the button would act as stop");
            ui_js(app, "document.getElementById('reload').click()");
        }
        25 => {
            page_eval(app, "`m=${window.__m};nav=${performance.getEntriesByType('navigation')[0].type}`", |r| {
                check("reload-button-reloads-page", field(&r, "m") == "undefined" && field(&r, "nav") == "reload", &r);
            });
            ui_eval(
                app,
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
            page_eval(app, "`m=${window.__m};nav=${performance.getEntriesByType('navigation')[0].type}`", |r| {
                check("reload-shortcut-reloads-page", field(&r, "m") == "undefined" && field(&r, "nav") == "reload", &r);
            });
            type_in_address(app, "https://example.org/");
        }
        28 => {
            let t = app.tabs.get(app.tabs.active().unwrap()).unwrap();
            check("history-setup-on-example-org", active_url(app).starts_with("https://example.org") && t.can_back, format!("{t:?}"));
            ui_eval(
                app,
                "`back=${document.getElementById('back').disabled};fwd=${document.getElementById('forward').disabled}`",
                |r| check("ui-back-button-enabled-fwd-disabled", field(&r, "back") == "false" && field(&r, "fwd") == "true", &r),
            );
            press(app, vk::LEFT, ALT);
        }
        29 => {
            check("alt-left-goes-back", active_url(app).starts_with("https://example.com"), active_url(app));
            ui_eval(
                app,
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
            ui_eval(app, "document.getElementById('address').value", |r| {
                check("typed-address-survives-url-update", r == "typed-text", r);
            });
            ui_js(
                app,
                "document.getElementById('address').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))",
            );
        }
        32 => {
            let url = active_url(app);
            ui_eval(app, "document.getElementById('address').value", move |r| {
                check("escape-restores-address-url", r == url, format!("address={r} want {url}"));
            });
            ui_js(app, "document.activeElement && document.activeElement.blur()");
        }
        33 => {
            ui_eval(app, "document.activeElement ? document.activeElement.id : 'none'", |r| {
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
            ui_eval(app, "document.activeElement ? document.activeElement.id : 'none'", |r| {
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
            ui_eval(app, "document.activeElement ? document.activeElement.id : 'none'", |r| {
                check("ctrl-l-setup-address-unfocused", r != "address", r);
            });
            check("ctrl-l-maps-to-focus-address", press(app, vk::L, CTRL) == Some(Shortcut::FocusAddress), "no mapping");
            ui_eval(
                app,
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
        }
        36 => {
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
            page_eval(app, "String(document.querySelectorAll('button').length)", |r| {
                check("window-open-setup-page-loaded", r == "6", r);
            });
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                for row in 0..5 {
                    cdp_click(v, 200.0, row_y(row)); // non-web schemes
                }
            }
        }
        38 => {
            if let Some(v) = app.view(recall("t1") as TabId) {
                eval(v, "String(window.__c)", |r| check("window-open-setup-clicks-reached-page", r == "5", r));
            }
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
            if let Some(v) = app.view(t1) {
                eval(v, "String(window.__c)", |r| check("window-open-setup-last-click-reached-page", r == "6", r));
            }
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
                "(() => { const t = document.querySelector('[role=tab][aria-selected=true] .title'); \
                 return `imgs=${document.querySelectorAll('#tabs img').length};pwn=${typeof window.__pwn};text=${t.textContent}`; })()",
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
            ui_eval(app, "`href=${location.href};xb=${typeof window.xb}`", |r| {
                check(
                    "ui-cannot-navigate-away",
                    crate::ui::is_ui_origin(field(&r, "href")) && field(&r, "xb") == "object",
                    &r,
                );
            });
            // Denied popups return null; an allowed one would be a real window
            // (which is not a tab, so the tab count alone would not notice).
            ui_eval(app, "String(window.__w)", |r| check("ui-window-open-denied", r == "null", format!("window.open returned {r}")));
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
            ui_eval(app, TAB_STRIP_JS, move |r| {
                log(&format!("INFO tab-strip-1200 {r}"));
                check("many-tabs-ui-lists-every-tab", num(&r, "n") == n as f64 && num(&r, "selected") == 1.0 && field(&r, "sel_id") == active.to_string(), &r);
                check("many-tabs-no-overflow", num(&r, "over") <= 0.0, &r);
                check("many-tabs-all-inside-strip-with-width", num(&r, "minw") > 0.0 && num(&r, "outside") == 0.0, &r);
                check("many-tabs-new-tab-button-visible", field(&r, "newtab") == "true", &r);
                check("many-tabs-window-buttons-visible", field(&r, "win") == "true", &r);
            });
            app.window.set_inner_size(LogicalSize::new(820.0, 520.0));
        }
        44 => {
            // Measured, not gated: below the default width 30 tabs do not fit
            // (a tab cannot shrink under its padding + border, ~14 px), so the
            // strip overflows and the "+" button is clipped. Reported as a
            // product limitation; the gated check is at the default width above.
            ui_eval(app, TAB_STRIP_JS, |r| {
                let fits = num(&r, "over") <= 0.0 && num(&r, "outside") == 0.0 && field(&r, "newtab") == "true";
                log(&format!("INFO tab-strip-820 fits={fits} {r}"));
                // iw proves the resize reached the UI; otherwise the line above measured 1200 px.
                check("many-tabs-820px-window-buttons-visible", num(&r, "iw") == 820.0 && field(&r, "win") == "true", &r);
            });
            let target = app.tabs.ids().nth(4).unwrap();
            remember("click", target as i64);
            ui_js(
                app,
                &format!(
                    "document.querySelector('[data-id=\"{target}\"]').dispatchEvent(new MouseEvent('mousedown', {{ button: 0, bubbles: true }}))"
                ),
            );
        }
        45 => {
            let target = recall("click") as TabId;
            check("ui-tab-click-activates", app.tabs.active() == Some(target), format!("{:?} want {target}", app.tabs.active()));
            check("ui-tab-click-shows-only-that-view", only_active_visible(app), "visibility mismatch");
            ui_js(
                app,
                &format!(
                    "document.querySelector('[data-id=\"{target}\"]').dispatchEvent(new MouseEvent('auxclick', {{ button: 1, bubbles: true }}))"
                ),
            );
        }
        46 => {
            let target = recall("click") as TabId;
            check(
                "middle-click-closes-tab",
                app.tabs.get(target).is_none() && app.tabs.len() == 29 && app.views.len() == 29,
                format!("{} tabs {} views", app.tabs.len(), app.views.len()),
            );
            close_extra_tabs(app);
            app.window.set_inner_size(LogicalSize::new(1200.0, 800.0));
        }
        47 => {
            check(
                "many-tabs-closed-back-down",
                app.tabs.len() == 1 && app.views.len() == 1 && only_active_visible(app),
                format!("{} tabs {} views", app.tabs.len(), app.views.len()),
            );
            ui_eval(app, TAB_STRIP_JS, |r| {
                check("ui-tab-strip-follows-closes", num(&r, "n") == 1.0 && num(&r, "selected") == 1.0 && num(&r, "over") <= 0.0, &r);
            });
            let size = app.window.inner_size();
            remember("w", size.width as i64);
            remember("h", size.height as i64);
            check("window-starts-unmaximized", !app.window.is_maximized() && !app.window.is_minimized(), "unexpected window state");
            ui_js(app, "document.getElementById('max').click()");
        }
        48 => {
            check("ui-max-button-maximizes", app.window.is_maximized(), "not maximized");
            ui_eval(
                app,
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
        49 => {
            let size = app.window.inner_size();
            check("ui-max-button-restores", !app.window.is_maximized(), "still maximized");
            check(
                "restore-returns-original-size",
                (size.width as i64 - recall("w")).abs() <= 2 && (size.height as i64 - recall("h")).abs() <= 2,
                format!("{}x{} want {}x{}", size.width, size.height, recall("w"), recall("h")),
            );
            ui_eval(
                app,
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
        50 => {
            check("strip-double-click-maximizes", app.window.is_maximized(), "not maximized");
            ui_js(app, "document.getElementById('max').click()");
        }
        51 => {
            check("window-restored-before-minimize", !app.window.is_maximized() && !app.window.is_minimized(), "unexpected window state");
            ui_js(app, "document.getElementById('min').click()");
        }
        52 => {
            check("ui-min-button-minimizes", app.window.is_minimized(), "not minimized");
            app.window.set_minimized(false);
        }
        53 => {
            check("window-restores-from-minimize", !app.window.is_minimized(), "still minimized");
            app.window.set_inner_size(LogicalSize::new(420.0, 240.0));
        }
        54 => {
            let (w, h) = logical_size(app);
            log(&format!("INFO minimum window {w}x{h}"));
            check("min-window-size-enforced", w >= 419.0 && h >= 239.0, format!("{w}x{h}"));
            ui_eval(app, UI_STATIC_JS, |r| {
                log(&format!("INFO ui-static-min {r}"));
                // The rest of the chrome (e.g. the "+" button) is measured, not
                // gated: at the 420 px minimum width it does not fit.
                let hidden: Vec<&str> = field(&r, "chrome").split(',').collect();
                let any = ["min", "max", "close"].iter().any(|id| hidden.contains(id));
                check("min-size-window-buttons-visible", num(&r, "iw") == 420.0 && !any, &r);
            });
            app.window.set_inner_size(LogicalSize::new(1200.0, 800.0));
        }
        55 => {
            app.handle(UserEvent::Shortcut(Shortcut::CloseTab));
            check("last-tab-close-quits", app.quit, "app still running");
            log(&format!("DONE failures={}", FAILS.load(Ordering::Relaxed)));
        }
        _ => {}
    }
}

fn cdp_click(v: &wry::WebView, x: f64, y: f64) {
    use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
    use windows::core::HSTRING;
    let wv = v.webview();
    for kind in ["mousePressed", "mouseReleased"] {
        let params = format!(r#"{{"type":"{kind}","x":{x},"y":{y},"button":"left","clickCount":1}}"#);
        unsafe {
            let _ = wv.CallDevToolsProtocolMethod(
                &HSTRING::from("Input.dispatchMouseEvent"),
                &HSTRING::from(params),
                &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(()))),
            );
        }
    }
}
