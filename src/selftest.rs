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
//! by the strip, dragging the top edge to resize, live switching of the
//! system light/dark theme, Ctrl+F itself (the find bar is opened through the
//! Find API instead) and Ctrl+P (the print preview is never opened here).
//! Checks named after a key (zoom step, Esc in fullscreen) test the key's
//! mapping and the app's handling of it, not delivery of a physical key; a
//! page's Keyboard Lock is tested as an API only.
//!
//! Single instance is tested from inside (WM_COPYDATA to its own IPC window,
//! as a second process would send it) and once for real: the scenario starts
//! this same build again against its own profile with an address, and checks
//! that it hands over and exits before any window or test code. The second
//! start bringing the window to the front, the saved session coming back
//! after a restart or a killed process, a saved window place on an unplugged
//! monitor and the icon in the shell are checked by hand.

use crate::app::{App, UserEvent};
use crate::commands::{Mods, Shortcut, UiCommand, vk};
use crate::native::Proxy;
use crate::overlay::Mode;
use crate::page_tools::PageKey;
use crate::policy::{Event, LaunchCmd, Refused};
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
const EXPECTED_CHECKS: u32 = 626;

/// The self-test's folder, the same one `store::data_dir` puts the profile in.
fn dir() -> PathBuf {
    crate::store::selftest_dir()
}

/// Every run starts like a first start: no saved session or window place in
/// the self-test's own profile (`store::data_dir`), whatever the last run left.
pub fn prepare_profile(profile: Option<&std::path::Path>) {
    if reached_in_child("prepare-profile") {
        return;
    }
    let Some(profile) = profile else { return };
    for name in ["session.json", "window.json", "history.jsonl", "bookmarks.json", "settings.json", "extensions.json"] {
        for suffix in ["", ".bak", ".tmp", ".bak.tmp", ".corrupt", ".bak.corrupt"] {
            let _ = std::fs::remove_file(profile.join(format!("{name}{suffix}")));
        }
    }
    plant_extensions(profile);
}

fn log(line: &str) {
    eprintln!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("selftest.log")) {
        let _ = writeln!(f, "{line}");
    }
}

/// A line for finding out why something happened (not a check).
pub fn trace(line: &str) {
    log(&format!("TRACE {line}"));
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
    4000, // 67 check tab survives (a busy machine can take >1.5 s to report the crash); crash the UI renderer
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
    1000, // 88 page tools: find/print/F12 keys, settings; navigate to example.com
    3500, // 89 check load; Keyboard Lock probe; Ctrl+=
    700,  // 90 check one zoom step and the indicator; Keyboard Lock result; same-origin second tab
    3500, // 91 zoom scope (per tab?); indicator follows tabs; navigate to example.org
    3500, // 92 zoom after a cross-origin navigation; Ctrl+0
    700,  // 93 check reset and hidden indicator; Ctrl+NumPad-
    700,  // 94 check zoom out; click the indicator
    700,  // 95 check reset by indicator; CDP Ctrl+wheel
    900,  // 96 note wheel; Ctrl+0; F11
    1500, // 97 check F11 covers the screen, the notice over it
    2600, // 98 check the notice timed out; Esc
    1500, // 99 check restored; maximize
    1000, // 100 check maximized; F11
    1500, // 101 check fullscreen from maximized, notice again; F11
    1500, // 102 check maximized again; restore; navigate to example.com
    3500, // 103 check 1200x800; write the requestFullscreen page
    1000, // 104 CDP click requestFullscreen
    1500, // 105 check element fullscreen and the site notice; Esc
    1500, // 106 check restored; second tab; back to a fresh page
    3500, // 107 write the requestFullscreen page
    1000, // 108 CDP click requestFullscreen again
    1500, // 109 check fullscreen, notice again; other tab fails; tab switch ends it
    1500, // 110 check restored, old tab left fullscreen; a fresh page
    3500, // 111 write the requestFullscreen page
    1000, // 112 CDP click requestFullscreen a third time
    1500, // 113 check fullscreen; this tab's renderer fails
    1500, // 114 check restored; F11
    1500, // 115 tab switch keeps F11 and the notice; F11 off
    1500, // 116 check restored; F11
    1500, // 117 Ctrl+L leaves fullscreen
    1500, // 118 check restored, address focused
    600,  // 119 F11
    1500, // 120 Ctrl+T leaves fullscreen
    1500, // 121 check restored, address focused; close the new tab
    600,  // 122 F11
    800,  // 123 focus the UI webview (the notice band) while the notice is up
    3300, // 124 check the timeout gave the page its keys back
    600,  // 125 F11 off
    600,  // 126 F11 (first entry)
    500,  // 127 F11 off
    500,  // 128 F11 (second entry, ~1 s after the first)
    3000, // 129 check the first entry's timer left the notice up
    1000, // 130 check the second deadline took it down; F11 off
    1500, // 131 open the find bar (Find API)
    1500, // 132 check matches and bar; move the window
    900,  // 133 note bar after move; tab switch
    900,  // 134 note bar on the other tab; switch back
    900,  // 135 note bar back on the tab; stop find
    900,  // 136 check bar closed; close extras
    900,  // 137 save session + window, check files; IPC: a web URL
    1500, // 138 check IPC tab; minimize; IPC: empty payload
    1000, // 139 check window shown; IPC: malformed / non-web payloads
    1500, // 140 check nothing opened; debounced save; restore a prepared session
    3500, // 141 check restored tabs, lazy views, zoom, titles; next tab
    3500, // 142 check the lazy tab loaded; close extras
    900,  // 143 start a second process with an address
    3500, // 144 check it handed over and exited, tab opened; close extras
    900,  // 145 history: visit https://example.com/?h=1
    3500, // 146 visit ?h=2
    3500, // 147 check visits recorded, on disk, bad lines skipped; type a prefix
    2000, // 148 check suggestions (first overlay show), focus stays; ↓
    900,  // 149 check ↓ filled the address; Enter
    3500, // 150 check Enter navigated, list closed; type
    900,  // 151 Esc
    900,  // 152 check Esc closed it; type
    900,  // 153 Ctrl+T closes it; close the tab
    900,  // 154 type
    900,  // 155 navigation closes it
    3500, // 156 type
    900,  // 157 resize
    900,  // 158 check the resize closed it; size back; F11
    900,  // 159 Ctrl+H (leaves F11)
    900,  // 160 check the history list; delete one
    900,  // 161 check delete one; set a cookie; delete all
    900,  // 162 check delete all, cookie set; clear browsing data
    1500, // 163 check cleared; Esc; reload the page
    3500, // 164 check closed, cookie gone
    900,  // 165 bookmarks: star state, Ctrl+D adds (editor opens)
    900,  // 166 editor shown, star filled; rename + Enter
    900,  // 167 rename on disk; Ctrl+D again
    900,  // 168 Remove focused; remove
    900,  // 169 removed, star empty; star click
    900,  // 170 star adds + editor under the star; Esc
    900,  // 171 Esc closed; Ctrl+Shift+O
    900,  // 172 list; filter
    900,  // 173 filter; Ctrl+click
    3000, // 174 new tab; reopen list
    900,  // 175 unfiltered; filter by title
    900,  // 176 filter; click row
    3000, // 177 opened in the tab; reopen list
    900,  // 178 delete a row
    900,  // 179 deleted, on disk; Esc
    900,  // 180 closed; type for suggestions
    900,  // 181 check the marked suggestion; Esc
    900,  // 182 closed; hostile file; Ctrl+D, then the page tries bm-* IPC
    900,  // 183 content page could not reach it; Esc
    900,  // 184 serve the file; Alt+F
    900,  // 185 check the menu, focus; Down
    600,  // 186 check Down; Up twice
    600,  // 187 check wrap; Esc
    900,  // 188 check Esc, focus; Alt+E
    900,  // 189 check Alt+E; zoom in
    900,  // 190 check zoom; zoom reset
    900,  // 191 check reset; Find
    900,  // 192 check Find; the menu button
    900,  // 193 check the button; History
    900,  // 194 check History; Esc
    900,  // 195 check closed; Alt+F
    900,  // 196 check the menu; Bookmarks
    900,  // 197 check the hook; Alt+F
    900,  // 198 check the menu; Settings
    900,  // 199 check the page; change the settings
    900,  // 200 check the changes; content IPC
    900,  // 201 check IPC ignored; Esc; Ctrl+T
    900,  // 202 check closed, new tab; download
    3500, // 203 check the download; Ctrl+J
    1200, // 204 check open; Ctrl+J
    1200, // 205 check closed; menu Downloads
    1200, // 206 check open; Ctrl+J
    1200, // 207 check closed; open Settings
    900,  // 208 defaults back; clear data
    2500, // 209 check; hostile file; Esc
    900,  // 210 check closed; Print
    1500, // 211 check Print; ShowPrintUI
    900,  // 212 close the menu block's tab; policy: recording launcher; open an http page served here
    2000, // 213 check this machine has no mark; open a public http page
    4000, // 214 check "not secure"; open an https page
    2000, // 215 check the lock; ask permission states; put a mailto link on the page
    700,  // 216 check permissions still ask; click the link (a user gesture)
    1500, // 217 check the question; a wrong answer; "open" with nothing waiting
    800,  // 218 check nothing opened; click again
    1200, // 219 the question again; answer "cancel"
    800,  // 220 check nothing opened; click again
    1200, // 221 answer "open"
    800,  // 222 check the recorder got it once; ask and answer "open" at once
    700,  // 223 the strip's page answers "open"
    500,  // 224 check it was not heard; "open" with a launcher that fails
    500,  // 225 check the question says so; a tab switch closes it; blank tab has no mark
    900,  // 226 a page opens mailto by itself (no gesture)
    1500, // 227 check it was dropped unseen; navigate to a file: address
    1000, // 228 check the file never loaded; a certificate error on example.com
    2000, // 229 check the certificate mark; another host
    2000, // 230 check that host is clean; an address with a Unicode host
    2500, // 231 check the address box shows it in ASCII; a page to crash
    2000, // 232 crash its renderer
    1500, // 233 check the crashed mark; reload with its button
    2500, // 234 check the page is back
    900,  // 235 crash again
    1500, // 236 reload with Ctrl+R
    2500, // 237 check the page is back
    900,  // 238 crash a third time
    1500, // 239 go to an address that cannot load
    2500, // 240 check the crashed mark went with the navigation
    900,  // 241 v0.2 fixes: a page on example.org
    2500, // 242 Ctrl+H
    1000, // 243 a jump within the page
    1200, // 244 check the history list stayed up; Esc
    800,  // 245 Ctrl+D: bookmark and editor
    1000, // 246 press and click the star
    900,  // 247 check the editor closed; too many certificate errors
    900,  // 248 the launch question
    1200, // 249 click the ⋮ button from the strip
    1000, // 250 check the menu has the keyboard; Esc
    900,  // 251 extensions: the start's reconcile; UI profile; add v1.1
    2500, // 252 check (a) the id; load the ad page
    2500, // 253 check (b) the block and (c) the UI; disable
    1500, // 254 check disabled; load the ad page
    2500, // 255 check (d) it loads; enable
    1500, // 256 check enabled; load the ad page
    2500, // 257 check (d) blocked again; click ⋮
    1000, // 258 check the menu; Uzantılar
    1000, // 259 check the page and the button; switch off
    1500, // 260 check off; a disabled extension's page in the tab
    1000, // 261 check refused; switch on
    1500, // 262 check on; Seçenekler
    2000, // 263 check the options tab
    1000, // 264 close the options tab; a web page under it; the strip's button
    1000, // 265 check its list; pick the popup
    2000, // 266 check the popup and its tabs.query; window.close()
    1000, // 267 check closed; tab switch
    1500, // 268 the popup again; a frame in it
    1500, // 269 check the frame; click its mail link
    1000, // 270 check the launch refused; window move
    1200, // 271 check closed by the move; the page; Kaldır
    1000, // 272 check the question; Esc; Kaldır, yes
    2500, // 273 check (e) removed; typed address refused; the UI and overlay try an extension page
    1000, // 274 check those refused; an extension X-Browser did not install
    2000, // 275 WebView2 lists it: the reconcile
    1500, // 276 check it is off; the page
    1500, // 277 check its row; switch on
    1500, // 278 check on, not recorded; Kaldır, yes
    2500, // 279 check removed
    600,  // 280 first-run uBOL: offline at the start; a next start, busy
    600,  // 281 check busy waits; another install finishes
    1500, // 282 check done; a start after it; remove uBOL
    600,  // 283 check user_removed; a start after it
    2500, // 284 Web Store: add the extension again; load an ordinary page
    1200, // 285 check the store button is off; the page goes to the sentinel
    900,  // 286 check it was refused; open the remove question
    900,  // 287 check the question; answer "no"
    900,  // 288 check it closed
    300,  // 289 store updates: install 1.0 from the fake store
    2500, // 290 check it; turn it off
    900,  // 291 check it is off; the check offers 1.1
    1500, // 292 check nothing was asked; turn it on
    900,  // 293 check it is on; the check offers 1.1
    3000, // 294 check 1.1 is in; the check offers 1.1 again
    1500, // 295 check the old folder is gone and nothing was fetched; a bad hash
    1500, // 296 check nothing changed; the bad 1.2 again
    1500, // 297 check it was not fetched; in front; open the menu; 1.3 wants more
    1500, // 298 check no question over the menu; Escape
    900,  // 299 check the menu closed; 1.3 again
    1500, // 300 check the question lists only the new; answer "no"
    1500, // 301 check 1.1 stays; in the background; 1.3 again
    900,  // 302 check no question in the background; open the remove question; 1.4
    1500, // 303 check 1.4 waits; answer "no"
    3000, // 304 check 1.4 is in; a timer tick, last check long ago
    1500, // 305 check the tick checked; another tick
    900,  // 306 check it asked nothing
    900,  // 307 last tab close quits
];

pub fn start(proxy: Proxy) {
    if reached_in_child("selftest") {
        return;
    }
    let _ = std::fs::create_dir_all(dir());
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

/// Takes tab `id`'s webview away (hidden first, as `close_tab` does), leaving
/// the tab as one whose webview could not be made.
fn drop_view(app: &mut App, id: TabId) {
    if let Some(i) = app.views.iter().position(|(v, _)| *v == id) {
        let (_, view) = app.views.remove(i);
        let _ = view.set_visible(false);
        drop(view);
    }
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

/// Types `text` into the focused address bar as an edit (an `input` event),
/// which asks for suggestions.
fn suggest_type(app: &App, text: &str) {
    let _ = app.ui.focus();
    type_in_focused_address(app, text);
}

/// `suggest_type` for an address bar that already has focus. No
/// `WebView::focus`: called right after a queued read, it blurred the address
/// (and the window) before that read ran, and the blur puts the page's URL
/// back in the field.
fn type_in_focused_address(app: &App, text: &str) {
    let mut lit = String::new();
    crate::tabs::push_json_str(&mut lit, text);
    ui_js(
        app,
        &format!(
            "(() => {{ const a = document.getElementById('address'); a.focus(); a.value = {lit}; \
             a.dispatchEvent(new Event('input', {{ bubbles: true }})); }})()"
        ),
    );
}

/// A key-down on the address input (as the page sees a real key).
fn address_key(app: &App, key: &str) {
    ui_js(
        app,
        &format!(
            "document.getElementById('address').dispatchEvent(new KeyboardEvent('keydown', {{ key: '{key}', bubbles: true, cancelable: true }}))"
        ),
    );
}

fn overlay_js(app: &App, js: &str) {
    if let Some(v) = app.overlay.view() {
        let _ = v.evaluate_script(js);
    } else {
        check("overlay-exists", false, "no overlay webview");
    }
}

/// The overlay is hidden, as a mode and as a webview.
fn overlay_closed(app: &App) -> bool {
    app.overlay.mode == Mode::Hidden && !app.overlay.view().is_some_and(visible)
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
/// Virtual-key codes of Ctrl+D and Ctrl+Shift+O (bookmarks).
const BM_D: u32 = 0x44;
const BM_O: u32 = 0x4F;

/// The active tab's committed page address, as the star sees it.
fn page_url(app: &App) -> String {
    app.tabs.active().and_then(|id| app.tabs.get(id)).map(|t| crate::history::without_userinfo(&t.page_url)).unwrap_or_default()
}

/// bookmarks.json as parsed from disk.
fn bm_file(app: &App) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(app.bookmarks.path()?).ok()?).ok()
}

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
const MIN_W: f64 = 570.0;

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
            app.window.set_inner_size(LogicalSize::new(880.0, 520.0));
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
            // Page tools. Find, print and DevTools keys are not ours: they reach
            // WebView2 (find bar, print preview) or nothing (F12, DevTools off).
            use crate::page_tools::keys as pk;
            check(
                "find-print-devtools-keys-reach-webview2",
                Shortcut::from_key(pk::F, CTRL).is_none() && Shortcut::from_key(pk::P, CTRL).is_none() && Shortcut::from_key(pk::F12, NONE).is_none(),
                "Ctrl+F, Ctrl+P or F12 is mapped",
            );
            check(
                "esc-is-the-pages-outside-fullscreen",
                !crate::page_tools::esc_exits_fullscreen() && press_live(app, pk::ESCAPE, NONE).is_none(),
                "Esc mapped while not fullscreen",
            );
            let id = app.tabs.active().unwrap();
            let content = settings_of(app.view(id).unwrap());
            let ui = settings_of(&app.ui);
            log(&format!("INFO settings (devtools, browser keys, zoom): content {content:?}, ui {ui:?}"));
            check("content-keeps-browser-keys", content.1, format!("{content:?}"));
            check("devtools-off-in-ui-and-content", !content.0 && !ui.0, format!("content {content:?} ui {ui:?}"));
            check("zoom-control-on-content-not-ui", content.2 && !ui.2, format!("content {content:?} ui {ui:?}"));
            remember("zoom-a", i64::from(id));
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/".into())));
        }
        89 => {
            let a = recall("zoom-a") as TabId;
            check("zoom-test-page-loaded", tab_url(app, a).starts_with("https://example.com/"), tab_url(app, a));
            // A page's Keyboard Lock would take Esc and F11 from the app (and
            // Alt+Tab from the system) in fullscreen; it must not be there.
            page_js(app, KEYBOARD_LOCK_JS);
            let sc = press(app, crate::page_tools::keys::OEM_PLUS, CTRL);
            check("ctrl-plus-is-zoom-in", sc == Some(Shortcut::Page(PageKey::ZoomIn)), format!("{sc:?}"));
        }
        90 => {
            let a = recall("zoom-a") as TabId;
            // The shortcut's handling moves one step (100 -> 110). That a physical
            // key press arrives once (not also as WebView2's own zoom key) is on
            // the manual checklist.
            let native = native_zoom(app, a);
            check(
                "zoom-in-shortcut-moves-one-step",
                app.tabs.get(a).is_some_and(|t| t.zoom == 110) && (native - 1.1).abs() < 0.001,
                format!("native {native}: {}", app.tabs.to_json()),
            );
            page_eval(app, "zoom-scales-page", "`w=${innerWidth}`", |r| {
                check("zoom-scales-page", (num(&r, "w") - 1200.0 / 1.1).abs() <= 2.0, &r);
            });
            page_eval(app, "keyboard-lock-unavailable", "`main=${window.__kl};iframe=${window.__kl2}`", |r| {
                log(&format!("INFO Keyboard Lock in a content tab: {r}"));
                let main = field(&r, "main");
                check("keyboard-lock-unavailable", main == "missing" || main.starts_with("rejected"), &r);
                let borrowed = field(&r, "iframe");
                check("keyboard-lock-not-borrowable-from-iframe", borrowed == "missing" || borrowed.starts_with("rejected"), &r);
            });
            ui_eval(app, "zoom-indicator-shows-level", ZOOM_UI_JS, |r| {
                check(
                    "zoom-indicator-shows-level",
                    field(&r, "hidden") == "false" && field(&r, "text") == "%110" && field(&r, "inside") == "true"
                        && field(&r, "top") == "true" && field(&r, "label").contains("%110"),
                    &r,
                );
            });
            // Per tab or per origin: a second tab on the same origin.
            app.handle(UserEvent::Ui(UiCommand::NewTab));
            remember("zoom-b", i64::from(app.tabs.active().unwrap()));
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?second-tab".into())));
        }
        91 => {
            let (a, b) = (recall("zoom-a") as TabId, recall("zoom-b") as TabId);
            let (za, zb) = (app.tabs.get(a).map_or(0, |t| t.zoom), app.tabs.get(b).map_or(0, |t| t.zoom));
            let nb = native_zoom(app, b);
            log(&format!(
                "INFO zoom scope: tab A (example.com) {za}%, new tab B on the same origin {zb}% (native {nb}), B url {}",
                tab_url(app, b)
            ));
            check("zoom-state-matches-webview2", zb == crate::page_tools::zoom_percent(nb), format!("{zb} vs {nb}"));
            let want = if zb == 100 { "hidden=true".to_string() } else { format!("text=%{zb}") };
            ui_eval(app, "zoom-indicator-follows-new-tab", ZOOM_UI_JS, move |r| {
                check("zoom-indicator-follows-new-tab", r.contains(&want), format!("want {want}: {r}"));
            });
            app.activate(a);
            app.flush();
            ui_eval(app, "zoom-indicator-follows-tab-switch", ZOOM_UI_JS, |r| {
                check("zoom-indicator-follows-tab-switch", field(&r, "hidden") == "false" && field(&r, "text") == "%110", &r);
            });
            // Does a zoom set by the host survive a navigation to another origin?
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.org/".into())));
        }
        92 => {
            let a = recall("zoom-a") as TabId;
            let za = app.tabs.get(a).map_or(0, |t| t.zoom);
            let na = native_zoom(app, a);
            log(&format!("INFO zoom scope: tab A after navigating to {} is at {za}% (native {na})", tab_url(app, a)));
            check(
                "zoom-state-matches-webview2-after-navigation",
                tab_url(app, a).starts_with("https://example.org/") && za == crate::page_tools::zoom_percent(na),
                format!("{za} vs {na} at {}", tab_url(app, a)),
            );
            let sc = press(app, crate::page_tools::keys::ZERO, CTRL);
            check("ctrl-0-is-zoom-reset", sc == Some(Shortcut::Page(PageKey::ZoomReset)), format!("{sc:?}"));
        }
        93 => {
            let a = recall("zoom-a") as TabId;
            let na = native_zoom(app, a);
            check(
                "ctrl-0-resets-zoom",
                app.tabs.get(a).is_some_and(|t| t.zoom == 100) && (na - 1.0).abs() < 0.001,
                format!("native {na}: {}", app.tabs.to_json()),
            );
            ui_eval(app, "zoom-indicator-hidden-at-100", ZOOM_UI_JS, |r| {
                check("zoom-indicator-hidden-at-100", field(&r, "hidden") == "true" && num(&r, "w") == 0.0, &r);
            });
            let sc = press(app, crate::page_tools::keys::SUBTRACT, CTRL);
            check("ctrl-numpad-minus-is-zoom-out", sc == Some(Shortcut::Page(PageKey::ZoomOut)), format!("{sc:?}"));
        }
        94 => {
            let a = recall("zoom-a") as TabId;
            let na = native_zoom(app, a);
            check(
                "zoom-out-one-step",
                app.tabs.get(a).is_some_and(|t| t.zoom == 90) && (na - 0.9).abs() < 0.001,
                format!("native {na}: {}", app.tabs.to_json()),
            );
            ui_eval(app, "zoom-indicator-shows-zoom-out", ZOOM_UI_JS, |r| {
                check("zoom-indicator-shows-zoom-out", field(&r, "hidden") == "false" && field(&r, "text") == "%90", &r);
            });
            // The indicator is the reset button (click -> IPC zoom-reset:<id>).
            ui_js(app, "document.getElementById('zoom').click()");
        }
        95 => {
            let a = recall("zoom-a") as TabId;
            let na = native_zoom(app, a);
            check(
                "zoom-indicator-click-resets",
                app.tabs.get(a).is_some_and(|t| t.zoom == 100) && (na - 1.0).abs() < 0.001,
                format!("native {na}: {}", app.tabs.to_json()),
            );
            ui_eval(app, "zoom-indicator-hidden-after-reset", ZOOM_UI_JS, |r| {
                check("zoom-indicator-hidden-after-reset", field(&r, "hidden") == "true", &r);
            });
            // Tab B has served its purpose (zoom scope); later steps count on two tabs.
            app.close_tab(recall("zoom-b") as TabId, true);
            // Ctrl+wheel is WebView2's own; whether a CDP wheel reaches it is only noted.
            if let Some(v) = app.view(a) {
                cdp(v, "Input.dispatchMouseEvent", r#"{"type":"mouseWheel","x":300,"y":300,"deltaX":0,"deltaY":-120,"modifiers":2}"#);
            }
        }
        96 => {
            let a = recall("zoom-a") as TabId;
            log(&format!("INFO CDP Ctrl+wheel up: tab A at {}% (native {})", app.tabs.get(a).map_or(0, |t| t.zoom), native_zoom(app, a)));
            press(app, crate::page_tools::keys::ZERO, CTRL);
            // F11 from a normal 1200x800 window.
            if let Ok(p) = app.window.outer_position() {
                remember("win-x", i64::from(p.x));
                remember("win-y", i64::from(p.y));
            }
            let sc = press(app, crate::page_tools::keys::F11, NONE);
            check("f11-is-toggle-fullscreen", sc == Some(Shortcut::Page(PageKey::ToggleFullscreen)), format!("{sc:?}"));
            // The strip gives way to the notice band (checked in the next step).
            check(
                "f11-hides-strip",
                app.fullscreen.window && app.window.fullscreen().is_some() && app.fullscreen.notice_up(),
                format!("{:?} ui visible {}", app.fullscreen, visible(&app.ui)),
            );
            // State only: the Esc mapping is armed (a real Esc key is on the manual list).
            check("esc-mapping-armed-in-f11", crate::page_tools::esc_exits_fullscreen(), "Esc not armed");
        }
        97 => {
            check_layout("f11-covers-screen", fullscreen_layout(app));
            page_eval(app, "f11-page-gets-whole-screen", "`w=${innerWidth};h=${innerHeight};sw=${screen.width};sh=${screen.height}`", |r| {
                check(
                    "f11-page-gets-whole-screen",
                    (num(&r, "w") - num(&r, "sw")).abs() <= 1.0 && (num(&r, "h") - num(&r, "sh")).abs() <= 1.0,
                    &r,
                );
            });
            check_notice(app, ["f11-notice-over-page", "f11-notice-shows", "f11-notice-keeps-page-focus"], "", F11_NOTICE);
            capture(app, "16-fullscreen-f11");
        }
        98 => {
            // NOTICE_TIME (3.5 s) after F11: the notice is gone, the strip stays hidden.
            check(
                "f11-notice-times-out",
                !app.fullscreen.notice_up() && !visible(&app.ui) && app.fullscreen.window,
                format!("{:?} ui visible {}", app.fullscreen, visible(&app.ui)),
            );
            ui_eval(app, "f11-notice-hidden-after-timeout", NOTICE_UI_JS, |r| {
                check("f11-notice-hidden-after-timeout", field(&r, "hidden") == "true" && field(&r, "inert") == "false", &r);
            });
            check_layout("f11-covers-screen-after-notice", fullscreen_layout(app));
            // The mapping with the live Esc state, then the app's handling of it.
            let sc = press_live(app, crate::page_tools::keys::ESCAPE, NONE);
            check("esc-mapping-leaves-f11", sc == Some(Shortcut::Page(PageKey::ExitFullscreen)), format!("{sc:?}"));
        }
        99 => {
            check_layout("esc-restores-window", normal_layout(app, false));
            let p = app.window.outer_position().map(|p| (i64::from(p.x), i64::from(p.y))).unwrap_or((-1, -1));
            check(
                "f11-restores-position",
                p == (recall("win-x"), recall("win-y")),
                format!("{p:?} want {}, {}", recall("win-x"), recall("win-y")),
            );
            check(
                "esc-disarmed-after-fullscreen",
                !crate::page_tools::esc_exits_fullscreen() && press_live(app, crate::page_tools::keys::ESCAPE, NONE).is_none(),
                "Esc still mapped",
            );
            app.window.set_maximized(true);
        }
        100 => {
            check("maximized-before-f11", app.window.is_maximized(), "not maximized");
            press(app, crate::page_tools::keys::F11, NONE);
        }
        101 => {
            check_layout("f11-from-maximized-covers-screen", fullscreen_layout(app));
            // Every entry shows the notice again.
            check("f11-notice-again-on-reentry", app.fullscreen.notice_up() && visible(&app.ui), format!("{:?}", app.fullscreen));
            ui_eval(app, "f11-notice-text-on-reentry", NOTICE_UI_JS, |r| {
                check("f11-notice-text-on-reentry", field(&r, "hidden") == "false" && field(&r, "text") == F11_NOTICE, &r);
            });
            press(app, crate::page_tools::keys::F11, NONE);
        }
        102 => {
            check_layout("f11-again-restores-maximized", normal_layout(app, true));
            app.window.set_maximized(false);
            // The requestFullscreen page, on a real origin (the notice names it).
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?fullscreen".into())));
        }
        103 => {
            check_layout("unmaximized-after-fullscreen", normal_layout(app, false));
            write_fullscreen_page(app);
        }
        104 => {
            // A trusted click on the page's requestFullscreen button.
            if let Some(v) = app.view(recall("zoom-a") as TabId) {
                cdp_click(v, 150.0, 100.0);
            }
        }
        105 => {
            let a = recall("zoom-a") as TabId;
            check("page-fullscreen-reported", app.fullscreen.page == Some(a) && !app.fullscreen.window, format!("{:?}", app.fullscreen));
            check_layout("page-fullscreen-covers-screen", fullscreen_layout(app));
            page_eval(app, "page-fullscreen-element-fills-screen", FS_PAGE_JS, |r| {
                check(
                    "page-fullscreen-element-fills-screen",
                    field(&r, "fs") == "stage" && (num(&r, "ew") - num(&r, "sw")).abs() <= 1.0 && (num(&r, "eh") - num(&r, "sh")).abs() <= 1.0,
                    &r,
                );
            });
            check_notice(
                app,
                ["page-notice-over-page", "page-notice-names-site", "page-notice-keeps-page-focus"],
                "example.com",
                PAGE_NOTICE,
            );
            check("esc-mapping-armed-in-page-fullscreen", crate::page_tools::esc_exits_fullscreen(), "Esc left to the page");
            capture(app, "17-fullscreen-page");
            // Esc through the host: it restores the window whatever the page does.
            let sc = press_live(app, crate::page_tools::keys::ESCAPE, NONE);
            check("esc-mapping-leaves-page-fullscreen", sc == Some(Shortcut::Page(PageKey::ExitFullscreen)), format!("{sc:?}"));
        }
        106 => {
            check_layout("page-fullscreen-exit-restores-window", normal_layout(app, false));
            check("page-fullscreen-state-cleared", !app.fullscreen.on() && !app.fullscreen.notice_up(), format!("{:?}", app.fullscreen));
            page_eval(app, "page-left-fullscreen", "`fs=${document.fullscreenElement}`", |r| {
                check("page-left-fullscreen", field(&r, "fs") == "null", &r);
            });
            ui_eval(app, "notice-gone-strip-back", NOTICE_UI_JS, |r| {
                check("notice-gone-strip-back", field(&r, "hidden") == "true" && field(&r, "inert") == "false", &r);
            });
            // A second tab to switch to while the page is fullscreen.
            let a = recall("zoom-a") as TabId;
            app.handle(UserEvent::Ui(UiCommand::NewTab));
            remember("fs-other", i64::from(app.tabs.active().unwrap()));
            app.activate(a);
            app.flush();
            // A fresh document for the second request.
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?fullscreen-2".into())));
        }
        107 => write_fullscreen_page(app),
        108 => {
            if let Some(v) = app.view(recall("zoom-a") as TabId) {
                cdp_click(v, 150.0, 100.0);
            }
        }
        109 => {
            let a = recall("zoom-a") as TabId;
            let other = recall("fs-other") as TabId;
            log(&format!("INFO second fullscreen request: {:?}, active {:?}, tabs {}", app.fullscreen, app.tabs.active(), app.tabs.len()));
            page_eval(app, "second-fullscreen-request-state", "`req=${window.__fs};fs=${document.fullscreenElement && document.fullscreenElement.id};vis=${document.visibilityState};focus=${document.hasFocus()}`", |r| {
                log(&format!("INFO second fullscreen request in page: {r}"));
            });
            check("page-fullscreen-again", app.fullscreen.page == Some(a), format!("{:?}", app.fullscreen));
            check_layout("page-fullscreen-again-covers-screen", fullscreen_layout(app));
            check("page-notice-again-on-reentry", app.fullscreen.notice_up() && visible(&app.ui), format!("{:?}", app.fullscreen));
            ui_eval(app, "page-notice-site-on-reentry", NOTICE_UI_JS, |r| {
                check("page-notice-site-on-reentry", field(&r, "hidden") == "false" && field(&r, "site") == "example.com", &r);
            });
            // Another tab's renderer failing leaves this tab's fullscreen alone.
            app.handle(UserEvent::ProcessFailed { tab: Some(other), failure: crate::app::Failure::RendererExited });
            check("other-tab-failure-keeps-fullscreen", app.fullscreen.page == Some(a), format!("{:?}", app.fullscreen));
            app.handle(UserEvent::Shortcut(Shortcut::NextTab));
            check(
                "tab-switch-ends-page-fullscreen",
                app.tabs.active() == Some(other) && !app.fullscreen.on() && !app.fullscreen.notice_up() && visible(&app.ui),
                format!("{:?} ui visible {}", app.fullscreen, visible(&app.ui)),
            );
        }
        110 => {
            let a = recall("zoom-a") as TabId;
            check_layout("tab-switch-restores-window", normal_layout(app, false));
            check("tab-switch-shows-only-new-tab", only_active_visible(app), "views out of step");
            eval(app.view(a), "old-tab-left-element-fullscreen", "`fs=${document.fullscreenElement}`", |r| {
                check("old-tab-left-element-fullscreen", field(&r, "fs") == "null", &r);
            });
            // A third entry, for a renderer failure of the fullscreen tab itself.
            app.activate(a);
            app.flush();
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?fullscreen-3".into())));
        }
        111 => write_fullscreen_page(app),
        112 => {
            if let Some(v) = app.view(recall("zoom-a") as TabId) {
                cdp_click(v, 150.0, 100.0);
            }
        }
        113 => {
            let a = recall("zoom-a") as TabId;
            check("page-fullscreen-third", app.fullscreen.page == Some(a) && app.fullscreen.notice_up(), format!("{:?}", app.fullscreen));
            // The event WebView2 raises when this tab's renderer dies.
            app.handle(UserEvent::ProcessFailed { tab: Some(a), failure: crate::app::Failure::RendererExited });
            check(
                "failed-tab-gives-up-fullscreen",
                !app.fullscreen.on() && !app.fullscreen.notice_up() && visible(&app.ui) && !crate::page_tools::esc_exits_fullscreen(),
                format!("{:?} ui visible {}", app.fullscreen, visible(&app.ui)),
            );
        }
        114 => {
            check_layout("failed-tab-restores-window", normal_layout(app, false));
            // F11 belongs to the window: it stays across a tab switch.
            press(app, crate::page_tools::keys::F11, NONE);
        }
        115 => {
            app.handle(UserEvent::Shortcut(Shortcut::NextTab));
            check(
                "f11-survives-tab-switch",
                app.tabs.active() == Some(recall("fs-other") as TabId) && app.fullscreen.window,
                format!("{:?}", app.fullscreen),
            );
            check_layout("f11-switched-tab-covers-screen", fullscreen_layout(app));
            // The notice (still up) stays above the newly shown page.
            check_layout("notice-above-switched-tab", notice_on_top(app));
            press(app, crate::page_tools::keys::F11, NONE);
        }
        116 => {
            check_layout("f11-off-after-tab-switch", normal_layout(app, false));
            press(app, crate::page_tools::keys::F11, NONE);
        }
        117 => {
            // Ctrl+L needs the strip: fullscreen gives way.
            let sc = press(app, vk::L, CTRL);
            check(
                "ctrl-l-leaves-fullscreen",
                sc == Some(Shortcut::FocusAddress) && !app.fullscreen.on(),
                format!("{sc:?} {:?}", app.fullscreen),
            );
        }
        118 => {
            check_layout("ctrl-l-restores-window", normal_layout(app, false));
            ui_eval(app, "ctrl-l-focuses-address-after-fullscreen", "`focus=${document.hasFocus()};el=${document.activeElement && document.activeElement.id}`", |r| {
                check("ctrl-l-focuses-address-after-fullscreen", field(&r, "focus") == "true" && field(&r, "el") == "address", &r);
            });
        }
        119 => {
            press(app, crate::page_tools::keys::F11, NONE);
            remember("tabs-before-ctrl-t", app.tabs.len() as i64);
        }
        120 => {
            // Ctrl+T: a new tab whose address bar must be usable.
            let sc = press(app, vk::T, CTRL);
            check(
                "ctrl-t-leaves-fullscreen",
                sc == Some(Shortcut::NewTab) && !app.fullscreen.on() && app.tabs.len() as i64 == recall("tabs-before-ctrl-t") + 1,
                format!("{sc:?} {:?}, {} tabs", app.fullscreen, app.tabs.len()),
            );
        }
        121 => {
            check_layout("ctrl-t-restores-window", normal_layout(app, false));
            ui_eval(app, "ctrl-t-focuses-address-after-fullscreen", "`focus=${document.hasFocus()};el=${document.activeElement && document.activeElement.id}`", |r| {
                check("ctrl-t-focuses-address-after-fullscreen", field(&r, "focus") == "true" && field(&r, "el") == "address", &r);
            });
            if let Some(id) = app.tabs.active() {
                app.close_tab(id, false);
            }
            app.activate(recall("zoom-a") as TabId);
        }
        122 => {
            // N1: a click on the notice band takes the keyboard into the UI
            // webview; the timeout must hand it back to the page.
            press(app, crate::page_tools::keys::F11, NONE);
        }
        123 => {
            // In-app stand-in for that click: focus moved into the UI webview
            // through its controller (no desktop input).
            let _ = app.ui.focus();
            ui_eval(app, "notice-band-focus-moves-to-ui", "`up=${!document.getElementById('fs-notice').hidden};focus=${document.hasFocus()}`", |r| {
                check("notice-band-focus-moves-to-ui", field(&r, "up") == "true" && field(&r, "focus") == "true", &r);
            });
        }
        124 => {
            check(
                "notice-timeout-hides-band",
                app.fullscreen.window && !app.fullscreen.notice_up() && !visible(&app.ui),
                format!("{:?} ui visible {}", app.fullscreen, visible(&app.ui)),
            );
            page_eval(app, "notice-timeout-gives-page-keys-back", "`focus=${document.hasFocus()}`", |r| {
                check("notice-timeout-gives-page-keys-back", field(&r, "focus") == "true", &r);
            });
        }
        125 => {
            press(app, crate::page_tools::keys::F11, NONE);
            check("f11-off-before-stale-timer", !app.fullscreen.on(), format!("{:?}", app.fullscreen));
        }
        126 => {
            // N2: F11 on, off and on again within about 1 s; the first entry's
            // timer must not take the second entry's notice down.
            press(app, crate::page_tools::keys::F11, NONE);
        }
        127 => {
            press(app, crate::page_tools::keys::F11, NONE);
        }
        128 => {
            press(app, crate::page_tools::keys::F11, NONE);
        }
        129 => {
            // About 4 s after the first entry (its deadline passed), 3 s after
            // the second.
            check(
                "stale-notice-timer-ignored",
                app.fullscreen.window && app.fullscreen.notice_up() && visible(&app.ui),
                format!("{:?} ui visible {}", app.fullscreen, visible(&app.ui)),
            );
        }
        130 => {
            // Past the second entry's deadline.
            check(
                "notice-times-out-after-reentry",
                app.fullscreen.window && !app.fullscreen.notice_up() && !visible(&app.ui),
                format!("{:?} ui visible {}", app.fullscreen, visible(&app.ui)),
            );
            press(app, crate::page_tools::keys::F11, NONE);
        }
        131 => {
            // The find bar, opened in-app through WebView2's Find API (the same bar
            // Ctrl+F opens): a window of the browser process, outside our HWND tree.
            BASE_WINDOWS.set(browser_windows(app).into_iter().map(|(h, _)| h).collect());
            log(&format!("INFO browser-process windows before find: {:?}", browser_windows(app)));
            let started = app.view(recall("zoom-a") as TabId).map(|v| {
                // Ctrl+F comes from a focused page.
                let _ = v.focus();
                find_start(v, "needle")
            });
            match started {
                Some(Ok(f)) => FIND.with(|c| *c.borrow_mut() = Some(f)),
                other => check("find-api-starts", false, format!("{other:?}")),
            }
        }
        132 => {
            let hr = FIND_START.load(Ordering::Relaxed);
            let count = FIND.with(|c| {
                c.borrow().as_ref().map_or(-1, |f| {
                    let mut n = -1;
                    unsafe {
                        let _ = f.MatchCount(&mut n);
                    }
                    n
                })
            });
            check("find-api-starts", hr == 0, format!("Start completed with {hr:#x}"));
            check("find-counts-matches", count == 3, format!("{count} matches"));
            let bar = find_bar(app);
            log(&format!("INFO find bar windows: {bar:?}; app window at {:?} size {:?}", app.window.outer_position(), app.window.outer_size()));
            check("find-bar-opens", !bar.is_empty(), "no new browser-process window");
            if let Some((_, r)) = bar.first() {
                remember("bar-x", i64::from(r[0]));
                remember("bar-y", i64::from(r[1]));
            }
            capture(app, "18-find");
            // Window move: does the bar follow?
            if let Ok(p) = app.window.outer_position() {
                app.window.set_outer_position(tao::dpi::PhysicalPosition::new(p.x + 80, p.y + 50));
            }
        }
        133 => {
            let bar = find_bar(app);
            let moved = bar.first().map(|(_, r)| (i64::from(r[0]) - recall("bar-x"), i64::from(r[1]) - recall("bar-y")));
            log(&format!(
                "INFO find bar after the window moved by (80, 50) px to {:?}: {bar:?}, bar moved by {moved:?}",
                app.window.outer_position()
            ));
            app.window.set_outer_position(tao::dpi::PhysicalPosition::new(recall("win-x") as i32, recall("win-y") as i32));
            app.handle(UserEvent::Shortcut(Shortcut::NextTab));
        }
        134 => {
            let bar = find_bar(app);
            log(&format!("INFO find bar after switching to another tab: {bar:?}"));
            app.handle(UserEvent::Shortcut(Shortcut::NextTab));
        }
        135 => {
            let bar = find_bar(app);
            log(&format!("INFO find bar after switching back to the searched tab: {bar:?}"));
            FIND.with(|c| {
                if let Some(f) = c.borrow_mut().take() {
                    unsafe {
                        let _ = f.Stop();
                    }
                }
            });
        }
        136 => {
            let bar = find_bar(app);
            check("find-stop-closes-bar", bar.is_empty(), format!("{bar:?}"));
            close_extra_tabs(app);
            app.window.set_inner_size(LogicalSize::new(1200.0, 800.0));
        }
        137 => {
            // Session + window place: saved, readable, matching the app.
            crate::session::save(app);
            check("store-flushes", crate::store::flush(Duration::from_secs(2)), "writer did not finish in 2 s");
            let (session_path, window_path) = app.persist.paths().unwrap_or_default();
            let read = |p: &std::path::Path| std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
            let want: Vec<String> = crate::session::Session::capture(&app.tabs).tabs.into_iter().map(|t| t.url).collect();
            let s = read(&session_path).unwrap_or_default();
            let got: Vec<String> = s["tabs"].as_array().into_iter().flatten().filter_map(|t| t["url"].as_str().map(str::to_string)).collect();
            check(
                "session-file-saved",
                s["v"] == 1 && !want.is_empty() && got == want,
                format!("{}: {s} (want tabs {want:?})", session_path.display()),
            );
            let w = read(&window_path).unwrap_or_default();
            let (pos, size) = (app.window.outer_position().unwrap_or_default(), app.window.inner_size());
            check(
                "window-file-saved",
                w["v"] == 1
                    && w["x"] == pos.x
                    && w["y"] == pos.y
                    && w["width"] == size.width
                    && w["height"] == size.height
                    && w["maximized"] == false,
                format!("{}: {w} (window at {pos:?}, inner {size:?})", window_path.display()),
            );
            remember("tabs-before-ipc", app.tabs.len() as i64);
            let r = send_ipc(crate::instance::OPEN, &utf16("https://example.com/?ipc=1"));
            check("ipc-accepts-web-url", r == Some(1), format!("{r:?}"));
            // A late copy of the same message (the sender timed out and
            // retried) is taken but must not open a second tab (step 138).
            let r = send_ipc(crate::instance::OPEN, &utf16("https://example.com/?ipc=1"));
            check("ipc-accepts-repeat", r == Some(1), format!("{r:?}"));
        }
        138 => {
            let before = recall("tabs-before-ipc");
            check(
                "ipc-url-opens-new-active-tab",
                app.tabs.len() as i64 == before + 1 && active_url(app).starts_with("https://example.com/?ipc=1"),
                format!("{} tabs (were {before}), active {}", app.tabs.len(), active_url(app)),
            );
            let ipc_tabs = app.tabs.ids().filter(|id| app.tabs.get(*id).is_some_and(|t| t.url.contains("ipc=1"))).count();
            check("ipc-repeat-opens-once", ipc_tabs == 1, format!("{ipc_tabs} tabs with the address"));
            remember("tabs-before-ipc", app.tabs.len() as i64);
            app.window.set_minimized(true);
            let r = send_ipc(crate::instance::OPEN, &[]);
            check("ipc-accepts-empty-payload", r == Some(1), format!("{r:?}"));
        }
        139 => {
            let before = recall("tabs-before-ipc");
            check(
                "ipc-empty-payload-shows-window",
                !app.window.is_minimized() && app.window.is_visible() && app.tabs.len() as i64 == before,
                format!("minimized {}, visible {}, {} tabs (were {before})", app.window.is_minimized(), app.window.is_visible(), app.tabs.len()),
            );
            let open = crate::instance::OPEN;
            let url = utf16("https://example.com/?ipc=bad");
            let cases: [(&'static str, usize, Vec<u8>); 8] = [
                ("ipc-rejects-odd-length", open, url[..3].to_vec()),
                ("ipc-rejects-oversize", open, vec![b'a'; crate::instance::MAX_PAYLOAD + 2]),
                ("ipc-rejects-nul", open, utf16("https://example.com/\0?ipc=bad")),
                ("ipc-rejects-invalid-utf16", open, vec![0x00, 0xD8, b'a', 0]),
                ("ipc-rejects-unknown-kind", crate::instance::PROBE + 1, url.clone()),
                ("ipc-rejects-probe", crate::instance::PROBE, Vec::new()),
                ("ipc-rejects-javascript", open, utf16("javascript:alert(1)")),
                ("ipc-rejects-file", open, utf16("file:///C:/Windows/win.ini")),
            ];
            for (name, kind, bytes) in cases {
                let r = send_ipc(kind, &bytes);
                check(name, r == Some(0), format!("{r:?}"));
            }
            // Once the app is exiting, even a valid address is refused (the
            // sender retries and then starts itself).
            crate::instance::set_quitting(true);
            let r = send_ipc(open, &utf16("https://example.com/?ipc=quitting"));
            crate::instance::set_quitting(false);
            check("ipc-refused-while-quitting", r == Some(0), format!("{r:?}"));
            // A second start finds our IPC window as genuine X-Browser, and
            // uses no window whose process fails the check.
            let (trusted, unverified) =
                crate::store::data_dir().map(|d| crate::instance::probe_find(&d)).unwrap_or_default();
            check("ipc-own-window-verified", trusted == Some(std::process::id()), format!("{trusted:?}"));
            check("ipc-unverified-window-skipped", unverified.is_none(), format!("{unverified:?}"));
            // The UIPI probe a normal start sends an elevated X-Browser's
            // window gets through to one at our own level.
            let blocked = crate::store::data_dir().and_then(|d| crate::instance::probe_blocks_self(&d));
            check("ipc-probe-reaches-own-level", blocked == Some(false), format!("{blocked:?}"));
        }
        140 => {
            let before = recall("tabs-before-ipc");
            check("ipc-rejects-open-nothing", app.tabs.len() as i64 == before, format!("{} tabs (were {before})", app.tabs.len()));
            // The IPC tab (step 138) was never saved explicitly: the debounce did it.
            let (session_path, _) = app.persist.paths().unwrap_or_default();
            let saved = std::fs::read_to_string(&session_path).unwrap_or_default();
            check("session-saves-itself-after-changes", saved.contains("ipc=1"), &saved);

            // A session file as a crash (or another hand) could leave it.
            let src = dir().join("restore-src");
            let r = |i: u32| format!("https://example.com/?r={i}");
            let file = serde_json::json!({
                "v": 1,
                "active": 2,
                "tabs": [
                    { "url": r(0), "title": "Saved R0", "zoom": 100 },
                    { "url": "javascript:alert(1)", "title": "script" },
                    { "url": r(1), "title": "Saved R1", "zoom": 150 },
                    { "url": "file:///C:/Windows/win.ini", "title": "file" },
                    { "url": r(2), "title": format!("Saved R2 {}", "x".repeat(600)), "zoom": 9000 },
                    { "url": format!("https://example.com/?{}", "a".repeat(9000)), "title": "oversize" },
                    { "url": "about:blank", "title": "blank" },
                ],
            });
            let _ = std::fs::create_dir_all(&src);
            let _ = std::fs::write(src.join("session.json"), file.to_string());
            let loaded = crate::store::Store::<crate::session::Session>::new(&src).load();
            let s = loaded.value;
            let urls: Vec<&str> = s.tabs.iter().map(|t| t.url.as_str()).collect();
            check(
                "restore-file-filtered",
                loaded.source == crate::store::Source::File
                    && urls == [r(0), r(1), r(2)]
                    && s.active == 1
                    && s.tabs[1].zoom == 150
                    && s.tabs[2].zoom == 100
                    && s.tabs[2].title.chars().count() == crate::session::MAX_TITLE,
                format!("{:?} {urls:?} active {}", loaded.source, s.active),
            );
            let old: Vec<TabId> = app.tabs.ids().collect();
            let active = crate::session::restore(app, &s);
            for id in old {
                app.close_tab(id, false);
            }
            let ids: Vec<TabId> = app.tabs.ids().collect();
            let got: Vec<String> = ids.iter().map(|&id| tab_url(app, id)).collect();
            check(
                "restore-order-and-active",
                got == [r(0), r(1), r(2)] && active.is_some() && app.tabs.active() == active && active == ids.get(1).copied(),
                format!("{got:?}, active {:?} of {ids:?}", app.tabs.active()),
            );
            let with_view: Vec<TabId> = app.views.iter().map(|(id, _)| *id).collect();
            check("restore-only-active-gets-view", active.is_some() && with_view == [active.unwrap()], format!("views {with_view:?}, tabs {ids:?}"));
            for (i, id) in ids.iter().enumerate().take(3) {
                remember(["restored-0", "restored-1", "restored-2"][i], i64::from(*id));
            }
        }
        141 => {
            let ids = [recall("restored-0"), recall("restored-1"), recall("restored-2")].map(|v| v as TabId);
            let r1 = app.tabs.get(ids[1]);
            check(
                "restored-active-tab-loads",
                r1.is_some_and(|t| t.url.starts_with("https://example.com/?r=1") && !t.loading),
                format!("{r1:?}"),
            );
            let z = native_zoom(app, ids[1]);
            check("restored-zoom-applied", (z - 1.5).abs() < 0.01, format!("native zoom {z}"));
            ui_eval(
                app,
                "restored-lazy-tabs-show-titles",
                "[...document.querySelectorAll('#tablist [role=tab] .title')].map(e => e.textContent).join('|')",
                |r| {
                    let t: Vec<&str> = r.split('|').collect();
                    check(
                        "restored-lazy-tabs-show-titles",
                        t.len() == 3 && t[0] == "Saved R0" && t[2].starts_with("Saved R2 xxx"),
                        format!("{:?}", r.chars().take(120).collect::<String>()),
                    );
                },
            );
            app.handle(UserEvent::Shortcut(Shortcut::NextTab));
            check(
                "lazy-tab-gets-view-on-activate",
                app.tabs.active() == Some(ids[2]) && app.view(ids[2]).is_some() && app.views.len() == 2,
                format!("active {:?}, views {:?}", app.tabs.active(), app.views.iter().map(|(id, _)| *id).collect::<Vec<_>>()),
            );
        }
        142 => {
            let id = recall("restored-2") as TabId;
            let t = app.tabs.get(id);
            check(
                "lazy-tab-loads-on-activate",
                t.is_some_and(|t| t.url.starts_with("https://example.com/?r=2") && !t.loading) && only_active_visible(app),
                format!("{t:?}"),
            );
            capture(app, "19-restored");
            // A tab whose webview is gone (as when it could not be made)
            // gets one again from reload, and from navigate.
            if let Some(id) = app.tabs.active() {
                drop_view(app, id);
                app.handle(UserEvent::Shortcut(Shortcut::Reload));
                let revived = app.view(id).is_some_and(visible);
                check("reload-recreates-missing-view", revived, "no visible webview after reload");
                drop_view(app, id);
                app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?revived=1".into())));
                let revived = app.view(id).is_some_and(visible);
                check(
                    "navigate-recreates-missing-view",
                    revived && active_url(app).contains("revived=1"),
                    format!("visible {revived}, active {}", active_url(app)),
                );
            }
            close_extra_tabs(app);
        }
        143 => {
            // A real second start: this same build, same self-test profile,
            // with an address. It must hand over and exit, never run.
            remember("tabs-before-child", app.tabs.len() as i64);
            remember("windows-before-child", windows_of(std::process::id()).len() as i64);
            let spawned = spawn_second_start("https://example.com/?second=1");
            check("second-start-spawned", spawned.is_ok(), spawned.err().unwrap_or_default());
        }
        144 => {
            // Never wait here: the event loop must stay free. A child still
            // running fails the check; its watchdog (detached) kills it later.
            let outcome = SECOND_START.take().map(|h| {
                if h.is_finished() {
                    h.join().unwrap_or_else(|_| Err("watchdog panicked".into()))
                } else {
                    Err("child still running".into())
                }
            });
            let err = std::fs::read_to_string(dir().join("child.err")).unwrap_or_default();
            check(
                "second-start-hands-over-and-exits",
                matches!(outcome, Some(Ok(0))),
                format!("{outcome:?}, stderr {err:?}"),
            );
            let reached: Vec<String> = std::fs::read_dir(dir())
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("child-reached-"))
                .collect();
            check("second-start-runs-no-selftest", reached.is_empty(), format!("{reached:?}"));
            let before = recall("tabs-before-child");
            check(
                "second-start-url-opens-tab",
                app.tabs.len() as i64 == before + 1 && active_url(app).starts_with("https://example.com/?second=1"),
                format!("{} tabs (were {before}), active {}", app.tabs.len(), active_url(app)),
            );
            let (now, was) = (windows_of(std::process::id()).len() as i64, recall("windows-before-child"));
            check("second-start-opens-no-window", now == was, format!("{now} top-level windows (were {was})"));
            close_extra_tabs(app);
        }
        145 => {
            // History and suggestions (task 5). Two visits of our own first.
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?h=1".into())));
        }
        146 => {
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?h=2".into())));
        }
        147 => {
            let (rows, _) = app.history.list("example.com/?h=");
            let has = |q: &str| rows.iter().any(|e| e.u.starts_with(q) && e.t == "Example Domain");
            check(
                "history-visit-recorded",
                has("https://example.com/?h=1") && has("https://example.com/?h=2"),
                format!("{:?}", rows.iter().map(|e| (&e.u, &e.t)).collect::<Vec<_>>()),
            );
            let flushed = app.history.flush_now(Duration::from_secs(2));
            let file = app.history.reload();
            let kept = |q: &str| file.iter().any(|v| v.u.starts_with(q) && v.ts > 0);
            check(
                "history-survives-reload",
                flushed && kept("https://example.com/?h=1") && kept("https://example.com/?h=2"),
                format!("flushed {flushed}, {} visits in {:?}", file.len(), app.history.path()),
            );
            // Typed in the address bar: WebView2 calls it host-initiated, but
            // it is the user's, so it counts toward frequency.
            let typed: Vec<u8> = file.iter().filter(|v| v.u.starts_with("https://example.com/?h=")).map(|v| v.k).collect();
            check(
                "history-typed-visit-counts",
                !typed.is_empty() && typed.iter().all(|&k| k == crate::history::VISIT_USER),
                format!("kinds {typed:?}"),
            );
            let bad = dir().join("history-bad");
            let _ = std::fs::create_dir_all(&bad);
            let lines = "{\"u\":\"https://example.com/good\",\"t\":\"Good\",\"ts\":1700000000}\n\
                         not json at all\n\
                         {\"u\":\"javascript:alert(1)\",\"t\":\"x\",\"ts\":1700000000}\n\
                         {\"u\":\"http://xb.ui/index.html\",\"t\":\"x\",\"ts\":1700000000}\n\
                         {\"u\":\"https://example.com/torn\",\"t\":\"To";
            let written = std::fs::write(bad.join("history.jsonl"), lines);
            let (visits, skipped) = crate::history::load_file(&bad.join("history.jsonl"));
            check(
                "history-bad-lines-skipped",
                written.is_ok() && visits.len() == 1 && visits[0].u == "https://example.com/good" && skipped >= 3,
                format!("{written:?}, kept {:?}, skipped {skipped}", visits.iter().map(|v| &v.u).collect::<Vec<_>>()),
            );
            suggest_type(app, "example.com/?h=1");
        }
        148 => {
            let o = &app.overlay;
            let shown = o.mode == Mode::Suggest && o.view().is_some_and(visible);
            check("suggest-shows-for-prefix", shown, format!("mode {:?}", o.mode));
            match o.first_show {
                Some(t) => log(&format!("INFO overlay first show {} ms (build to visible)", t.as_millis())),
                None => check("overlay-first-show-timed", false, "no first show"),
            }
            if let Some(v) = o.view() {
                let (x, y, w, h) = bounds_of(app, v);
                check(
                    "suggest-sits-under-address",
                    (y - (crate::app::UI_HEIGHT - 4.0)).abs() < 1.5 && (h - 40.0).abs() < 1.5 && w >= 359.0 && x >= 0.0,
                    format!("({x},{y},{w},{h})"),
                );
                let s = settings_of(v);
                check("overlay-no-devtools-keys-zoom", s == (false, false, false), format!("{s:?}"));
            }
            eval(
                app.overlay.view(),
                "suggest-rows-and-no-focus",
                "(() => { const s = document.querySelector('#suggest li span'); \
                 return `rows=${document.querySelectorAll('#suggest li').length};focus=${document.hasFocus()};first=${s ? s.textContent : ''}`; })()",
                |r| {
                    check(
                        "suggest-rows-and-no-focus",
                        field(&r, "rows") == "1" && field(&r, "focus") == "false" && field(&r, "first").starts_with("https://example.com/?h=1"),
                        &r,
                    );
                },
            );
            ui_eval(app, "suggest-address-keeps-focus", "`el=${document.activeElement.id};focus=${document.hasFocus()}`", |r| {
                check("suggest-address-keeps-focus", field(&r, "el") == "address" && field(&r, "focus") == "true", &r);
            });
            address_key(app, "ArrowDown");
        }
        149 => {
            ui_eval(app, "suggest-down-fills-address", "document.getElementById('address').value", |r| {
                check("suggest-down-fills-address", r.starts_with("https://example.com/?h=1"), &r);
            });
            eval(app.overlay.view(), "suggest-down-marks-row", "`${document.querySelector('#suggest li').hasAttribute('data-sel')}`", |r| {
                check("suggest-down-marks-row", r == "true", &r);
            });
            address_key(app, "Enter");
        }
        150 => {
            check("suggest-enter-navigates", active_url(app).starts_with("https://example.com/?h=1"), active_url(app));
            check("suggest-closed-after-pick", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            suggest_type(app, "example");
        }
        151 => {
            check("suggest-open-for-esc", app.overlay.mode == Mode::Suggest, format!("mode {:?}", app.overlay.mode));
            address_key(app, "Escape");
        }
        152 => {
            check("suggest-esc-closes", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            ui_eval(
                app,
                "suggest-esc-keeps-text-and-focus",
                "`v=${document.getElementById('address').value};el=${document.activeElement.id};focus=${document.hasFocus()}`",
                |r| {
                    check(
                        "suggest-esc-keeps-text-and-focus",
                        field(&r, "v") == "example" && field(&r, "el") == "address" && field(&r, "focus") == "true",
                        &r,
                    );
                },
            );
            type_in_focused_address(app, "example");
        }
        153 => {
            check("suggest-open-for-tab-switch", app.overlay.mode == Mode::Suggest, format!("mode {:?}", app.overlay.mode));
            press(app, vk::T, CTRL);
            check("suggest-closes-on-tab-switch", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            close_extra_tabs(app);
        }
        154 => {
            // Typed a step later: the closed tab's focus moves settle first
            // (focus leaving the address closes the list, as it should).
            suggest_type(app, "example");
        }
        155 => {
            check("suggest-open-for-navigation", app.overlay.mode == Mode::Suggest, format!("mode {:?}", app.overlay.mode));
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?h=3".into())));
            check("suggest-closes-on-navigation", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
        }
        156 => {
            suggest_type(app, "example");
        }
        157 => {
            check("suggest-open-for-resize", app.overlay.mode == Mode::Suggest, format!("mode {:?}", app.overlay.mode));
            let (w, h) = logical_size(app);
            remember("pre-resize-w", w as i64);
            remember("pre-resize-h", h as i64);
            app.window.set_inner_size(LogicalSize::new(w - 100.0, h));
        }
        158 => {
            check("suggest-closes-on-resize", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            let (w, h) = (recall("pre-resize-w") as f64, recall("pre-resize-h") as f64);
            app.window.set_inner_size(LogicalSize::new(w, h));
            press(app, crate::page_tools::keys::F11, NONE);
        }
        159 => {
            // From F11: leaving fullscreen resizes and moves the window, and
            // those events (delivered after this handler) must not close it.
            press(app, vk::H, CTRL);
        }
        160 => {
            let o = &app.overlay;
            check(
                "ctrl-h-leaves-fullscreen-and-stays-open",
                !app.fullscreen.on() && app.window.fullscreen().is_none() && o.mode == Mode::History,
                format!("{:?} mode {:?}", app.fullscreen, o.mode),
            );
            check("ctrl-h-opens-history", o.mode == Mode::History && o.view().is_some_and(visible), format!("mode {:?}", o.mode));
            if let Some(v) = o.view() {
                let (x, y, w, h) = bounds_of(app, v);
                let (ww, _) = logical_size(app);
                check(
                    "history-panel-place",
                    (y - (crate::app::UI_HEIGHT + 4.0)).abs() < 1.5 && (x + w - (ww - 8.0)).abs() < 1.5 && h > 100.0,
                    format!("({x},{y},{w},{h}) in width {ww}"),
                );
            }
            eval(
                app.overlay.view(),
                "history-list-shown-and-focused",
                "`rows=${document.querySelectorAll('#h-list li').length};el=${document.activeElement.id};focus=${document.hasFocus()}`",
                |r| {
                    check(
                        "history-list-shown-and-focused",
                        num(&r, "rows") >= 3.0 && field(&r, "el") == "h-search" && field(&r, "focus") == "true",
                        &r,
                    );
                },
            );
            remember("history-pages", app.history.pages() as i64);
            // × arms the delete, the second click deletes the newest address.
            overlay_js(app, "(() => { const b = document.querySelector('#h-list li button:last-child'); b.click(); b.click(); })()");
        }
        161 => {
            let (pages, was) = (app.history.pages() as i64, recall("history-pages"));
            check(
                "history-delete-one",
                pages == was - 1 && app.overlay.mode == Mode::History,
                format!("{pages} pages (were {was}), mode {:?}", app.overlay.mode),
            );
            page_js(app, "document.cookie = 'xbtest=1; max-age=3600; path=/'");
            remember("closed-before-delete", app.closed.len() as i64);
            overlay_js(app, "document.getElementById('h-delete-all').click(); document.getElementById('h-yes').click();");
        }
        162 => {
            let flushed = app.history.flush_now(Duration::from_secs(2));
            let file = app.history.reload();
            check(
                "history-delete-all",
                flushed && app.history.pages() == 0 && file.is_empty(),
                format!("flushed {flushed}, {} pages, {} visits on disk", app.history.pages(), file.len()),
            );
            let was = recall("closed-before-delete");
            check(
                "history-delete-all-forgets-closed-tabs",
                was > 0 && app.closed.len() == 0,
                format!("{} closed tabs remembered (were {was})", app.closed.len()),
            );
            page_eval(app, "clear-data-cookie-set-first", "document.cookie", |r| {
                check("clear-data-cookie-set-first", r.contains("xbtest=1"), &r);
            });
            app.history.record("https://example.com/?dummy=1", "dummy", true);
            overlay_js(app, "document.getElementById('h-clear').click(); document.getElementById('h-yes').click();");
        }
        163 => {
            let flushed = app.history.flush_now(Duration::from_secs(2));
            let file = app.history.reload();
            check(
                "clear-data-succeeds-and-empties-history",
                app.overlay.cleared == Some(true) && flushed && app.history.pages() == 0 && file.is_empty(),
                format!(
                    "cleared {:?}, {} pages, {} on disk, mode {:?}, foreground {}",
                    app.overlay.cleared,
                    app.history.pages(),
                    file.len(),
                    app.overlay.mode,
                    crate::native::is_foreground(&app.window)
                ),
            );
            overlay_js(app, "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))");
            page_js(app, "location.reload()");
        }
        164 => {
            check("history-esc-closes", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            page_eval(app, "clear-data-cookie-gone", "document.cookie", |r| {
                check("clear-data-cookie-gone", !r.contains("xbtest"), &r);
            });
            // Who started a navigation, as WebView2 reports it. A real script
            // navigation here would not do: the probe's SendInput keys give
            // the page user activation, which WebView2 reports as the user's.
            use crate::app::PageEvent;
            use crate::history::{VISIT_AUTO, VISIT_USER};
            if let Some(id) = app.tabs.active() {
                let (script, redirected) = (u64::MAX - 2, u64::MAX - 1);
                let visit = |nav, url: &str| PageEvent::Visited { url: url.into(), title: "T".into(), nav };
                app.handle(UserEvent::Page(id, PageEvent::Navigating { nav: script, user: false }));
                app.handle(UserEvent::Page(id, visit(script, "https://example.com/?by=script")));
                // A link click whose redirect starts again under the same id.
                app.handle(UserEvent::Page(id, PageEvent::Navigating { nav: redirected, user: true }));
                app.handle(UserEvent::Page(id, PageEvent::Navigating { nav: redirected, user: false }));
                app.handle(UserEvent::Page(id, visit(redirected, "https://example.com/?by=click")));
            }
            let flushed = app.history.flush_now(Duration::from_secs(2));
            let file = app.history.reload();
            let kind = |q: &str| file.iter().find(|v| v.u.ends_with(q)).map(|v| v.k);
            check(
                "history-who-started-decides-count",
                flushed && kind("?by=script") == Some(VISIT_AUTO) && kind("?by=click") == Some(VISIT_USER),
                format!("flushed {flushed}, script {:?}, click {:?}", kind("?by=script"), kind("?by=click")),
            );
        }
        // ---- bookmarks (task 6): one contiguous block ----
        165 => {
            app.bookmarks.ensure_loaded();
            check("bm-file-starts-empty", app.bookmarks.items().is_empty(), format!("{:?}", app.bookmarks.items()));
            ui_eval(
                app,
                "bm-star-enabled-and-labelled",
                "(() => { const b = document.getElementById('bm-star'); \
                 return `disabled=${b.disabled};pressed=${b.getAttribute('aria-pressed')};label=${b.getAttribute('aria-label')};tab=${b.tabIndex}`; })()",
                |r| {
                    check(
                        "bm-star-enabled-and-labelled",
                        field(&r, "disabled") == "false"
                            && field(&r, "pressed") == "false"
                            && field(&r, "label") == "Yer imi"
                            && field(&r, "tab") == "0",
                        &r,
                    );
                },
            );
            check("bm-altgr-d-is-no-shortcut", Shortcut::from_key(BM_D, ALTGR).is_none(), "AltGr+D mapped");
            let page = page_url(app);
            press(app, BM_D, CTRL);
            check("bm-ctrl-d-adds-the-page", app.bookmarks.contains(&page), format!("{page} in {:?}", app.bookmarks.items()));
            check("bm-ctrl-d-opens-editor", app.overlay.mode == Mode::Bookmarks, format!("mode {:?}", app.overlay.mode));
        }
        166 => {
            let o = &app.overlay;
            check("bm-editor-visible", o.view().is_some_and(visible), format!("mode {:?}", o.mode));
            eval(
                app.overlay.view(),
                "bm-editor-shown-and-focused",
                "(() => { const t = document.getElementById('bm-title'); \
                 return `shown=${!document.getElementById('bm-edit').hidden};list=${!document.getElementById('bm-list').hidden};\
                 el=${document.activeElement.id};focus=${document.hasFocus()};added=${document.getElementById('bm-heading').textContent.includes('eklendi')}`; })()",
                |r| {
                    check(
                        "bm-editor-shown-and-focused",
                        field(&r, "shown") == "true"
                            && field(&r, "list") == "false"
                            && field(&r, "el") == "bm-title"
                            && field(&r, "focus") == "true"
                            && field(&r, "added") == "true",
                        &r,
                    );
                },
            );
            ui_eval(app, "bm-star-filled-after-add", "document.getElementById('bm-star').getAttribute('aria-pressed')", |r| {
                check("bm-star-filled-after-add", r == "true", &r);
            });
            // Enter in the title field saves and closes.
            overlay_js(
                app,
                "(() => { const t = document.getElementById('bm-title'); t.value = 'Zebra Corner'; \
                 t.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })); })()",
            );
        }
        167 => {
            let page = page_url(app);
            let title = app.bookmarks.items().iter().find(|b| b.url == page).map(|b| b.title.clone());
            check(
                "bm-editor-enter-renames-and-closes",
                overlay_closed(app) && title.as_deref() == Some("Zebra Corner"),
                format!("title {title:?}, mode {:?}", app.overlay.mode),
            );
            let flushed = crate::store::flush(Duration::from_secs(5));
            let on_disk = bm_file(app);
            let titled = on_disk
                .as_ref()
                .and_then(|v| v["items"].as_array())
                .is_some_and(|a| a.iter().any(|i| i["url"] == page.as_str() && i["title"] == "Zebra Corner" && i["added"].is_u64()));
            check(
                "bm-rename-persists-in-file",
                flushed && titled && on_disk.as_ref().is_some_and(|v| v["v"] == 1),
                format!("flushed {flushed}, {on_disk:?}"),
            );
            press(app, BM_D, CTRL);
        }
        168 => {
            check("bm-ctrl-d-on-bookmarked-opens-editor", app.overlay.mode == Mode::Bookmarks, format!("mode {:?}", app.overlay.mode));
            eval(
                app.overlay.view(),
                "bm-ctrl-d-focuses-remove",
                "`el=${document.activeElement.id};edit=${document.getElementById('bm-heading').textContent.includes('imini')};title=${document.getElementById('bm-title').value}`",
                |r| {
                    check(
                        "bm-ctrl-d-focuses-remove",
                        field(&r, "el") == "bm-remove" && field(&r, "edit") == "true" && field(&r, "title") == "Zebra Corner",
                        &r,
                    );
                },
            );
            // Enter on the focused button is a click on it.
            overlay_js(app, "document.getElementById('bm-remove').click()");
        }
        169 => {
            let page = page_url(app);
            check("bm-remove-in-editor", !app.bookmarks.contains(&page) && overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            ui_eval(app, "bm-star-empty-after-remove", "document.getElementById('bm-star').getAttribute('aria-pressed')", |r| {
                check("bm-star-empty-after-remove", r == "false", &r);
            });
            ui_js(app, "document.getElementById('bm-star').click()");
        }
        170 => {
            let page = page_url(app);
            check(
                "bm-star-click-adds-and-opens-editor",
                app.bookmarks.contains(&page) && app.overlay.mode == Mode::Bookmarks,
                format!("mode {:?}", app.overlay.mode),
            );
            if let Some(v) = app.overlay.view() {
                let (x, y, w, h) = bounds_of(app, v);
                check(
                    "bm-editor-hangs-under-star",
                    (y - (crate::app::UI_HEIGHT - 2.0)).abs() < 1.5 && w > 200.0 && h > 100.0,
                    format!("({x},{y},{w},{h})"),
                );
                let right = x + w;
                ui_eval(app, "bm-editor-right-edge-at-star", "Math.round(document.getElementById('bm-star').getBoundingClientRect().right)", move |r| {
                    let star: f64 = r.parse().unwrap_or(f64::NAN);
                    check("bm-editor-right-edge-at-star", (star - right).abs() < 2.0, format!("star right {r}, editor right {right}"));
                });
            }
            app.bookmarks.add("https://example.com/?bm=zebra", "Zebra crossing");
            app.bookmarks.add("https://example.com/?bm=alpha", "Alpha page");
            overlay_js(app, "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))");
        }
        171 => {
            check("bm-esc-closes-editor", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            press(app, BM_O, CTRL_SHIFT);
        }
        172 => {
            let o = &app.overlay;
            check("bm-ctrl-shift-o-opens-list", o.mode == Mode::Bookmarks && o.view().is_some_and(visible), format!("mode {:?}", o.mode));
            if let Some(v) = o.view() {
                let (x, y, w, h) = bounds_of(app, v);
                let (ww, _) = logical_size(app);
                check(
                    "bm-list-panel-place",
                    (y - (crate::app::UI_HEIGHT + 4.0)).abs() < 1.5 && (x + w - (ww - 8.0)).abs() < 1.5 && h > 100.0,
                    format!("({x},{y},{w},{h}) in width {ww}"),
                );
            }
            eval(
                app.overlay.view(),
                "bm-list-shown-and-focused",
                "`rows=${document.querySelectorAll('#bm-rows li').length};el=${document.activeElement.id};focus=${document.hasFocus()};edit=${document.getElementById('bm-edit').hidden}`",
                |r| {
                    check(
                        "bm-list-shown-and-focused",
                        num(&r, "rows") == 3.0 && field(&r, "el") == "bm-search" && field(&r, "focus") == "true" && field(&r, "edit") == "true",
                        &r,
                    );
                },
            );
            overlay_js(
                app,
                "(() => { const s = document.getElementById('bm-search'); s.value = 'zebra'; \
                 s.dispatchEvent(new Event('input', { bubbles: true })); })()",
            );
        }
        173 => {
            eval(
                app.overlay.view(),
                "bm-list-filter",
                "(() => { const s = document.querySelector('#bm-rows li button span'); \
                 return `rows=${document.querySelectorAll('#bm-rows li').length};first=${s ? s.textContent : ''};q=${document.getElementById('bm-search').value}`; })()",
                |r| {
                    check("bm-list-filter", num(&r, "rows") == 1.0 && field(&r, "first") == "Zebra crossing" && field(&r, "q") == "zebra", &r);
                },
            );
            remember("bm-tabs", app.tabs.len() as i64);
            // Ctrl+click: a new tab.
            overlay_js(
                app,
                "document.querySelector('#bm-rows li button').dispatchEvent(new MouseEvent('click', { bubbles: true, ctrlKey: true }))",
            );
        }
        174 => {
            check(
                "bm-list-ctrl-click-opens-new-tab",
                app.tabs.len() as i64 == recall("bm-tabs") + 1 && active_url(app).starts_with("https://example.com/?bm=zebra") && overlay_closed(app),
                format!("{} tabs (were {}), {}, mode {:?}", app.tabs.len(), recall("bm-tabs"), active_url(app), app.overlay.mode),
            );
            press(app, BM_O, CTRL_SHIFT);
        }
        175 => {
            eval(app.overlay.view(), "bm-list-reopens-unfiltered", "`rows=${document.querySelectorAll('#bm-rows li').length};q=${document.getElementById('bm-search').value}`", |r| {
                check("bm-list-reopens-unfiltered", num(&r, "rows") == 3.0 && field(&r, "q").is_empty(), &r);
            });
            overlay_js(
                app,
                "(() => { const s = document.getElementById('bm-search'); s.value = 'alpha'; \
                 s.dispatchEvent(new Event('input', { bubbles: true })); })()",
            );
        }
        176 => {
            eval(app.overlay.view(), "bm-list-filter-by-title", "`rows=${document.querySelectorAll('#bm-rows li').length}`", |r| {
                check("bm-list-filter-by-title", r == "rows=1", &r);
            });
            // A plain click opens it in the active tab.
            overlay_js(app, "document.querySelector('#bm-rows li button').click()");
        }
        177 => {
            check(
                "bm-list-click-opens-in-active-tab",
                app.tabs.len() as i64 == recall("bm-tabs") + 1 && active_url(app).starts_with("https://example.com/?bm=alpha") && overlay_closed(app),
                format!("{} tabs, {}, mode {:?}", app.tabs.len(), active_url(app), app.overlay.mode),
            );
            press(app, BM_O, CTRL_SHIFT);
        }
        178 => {
            // × arms the delete, the second click deletes (alpha, by its address).
            overlay_js(
                app,
                "(() => { const row = [...document.querySelectorAll('#bm-rows li')].find(li => li.querySelector('button').title.endsWith('?bm=alpha')); \
                 const b = row.querySelector('button:last-child'); b.click(); b.click(); })()",
            );
        }
        179 => {
            check(
                "bm-list-delete-keeps-list-open",
                !app.bookmarks.contains("https://example.com/?bm=alpha") && app.bookmarks.contains("https://example.com/?bm=zebra") && app.overlay.mode == Mode::Bookmarks,
                format!("mode {:?}, {:?}", app.overlay.mode, app.bookmarks.items().iter().map(|b| &b.url).collect::<Vec<_>>()),
            );
            eval(
                app.overlay.view(),
                "bm-list-after-delete-focus-and-rows",
                "`rows=${document.querySelectorAll('#bm-rows li').length};el=${document.activeElement.id}`",
                |r| {
                    check("bm-list-after-delete-focus-and-rows", num(&r, "rows") == 2.0 && field(&r, "el") == "bm-search", &r);
                },
            );
            let flushed = crate::store::flush(Duration::from_secs(5));
            let urls: Vec<String> = bm_file(app)
                .and_then(|v| v["items"].as_array().map(|a| a.iter().filter_map(|i| i["url"].as_str().map(String::from)).collect()))
                .unwrap_or_default();
            check(
                "bm-delete-persists-in-file",
                flushed && urls.len() == 2 && !urls.iter().any(|u| u.ends_with("?bm=alpha")) && urls.iter().any(|u| u.ends_with("?bm=zebra")),
                format!("flushed {flushed}, {urls:?}"),
            );
            overlay_js(app, "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))");
        }
        180 => {
            check("bm-esc-closes-list", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            suggest_type(app, "zebra");
        }
        181 => {
            check("bm-suggestion-opens", app.overlay.mode == Mode::Suggest, format!("mode {:?}", app.overlay.mode));
            eval(
                app.overlay.view(),
                "bm-suggestion-is-one-marked-row",
                "(() => { const s = document.querySelector('#suggest li span:not([role])'); \
                 return `rows=${document.querySelectorAll('#suggest li').length};star=${document.querySelectorAll('#suggest li span[role=img]').length};\
                 first=${s ? s.textContent : ''};label=${document.querySelector('#suggest li span[role=img]')?.getAttribute('aria-label')}`; })()",
                |r| {
                    check(
                        "bm-suggestion-is-one-marked-row",
                        field(&r, "rows") == "1" && field(&r, "star") == "1" && field(&r, "first").ends_with("?bm=zebra") && field(&r, "label") == "Yer imi",
                        &r,
                    );
                },
            );
            address_key(app, "Escape");
        }
        182 => {
            check("bm-suggestion-esc-closes", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            // A hostile file, loaded by a second store in its own folder.
            let hostile = dir().join("bm-hostile");
            let _ = std::fs::create_dir_all(&hostile);
            let big = "T".repeat(100_000);
            let file = serde_json::json!({"v": 1, "items": [
                {"url": "https://ok.example/", "title": "ok", "added": 5},
                {"url": "javascript:alert(document.cookie)", "title": "x", "added": 1},
                {"url": "file:///C:/Windows/win.ini", "title": "x", "added": 1},
                {"url": "https://big.example/", "title": big, "added": 1},
                // The same page once the login is dropped (a #fragment would
                // be a page of its own: bookmarks keep it).
                {"url": "https://me:pw@ok.example/", "title": "duplicate", "added": 9},
                {"url": 5, "title": "wrong type"},
                null,
                42,
            ]});
            let written = std::fs::write(hostile.join(crate::bookmarks::FILE), file.to_string());
            let mut b = crate::bookmarks::Bookmarks::new(Some(&hostile));
            b.ensure_loaded();
            let urls: Vec<&str> = b.items().iter().map(|i| i.url.as_str()).collect();
            check(
                "bm-hostile-file-loads-sanitized",
                written.is_ok()
                    && urls == ["https://ok.example/", "https://big.example/"]
                    && b.items()[1].title.chars().count() == crate::bookmarks::MAX_TITLE
                    && !b.contains("javascript:alert(document.cookie)"),
                format!("{written:?}, {urls:?}"),
            );
            // Ctrl+D on the (now unbookmarked) alpha page: an editor is open
            // for it; the content page then tries the same commands. A
            // content webview has no IPC handler at all, so they go nowhere
            // (that a page of another origin is refused even where there is
            // a handler is `ui::tests`' job).
            press(app, BM_D, CTRL);
            remember("bm-count", app.bookmarks.items().len() as i64);
            page_js(app, "try { window.ipc.postMessage('bm-remove'); window.ipc.postMessage('bm-save:evil'); window.ipc.postMessage('bm-star:1:1'); } catch (e) {}");
        }
        183 => {
            check(
                "bm-content-webview-has-no-ipc",
                app.overlay.mode == Mode::Bookmarks
                    && app.bookmarks.items().len() as i64 == recall("bm-count")
                    && !app.bookmarks.items().iter().any(|b| b.title == "evil"),
                format!("mode {:?}, {} bookmarks (were {})", app.overlay.mode, app.bookmarks.items().len(), recall("bm-count")),
            );
            overlay_js(app, "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))");
            close_extra_tabs(app);
        }
        // Downloads, main menu, settings (task 7): one block, last before the finale.
        184 => {
            let port = serve_attachment();
            check("download-server-started", port.is_some(), "no local port");
            remember("dl-port", port.map_or(0, i64::from));
            let sc = press(app, vk::F, ALT);
            check("alt-f-is-the-menu-shortcut", sc == Some(Shortcut::Menu), format!("{sc:?}"));
        }
        185 => {
            check("menu-opens-with-alt-f", app.overlay.mode == Mode::Menu, format!("mode {:?}", app.overlay.mode));
            let (w, h) = logical_size(app);
            let b = app.overlay.view().map(|v| bounds_of(app, v)).unwrap_or((f64::NAN, f64::NAN, f64::NAN, f64::NAN));
            let inside = b.0 >= 0.0 && b.1 >= crate::app::UI_HEIGHT && b.0 + b.2 <= w + 0.5 && b.1 + b.3 <= h + 0.5;
            check("menu-is-clamped-inside-the-window", inside, format!("{b:?} in {w}x{h}"));
            overlay_active(app, "menu-focus-starts-on-the-first-item", "new-tab");
            overlay_js(app, &key_js("ArrowDown"));
        }
        186 => {
            overlay_active(app, "menu-arrow-down-moves-on", "find");
            overlay_js(app, &format!("{}{}", key_js("ArrowUp"), key_js("ArrowUp")));
        }
        187 => {
            overlay_active(app, "menu-arrow-up-wraps-to-the-last-item", "extensions");
            overlay_js(app, &key_js("Escape"));
        }
        188 => {
            check("menu-esc-closes", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            page_eval(app, "menu-esc-returns-focus-to-page", "document.hasFocus()", |r| {
                check("menu-esc-returns-focus-to-page", r == "true", &r);
            });
            let sc = press(app, vk::E, ALT);
            check("alt-e-is-the-menu-shortcut", sc == Some(Shortcut::Menu), format!("{sc:?}"));
        }
        189 => {
            check("menu-opens-with-alt-e", app.overlay.mode == Mode::Menu, format!("mode {:?}", app.overlay.mode));
            remember("zoom-before", i64::from(tab_zoom(app)));
            overlay_click(app, "[data-item=zoom-in]");
        }
        190 => {
            let (was, now) = (recall("zoom-before"), i64::from(tab_zoom(app)));
            check("menu-zoom-in-zooms", now > was && app.overlay.mode == Mode::Menu, format!("{was} -> {now}, {:?}", app.overlay.mode));
            let shown = format!("%{now}");
            overlay_eval(app, "menu-shows-the-zoom-percent", "document.getElementById('m-zoom').textContent", move |r| {
                check("menu-shows-the-zoom-percent", r == shown, format!("{r} want {shown}"));
            });
            overlay_click(app, "[data-item=zoom-reset]");
        }
        191 => {
            let now = tab_zoom(app);
            check("menu-zoom-reset-resets", now == 100 && app.overlay.mode == Mode::Menu, format!("{now}, {:?}", app.overlay.mode));
            overlay_click(app, "[data-item=find]");
        }
        192 => {
            check("menu-find-closes-the-menu", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            // Whether the bar shows cannot be seen from here; an error HRESULT can.
            let hr = crate::native::FIND_START.load(Ordering::Relaxed);
            check("menu-find-start-reports-no-error", hr == 0 || hr == 1, format!("Find::Start HRESULT {hr:#x} (1: not completed)"));
            ui_js(app, "document.getElementById('menu-btn').click()");
        }
        193 => {
            check("menu-opens-with-its-button", app.overlay.mode == Mode::Menu, format!("mode {:?}", app.overlay.mode));
            overlay_click(app, "[data-item=history]");
        }
        194 => {
            check("menu-history-opens-the-history", app.overlay.mode == Mode::History, format!("mode {:?}", app.overlay.mode));
            overlay_js(app, &key_js("Escape"));
        }
        195 => {
            check("history-from-menu-esc-closes", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            press(app, vk::F, ALT);
        }
        196 => {
            check("menu-reopens-for-bookmarks", app.overlay.mode == Mode::Menu, format!("mode {:?}", app.overlay.mode));
            overlay_click(app, "[data-item=bookmarks]");
        }
        197 => {
            check("menu-bookmarks-opens-the-list", app.overlay.mode == Mode::Bookmarks, format!("mode {:?}", app.overlay.mode));
            press(app, vk::F, ALT);
        }
        198 => {
            check("menu-reopens-for-settings", app.overlay.mode == Mode::Menu, format!("mode {:?}", app.overlay.mode));
            overlay_click(app, "[data-item=settings]");
        }
        199 => {
            check("menu-settings-opens-the-settings", app.overlay.mode == Mode::Settings, format!("mode {:?}", app.overlay.mode));
            let s = &app.settings.value;
            check(
                "settings-start-at-the-defaults",
                (s.engine, s.startup, s.autofill) == (crate::settings::Engine::DuckDuckGo, crate::settings::Startup::Last, true)
                    && s.download_dir.is_empty(),
                format!("{s:?}"),
            );
            let all_on = app.views.iter().all(|(_, v)| crate::native::content_settings(v) == Some((true, false)));
            check("autofill-on-and-password-save-off-by-default", all_on && !app.views.is_empty(), "a view differs");
            let folder = dir().join("dl");
            let _ = std::fs::create_dir_all(&folder);
            remember("dl-ready", i64::from(folder.is_dir()));
            let mut lit = String::new();
            crate::tabs::push_json_str(&mut lit, &folder.to_string_lossy());
            overlay_js(
                app,
                &format!(
                    "document.querySelector('input[name=s-engine][value=google]').click(); \
                     document.querySelector('input[name=s-startup][value=blank]').click(); \
                     document.getElementById('s-autofill').click(); \
                     document.getElementById('s-folder').value = {lit}; \
                     document.getElementById('s-folder-apply').click();"
                ),
            );
        }
        200 => {
            use crate::settings::{Engine, Startup};
            let s = &app.settings.value;
            check("settings-engine-changes", s.engine == Engine::Google && crate::settings::engine() == Engine::Google, format!("{:?}", s.engine));
            let url = crate::url::search_url("a b");
            check("settings-engine-drives-the-address-bar", url.starts_with("https://www.google.com/search?q=a"), &url);
            check("settings-startup-changes", s.startup == Startup::Blank, format!("{:?}", s.startup));
            check("settings-autofill-off-reaches-open-views", app.views.iter().all(|(_, v)| crate::native::content_settings(v) == Some((false, false))), "a view differs");
            let want = dir().join("dl");
            check("settings-download-folder-saved", s.download_dir == want.to_string_lossy(), format!("{:?}", s.download_dir));
            let now = app.views.first().and_then(|(_, v)| crate::native::download_folder(v)).unwrap_or_default();
            check("settings-download-folder-reaches-webview2", std::path::Path::new(&now).ends_with("dl"), &now);
            let flushed = crate::store::flush(Duration::from_secs(2));
            let file = crate::store::data_dir().map(|d| d.join("settings.json")).and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            check(
                "settings-are-written-to-settings-json",
                flushed && file.contains("\"v\":1") && file.contains("google") && file.contains("blank") && file.contains("\"autofill\":false"),
                &file,
            );
            // The page's own scripts reach none of it.
            app.overlay.cleared = None;
            page_js(
                app,
                "for (const m of ['menu-toggle','menu-pick:history','st-engine:bing','st-startup:last','st-autofill:1','st-folder:','ov-clear-data']) \
                 { try { window.ipc.postMessage(m); } catch (e) {} }",
            );
        }
        201 => {
            let s = &app.settings.value;
            check(
                "content-ipc-cannot-reach-menu-or-settings",
                app.overlay.mode == Mode::Settings
                    && s.engine == crate::settings::Engine::Google
                    && s.startup == crate::settings::Startup::Blank
                    && !s.autofill
                    && !s.download_dir.is_empty()
                    && app.overlay.cleared.is_none(),
                format!("{:?} {s:?} {:?}", app.overlay.mode, app.overlay.cleared),
            );
            overlay_js(app, &key_js("Escape"));
            press(app, vk::T, CTRL);
        }
        202 => {
            check("settings-esc-closes", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            let new_view = app.views.last().is_some_and(|(_, v)| crate::native::content_settings(v) == Some((false, false)));
            check("autofill-off-reaches-new-tabs", new_view, "the new tab's webview differs");
            let port = recall("dl-port");
            app.handle(UserEvent::Ui(UiCommand::Navigate(format!("http://127.0.0.1:{port}/xb-test.txt"))));
        }
        203 => {
            let d = &app.downloads;
            check("download-started-and-finished", d.started == 1 && d.completed == 1 && d.failed == 0 && d.active == 0, format!("{d:?}"));
            let text = d.last_file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
            check("download-file-has-the-served-bytes", text == DOWNLOAD_BODY, &text);
            let in_folder = d.last_file.as_ref().is_some_and(|f| f.parent().is_some_and(|p| p.ends_with("dl")));
            check("download-lands-in-the-chosen-folder", in_folder, format!("{:?}", d.last_file));
            ui_eval(app, "downloads-button-shows-after-a-download", "String(!document.getElementById('downloads-btn').hidden)", |r| {
                check("downloads-button-shows-after-a-download", r == "true", &r);
            });
            // WebView2 opened its own flyout when the download began.
            let open = app.active_view().is_some_and(crate::native::downloads_open);
            check("download-flyout-opens-by-itself", open, "closed");
            let sc = press(app, vk::J, CTRL);
            check("ctrl-j-is-the-downloads-shortcut", sc == Some(Shortcut::Downloads), format!("{sc:?}"));
        }
        204 => {
            let open = app.active_view().is_some_and(crate::native::downloads_open);
            check("ctrl-j-closes-the-download-flyout", !open, "still open");
            press(app, vk::J, CTRL);
        }
        205 => {
            let open = app.active_view().is_some_and(crate::native::downloads_open);
            check("ctrl-j-opens-the-download-flyout", open, "closed");
            press(app, vk::F, ALT);
            overlay_click(app, "[data-item=downloads]");
        }
        206 => {
            let open = app.active_view().is_some_and(crate::native::downloads_open);
            check("menu-downloads-closes-the-open-flyout", !open && overlay_closed(app), format!("open {open}, mode {:?}", app.overlay.mode));
            press(app, vk::F, ALT);
            overlay_click(app, "[data-item=downloads]");
        }
        207 => {
            let open = app.active_view().is_some_and(crate::native::downloads_open);
            check("menu-downloads-opens-the-flyout", open && overlay_closed(app), format!("open {open}, mode {:?}", app.overlay.mode));
            press(app, vk::J, CTRL);
            crate::settings::open(app);
        }
        208 => {
            let open = app.active_view().is_some_and(crate::native::downloads_open);
            check("download-flyout-closes-again", !open, "still open");
            // Back to the defaults through the page, then clear browsing data from it.
            app.overlay.cleared = None;
            overlay_js(
                app,
                "document.querySelector('input[name=s-engine][value=duckduckgo]').click(); \
                 document.querySelector('input[name=s-startup][value=last]').click(); \
                 document.getElementById('s-autofill').click(); \
                 document.getElementById('s-folder-reset').click(); \
                 document.getElementById('s-clear').click(); document.getElementById('s-yes').click();",
            );
        }
        209 => {
            use crate::settings::{Engine, Startup};
            let s = &app.settings.value;
            check(
                "settings-go-back-to-the-defaults",
                (s.engine, s.startup, s.autofill) == (Engine::DuckDuckGo, Startup::Last, true) && s.download_dir.is_empty(),
                format!("{s:?}"),
            );
            check("settings-engine-restored-for-the-address-bar", crate::url::search_url("a").starts_with(crate::url::SEARCH_URL), crate::url::search_url("a"));
            let all_on = app.views.iter().all(|(_, v)| crate::native::content_settings(v) == Some((true, false)));
            check("autofill-back-on-and-password-save-still-off", all_on, "a view differs");
            check("settings-clear-data-clears", app.overlay.cleared == Some(true), format!("{:?}", app.overlay.cleared));
            // A hostile settings.json costs only its fields; an oversize one is ignored.
            let hostile = dir().join("hostile");
            let _ = std::fs::create_dir_all(&hostile);
            let _ = std::fs::write(
                hostile.join("settings.json"),
                r#"{"v":1,"engine":"evil","startup":"blank","autofill":"x","download_dir":"..\\x"}"#,
            );
            let read = crate::settings::State::open(Some(&hostile));
            let v = &read.value;
            check(
                "hostile-settings-file-is-sanitized",
                (v.engine, v.startup, v.autofill) == (Engine::DuckDuckGo, Startup::Blank, true) && v.download_dir.is_empty(),
                format!("{v:?}"),
            );
            overlay_js(app, &key_js("Escape"));
        }
        210 => {
            check("settings-esc-closes-the-page", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            press(app, vk::F, ALT);
            overlay_click(app, "[data-item=print]");
        }
        211 => {
            check("menu-print-closes-the-menu", overlay_closed(app), format!("mode {:?}", app.overlay.mode));
            let shown = app.active_view().is_some_and(crate::native::print);
            check("print-preview-opens", shown, "ShowPrintUI failed");
        }
        // Security indicators and policy (src/policy.rs). No application is
        // ever started here: launches go to a recorder.
        212 => {
            // The menu block opened a second tab (Ctrl+T, the print preview
            // is up in it): back to one tab.
            app.handle(UserEvent::Shortcut(Shortcut::CloseTab));
            LAUNCHED.lock().unwrap().clear();
            app.policy.launch.set_launcher(record_launch);
            // Served from this process: WebView2 moves a public http address
            // to https by itself when the site has it (example.com does).
            let port = serve_http_page();
            check("policy-http-server-started", port.is_ok(), format!("{port:?}"));
            app.handle(UserEvent::Ui(UiCommand::Navigate(format!("http://127.0.0.1:{}/policy", port.unwrap_or(0)))));
        }
        213 => {
            check("policy-loopback-page-loaded", active_url(app).starts_with("http://127.0.0.1:") && active_title(app) == "Plain", active_url(app));
            // http to this machine itself: no warning, and no lock either.
            ui_eval(app, "indicator-loopback-http-is-neutral", SECURITY_JS, |r| {
                check("indicator-loopback-http-is-neutral", field(&r, "hidden") == "true" && field(&r, "state").is_empty() && num(&r, "w") == 0.0, &r);
            });
            // A public address that has no https to be moved to.
            app.handle(UserEvent::Ui(UiCommand::Navigate("http://www.msftconnecttest.com/connecttest.txt".into())));
        }
        214 => {
            check("policy-http-page-loaded", active_url(app).starts_with("http://www.msftconnecttest.com/"), active_url(app));
            ui_eval(app, "indicator-http-says-not-secure", SECURITY_JS, |r| {
                check(
                    "indicator-http-says-not-secure",
                    field(&r, "state") == "insecure" && field(&r, "text") == "Güvenli değil" && field(&r, "hidden") == "false" && num(&r, "w") > 0.0,
                    &r,
                );
                // No control and no picture role: the meaning is the status text.
                check(
                    "indicator-http-has-a-name",
                    field(&r, "tag") == "DIV" && field(&r, "role").is_empty() && field(&r, "live") == "polite" && field(&r, "name").contains("HTTP") && field(&r, "reload") == "false",
                    &r,
                );
            });
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?policy".into())));
        }
        215 => {
            check("policy-https-page-loaded", active_url(app).starts_with("https://example.com/"), active_url(app));
            ui_eval(app, "indicator-https-shows-lock", SECURITY_JS, |r| {
                check(
                    "indicator-https-shows-lock",
                    field(&r, "state") == "secure" && field(&r, "lock") == "block" && field(&r, "text").is_empty() && num(&r, "w") > 0.0,
                    &r,
                );
                check("indicator-https-has-a-name", field(&r, "role").is_empty() && field(&r, "name").contains("HTTPS"), &r);
            });
            page_js(
                app,
                &format!(
                    "Promise.all(['camera', 'microphone', 'geolocation', 'notifications'].map((name) => navigator.permissions.query({{ name }}))) \
                     .then((r) => {{ window.__perm = r.map((s) => s.state).join(','); }}, (e) => {{ window.__perm = 'error ' + e; }}); \
                     document.body.innerHTML = '<a id=\"m\" href=\"{MAIL_URI}\" style=\"position:fixed;inset:0\">mail</a>';"
                ),
            );
        }
        216 => {
            // Nothing grants these without WebView2's own question.
            page_eval(app, "permissions-are-never-pre-granted", "`perm=${window.__perm}`", |r| {
                check("permissions-are-never-pre-granted", field(&r, "perm") == "prompt,prompt,prompt,prompt", &r);
            });
            click_page(app);
        }
        217 => {
            // The question closes when its window loses the keyboard: a run
            // with another window in front fails here, and this says why.
            log(&format!("INFO step 217: X-Browser is the foreground window: {}", crate::native::is_foreground(&app.window)));
            let now = std::time::Instant::now();
            let id = app.tabs.active().unwrap();
            let p = app.policy.launch.pending(now).cloned();
            check(
                "external-launch-with-gesture-waits",
                p.as_ref().is_some_and(|p| p.tab == id && p.scheme == "mailto" && p.uri == MAIL_URI && p.origin.starts_with("https://example.com")),
                format!("{p:?}"),
            );
            let loading = app.tabs.get(id).is_some_and(|t| t.loading);
            check(
                "external-launch-is-cancelled",
                launched().is_empty() && !loading && active_url(app).starts_with("https://example.com/"),
                format!("launched {:?}, loading {loading}, url {}", launched(), active_url(app)),
            );
            let shown = app.overlay.view().is_some_and(visible);
            check("launch-question-opens", app.overlay.mode == Mode::Launch && shown, format!("{:?}, visible {shown}", app.overlay.mode));
            eval(app.overlay.view(), "launch-question-shows-the-request", LAUNCH_JS, |r| {
                check(
                    "launch-question-shows-the-request",
                    field(&r, "hidden") == "false"
                        && field(&r, "scheme") == "mailto"
                        && field(&r, "origin").starts_with("https://example.com")
                        && field(&r, "uri") == MAIL_URI
                        && field(&r, "markup") == "0"
                        && num(&r, "h") > 0.0,
                    &r,
                );
                check("launch-question-starts-on-cancel", field(&r, "focus") == "l-cancel", &r);
            });
            // An answer for anything but the waiting request is refused, and
            // "open" then finds nothing to open.
            let r = app.policy.launch.confirm(id, "mailto:someone-else@example.invalid", now);
            check(
                "external-launch-wrong-uri-refused",
                r == Err(Refused::Mismatch) && launched().is_empty() && app.policy.launch.pending(now).is_none(),
                format!("{r:?}, launched {:?}", launched()),
            );
            app.handle(UserEvent::Ui(UiCommand::Launch(LaunchCmd::Open)));
        }
        218 => {
            check(
                "launch-open-with-nothing-waiting-opens-nothing",
                launched().is_empty() && overlay_closed(app),
                format!("launched {:?}, {:?}", launched(), app.overlay.mode),
            );
            click_page(app);
        }
        219 => {
            // The question closes when its window loses the keyboard: a run
            // with another window in front fails here, and this says why.
            log(&format!("INFO step 219: X-Browser is the foreground window: {}", crate::native::is_foreground(&app.window)));
            let waits = app.policy.launch.pending(std::time::Instant::now()).is_some();
            check("launch-question-opens-again", app.overlay.mode == Mode::Launch && waits, format!("{:?}, waits {waits}", app.overlay.mode));
            overlay_js(app, "document.getElementById('l-cancel').click()");
        }
        220 => {
            let waits = app.policy.launch.pending(std::time::Instant::now()).is_some();
            check(
                "launch-cancel-opens-nothing",
                launched().is_empty() && !waits && overlay_closed(app),
                format!("launched {:?}, waits {waits}, {:?}", launched(), app.overlay.mode),
            );
            click_page(app);
        }
        221 => {
            // The page names nothing: Rust opens the request it holds. The
            // question has been up long enough for its button to work.
            overlay_js(app, "document.getElementById('l-open').click()");
        }
        222 => {
            // The question closes when its window loses the keyboard: a run
            // with another window in front fails here, and this says why.
            log(&format!("INFO step 222: X-Browser is the foreground window: {}", crate::native::is_foreground(&app.window)));
            let now = std::time::Instant::now();
            let id = app.tabs.active().unwrap();
            let waits = app.policy.launch.pending(now).is_some();
            check(
                "launch-open-reaches-the-launcher-once",
                launched() == [MAIL_URI] && !waits && overlay_closed(app),
                format!("launched {:?}, waits {waits}, {:?}", launched(), app.overlay.mode),
            );
            app.handle(UserEvent::Ui(UiCommand::Launch(LaunchCmd::Open)));
            let again = app.policy.launch.confirm(id, MAIL_URI, now);
            check("external-launch-one-answer-only", again == Err(Refused::NoRequest) && launched().len() == 1, format!("{again:?}, launched {:?}", launched()));
            // "Open" right as the question appears is a click meant for the page.
            app.handle(UserEvent::Policy(id, Event::ExternalLaunch { uri: MAIL_URI.into(), origin: String::new(), user: true }));
            app.handle(UserEvent::Ui(UiCommand::Launch(LaunchCmd::Open)));
            let waits = app.policy.launch.pending(std::time::Instant::now()).is_some();
            check(
                "launch-open-too-early-starts-the-wait-again",
                launched().len() == 1 && waits && app.overlay.mode == Mode::Launch,
                format!("launched {:?}, waits {waits}, {:?}", launched(), app.overlay.mode),
            );
            eval(app.overlay.view(), "launch-open-button-waits", LAUNCH_JS, |r| {
                check("launch-open-button-waits", field(&r, "open") == "disabled" && field(&r, "native") == "false" && field(&r, "failed") == "false", &r);
            });
        }
        223 => {
            // The wait is over by now: only where the answer comes from
            // keeps this one out.
            ui_js(app, "ui.send('launch-open')");
        }
        224 => {
            // The question closes when its window loses the keyboard: a run
            // with another window in front fails here, and this says why.
            log(&format!("INFO step 224: X-Browser is the foreground window: {}", crate::native::is_foreground(&app.window)));
            let waits = app.policy.launch.pending(std::time::Instant::now()).is_some();
            check(
                "launch-answer-from-the-strip-is-not-heard",
                launched().len() == 1 && waits && app.overlay.mode == Mode::Launch,
                format!("launched {:?}, waits {waits}, {:?}", launched(), app.overlay.mode),
            );
            app.policy.launch.set_launcher(|_| false);
            app.handle(UserEvent::Ui(UiCommand::Launch(LaunchCmd::Open)));
        }
        225 => {
            let now = std::time::Instant::now();
            let waited = app.policy.launch.asking(now) && app.policy.launch.pending(now).is_none() && app.overlay.mode == Mode::Launch;
            app.policy.launch.set_launcher(record_launch);
            eval(app.overlay.view(), "launch-failure-is-said", LAUNCH_JS, |r| {
                check("launch-failure-is-said", field(&r, "failed") == "true" && field(&r, "open") == "hidden" && field(&r, "cancel") == "Kapat" && field(&r, "focus") == "l-cancel", &r);
                check("launch-question-unknown-page", field(&r, "origin") == "bilinmeyen sayfa" && field(&r, "uri") == MAIL_URI, &r);
            });
            press(app, vk::T, CTRL);
            check(
                "launch-question-closes-on-tab-switch",
                waited && !app.policy.launch.asking(now) && overlay_closed(app) && launched().len() == 1 && app.tabs.len() == 2,
                format!("waited {waited}, asking {}, {:?}, launched {:?}, {} tabs", app.policy.launch.asking(now), app.overlay.mode, launched(), app.tabs.len()),
            );
            app.flush();
            ui_eval(app, "indicator-nothing-on-blank-tab", SECURITY_JS, |r| {
                check("indicator-nothing-on-blank-tab", field(&r, "hidden") == "true" && field(&r, "state").is_empty() && field(&r, "name").is_empty() && num(&r, "w") == 0.0, &r);
            });
            close_extra_tabs(app);
        }
        226 => {
            // The page's own load-time script: no user gesture. (A script
            // run from the host would count as one.)
            remember("silent-drops", i64::from(app.policy.silent_drops));
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                let _ = v.load_html(&format!("<!doctype html><title>Auto</title><body>auto<script>location.href = '{MAIL_URI}';</script></body>"));
            }
        }
        227 => {
            let now = std::time::Instant::now();
            let (drops, before) = (i64::from(app.policy.silent_drops), recall("silent-drops"));
            let loading = app.tabs.active().and_then(|id| app.tabs.get(id)).is_some_and(|t| t.loading);
            check(
                "external-launch-without-gesture-dropped",
                drops == before + 1 && app.policy.launch.pending(now).is_none() && launched().len() == 1 && !loading,
                format!("drops {drops} (were {before}), waiting {:?}, launched {:?}, loading {loading}", app.policy.launch.pending(now), launched()),
            );
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                let _ = v.load_url("file:///C:/Windows/win.ini");
            }
        }
        228 => {
            let id = app.tabs.active().unwrap();
            let loading = app.tabs.get(id).is_some_and(|t| t.loading);
            check("file-navigation-cancelled", !active_url(app).starts_with("file:") && !loading, format!("url {}, loading {loading}", active_url(app)));
            page_eval(app, "file-navigation-leaves-page", "`title=${document.title};file=${location.protocol === 'file:'}`", |r| {
                check("file-navigation-leaves-page", field(&r, "title") == "Auto" && field(&r, "file") == "false", &r);
            });
            // What WebView2 reports for a bad certificate, while its own
            // warning page is up.
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/?policy-cert".into())));
            let uri = "https://example.com/?policy-cert";
            app.handle(UserEvent::Policy(id, Event::CertError { uri: uri.into(), navs: vec![uri.into()] }));
        }
        229 => {
            check("policy-cert-page-loaded", active_url(app).starts_with("https://example.com/?policy-cert"), active_url(app));
            ui_eval(app, "indicator-certificate-error", SECURITY_JS, |r| {
                check(
                    "indicator-certificate-error",
                    field(&r, "state") == "cert" && field(&r, "text") == "Sertifika hatası" && field(&r, "name").contains("Sertifika"),
                    &r,
                );
            });
            // Something the page loads from another server fails the same
            // way: that says nothing about the other server's own pages.
            let id = app.tabs.active().unwrap();
            app.handle(UserEvent::Policy(id, Event::CertError { uri: "https://example.org/a.png".into(), navs: vec![active_url(app)] }));
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.org/?policy".into())));
        }
        230 => {
            ui_eval(app, "indicator-certificate-error-is-per-host", SECURITY_JS, |r| {
                check("indicator-certificate-error-is-per-host", field(&r, "state") == "secure" && field(&r, "addr").starts_with("https://example.org/"), &r);
            });
            // A host that cannot load: the address box shows what was typed.
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://bücher.example/yol".into())));
            app.flush();
            log(&format!("INFO unicode host, right after typing: tab url {:?}", active_url(app)));
            ui_eval(app, "address-box-unicode-host-typed", SECURITY_JS, |r| {
                check("address-box-unicode-host-typed", field(&r, "addr").is_ascii() && field(&r, "addr").contains("xn--bcher-kva.example"), &r);
                // The lock on screen was example.org's, not this address's.
                check("indicator-none-until-the-typed-address-loads", field(&r, "state").is_empty() && field(&r, "hidden") == "true", &r);
            });
        }
        231 => {
            log(&format!("INFO unicode host, after the load failed: tab url {:?}", active_url(app)));
            ui_eval(app, "address-box-unicode-host-reported", SECURITY_JS, |r| {
                check("address-box-unicode-host-reported", field(&r, "addr").is_ascii() && field(&r, "addr").contains("xn--bcher-kva.example"), &r);
                check("indicator-no-lock-on-an-error-page", field(&r, "state").is_empty() && field(&r, "hidden") == "true", &r);
            });
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.org/?policy-crash".into())));
        }
        232 => {
            let id = app.tabs.active().unwrap();
            check(
                "policy-crash-page-loaded",
                active_url(app).starts_with("https://example.org/?policy-crash") && !app.policy.is_crashed(id),
                active_url(app),
            );
            if let Some(v) = app.view(id) {
                cdp(v, "Page.crash", "{}");
            }
        }
        233 => {
            let id = app.tabs.active().unwrap();
            check("crashed-tab-is-marked", app.policy.is_crashed(id) && app.tabs.get(id).is_some_and(|t| !t.loading), app.tabs.to_json());
            ui_eval(app, "indicator-crashed-has-a-reload-button", SECURITY_JS, |r| {
                check(
                    "indicator-crashed-has-a-reload-button",
                    field(&r, "state") == "crashed" && field(&r, "reload") == "true" && !field(&r, "text").is_empty() && field(&r, "name").contains("Ctrl+R") && num(&r, "w") > 0.0,
                    &r,
                );
            });
            ui_js(app, "document.getElementById('security-reload').click()");
        }
        234 => {
            let id = app.tabs.active().unwrap();
            check("crashed-tab-button-reloads", !app.policy.is_crashed(id) && active_url(app).starts_with("https://example.org/?policy-crash"), active_url(app));
            page_eval(app, "crashed-tab-page-is-back", "`href=${location.href};state=${document.readyState}`", |r| {
                check("crashed-tab-page-is-back", field(&r, "href").starts_with("https://example.org/?policy-crash"), &r);
            });
            ui_eval(app, "indicator-after-reload", SECURITY_JS, |r| {
                check("indicator-after-reload", field(&r, "state") == "secure" && field(&r, "reload") == "false", &r);
            });
        }
        235 => {
            // In a step of its own: a crash right behind a script would take
            // the script's answer with it.
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                cdp(v, "Page.crash", "{}");
            }
        }
        236 => {
            let id = app.tabs.active().unwrap();
            let crashed = app.policy.is_crashed(id);
            let sc = press(app, vk::R, CTRL);
            check("crashed-tab-marked-again", crashed && sc == Some(Shortcut::Reload), format!("crashed {crashed}, {sc:?}"));
        }
        237 => {
            let id = app.tabs.active().unwrap();
            check("crashed-tab-shortcut-reloads", !app.policy.is_crashed(id), app.tabs.to_json());
            page_eval(app, "crashed-tab-page-is-back-again", "`href=${location.href}`", |r| {
                check("crashed-tab-page-is-back-again", field(&r, "href").starts_with("https://example.org/?policy-crash"), &r);
            });
        }
        238 => {
            if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
                cdp(v, "Page.crash", "{}");
            }
        }
        239 => {
            let id = app.tabs.active().unwrap();
            check("crashed-tab-marked-a-third-time", app.policy.is_crashed(id), app.tabs.to_json());
            // This load fails: no page comes back to end the mark.
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://bücher.example/yol".into())));
        }
        240 => {
            let id = app.tabs.active().unwrap();
            check("crashed-mark-ends-with-the-next-navigation", !app.policy.is_crashed(id), active_url(app));
            ui_eval(app, "indicator-after-crash-and-failed-load", SECURITY_JS, |r| {
                check("indicator-after-crash-and-failed-load", field(&r, "state").is_empty() && field(&r, "reload") == "false" && field(&r, "name").is_empty(), &r);
            });
        }
        // ---- v0.2 fixes: one contiguous block ----
        241 => {
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.org/?frag".into())));
        }
        242 => {
            press(app, vk::H, CTRL);
        }
        243 => {
            // A jump within the page loads nothing: the list stays up.
            page_js(app, "location.hash = 'x'");
        }
        244 => {
            check("history-stays-open-on-a-fragment-change", app.overlay.mode == Mode::History, format!("mode {:?}, url {}", app.overlay.mode, active_url(app)));
            page_eval(app, "history-fragment-change-happened", "`hash=${location.hash}`", |r| {
                check("history-fragment-change-happened", field(&r, "hash") == "#x", &r);
            });
            overlay_js(app, &key_js("Escape"));
        }
        245 => {
            press(app, BM_D, CTRL);
        }
        246 => {
            check("bm-editor-open-before-star-press", app.overlay.mode == Mode::Bookmarks, format!("mode {:?}", app.overlay.mode));
            // A press then a click on the star, as a mouse makes them.
            ui_js(
                app,
                "(() => { const b = document.getElementById('bm-star'); \
                 b.dispatchEvent(new PointerEvent('pointerdown', { button: 0, bubbles: true })); b.click(); })()",
            );
        }
        247 => {
            let page = page_url(app);
            check(
                "bm-star-click-closes-the-open-editor",
                overlay_closed(app) && app.bookmarks.contains(&page),
                format!("mode {:?}, {page} kept {}", app.overlay.mode, app.bookmarks.contains(&page)),
            );
            // More hosts with certificate errors than are remembered: no
            // https page is secure for the rest of the run, and it is said.
            let id = app.tabs.active().unwrap();
            for i in 0..70 {
                let uri = format!("https://bad{i}.example/");
                app.handle(UserEvent::Policy(id, Event::CertError { uri: uri.clone(), navs: vec![uri] }));
            }
            app.flush();
            ui_eval(app, "indicator-distrust-is-said", SECURITY_JS, |r| {
                check(
                    "indicator-distrust-is-said",
                    field(&r, "state") == "cert" && field(&r, "text") == "Sertifikalara güvenilmiyor" && field(&r, "name").contains("yeniden başlatılana kadar"),
                    &r,
                );
            });
        }
        248 => {
            let id = app.tabs.active().unwrap();
            app.handle(UserEvent::Policy(id, Event::ExternalLaunch { uri: MAIL_URI.into(), origin: String::new(), user: true }));
        }
        249 => {
            log(&format!("INFO step 249: X-Browser is the foreground window: {}", crate::native::is_foreground(&app.window)));
            check("launch-question-up-before-menu-click", app.overlay.mode == Mode::Launch, format!("mode {:?}", app.overlay.mode));
            // A mouse click on ⋮: the strip takes the keyboard first.
            let _ = app.ui.focus();
            ui_js(app, "document.getElementById('menu-btn').click()");
        }
        250 => {
            let now = std::time::Instant::now();
            check(
                "menu-click-answers-the-launch-question-no",
                app.overlay.mode == Mode::Menu && !app.policy.launch.asking(now) && launched().len() == 1,
                format!("mode {:?}, asking {}, launched {:?}", app.overlay.mode, app.policy.launch.asking(now), launched()),
            );
            overlay_eval(app, "menu-from-launch-question-has-the-keyboard", "`focus=${document.hasFocus()};item=${document.activeElement.dataset.item || 'none'}`", |r| {
                check("menu-from-launch-question-has-the-keyboard", field(&r, "focus") == "true" && field(&r, "item") != "none", &r);
            });
            overlay_js(app, &key_js("Escape"));
        }
        251 => {
            // The start (see `plant_extensions`): leftovers swept, the bad
            // entries dropped and nothing else, the old file kept as .bak,
            // the recorded extension in WebView2 again.
            let root = dir().join("profile").join(crate::extensions::DIR);
            let left: Vec<String> = ext_leftovers().into_iter().filter(|l| root.join(l).exists()).collect();
            check(
                "ext-start-sweeps-unfinished-folders",
                left.is_empty() && root.join(EXT_ID).join("1.0").join("manifest.json").is_file(),
                format!("left {left:?}"),
            );
            let ids: Vec<&str> = app.extensions.records().iter().map(|e| e.id.as_str()).collect();
            check("ext-start-drops-only-the-bad-entries", ids == [EXT_ID], format!("{ids:?}"));
            crate::store::flush(Duration::from_secs(2));
            let file = dir().join("profile").join(crate::extensions::FILE);
            let (now, bak) = (read_text(&file), read_text(&crate::store::sibling(&file, ".bak")));
            check(
                "ext-start-keeps-the-old-file-as-bak",
                bak.contains("not an id") && !now.contains("not an id") && now.contains(EXT_ID),
                format!("file {now}, bak {bak}"),
            );
            let list = app.extensions.list();
            check(
                "ext-start-reconciled",
                app.extensions.reconcile == crate::extensions::Reconcile::Done
                    && list.len() == 1
                    && list[0].id == EXT_ID
                    && list[0].installed
                    && list[0].enabled,
                format!("{:?}, {list:?}", app.extensions.reconcile),
            );
            check("ext-list-hides-the-builtins", !list.iter().any(|l| crate::extensions::is_builtin(&l.id)), format!("{list:?}"));
            if let Some(v) = app.active_view() {
                crate::native::list_extensions(v, |r| {
                    let ids = r.map(|all| all.into_iter().map(|e| e.id).collect::<Vec<_>>());
                    let ok = ids.as_ref().is_ok_and(|ids| crate::extensions::BUILTIN.iter().all(|b| ids.iter().any(|i| i == b)) && ids.iter().any(|i| i == EXT_ID));
                    check("ext-content-profile-has-builtins-and-the-extension", ok, format!("{ids:?}"));
                });
            } else {
                check("ext-content-profile-has-builtins-and-the-extension", false, "no content view");
            }
            // (c) The UI strip's and the overlay's profile: nothing added.
            ui_profile_only_builtins(Some(&app.ui), "ext-ui-profile-has-only-the-builtins");
            ui_profile_only_builtins(app.overlay.view(), "ext-overlay-profile-has-only-the-builtins");
            match serve_ext_page() {
                Ok(port) => remember("ext-port", port as i64),
                Err(e) => check("ext-server-started", false, e),
            }
            // A newer version, added over the recorded one.
            let v2 = root.join(EXT_ID).join("1.1");
            match write_test_extension(&v2, "1.1", true) {
                Ok(()) => remember("ext-op", crate::extensions::add(app, &v2, crate::extensions::Source::Unpacked) as i64),
                Err(e) => check("ext-test-extension-written", false, e),
            }
        }
        252 => {
            // (a) The id is the one its key gives.
            let outcome = ext_outcome(app, "ext-op");
            let key: serde_json::Value = serde_json::from_str(EXT_MANIFEST).unwrap_or_default();
            let from_key = key["key"].as_str().and_then(crate::extensions::id_from_key);
            let rec = app.extensions.records().iter().find(|e| e.id == EXT_ID).map(|e| (e.version.clone(), e.dir.ends_with("1.1")));
            check(
                "ext-added-with-the-id-of-its-key",
                outcome == Some(Ok(EXT_ID.to_string())) && from_key.as_deref() == Some(EXT_ID) && rec == Some(("1.1".into(), true)) && app.extensions.records().len() == 1,
                format!("outcome {outcome:?}, key {from_key:?}, record {rec:?}"),
            );
            ext_load(app, 1);
        }
        253 => {
            // (b) The rule blocks the ad script, the other one loads, and the
            // content script ran in the page.
            ext_page_check(app, 1, true);
            // (c) ... but in neither the UI strip nor the overlay.
            const MARK_JS: &str = "`mark=${document.documentElement.hasAttribute('data-xbext') || !!document.querySelector('[data-xbext]')};url=${location.protocol}`";
            ui_eval(app, "ext-content-script-not-in-the-ui", MARK_JS, |r| {
                check("ext-content-script-not-in-the-ui", field(&r, "mark") == "false", &r);
            });
            overlay_eval(app, "ext-content-script-not-in-the-overlay", MARK_JS, |r| {
                check("ext-content-script-not-in-the-overlay", field(&r, "mark") == "false", &r);
            });
            // Those documents were loaded before the add; what keeps any
            // later one clean is that the add, with the extension installed
            // and on, still left the UI's profile with only the built-ins.
            ui_profile_only_builtins(Some(&app.ui), "ext-ui-profile-still-only-builtins-after-the-add");
            ui_profile_only_builtins(app.overlay.view(), "ext-overlay-profile-still-only-builtins-after-the-add");
            remember("ext-op", crate::extensions::set_enabled(app, EXT_ID, false) as i64);
        }
        254 => {
            ext_state_check(app, "ext-disabled", false);
            ext_load(app, 2);
        }
        255 => {
            // (d) Off: the ad script loads and no content script runs.
            ext_page_check(app, 2, false);
            remember("ext-op", crate::extensions::set_enabled(app, EXT_ID, true) as i64);
        }
        256 => {
            ext_state_check(app, "ext-enabled-again", true);
            ext_load(app, 3);
        }
        257 => {
            // (d) On again: blocked again.
            ext_page_check(app, 3, true);
            // The Uzantılar page, from the menu (a click on ⋮ from the strip).
            let _ = app.ui.focus();
            ui_js(app, "document.getElementById('menu-btn').click()");
        }
        258 => {
            check("ext-menu-opens", app.overlay.mode == Mode::Menu, format!("mode {:?}", app.overlay.mode));
            overlay_click(app, "[data-item=\"extensions\"]");
        }
        259 => {
            check("ext-page-opens-from-the-menu", app.overlay.mode == Mode::Extensions, format!("mode {:?}", app.overlay.mode));
            overlay_eval(app, "ext-page-lists-the-extension", EXT_ROW_JS, |r| {
                check(
                    "ext-page-lists-the-extension",
                    field(&r, "rows") == "1"
                        && field(&r, "id") == EXT_ID
                        && field(&r, "text").contains("XB Selftest")
                        && field(&r, "text").contains("Sürüm 1.1")
                        && field(&r, "on") == "true"
                        && field(&r, "role") == "switch"
                        && field(&r, "label") == "XB Selftest etkin"
                        && field(&r, "options") == "true"
                        && field(&r, "busy") == "false"
                        && field(&r, "focus") == "switch"
                        && field(&r, "empty") == "true",
                    &r,
                );
            });
            ui_eval(app, "ext-button-shown", "`${document.getElementById('ext-btn').hidden}`", |r| {
                check("ext-button-shown", r == "false", &r);
            });
            check("ext-allowed-in-tabs", crate::ext_ui::allowed() == [EXT_ID], format!("{:?}", crate::ext_ui::allowed()));
            // Off, with the page's switch.
            overlay_click(app, "#x-list [data-ctl=\"switch\"]");
        }
        260 => {
            ext_page_state_check(app, "ext-page-switch-turns-it-off", false);
            overlay_eval(app, "ext-page-row-off", EXT_ROW_JS, |r| {
                check(
                    "ext-page-row-off",
                    field(&r, "on") == "false" && field(&r, "options") == "false" && field(&r, "busy") == "false" && field(&r, "state").is_empty() && field(&r, "focus") == "switch",
                    &r,
                );
            });
            ui_eval(app, "ext-button-hidden-when-off", "`${document.getElementById('ext-btn').hidden}`", |r| {
                check("ext-button-hidden-when-off", r == "true", &r);
            });
            // A page of a disabled extension: refused in a tab. (Loaded
            // from Rust: a web page's own script navigating there is
            // stopped by Chromium before X-Browser sees it.)
            if let Some(v) = app.active_view() {
                let _ = v.load_url(&format!("chrome-extension://{EXT_ID}/options.html"));
            }
        }
        261 => {
            page_eval(app, "ext-disabled-page-refused-in-a-tab", "`${location.href}`", |r| {
                // Refused: the page stays, or Chromium shows its own error
                // page (it also blocks a disabled extension's pages).
                check("ext-disabled-page-refused-in-a-tab", r.starts_with("http://127.0.0.1:") || r.starts_with("chrome-error:"), &r);
            });
            overlay_click(app, "#x-list [data-ctl=\"switch\"]");
        }
        262 => {
            ext_page_state_check(app, "ext-page-switch-turns-it-on", true);
            overlay_eval(app, "ext-page-row-on", EXT_ROW_JS, |r| {
                check("ext-page-row-on", field(&r, "on") == "true" && field(&r, "options") == "true" && field(&r, "focus") == "switch", &r);
            });
            ui_eval(app, "ext-button-shown-again", "`${document.getElementById('ext-btn').hidden}`", |r| {
                check("ext-button-shown-again", r == "false", &r);
            });
            remember("tabs-before-options", app.tabs.len() as i64);
            overlay_click(app, "#x-list [data-ctl=\"options\"]");
        }
        263 => {
            check(
                "ext-options-open-in-a-new-tab",
                app.overlay.mode == Mode::Hidden && app.tabs.len() as i64 == recall("tabs-before-options") + 1,
                format!("mode {:?}, tabs {} (were {})", app.overlay.mode, app.tabs.len(), recall("tabs-before-options")),
            );
            log(&format!("INFO the options tab's address: {}", active_url(app)));
            page_eval(app, "ext-options-page-loaded", "`${location.href}|${document.title}`", |r| {
                check("ext-options-page-loaded", r == format!("chrome-extension://{EXT_ID}/options.html|XB Options"), &r);
            });
        }
        264 => {
            app.handle(UserEvent::Shortcut(Shortcut::CloseTab));
            // The tab under it tried the disabled extension's page and keeps
            // that address: give it a web page, for the popup's tabs.query.
            // (Loaded from Rust, as at step 260: no focus moves.)
            if let Some(v) = app.active_view() {
                let _ = v.load_url(&format!("http://127.0.0.1:{}/ext?n=9", recall("ext-port")));
            }
            // The strip's button: its list.
            let _ = app.ui.focus();
            ui_js(app, "document.getElementById('ext-btn').click()");
        }
        265 => {
            check("ext-button-opens-its-list", app.overlay.mode == Mode::ExtActions, format!("mode {:?}", app.overlay.mode));
            const JS: &str = "(() => { const b = document.querySelectorAll('#ext-actions button'); return `rows=${b.length};id=${b[0] && b[0].dataset.id};role=${b[0] && b[0].getAttribute('role')};name=${b[0] && b[0].textContent};focus=${document.activeElement === b[0]}`; })()";
            overlay_eval(app, "ext-list-has-the-popup", JS, |r| {
                check(
                    "ext-list-has-the-popup",
                    field(&r, "rows") == "1" && field(&r, "id") == EXT_ID && field(&r, "role") == "menuitem" && field(&r, "name") == "XB Selftest" && field(&r, "focus") == "true",
                    &r,
                );
            });
            overlay_click(app, "#ext-actions [data-id]");
        }
        266 => {
            log(&format!("INFO step 266: X-Browser is the foreground window: {}", crate::native::is_foreground(&app.window)));
            check(
                "ext-popup-opens",
                crate::popup::extension(app) == Some(EXT_ID) && crate::popup::view(app).is_some() && app.overlay.mode == Mode::Hidden,
                format!("popup {:?}, mode {:?}", crate::popup::extension(app), app.overlay.mode),
            );
            let want = active_url(app);
            const JS: &str = "`url=${document.body.dataset.url};id=${document.body.dataset.id};n=${document.body.dataset.n};href=${location.href};msg=${(() => { try { const w = window.chrome && window.chrome.webview; if (!w || typeof w.postMessage !== 'function') return 'none'; w.postMessage('xb-selftest'); return 'sent'; } catch (e) { return 'refused'; } })()}`";
            eval(crate::popup::view(app), "ext-popup-tabs-query", JS, move |r| {
                // WebView2's own tab for the active page, found by its
                // address: one tab, with WebView2's id.
                check(
                    "ext-popup-tabs-query",
                    want.starts_with("http")
                        && field(&r, "url") == want
                        && field(&r, "id") == "number"
                        && field(&r, "n") == "1"
                        && field(&r, "href") == format!("chrome-extension://{EXT_ID}/popup.html"),
                    format!("{r}, want {want}"),
                );
                // IsWebMessageEnabled false: WebView2 keeps postMessage, and
                // it throws.
                check("ext-popup-has-no-web-messages", matches!(field(&r, "msg"), "none" | "refused"), &r);
            });
            if let Some(v) = crate::popup::view(app) {
                log(&format!("INFO the popup's address: {}", v.url().unwrap_or_default()));
                let _ = v.evaluate_script("window.close()");
            }
        }
        267 => {
            check("ext-popup-closes-on-window-close", crate::popup::view(app).is_none(), "still open");
            // A tab switch closes it.
            crate::ext_ui::on_command(app, crate::ext_ui::ExtCmd::Actions(1100));
            crate::ext_ui::on_command(app, crate::ext_ui::ExtCmd::Popup(EXT_ID.into()));
            crate::ext_ui::on_command(app, crate::ext_ui::ExtCmd::OpenPopup);
            let opened = crate::popup::view(app).is_some();
            app.handle(UserEvent::Shortcut(Shortcut::NewTab));
            check("ext-popup-closes-on-tab-switch", opened && crate::popup::view(app).is_none(), format!("opened {opened}"));
            app.handle(UserEvent::Shortcut(Shortcut::CloseTab));
        }
        268 => {
            // Open again (a step after the tab close: the page's focus has
            // arrived), with a frame added once its page is parsed. The
            // frame is added only now, so the checks before do not depend
            // on it.
            crate::ext_ui::on_command(app, crate::ext_ui::ExtCmd::Actions(1100));
            crate::ext_ui::on_command(app, crate::ext_ui::ExtCmd::Popup(EXT_ID.into()));
            crate::ext_ui::on_command(app, crate::ext_ui::ExtCmd::OpenPopup);
            remember("ext-launches-refused", i64::from(crate::native::POPUP_LAUNCHES_REFUSED.load(Ordering::Relaxed)));
            if let Some(v) = crate::popup::view(app) {
                let _ = v.evaluate_script(EXT_FRAME_ADD_JS);
            }
        }
        269 => {
            log(&format!("INFO step 269: X-Browser is the foreground window: {}", crate::native::is_foreground(&app.window)));
            eval(crate::popup::view(app), "ext-popup-frame-has-its-link", EXT_FRAME_JS, |r| {
                check("ext-popup-frame-has-its-link", field(&r, "frame") == "true" && field(&r, "link") == "true" && num(&r, "w") > 0.0 && num(&r, "h") > 0.0, &r);
            });
            // A click on the frame's mail link (CDP input into the popup's
            // page, a user gesture there; nothing reaches the desktop).
            if let Some(v) = crate::popup::view(app) {
                cdp_click(v, 40.0, 40.0);
            }
        }
        270 => {
            // The frame's mail link: nothing started, nothing asked, the
            // popup still open. (Whether WebView2 raised its launch event
            // for the frame is only logged: a CDP click did not make it.)
            let refused = i64::from(crate::native::POPUP_LAUNCHES_REFUSED.load(Ordering::Relaxed)) - recall("ext-launches-refused");
            log(&format!("INFO the popup's launch handler refused {refused} launch(es)"));
            let waits = app.policy.launch.pending(std::time::Instant::now()).is_some();
            let open = crate::popup::view(app).is_some();
            check(
                "ext-popup-frame-launch-refused",
                open && !waits && app.overlay.mode == Mode::Hidden && launched().len() == 1,
                format!("refused {refused}, waits {waits}, mode {:?}, launched {:?}, popup open {open}", app.overlay.mode, launched()),
            );
            // Do its scripts still run after the frame's attempt? (Logged
            // only: says whether a refused launch holds up ExecuteScript.)
            if let Some(v) = crate::popup::view(app) {
                let _ = v.evaluate_script_with_callback("`${location.href}|${document.querySelector('iframe') && document.querySelector('iframe').contentWindow.location.href}`", |r| {
                    log(&format!("INFO the popup's scripts after the frame's launch: {r:?}"));
                });
            }
            // A window move closes it.
            remember("ext-popup-reopened", i64::from(open));
            if let Ok(p) = app.window.outer_position() {
                remember("ext-win-x", i64::from(p.x));
                remember("ext-win-y", i64::from(p.y));
                app.window.set_outer_position(tao::dpi::PhysicalPosition::new(p.x + 40, p.y + 30));
            }
        }
        271 => {
            check(
                "ext-popup-closes-on-window-move",
                recall("ext-popup-reopened") == 1 && crate::popup::view(app).is_none(),
                format!("reopened {}, maximized {}", recall("ext-popup-reopened"), app.window.is_maximized()),
            );
            app.window.set_outer_position(tao::dpi::PhysicalPosition::new(recall("ext-win-x") as i32, recall("ext-win-y") as i32));
            // Its page in a tab in the background: removal closes it.
            let _ = app.open_tab(&format!("chrome-extension://{EXT_ID}/options.html"), crate::tabs::Placement::End, false);
            remember("tabs-before-remove", app.tabs.len() as i64);
            // Removal, from the page: asked first.
            crate::ext_ui::open(app);
            overlay_click(app, "#x-list [data-ctl=\"remove\"]");
        }
        272 => {
            const ASK_JS: &str = "(() => { const no = document.querySelector('#x-list [data-ctl=\"no\"]'); return `ask=${!!no};text=${no ? no.parentElement.textContent : ''};focus=${document.activeElement.dataset.ctl}`; })()";
            overlay_eval(app, "ext-remove-asks-first", ASK_JS, |r| {
                check(
                    "ext-remove-asks-first",
                    field(&r, "ask") == "true" && field(&r, "text").contains("XB Selftest kaldırılsın mı? Uzantının ayarları da silinir.") && field(&r, "focus") == "no",
                    &r,
                );
            });
            overlay_js(app, &key_js("Escape"));
            overlay_eval(app, "ext-remove-esc-backs-out", ASK_JS, |r| {
                check("ext-remove-esc-backs-out", field(&r, "ask") == "false" && field(&r, "focus") == "remove", &r);
            });
            overlay_click(app, "#x-list [data-ctl=\"remove\"]");
            overlay_click(app, "#x-list [data-ctl=\"yes\"]");
        }
        273 => {
            // (e) Gone from the list, the file, WebView2 and the disk.
            let listed = app.extensions.list().iter().any(|l| l.id == EXT_ID) || app.extensions.records().iter().any(|e| e.id == EXT_ID);
            crate::store::flush(Duration::from_secs(2));
            let now = read_text(&dir().join("profile").join(crate::extensions::FILE));
            let folder = app.extensions.root().map_or_else(|| PathBuf::from("."), |r| r.join(EXT_ID));
            check(
                "ext-removed-from-the-list-file-and-disk",
                app.overlay.mode == Mode::Extensions && !listed && now.contains("\"items\":[]") && !folder.exists() && crate::ext_ui::allowed().is_empty(),
                format!("mode {:?}, listed {listed}, file {now}, folder {}, allowed {:?}", app.overlay.mode, folder.exists(), crate::ext_ui::allowed()),
            );
            const GONE_JS: &str = "`rows=${document.querySelectorAll('#x-list li').length};empty=${!document.getElementById('x-empty').hidden};status=${document.getElementById('x-status').textContent}`";
            overlay_eval(app, "ext-page-says-removed", GONE_JS, |r| {
                check("ext-page-says-removed", field(&r, "rows") == "0" && field(&r, "empty") == "true" && field(&r, "status") == "XB Selftest kaldırıldı.", &r);
            });
            let of_it = (0..app.tabs.len())
                .filter_map(|i| app.tabs.nth(i))
                .filter_map(|t| app.tabs.get(t))
                .filter(|t| t.url.contains(EXT_ID) || t.page_url.contains(EXT_ID))
                .count();
            check(
                "ext-remove-closes-its-pages",
                of_it == 0 && app.tabs.len() as i64 == recall("tabs-before-remove") - 1,
                format!("{of_it} tab(s) with its pages, {} tabs (were {})", app.tabs.len(), recall("tabs-before-remove")),
            );
            ui_eval(app, "ext-button-hidden-after-remove", "`${document.getElementById('ext-btn').hidden}`", |r| {
                check("ext-button-hidden-after-remove", r == "true", &r);
            });
            if let Some(v) = app.active_view() {
                crate::native::list_extensions(v, |r| {
                    let ids = r.map(|all| all.into_iter().map(|e| e.id).collect::<Vec<_>>());
                    check("ext-removed-from-webview2", ids.as_ref().is_ok_and(|ids| !ids.iter().any(|i| i == EXT_ID)), format!("{ids:?}"));
                });
            } else {
                check("ext-removed-from-webview2", false, "no content view");
            }
            // Typed, its address is refused, not searched.
            let before = active_url(app);
            app.handle(UserEvent::Ui(UiCommand::Navigate(format!("chrome-extension://{EXT_ID}/options.html"))));
            check("ext-typed-address-of-a-removed-extension-refused", active_url(app) == before, format!("{} (was {before})", active_url(app)));
            // Neither the strip nor the overlay may load an extension's
            // page, not even the built-in PDF viewer's, which their profile
            // has. (Last: a failure would leave them without their page.)
            let to = format!("chrome-extension://{}/index.html", crate::extensions::BUILTIN[1]);
            let _ = app.ui.load_url(&to);
            if let Some(v) = app.overlay.view() {
                let _ = v.load_url(&to);
            }
        }
        274 => {
            ui_eval(app, "ext-page-refused-in-the-ui", "`${location.href}`", |r| {
                check("ext-page-refused-in-the-ui", r.starts_with("http://xb.ui/"), &r);
            });
            overlay_eval(app, "ext-page-refused-in-the-overlay", "`${location.href}`", |r| {
                check("ext-page-refused-in-the-overlay", r.starts_with("http://xb.ui/"), &r);
            });
            // An extension WebView2 has but X-Browser did not install (as
            // after extensions.json is lost): put straight into WebView2.
            let folder = dir().join("unknown-ext");
            let _ = std::fs::remove_dir_all(&folder);
            let _ = write_test_extension(&folder, "1.2", false);
            if let Some(v) = app.active_view() {
                crate::native::add_extension(v, &folder, |r| log(&format!("INFO the unknown extension added: {:?}", r.map(|i| i.id))));
            }
        }
        275 => {
            // The keyboard in the strip, as when the menu is used. (The
            // typed address at 273 closed the page with the keyboard still
            // in the hidden overlay.)
            let _ = app.ui.focus();
            // The start's reconcile, as WebView2 lists it.
            let proxy = app.proxy().clone();
            if let Some(v) = app.active_view() {
                crate::native::list_extensions(v, move |r| {
                    let _ = proxy.send_event(UserEvent::Extension(crate::extensions::Event::Listed(r)));
                });
            }
        }
        276 => {
            let l = app.extensions.list().into_iter().find(|l| l.id == EXT_ID);
            let recorded = app.extensions.records().iter().any(|e| e.id == EXT_ID);
            check(
                "ext-unknown-turned-off-by-the-reconcile",
                l.as_ref().is_some_and(|l| l.source.is_none() && !l.enabled && l.installed) && !recorded && crate::ext_ui::allowed().is_empty(),
                format!("listed {:?}, recorded {recorded}, allowed {:?}", l.map(|l| (l.source, l.enabled, l.installed)), crate::ext_ui::allowed()),
            );
            crate::ext_ui::open(app);
        }
        277 => {
            overlay_eval(app, "ext-unknown-listed-and-marked", EXT_ROW_JS, |r| {
                check(
                    "ext-unknown-listed-and-marked",
                    field(&r, "rows") == "1"
                        && field(&r, "id") == EXT_ID
                        && field(&r, "text").contains("X-Browser dışında kurulmuş")
                        && field(&r, "on") == "false"
                        && field(&r, "options") == "false",
                    &r,
                );
            });
            overlay_click(app, "#x-list [data-ctl=\"switch\"]");
        }
        278 => {
            // On, and so allowed, but still not recorded (v0.3 task 5B).
            let on = app.extensions.list().iter().any(|l| l.id == EXT_ID && l.enabled);
            let recorded = app.extensions.records().iter().any(|e| e.id == EXT_ID);
            check(
                "ext-unknown-turned-on-not-recorded",
                on && !recorded && crate::ext_ui::allowed() == [EXT_ID] && app.overlay.mode == Mode::Extensions,
                format!("on {on}, recorded {recorded}, allowed {:?}, mode {:?}", crate::ext_ui::allowed(), app.overlay.mode),
            );
            overlay_click(app, "#x-list [data-ctl=\"remove\"]");
            overlay_click(app, "#x-list [data-ctl=\"yes\"]");
        }
        279 => {
            let listed = app.extensions.list().iter().any(|l| l.id == EXT_ID);
            check(
                "ext-unknown-removed",
                !listed && crate::ext_ui::allowed().is_empty() && app.overlay.mode == Mode::Extensions,
                format!("listed {listed}, allowed {:?}, mode {:?}", crate::ext_ui::allowed(), app.overlay.mode),
            );
            overlay_eval(app, "ext-unknown-page-says-removed", "`rows=${document.querySelectorAll('#x-list li').length};status=${document.getElementById('x-status').textContent}`", |r| {
                check("ext-unknown-page-says-removed", field(&r, "rows") == "0" && field(&r, "status") == "XB Selftest kaldırıldı.", &r);
            });
            if let Some(v) = app.active_view() {
                crate::native::list_extensions(v, |r| {
                    let ids = r.map(|all| all.into_iter().map(|e| e.id).collect::<Vec<_>>());
                    check("ext-unknown-removed-from-webview2", ids.as_ref().is_ok_and(|ids| !ids.iter().any(|i| i == EXT_ID)), format!("{ids:?}"));
                });
            } else {
                check("ext-unknown-removed-from-webview2", false, "no content view");
            }
        }
        280 => {
            // The Uzantılar page is still open from the last steps.
            crate::overlay::close(app);
            // First-run uBOL, against the fake store (`firstrun::fake`). The
            // planted file has no state (pending), and this start's try,
            // after the reconcile, was answered offline.
            use crate::firstrun::{FailureKind, Run, State, fake};
            crate::store::flush(Duration::from_secs(2));
            let file = read_text(&dir().join("profile").join(crate::extensions::FILE));
            let (state, run) = (app.extensions.first_run_ubol(), app.extensions.first_run.run);
            check(
                "firstrun-offline-leaves-pending",
                fake::starts() == 1 && state == State::Pending && run == Run::Over && file.contains(r#""first_run_ubol":"pending""#),
                format!("starts {}, {state:?}, {run:?}, file {file}", fake::starts()),
            );
            crate::firstrun::on_reconcile_done(app);
            check("firstrun-no-second-try-in-the-same-start", fake::starts() == 1, format!("starts {}", fake::starts()));
            // The next start: busy, then the one more try succeeds.
            fake::next_start(app);
            fake::plan(Err(FailureKind::Busy));
            fake::plan(Ok("2026.1116.1650".into()));
            crate::firstrun::on_reconcile_done(app);
        }
        281 => {
            use crate::firstrun::{Run, State, fake};
            let (state, run) = (app.extensions.first_run_ubol(), app.extensions.first_run.run);
            check(
                "firstrun-busy-waits-for-the-next-install",
                fake::starts() == 2 && state == State::Pending && run == Run::AfterBusy,
                format!("starts {}, {state:?}, {run:?}", fake::starts()),
            );
            // Another store install finishes.
            fake::send(app, EXT_ID, Ok("1.0".into()));
        }
        282 => {
            use crate::firstrun::{Run, State, fake};
            let profile = dir().join("profile");
            crate::store::flush(Duration::from_secs(2));
            let file = read_text(&profile.join(crate::extensions::FILE));
            let (state, run) = (app.extensions.first_run_ubol(), app.extensions.first_run.run);
            check(
                "firstrun-busy-tries-once-more-and-writes-done",
                fake::starts() == 3 && state == State::Done && run == Run::Over && file.contains(r#""first_run_ubol":"done""#),
                format!("starts {}, {state:?}, {run:?}, file {file}", fake::starts()),
            );
            let back = crate::store::Store::<crate::extensions::ExtFile>::new(&profile).load().value.first_run_ubol;
            check("firstrun-done-is-read-back", back == State::Done, format!("{back:?}"));
            fake::next_start(app);
            crate::firstrun::on_reconcile_done(app);
            check("firstrun-done-does-not-download-again", fake::starts() == 3, format!("starts {}", fake::starts()));
            // The user removes it (WebView2 answers: not installed).
            remember("ext-op", crate::extensions::remove(app, crate::firstrun::UBOL_ID) as i64);
        }
        283 => {
            use crate::firstrun::{Run, State, UBOL_ID, fake};
            let outcome = ext_outcome(app, "ext-op");
            crate::store::flush(Duration::from_secs(2));
            let file = read_text(&dir().join("profile").join(crate::extensions::FILE));
            let state = app.extensions.first_run_ubol();
            check(
                "firstrun-removal-makes-it-user-removed",
                outcome == Some(Ok(UBOL_ID.to_string())) && state == State::UserRemoved && file.contains(r#""first_run_ubol":"user_removed""#),
                format!("outcome {outcome:?}, {state:?}, file {file}"),
            );
            fake::next_start(app);
            fake::plan(Ok("2026.1116.1650".into()));
            crate::firstrun::on_reconcile_done(app);
            crate::firstrun::on_view_created(app);
            crate::firstrun::on_install_result(app, EXT_ID, Ok("1.0".into()));
            let run = app.extensions.first_run.run;
            check(
                "firstrun-user-removed-never-reinstalls",
                fake::starts() == 3 && run == Run::NotYet && app.extensions.first_run_ubol() == State::UserRemoved,
                format!("starts {}, {run:?}", fake::starts()),
            );
        }
        284 => {
            // The extension back, for the remove question below.
            let again = dir().join("profile").join(crate::extensions::DIR).join(EXT_ID).join("2.0");
            match write_test_extension(&again, "2.0", false) {
                Ok(()) => remember("ext-op", crate::extensions::add(app, &again, crate::extensions::Source::Unpacked) as i64),
                Err(e) => check("ext-test-extension-written", false, e),
            }
            app.handle(UserEvent::Ui(UiCommand::Navigate("https://example.com/".into())));
        }
        285 => {
            // Not a store detail page: no store button.
            eval(Some(&app.ui), "store-button-off-elsewhere", "String(document.getElementById('ext-store').hidden)", |r| {
                check("store-button-off-elsewhere", r == "true", r);
            });
            // Another site's page goes to the sentinel itself: cancelled, never asked.
            let to = crate::cws::sentinel_url(crate::cws::Action::Install, EXT_ID);
            page_js(app, &format!("location.href = '{to}'"));
        }
        286 => {
            let id = app.tabs.active().unwrap();
            let loading = app.tabs.get(id).is_some_and(|t| t.loading);
            check(
                "store-sentinel-from-another-page-is-refused",
                app.overlay.mode != Mode::Store && app.webstore.busy_with().is_none() && !loading && active_url(app).starts_with("https://example.com/"),
                format!("{:?}, job {:?}, loading {loading}, url {}", app.overlay.mode, app.webstore.busy_with(), active_url(app)),
            );
            // The question itself, as the button on a detail page opens it.
            crate::webstore::on_store_button(app, id, crate::cws::Action::Remove, EXT_ID);
        }
        287 => {
            log(&format!(
                "INFO step 283: X-Browser is the foreground window: {}; extension added again: {:?}",
                crate::native::is_foreground(&app.window),
                ext_outcome(app, "ext-op")
            ));
            let shown = app.overlay.view().is_some_and(visible);
            check(
                "store-question-opens",
                app.overlay.mode == Mode::Store && shown && app.webstore.busy_with() == Some(EXT_ID),
                format!("{:?}, visible {shown}, job {:?}", app.overlay.mode, app.webstore.busy_with()),
            );
            eval(app.overlay.view(), "store-question-shows-the-removal", STORE_ASK_JS, |r| {
                check(
                    "store-question-shows-the-removal",
                    field(&r, "hidden") == "false"
                        && field(&r, "launch") == "true"
                        && field(&r, "title") == "Bu uzantı kaldırılsın mı?"
                        && field(&r, "yes") == "Kaldır"
                        && field(&r, "meta").contains(EXT_ID)
                        && field(&r, "perms") == "true"
                        && field(&r, "focus") == "x-no",
                    r,
                );
            });
            app.handle(UserEvent::Ui(UiCommand::Store(crate::webstore::StoreCmd::No)));
        }
        288 => {
            check(
                "store-question-no-closes",
                overlay_closed(app) && app.webstore.busy_with().is_none(),
                format!("{:?}, job {:?}", app.overlay.mode, app.webstore.busy_with()),
            );
        }
        289 => {
            // Store updates (task 5B), against the fake store
            // (`webstore::fake`): a fresh self-signed key, its packages at
            // 1.0 to 1.4, the 1.0 one behind the store URL and the others at
            // their own addresses. All want storage; 1.3 also wants a site.
            use crate::crx::testpkg::{TestKey, crx, stored, zip};
            use crate::webstore::fake;
            fake::clear();
            let key = TestKey::rsa();
            let id = key.id();
            let mut sums = Vec::new();
            for v in ["1.0", "1.1", "1.2", "1.3", "1.4"] {
                let hosts = if v == "1.3" { r#","host_permissions":["https://example.com/*"]"# } else { "" };
                let manifest = format!(r#"{{"manifest_version":3,"name":"XB Update","version":"{v}","permissions":["storage"]{hosts}}}"#);
                let body = crx(&zip(&[stored("manifest.json", manifest.as_bytes())]), &[&key]);
                let hex: String = crate::crx::sha256(&[&body]).unwrap_or_default().iter().map(|b| format!("{b:02x}")).collect();
                sums.push((v.to_owned(), hex, body.len()));
                if v == "1.0" {
                    fake::serve(&format!("{STORE_URL}?response=redirect"), 200, body.clone());
                }
                fake::serve(&update_codebase(v), 200, body);
            }
            UPDATES.with(|u| *u.borrow_mut() = Some((id.clone(), sums)));
            crate::webstore::install_from_store(app, &id, false);
        }
        290 => {
            let id = update_id();
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| (e.version.clone(), e.source, e.dir.clone()));
            check(
                "update-fixture-installed-from-the-fake-store",
                rec.as_ref().is_some_and(|(v, s, d)| v == "1.0" && *s == crate::extensions::Source::Cws && d.ends_with("1.0"))
                    && app.overlay.mode != Mode::Store
                    && app.webstore.busy_with().is_none(),
                format!("record {rec:?}, {:?}, asked {:?}", app.overlay.mode, crate::webstore::fake::asked()),
            );
            remember("ext-op", crate::extensions::set_enabled(app, &id, false) as i64);
        }
        291 => {
            let id = update_id();
            let outcome = ext_outcome(app, "ext-op");
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| e.enabled);
            check("update-fixture-turned-off", outcome == Some(Ok(id.clone())) && rec == Some(false), format!("outcome {outcome:?}, enabled {rec:?}"));
            // An extension that is off is not checked at all.
            remember("upd-asked", crate::webstore::fake::asked().len() as i64);
            serve_update_check(&id, "1.1", None);
            crate::webstore::check_updates(app);
        }
        292 => {
            let id = update_id();
            let asked = crate::webstore::fake::asked();
            let new = &asked[(recall("upd-asked") as usize).min(asked.len())..];
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| (e.version.clone(), e.enabled));
            check(
                "update-leaves-an-extension-that-is-off-alone",
                new.is_empty() && rec == Some(("1.0".into(), false)) && app.webstore.busy_with().is_none(),
                format!("asked {new:?}, record {rec:?}, job {:?}", app.webstore.busy_with()),
            );
            remember("ext-op", crate::extensions::set_enabled(app, &id, true) as i64);
        }
        293 => {
            let id = update_id();
            let outcome = ext_outcome(app, "ext-op");
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| e.enabled);
            let listed = app.extensions.list().iter().find(|l| l.id == id).map(|l| l.enabled);
            check(
                "update-fixture-turned-on",
                outcome == Some(Ok(id.clone())) && rec == Some(true) && listed == Some(true),
                format!("outcome {outcome:?}, enabled {rec:?}, listed {listed:?}"),
            );
            serve_update_check(&id, "1.1", None);
            crate::webstore::check_updates(app);
        }
        294 => {
            // A newer version: fetched from its address, in without a
            // question, and still on.
            let id = update_id();
            let asked = crate::webstore::fake::asked();
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| (e.version.clone(), e.enabled, e.dir.ends_with("1.1")));
            check(
                "update-newer-version-goes-in-silently",
                rec.as_ref().is_some_and(|(v, on, d)| v == "1.1" && *on && *d)
                    && asked.iter().any(|u| *u == update_codebase("1.1"))
                    && app.overlay.mode != Mode::Store
                    && app.webstore.busy_with().is_none(),
                format!("record {rec:?}, {:?}, job {:?}, asked {asked:?}", app.overlay.mode, app.webstore.busy_with()),
            );
            // The same version offered again: nothing to fetch.
            remember("upd-asked", asked.len() as i64);
            serve_update_check(&id, "1.1", None);
            crate::webstore::check_updates(app);
        }
        295 => {
            let id = update_id();
            let home = app.extensions.root().map_or_else(|| PathBuf::from("."), |r| r.join(&id));
            check(
                "update-deletes-the-old-version-folder",
                !home.join("1.0").exists() && home.join("1.1").join("manifest.json").is_file(),
                format!("1.0 {}, 1.1 {}", home.join("1.0").exists(), home.join("1.1").exists()),
            );
            let asked = crate::webstore::fake::asked();
            let new = &asked[(recall("upd-asked") as usize).min(asked.len())..];
            check(
                "update-same-version-fetches-nothing",
                new.len() == 1 && new[0].contains("response=updatecheck") && app.webstore.busy_with().is_none(),
                format!("asked {new:?}, job {:?}", app.webstore.busy_with()),
            );
            // A download that is not the one the check named.
            serve_update_check(&id, "1.2", Some(&"00".repeat(32)));
            crate::webstore::check_updates(app);
        }
        296 => {
            let id = update_id();
            let home = app.extensions.root().map_or_else(|| PathBuf::from("."), |r| r.join(&id));
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| e.version.clone());
            let asked = crate::webstore::fake::asked();
            check(
                "update-hash-mismatch-changes-nothing",
                rec.as_deref() == Some("1.1")
                    && asked.iter().any(|u| *u == update_codebase("1.2"))
                    && !home.join("1.2").exists()
                    && app.overlay.mode != Mode::Store
                    && app.webstore.busy_with().is_none(),
                format!("record {rec:?}, 1.2 {}, {:?}, asked {asked:?}", home.join("1.2").exists(), app.overlay.mode),
            );
            // The same broken version offered again: not fetched again.
            remember("upd-asked", asked.len() as i64);
            serve_update_check(&id, "1.2", Some(&"00".repeat(32)));
            crate::webstore::check_updates(app);
        }
        297 => {
            let asked = crate::webstore::fake::asked();
            let new = &asked[(recall("upd-asked") as usize).min(asked.len())..];
            check(
                "update-failed-version-is-not-fetched-again",
                new.len() == 1 && new[0].contains("response=updatecheck") && app.webstore.busy_with().is_none(),
                format!("asked {new:?}, job {:?}", app.webstore.busy_with()),
            );
            // A version that wants more, while the menu is open: no
            // question over it. X-Browser counts as in front from here to
            // step 301 whoever uses the desktop (`App::in_front`), so the
            // menu does not close itself either.
            app.foreground = Some(true);
            let sc = press(app, vk::F, ALT);
            log(&format!("INFO step 297: X-Browser is really the foreground window: {}; Alt+F {sc:?}", crate::native::is_foreground(&app.window)));
            serve_update_check(&update_id(), "1.3", None);
            crate::webstore::check_updates(app);
        }
        298 => {
            let id = update_id();
            let home = app.extensions.root().map_or_else(|| PathBuf::from("."), |r| r.join(&id));
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| e.version.clone());
            let asked = crate::webstore::fake::asked();
            check(
                "update-does-not-ask-over-the-menu",
                app.overlay.mode == Mode::Menu
                    && app.webstore.busy_with().is_none()
                    && rec.as_deref() == Some("1.1")
                    && asked.iter().any(|u| *u == update_codebase("1.3"))
                    && !home.join("1.3").exists(),
                format!("{:?}, job {:?}, record {rec:?}, 1.3 {}, asked {asked:?}", app.overlay.mode, app.webstore.busy_with(), home.join("1.3").exists()),
            );
            overlay_js(app, &key_js("Escape"));
        }
        299 => {
            check("update-menu-closes", overlay_closed(app), format!("{:?}", app.overlay.mode));
            // In front (step 297), nothing else on screen: it asks.
            serve_update_check(&update_id(), "1.3", None);
            crate::webstore::check_updates(app);
        }
        300 => {
            let id = update_id();
            let shown = app.overlay.view().is_some_and(visible);
            check(
                "update-with-new-permissions-asks",
                app.overlay.mode == Mode::Store && shown && app.webstore.busy_with() == Some(id.as_str()),
                format!("{:?}, visible {shown}, job {:?}", app.overlay.mode, app.webstore.busy_with()),
            );
            // Only the site is new: storage was there before.
            eval(app.overlay.view(), "update-question-lists-only-new-permissions", STORE_ASK_JS, move |r| {
                check(
                    "update-question-lists-only-new-permissions",
                    field(&r, "hidden") == "false"
                        && field(&r, "title") == "Bu uzantı güncellensin mi?"
                        && field(&r, "yes") == "Güncelle"
                        && field(&r, "meta").contains("Sürüm 1.3")
                        && field(&r, "meta").contains(&id)
                        && field(&r, "note") == "Yeni sürüm şu yeni izinleri istiyor:"
                        && field(&r, "perms") == "false"
                        && field(&r, "items") == "1"
                        && field(&r, "list").contains("example.com"),
                    r,
                );
            });
            app.handle(UserEvent::Ui(UiCommand::Store(crate::webstore::StoreCmd::No)));
        }
        301 => {
            let id = update_id();
            let home = app.extensions.root().map_or_else(|| PathBuf::from("."), |r| r.join(&id));
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| (e.version.clone(), e.enabled));
            check(
                "update-question-no-keeps-the-old-version",
                overlay_closed(app) && app.webstore.busy_with().is_none() && rec == Some(("1.1".into(), true)) && !home.join("1.3").exists(),
                format!("{:?}, job {:?}, record {rec:?}, 1.3 {}", app.overlay.mode, app.webstore.busy_with(), home.join("1.3").exists()),
            );
            // The same version with X-Browser in the background: fetched,
            // but no question, and the job ends.
            app.foreground = Some(false);
            let asked = crate::webstore::fake::asked();
            remember("upd-asked", asked.iter().filter(|u| **u == update_codebase("1.3")).count() as i64);
            serve_update_check(&id, "1.3", None);
            crate::webstore::check_updates(app);
        }
        302 => {
            let id = update_id();
            let home = app.extensions.root().map_or_else(|| PathBuf::from("."), |r| r.join(&id));
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| e.version.clone());
            let fetched = crate::webstore::fake::asked().iter().filter(|u| **u == update_codebase("1.3")).count() as i64;
            check(
                "update-does-not-ask-in-the-background",
                overlay_closed(app)
                    && app.webstore.busy_with().is_none()
                    && rec.as_deref() == Some("1.1")
                    && fetched > recall("upd-asked")
                    && !home.join("1.3").exists(),
                format!("{:?}, job {:?}, record {rec:?}, 1.3 fetched {fetched} times, 1.3 {}", app.overlay.mode, app.webstore.busy_with(), home.join("1.3").exists()),
            );
            app.foreground = None;
            // An update found while the remove question is open waits for it.
            let tab = app.tabs.active().unwrap();
            crate::webstore::on_store_button(app, tab, crate::cws::Action::Remove, &id);
            serve_update_check(&id, "1.4", None);
            crate::webstore::check_updates(app);
        }
        303 => {
            let id = update_id();
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| e.version.clone());
            let asked = crate::webstore::fake::asked();
            check(
                "update-waits-for-the-open-question",
                app.overlay.mode == Mode::Store
                    && app.webstore.busy_with() == Some(id.as_str())
                    && rec.as_deref() == Some("1.1")
                    && !asked.iter().any(|u| *u == update_codebase("1.4")),
                format!("{:?}, job {:?}, record {rec:?}, asked {asked:?}", app.overlay.mode, app.webstore.busy_with()),
            );
            app.handle(UserEvent::Ui(UiCommand::Store(crate::webstore::StoreCmd::No)));
        }
        304 => {
            let id = update_id();
            let home = app.extensions.root().map_or_else(|| PathBuf::from("."), |r| r.join(&id));
            let rec = app.extensions.records().iter().find(|e| e.id == id).map(|e| (e.version.clone(), e.enabled));
            check(
                "update-runs-after-the-question-closes",
                overlay_closed(app)
                    && app.webstore.busy_with().is_none()
                    && rec == Some(("1.4".into(), true))
                    && home.join("1.4").join("manifest.json").is_file()
                    && !home.join("1.1").exists(),
                format!("{:?}, job {:?}, record {rec:?}, 1.1 {}", app.overlay.mode, app.webstore.busy_with(), home.join("1.1").exists()),
            );
            // The daily timer, with the last check long ago.
            app.extensions.set_last_update_check(0);
            remember("upd-asked", crate::webstore::fake::asked().len() as i64);
            app.handle(UserEvent::UpdateTimer);
        }
        305 => {
            let asked = crate::webstore::fake::asked();
            let new = &asked[(recall("upd-asked") as usize).min(asked.len())..];
            let now = crate::history::now_secs();
            let last = app.extensions.last_update_check();
            check(
                "update-timer-checks-when-due",
                new.len() == 1 && new[0].contains("response=updatecheck") && last.is_some_and(|l| l + 60 >= now && l <= now + 60),
                format!("asked {new:?}, last {last:?}, now {now}"),
            );
            // A tick right after: not due.
            remember("upd-asked", asked.len() as i64);
            app.handle(UserEvent::UpdateTimer);
        }
        306 => {
            let asked = crate::webstore::fake::asked();
            let new = &asked[(recall("upd-asked") as usize).min(asked.len())..];
            check("update-timer-waits-a-day", new.is_empty() && app.webstore.busy_with().is_none(), format!("asked {new:?}"));
        }
        307 => {
            crate::overlay::close(app);
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

/// The update service, as the fake store answers it (`cws::UPDATE_SERVICE`).
const STORE_URL: &str = "https://clients2.google.com/service/update2/crx";

/// The update steps' extension: its id, and `(version, sha256 hex, size)`
/// of each package the fake store holds.
type UpdateFixture = (String, Vec<(String, String, usize)>);

thread_local! {
    static UPDATES: std::cell::RefCell<Option<UpdateFixture>> = const { std::cell::RefCell::new(None) };
}

fn update_id() -> String {
    UPDATES.with(|u| u.borrow().as_ref().map(|(id, _)| id.clone()).unwrap_or_default())
}

/// Where the fake store keeps the update steps' package of `version`.
fn update_codebase(version: &str) -> String {
    format!("https://fake.example/xb-update-{version}.crx")
}

/// Makes the fake update service offer `version` of extension `id`, with
/// the package's own hash unless `hash` says otherwise.
fn serve_update_check(id: &str, version: &str, hash: Option<&str>) {
    let (sum, size) = UPDATES.with(|u| {
        let u = u.borrow();
        let found = u.as_ref().and_then(|(_, s)| s.iter().find(|(v, ..)| v == version).cloned());
        found.map(|(_, h, n)| (h, n)).unwrap_or_default()
    });
    let hash = hash.unwrap_or(&sum);
    let codebase = update_codebase(version);
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><gupdate xmlns="http://www.google.com/update2/response" protocol="2.0" server="prod"><app appid="{id}" status="ok"><updatecheck codebase="{codebase}" hash_sha256="{hash}" size="{size}" status="ok" version="{version}"/></app></gupdate>"#
    );
    crate::webstore::fake::serve(&format!("{STORE_URL}?response=updatecheck"), 200, xml.into_bytes());
}

/// The self-test extension (test-assets/selftest-ext), written out by the run.
const EXT_MANIFEST: &str = include_str!("../test-assets/selftest-ext/manifest.json");
const EXT_RULES: &str = include_str!("../test-assets/selftest-ext/rules.json");
const EXT_CS: &str = include_str!("../test-assets/selftest-ext/cs.js");
const EXT_POPUP_HTML: &str = include_str!("../test-assets/selftest-ext/popup.html");
const EXT_POPUP_JS: &str = include_str!("../test-assets/selftest-ext/popup.js");
const EXT_OPTIONS_HTML: &str = include_str!("../test-assets/selftest-ext/options.html");
const EXT_FRAME_HTML: &str = include_str!("../test-assets/selftest-ext/frame.html");
/// Adds a frame with `frame.html` over the whole popup, once the popup's
/// page is parsed (a script queued during its load runs before `body`).
const EXT_FRAME_ADD_JS: &str = "(() => { const add = () => document.body.insertAdjacentHTML('beforeend', \
  '<iframe src=\"frame.html\" title=\"frame\" style=\"position:fixed;inset:0;width:100%;height:100%;border:0\"></iframe>'); \
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', add); else add(); return 1; })()";
/// The popup's frame and its mail link, as laid out.
const EXT_FRAME_JS: &str = "(() => { const f = document.querySelector('iframe'), d = f && f.contentDocument, a = d && d.getElementById('m'), \
  r = a && a.getBoundingClientRect(); return `frame=${!!f};link=${!!a};w=${r ? r.width : 0};h=${r ? r.height : 0};src=${f ? f.src : ''}`; })()";
/// Its id, as Chrome derives it from the manifest's key.
const EXT_ID: &str = "mmbkafeccnpmgicaldnppdljpaogpjdm";

/// Writes the self-test extension into `dir`, as `version`. Without
/// `popup` its manifest declares no `action`: the strip's extensions button
/// then stays hidden, as it is for the steps before 251 (the tab strip's
/// width checks).
fn write_test_extension(dir: &std::path::Path, version: &str, popup: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut manifest = EXT_MANIFEST.replace("\"version\": \"1.0\"", &format!("\"version\": \"{version}\""));
    if !popup {
        manifest = manifest.replace("  \"action\": { \"default_popup\": \"popup.html\" },
", "");
    }
    std::fs::write(dir.join("manifest.json"), manifest)?;
    std::fs::write(dir.join("rules.json"), EXT_RULES)?;
    std::fs::write(dir.join("popup.html"), EXT_POPUP_HTML)?;
    std::fs::write(dir.join("popup.js"), EXT_POPUP_JS)?;
    std::fs::write(dir.join("options.html"), EXT_OPTIONS_HTML)?;
    std::fs::write(dir.join("frame.html"), EXT_FRAME_HTML)?;
    std::fs::write(dir.join("cs.js"), EXT_CS)
}

/// Half-written folders under the extensions folder, as an interrupted
/// unpack, update or remove leaves them.
fn ext_leftovers() -> [String; 3] {
    [".selftest.unpack-1-0".into(), format!("{EXT_ID}/0.9.tmp"), format!(".{EXT_ID}.removed-1")]
}

/// Gives the start something to reconcile: the extension recorded at 1.0
/// next to two bad entries (WebView2 may not have it: the last run removed
/// it), and leftovers to sweep.
fn plant_extensions(profile: &std::path::Path) {
    let root = profile.join(crate::extensions::DIR);
    let _ = std::fs::remove_dir_all(&root);
    let v1 = root.join(EXT_ID).join("1.0");
    let _ = write_test_extension(&v1, "1.0", false);
    for l in ext_leftovers() {
        let _ = write_test_extension(&root.join(l), "0.9", false);
    }
    let dir = v1.to_string_lossy();
    let file = serde_json::json!({ "v": 1, "items": [
        { "id": 7, "dir": dir },
        { "id": "not an id", "dir": dir, "source": "unpacked" },
        { "id": EXT_ID, "dir": dir, "source": "unpacked", "version": "1.0", "name": "XB Selftest", "enabled": true, "installed": 1 },
    ]});
    let _ = std::fs::write(profile.join(crate::extensions::FILE), file.to_string());
}

fn read_text(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// How the operation remembered under `key` ended.
fn ext_outcome(app: &App, key: &'static str) -> Option<Result<String, String>> {
    app.extensions.outcome(recall(key) as u64).cloned()
}

/// Checks that only the two built-ins are in the profile of `view` (the UI
/// strip's or the overlay's).
fn ui_profile_only_builtins(view: Option<&wry::WebView>, name: &'static str) {
    let Some(v) = view else { return check(name, false, "no webview") };
    crate::native::list_extensions(v, move |r| {
        let ids = r.map(|all| {
            let mut ids: Vec<_> = all.into_iter().map(|e| e.id).collect();
            ids.sort();
            ids
        });
        let mut want: Vec<String> = crate::extensions::BUILTIN.iter().map(|s| s.to_string()).collect();
        want.sort();
        check(name, ids.as_ref() == Ok(&want), format!("{ids:?}"));
    });
}

/// Checks the extension's state after a `set_enabled`.
fn ext_state_check(app: &App, name: &'static str, on: bool) {
    let outcome = ext_outcome(app, "ext-op");
    let rec = app.extensions.records().iter().find(|e| e.id == EXT_ID).map(|e| e.enabled);
    let listed = app.extensions.list().iter().find(|l| l.id == EXT_ID).map(|l| l.enabled);
    check(
        name,
        outcome == Some(Ok(EXT_ID.to_string())) && rec == Some(on) && listed == Some(on),
        format!("outcome {outcome:?}, record {rec:?}, listed {listed:?}"),
    );
}

/// The extension's row on the Uzantılar page.
const EXT_ROW_JS: &str = "(() => { const li = document.querySelectorAll('#x-list li'); const r = li[0]; if (!r) return `rows=0`; const sw = r.querySelector('[data-ctl=\"switch\"]'); const op = r.querySelector('[data-ctl=\"options\"]'); const st = r.querySelector('[role=\"status\"]'); return `rows=${li.length};id=${r.dataset.id};text=${r.textContent};on=${sw.checked};role=${sw.getAttribute('role')};label=${sw.getAttribute('aria-label')};options=${!op.disabled};busy=${r.hasAttribute('aria-busy')};state=${st ? st.textContent : ''};focus=${document.activeElement.dataset.ctl};empty=${document.getElementById('x-empty').hidden}`; })()";

/// Checks the extension's state after the page's switch: no operation left
/// waiting, and the tabs' list of extension pages follows.
fn ext_page_state_check(app: &App, name: &'static str, on: bool) {
    let rec = app.extensions.records().iter().find(|e| e.id == EXT_ID).map(|e| e.enabled);
    let listed = app.extensions.list().iter().find(|l| l.id == EXT_ID).map(|l| l.enabled);
    let allowed = crate::ext_ui::allowed();
    check(
        name,
        rec == Some(on) && listed == Some(on) && allowed.iter().any(|a| a == EXT_ID) == on && app.overlay.mode == Mode::Extensions,
        format!("record {rec:?}, listed {listed:?}, allowed {allowed:?}, mode {:?}", app.overlay.mode),
    );
}

/// Loads the ad page, round `n`.
fn ext_load(app: &mut App, n: u32) {
    let port = recall("ext-port");
    app.handle(UserEvent::Ui(UiCommand::Navigate(format!("http://127.0.0.1:{port}/ext?n={n}"))));
}

/// Checks round `n` of the ad page: with the extension on, the ad script
/// never reached the server and the content script marked the page; off,
/// the ad script ran and nothing marked the page.
fn ext_page_check(app: &App, n: u32, on: bool) {
    let hits = ext_hits();
    let (ok, ad) = (format!("/xb-ok.js?n={n}"), format!("/xb-ad.js?n={n}"));
    let (got_ok, got_ad) = (hits.contains(&ok), hits.contains(&ad));
    let name = match n {
        1 => "ext-rule-blocks-the-ad-script",
        2 => "ext-disabled-lets-the-ad-script-through",
        _ => "ext-enabled-again-blocks-it-again",
    };
    check(name, got_ok && got_ad != on, format!("ok {got_ok}, ad {got_ad}, hits {hits:?}"));
    let page = match n {
        1 => "ext-page-marked-and-ad-not-run",
        2 => "ext-page-unmarked-and-ad-run",
        _ => "ext-page-marked-and-ad-not-run-again",
    };
    page_eval(
        app,
        page,
        "(() => { const d = document.documentElement.dataset; return `ext=${d.xbext || ''};ok=${d.ok || ''};ad=${d.ad || ''};url=${location.href}`; })()",
        move |r| {
            let mark = if on { "1" } else { "" };
            let want_ad = if on { "" } else { "1" };
            check(page, field(&r, "ext") == mark && field(&r, "ok") == "1" && field(&r, "ad") == want_ad && field(&r, "url").ends_with(&format!("n={n}")), &r);
        },
    );
}

/// Request paths the ad page's server has seen.
static EXT_HITS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn ext_hits() -> Vec<String> {
    EXT_HITS.lock().unwrap().clone()
}

/// Serves the ad page on a free loopback port until the process ends:
/// `/ext?<q>` loads `/xb-ok.js?<q>` and `/xb-ad.js?<q>`, each of which marks
/// the page; every request path is recorded. Nothing is cached.
fn serve_ext_page() -> std::io::Result<u16> {
    use std::io::Read;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            EXT_HITS.lock().unwrap().push(path.clone());
            let query = path.split_once('?').map_or("", |(_, q)| q);
            let (kind, body) = if path.starts_with("/ext") {
                ("text/html", format!("<!doctype html><title>Ext</title><script src=\"/xb-ok.js?{query}\"></script><script src=\"/xb-ad.js?{query}\"></script><body>ext</body>"))
            } else if path.starts_with("/xb-ok.js") {
                ("text/javascript", "document.documentElement.dataset.ok = '1';".to_string())
            } else if path.starts_with("/xb-ad.js") {
                ("text/javascript", "document.documentElement.dataset.ad = '1';".to_string())
            } else {
                ("text/plain", String::new())
            };
            let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    Ok(port)
}

/// What the served download contains.
const DOWNLOAD_BODY: &str = "x-browser download test
";

/// A local server on 127.0.0.1 that answers every request with a small file
/// as an attachment (the only download the self-test makes; nothing is
/// fetched from the internet and the file is never opened). Returns its port.
fn serve_attachment() -> Option<u16> {
    use std::io::Read as _;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = s.read(&mut [0u8; 2048]);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                 Content-Disposition: attachment; filename=\"xb-test.txt\"\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                DOWNLOAD_BODY.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(DOWNLOAD_BODY.as_bytes());
        }
    });
    Some(port)
}

/// JS that sends a key-down to the overlay page.
fn key_js(key: &str) -> String {
    format!("document.dispatchEvent(new KeyboardEvent('keydown', {{ key: '{key}', bubbles: true, cancelable: true }}));")
}

fn overlay_eval(app: &App, name: &'static str, js: &str, then: impl Fn(String) + Send + 'static) {
    eval(app.overlay.view(), name, js, then);
}

/// Checks which menu item has the focus (its `data-item`).
fn overlay_active(app: &App, name: &'static str, want: &'static str) {
    overlay_eval(app, name, "document.activeElement.dataset.item || 'none'", move |r| {
        check(name, r == want, format!("{r}, want {want}"));
    });
}

fn overlay_click(app: &App, selector: &str) {
    overlay_js(app, &format!("document.querySelector('{selector}').click()"));
}

fn tab_zoom(app: &App) -> u16 {
    app.tabs.active().and_then(|id| app.tabs.get(id)).map_or(0, |t| t.zoom)
}

/// What the policy steps ask to open. Never opened: `record_launch` stands
/// in for the launcher, and the address is not a real one.
const MAIL_URI: &str = "mailto:selftest@example.invalid?subject=x-browser-selftest";

static LAUNCHED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn record_launch(uri: &str) -> bool {
    LAUNCHED.lock().unwrap().push(uri.to_string());
    true
}

fn launched() -> Vec<String> {
    LAUNCHED.lock().unwrap().clone()
}

/// Serves one plain page over http on a free loopback port, until the
/// process ends. Returns the port.
fn serve_http_page() -> std::io::Result<u16> {
    use std::io::Read;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.read(&mut [0u8; 2048]);
            let body = "<!doctype html><title>Plain</title><body>plain http</body>";
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    Ok(port)
}

/// A real (CDP) click in the middle of the active page: a user gesture.
fn click_page(app: &App) {
    if let Some(v) = app.tabs.active().and_then(|id| app.view(id)) {
        cdp_click(v, 200.0, 200.0);
    }
}

/// The address box's connection mark as drawn, and as told to a screen
/// reader (`name`); `reload`: its button is shown and reachable by Tab.
const SECURITY_JS: &str = "(() => { const g = (id) => document.getElementById(id), m = g('security'), r = m.getBoundingClientRect(), \
  s = g('security-status'), b = g('security-reload'); \
  return [`hidden=${m.hidden}`, `state=${m.dataset.state || ''}`, `tag=${m.tagName}`, `role=${m.getAttribute('role') || ''}`, \
  `text=${m.hidden ? '' : g('security-text').textContent}`, `name=${s.textContent}`, `live=${s.getAttribute('aria-live')}`, \
  `reload=${!m.hidden && !b.hidden && b.tagName === 'BUTTON' && b.tabIndex === 0 && b.getBoundingClientRect().width > 0}`, \
  `w=${r.width}`, `lock=${getComputedStyle(m.querySelector('svg')).display}`, `addr=${g('address').value}`].join(';'); })()";

/// The launch question as the overlay page draws it. `open`: its "Aç" button
/// is "hidden", "disabled" (shown as off, `aria-disabled`) or "on";
/// `native`: it is a `disabled` button, which would swallow the presses that
/// restart the wait; `failed`: the failure line is shown.
/// The Web Store question's fields, read back as `key=value;...`.
const STORE_ASK_JS: &str = "(() => { const g = (id) => document.getElementById(id); \
  return [`hidden=${g('ext-ask').hidden}`, `launch=${g('launch').hidden}`, `title=${g('x-title').textContent}`, \
  `yes=${g('x-yes').hidden ? 'hidden' : g('x-yes').textContent}`, `meta=${g('x-meta').textContent}`, \
  `perms=${g('x-perms').hidden}`, `items=${g('x-perms').children.length}`, \
  `list=${g('x-perms').textContent}`, `note=${g('x-note').textContent}`, `focus=${document.activeElement ? document.activeElement.id : ''}`].join(';'); })()";

const LAUNCH_JS: &str = "(() => { const g = (id) => document.getElementById(id), u = g('l-uri'), o = g('l-open'); \
  return [`hidden=${g('launch').hidden}`, `scheme=${g('l-scheme').textContent}`, `origin=${g('l-origin').textContent}`, \
  `markup=${u.children.length}`, `h=${u.getBoundingClientRect().height}`, \
  `open=${o.hidden ? 'hidden' : o.getAttribute('aria-disabled') === 'true' ? 'disabled' : 'on'}`, `native=${o.disabled}`, \
  `failed=${!g('l-failed').hidden}`, `cancel=${g('l-cancel').textContent}`, \
  `focus=${document.activeElement ? document.activeElement.id : ''}`, `uri=${u.textContent}`].join(';'); })()";

/// A key-down with the live Esc state, as the native hook reads it
/// (`Shortcut::from_key_in` with `esc_exits_fullscreen`).
fn press_live(app: &mut App, key: u32, mods: Mods) -> Option<Shortcut> {
    let sc = Shortcut::from_key_in(key, mods, crate::page_tools::esc_exits_fullscreen())?;
    app.handle(UserEvent::Shortcut(sc));
    Some(sc)
}

/// WebView2's zoom factor of tab `id` (NaN without a view).
fn native_zoom(app: &App, id: TabId) -> f64 {
    app.view(id).and_then(crate::native::zoom_factor).unwrap_or(f64::NAN)
}

/// (DevTools, browser accelerator keys, zoom control) of a webview's settings.
fn settings_of(v: &wry::WebView) -> (bool, bool, bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
    use windows::core::{BOOL, Interface};
    let (mut dev, mut keys, mut zoom) = (BOOL(1), BOOL(0), BOOL(0));
    unsafe {
        if let Ok(s) = v.webview().Settings() {
            let _ = s.AreDevToolsEnabled(&mut dev);
            let _ = s.IsZoomControlEnabled(&mut zoom);
            if let Ok(s3) = s.cast::<ICoreWebView2Settings3>() {
                let _ = s3.AreBrowserAcceleratorKeysEnabled(&mut keys);
            }
        }
    }
    (dev.as_bool(), keys.as_bool(), zoom.as_bool())
}

/// The address bar's zoom indicator: shown, text, inside the address box,
/// topmost at its centre, accessible name.
const ZOOM_UI_JS: &str = "(() => { const b = document.getElementById('zoom'), r = b.getBoundingClientRect(), \
  a = document.getElementById('address-box').getBoundingClientRect(); \
  const hit = r.width > 0 ? document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2) : null; \
  return `hidden=${b.hidden};text=${b.textContent.trim()};w=${r.width};inside=${r.left >= a.left && r.right <= a.right};\
top=${!!hit && b.contains(hit)};label=${b.getAttribute('aria-label')}`; })()";

/// A requestFullscreen button (top left, 300x200) and some text to find.
const FULLSCREEN_HTML: &str = r#"<!doctype html><title>Fullscreen</title>
<style>html,body{margin:0}#go{position:absolute;left:0;top:0;width:300px;height:200px}#stage{margin-top:220px;background:#246;color:#fff}</style>
<button id="go" onclick="document.getElementById('stage').requestFullscreen().then(() => { window.__fs = 'on'; }, (e) => { window.__fs = e.name + ': ' + e.message; })">go</button>
<div id="stage">needle one, needle two, needle three</div>"#;

/// The fullscreen element and its size against the screen (CSS px).
const FS_PAGE_JS: &str = "(() => { const e = document.fullscreenElement, r = e ? e.getBoundingClientRect() : { width: 0, height: 0 }; \
  return `fs=${e && e.id};ew=${r.width};eh=${r.height};sw=${screen.width};sh=${screen.height};req=${window.__fs}`; })()";

/// The F11 notice text, and the page notice's text after the site name.
const F11_NOTICE: &str = "Tam ekran — çıkmak için F11 ya da Esc";
const PAGE_NOTICE: &str = " tam ekran — çıkmak için Esc";

/// The fullscreen notice as the UI draws it: hidden, its two texts, shown
/// (computed style), covering the UI viewport, topmost at its centre, and the
/// strip under it inert.
const NOTICE_UI_JS: &str = "(() => { const n = document.getElementById('fs-notice'), s = getComputedStyle(n), r = n.getBoundingClientRect(); \
  const hit = document.elementFromPoint(innerWidth / 2, innerHeight / 2); \
  return `hidden=${n.hidden};site=${document.getElementById('fs-notice-site').textContent};\
text=${document.getElementById('fs-notice-text').textContent};\
shown=${s.display !== 'none' && s.visibility === 'visible' && s.opacity === '1'};\
covers=${r.left <= 0 && r.top <= 0 && r.right >= innerWidth && r.bottom >= innerHeight && innerWidth > 0};\
hit=${!!hit && n.contains(hit)};inert=${document.getElementById('strip').inert}`; })()";

/// The notice up over the page, from both sides: the UI webview shown as the
/// notice band, the page under it over the whole window, and the band the
/// topmost of the window's children at its centre (what a click there hits).
fn notice_on_top(app: &App) -> Result<String, String> {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::UI::WindowsAndMessaging::{CWP_SKIPINVISIBLE, ChildWindowFromPointEx};
    use tao::platform::windows::WindowExtWindows;
    let v = app.tabs.active().and_then(|id| app.view(id)).ok_or("no active view")?;
    let (w, h) = logical_size(app);
    let want = crate::app::notice_rect(LogicalSize::new(w, h));
    let scale = app.window.scale_factor();
    let (wp, ws) = (want.position.to_logical::<f64>(scale), want.size.to_logical::<f64>(scale));
    let (x, y, bw, bh) = bounds_of(app, &app.ui);
    let (cx, cy, cw, ch) = bounds_of(app, v);
    let mut host = HWND::default();
    let hit = unsafe {
        let _ = app.ui.controller().ParentWindow(&mut host);
        let pt = POINT { x: ((x + bw / 2.0) * scale) as i32, y: ((y + bh / 2.0) * scale) as i32 };
        ChildWindowFromPointEx(HWND(app.window.hwnd() as _), pt, CWP_SKIPINVISIBLE)
    };
    let info = format!(
        "notice up {}, ui visible {}, band ({x}, {y}) {bw}x{bh} want ({}, {}) {}x{}, content ({cx}, {cy}) {cw}x{ch} of {w}x{h}, \
         child at band centre {:?}, ui host {:?}",
        app.fullscreen.notice_up(),
        visible(&app.ui),
        wp.x,
        wp.y,
        ws.width,
        ws.height,
        hit.0,
        host.0
    );
    let ok = app.fullscreen.notice_up()
        && visible(&app.ui)
        && visible(v)
        && (x - wp.x).abs() <= 1.0
        && (y - wp.y).abs() <= 1.0
        && (bw - ws.width).abs() <= 1.0
        && (bh - ws.height).abs() <= 1.0
        && cx.abs() <= 1.0
        && cy.abs() <= 1.0
        && (cw - w).abs() <= 1.0
        && (ch - h).abs() <= 1.0
        && !host.is_invalid()
        && hit == host;
    if ok { Ok(info) } else { Err(info) }
}

/// The notice checks: `names` = [over the page (native), drawn with `site`
/// and `text` (UI), keyboard focus left with the page].
fn check_notice(app: &App, names: [&'static str; 3], site: &'static str, text: &'static str) {
    check_layout(names[0], notice_on_top(app));
    let name = names[1];
    ui_eval(app, name, NOTICE_UI_JS, move |r| {
        check(
            name,
            field(&r, "hidden") == "false"
                && field(&r, "site") == site
                && field(&r, "text") == text
                && field(&r, "shown") == "true"
                && field(&r, "covers") == "true"
                && field(&r, "hit") == "true"
                && field(&r, "inert") == "true",
            &r,
        );
    });
    let name = names[2];
    page_eval(app, name, "`focus=${document.hasFocus()}`", move |r| {
        check(name, field(&r, "focus") == "true", &r);
    });
}

/// Replaces the active tab's document (a fresh https://example.com/ page)
/// with `FULLSCREEN_HTML`: a real origin, so the notice has a site to name.
fn write_fullscreen_page(app: &App) {
    let mut lit = String::new();
    crate::tabs::push_json_str(&mut lit, FULLSCREEN_HTML);
    page_js(app, &format!("document.open(); document.write({lit}); document.close();"));
}

/// Keyboard Lock probes, in the top frame and with the method borrowed from
/// a fresh `about:blank` iframe's realm (one no injected script runs in):
/// `window.__kl` / `window.__kl2` become `missing`, `resolved` or
/// `rejected:<name>`.
const KEYBOARD_LOCK_JS: &str = "window.__kl = window.__kl2 = 'pending'; \
  const kl = async (lock) => { if (typeof lock !== 'function') return 'missing'; \
    try { await lock.call(navigator.keyboard, ['Escape', 'F11']); navigator.keyboard.unlock(); return 'resolved'; } \
    catch (e) { return 'rejected:' + e.name; } }; \
  (async () => { const k = navigator.keyboard; window.__kl = await kl(k && k.lock); \
    const f = document.createElement('iframe'); document.body.appendChild(f); \
    const K = f.contentWindow.Keyboard; window.__kl2 = await kl(K && K.prototype.lock); f.remove(); })()";

fn check_layout(name: &str, r: Result<String, String>) {
    match r {
        Ok(info) => {
            log(&format!("INFO {name}: {info}"));
            check(name, true, "");
        }
        Err(detail) => check(name, false, detail),
    }
}

/// Logical (x, y, w, h) of a webview's bounds.
fn bounds_of(app: &App, v: &wry::WebView) -> (f64, f64, f64, f64) {
    let scale = app.window.scale_factor();
    let Ok(b) = v.bounds() else { return (f64::NAN, f64::NAN, f64::NAN, f64::NAN) };
    let (p, s) = (b.position.to_logical::<f64>(scale), b.size.to_logical::<f64>(scale));
    (p.x, p.y, s.width, s.height)
}

/// The window fullscreen on its monitor, the strip hidden (the UI webview
/// shown only as the notice band while the notice is up), the active page
/// over the whole window.
fn fullscreen_layout(app: &App) -> Result<String, String> {
    let v = app.tabs.active().and_then(|id| app.view(id)).ok_or("no active view")?;
    let m = app.window.current_monitor().ok_or("no monitor")?;
    let (ms, mp) = (m.size(), m.position());
    let is = app.window.inner_size();
    let ip = app.window.inner_position().map_err(|e| e.to_string())?;
    let (w, h) = logical_size(app);
    let (x, y, cw, ch) = bounds_of(app, v);
    let (ux, uy, uw, uh) = bounds_of(app, &app.ui);
    let band = crate::app::notice_rect(LogicalSize::new(w, h));
    let scale = app.window.scale_factor();
    let (bp, bs) = (band.position.to_logical::<f64>(scale), band.size.to_logical::<f64>(scale));
    let info = format!(
        "fullscreen {:?}, ui visible {} at ({ux}, {uy}) {uw}x{uh}, notice up {}, monitor {ms:?} at {mp:?}, window {is:?} at {ip:?}, \
         content ({x}, {y}) {cw}x{ch} of {w}x{h}",
        app.window.fullscreen().is_some(),
        visible(&app.ui),
        app.fullscreen.notice_up()
    );
    let ui_ok = if app.fullscreen.notice_up() {
        visible(&app.ui)
            && (ux - bp.x).abs() <= 1.0
            && (uy - bp.y).abs() <= 1.0
            && (uw - bs.width).abs() <= 1.0
            && (uh - bs.height).abs() <= 1.0
    } else {
        !visible(&app.ui)
    };
    let ok = app.window.fullscreen().is_some()
        && ui_ok
        && visible(v)
        && is.width.abs_diff(ms.width) <= 1
        && is.height.abs_diff(ms.height) <= 1
        && (ip.x - mp.x).abs() <= 1
        && (ip.y - mp.y).abs() <= 1
        && x.abs() <= 1.0
        && y.abs() <= 1.0
        && (cw - w).abs() <= 1.0
        && (ch - h).abs() <= 1.0;
    if ok { Ok(info) } else { Err(info) }
}

/// Not fullscreen, the strip shown, the active page below it; 1200x800
/// unless `maximized`.
fn normal_layout(app: &App, maximized: bool) -> Result<String, String> {
    let v = app.tabs.active().and_then(|id| app.view(id)).ok_or("no active view")?;
    let (w, h) = logical_size(app);
    let (x, y, cw, ch) = bounds_of(app, v);
    let ui_h = crate::app::UI_HEIGHT;
    let info = format!(
        "fullscreen {}, maximized {}, ui visible {}, window {w}x{h}, content ({x}, {y}) {cw}x{ch}",
        app.window.fullscreen().is_some(),
        app.window.is_maximized(),
        visible(&app.ui)
    );
    let size_ok = if maximized { app.window.is_maximized() } else { !app.window.is_maximized() && (w - 1200.0).abs() <= 1.0 && (h - 800.0).abs() <= 1.0 };
    let ok = app.window.fullscreen().is_none()
        && visible(&app.ui)
        && visible(v)
        && size_ok
        && x.abs() <= 1.0
        && (y - ui_h).abs() <= 1.0
        && (cw - w).abs() <= 1.0
        && (ch - (h - ui_h)).abs() <= 1.0;
    if ok { Ok(info) } else { Err(info) }
}

thread_local! {
    /// The find session of the find-bar probe.
    static FIND: std::cell::RefCell<Option<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Find>> =
        const { std::cell::RefCell::new(None) };
    /// Browser-process windows before the find bar opened.
    static BASE_WINDOWS: std::cell::RefCell<Vec<isize>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// HRESULT `ICoreWebView2Find::Start` completed with (1 = not yet).
static FIND_START: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

/// Starts WebView2's find (ICoreWebView2_28::Find) for `term` with its own
/// find bar shown: what Ctrl+F does, opened without any key input.
fn find_start(v: &wry::WebView, term: &str) -> windows::core::Result<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Find> {
    use webview2_com::FindStartCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2_28, ICoreWebView2Environment15};
    use windows::core::{HSTRING, Interface};
    unsafe {
        let find = v.webview().cast::<ICoreWebView2_28>()?.Find()?;
        let options = v.environment().cast::<ICoreWebView2Environment15>()?.CreateFindOptions()?;
        options.SetFindTerm(&HSTRING::from(term))?;
        options.SetSuppressDefaultFindDialog(false)?;
        find.Start(
            &options,
            &FindStartCompletedHandler::create(Box::new(|hr| {
                FIND_START.store(hr.err().map_or(0, |e| e.code().0), Ordering::Relaxed);
                Ok(())
            })),
        )?;
        Ok(find)
    }
}

/// Visible top-level windows of WebView2's browser process: (HWND, screen
/// rect [left, top, right, bottom]).
fn browser_windows(app: &App) -> Vec<(isize, [i32; 4])> {
    let Some(v) = app.tabs.active().and_then(|id| app.view(id)) else { return Vec::new() };
    let mut pid = 0;
    unsafe {
        if v.webview().BrowserProcessId(&mut pid).is_err() {
            return Vec::new();
        }
    }
    windows_of(pid)
}

/// Visible top-level windows of process `pid` (0: none).
fn windows_of(pid: u32) -> Vec<(isize, [i32; 4])> {
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible};
    use windows::core::BOOL;
    struct Ctx {
        pid: u32,
        out: Vec<(isize, [i32; 4])>,
    }
    unsafe extern "system" fn each(h: HWND, l: LPARAM) -> BOOL {
        // SAFETY: `l` is the `Ctx` passed to EnumWindows below, alive for the call.
        let ctx = unsafe { &mut *(l.0 as *mut Ctx) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid == ctx.pid && unsafe { IsWindowVisible(h) }.as_bool() {
            let mut r = RECT::default();
            let _ = unsafe { GetWindowRect(h, &mut r) };
            ctx.out.push((h.0 as isize, [r.left, r.top, r.right, r.bottom]));
        }
        BOOL(1)
    }
    if pid == 0 {
        return Vec::new();
    }
    let mut ctx = Ctx { pid, out: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.out
}

/// Browser-process windows that were not there before the find started.
fn find_bar(app: &App) -> Vec<(isize, [i32; 4])> {
    let base = BASE_WINDOWS.with(|b| b.borrow().clone());
    browser_windows(app).into_iter().filter(|(h, _)| !base.contains(h)).collect()
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

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Exit code of the second start, or why there is none.
type SecondStart = std::thread::JoinHandle<Result<i32, String>>;

thread_local! {
    static SECOND_START: std::cell::Cell<Option<SecondStart>> = const { std::cell::Cell::new(None) };
}

/// Hard limit for the second start; it is killed and reaped after that.
const SECOND_START_LIMIT: Duration = Duration::from_secs(10);

/// Starts this same executable again against the self-test profile (never
/// the user's), as a second X-Browser start would be. A watchdog thread
/// waits for it, so the event loop stays free to answer its WM_COPYDATA.
fn spawn_second_start(url: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let err = std::fs::File::create(dir().join("child.err")).map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg(url)
        .env("XB_SELFTEST_DIR", dir())
        .env("XB_SELFTEST_CHILD", "1")
        // No dialog if it cannot hand over: it just exits.
        .env("XB_TIMING", dir().join("child-timing.txt"))
        .env("XB_TIMING_QUIT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(err)
        .spawn()
        .map_err(|e| e.to_string())?;
    let watchdog = std::thread::spawn(move || {
        let end = std::time::Instant::now() + SECOND_START_LIMIT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status.code().unwrap_or(-1)),
                Ok(None) if std::time::Instant::now() < end => std::thread::sleep(Duration::from_millis(50)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("still running after {SECOND_START_LIMIT:?}: killed"));
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e.to_string());
                }
            }
        }
    });
    SECOND_START.set(Some(watchdog));
    Ok(())
}

/// In the second start (which must only hand over), notes that `what` was
/// reached anyway and says to stop; the scenario checks for such notes.
fn reached_in_child(what: &str) -> bool {
    if std::env::var_os("XB_SELFTEST_CHILD").is_none() {
        return false;
    }
    let _ = std::fs::write(dir().join(format!("child-reached-{what}")), b"");
    true
}

/// Sends WM_COPYDATA to this profile's own IPC window, as a second instance
/// would; the reply is what the receiver returned (1 taken, 0 refused).
fn send_ipc(kind: usize, bytes: &[u8]) -> Option<usize> {
    crate::instance::send_to_self(&crate::store::data_dir()?, kind, bytes)
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
