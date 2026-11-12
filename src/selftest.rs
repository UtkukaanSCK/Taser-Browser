//! Scripted end-to-end scenario, compiled only with `--features selftest`.
//! Drives the real code paths (UI DOM events -> IPC, shortcut handling,
//! page events) and logs PASS/FAIL lines plus PNG captures of the webviews
//! to `$XB_SELFTEST_DIR` (default: current dir).
//!
//! Not covered here: physical key presses reaching the native
//! AcceleratorKeyPressed hook (needs real input; tested by hand).

use crate::app::{App, UserEvent};
use crate::commands::{Shortcut, UiCommand};
use crate::native::Proxy;
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
    500,  // 20 shortcuts close down to 1, then close last
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
}

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
                app.handle(UserEvent::Shortcut(Shortcut::CloseTab));
            }
            check("ctrl-w-closes", app.tabs.len() == 1 && !app.quit, app.tabs.len());
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
