# X-Browser

Light, minimal desktop browser in Rust on the system WebView (WebView2 on
Windows), built with `wry` + `tao` only.

## Requirements

- Rust (stable, edition 2024)
- Microsoft Edge WebView2 Runtime (ships with Windows 11)
- Tailwind CSS v4 standalone CLI, only when changing the UI styles

## Build and run

The compiled stylesheet `ui/dist/app.css` is checked in, so a plain build needs
no Tailwind. After changing `ui/index.html`, `ui/js/*.js` or `ui/input.css`,
rebuild the CSS first:

```sh
# one-time: download the Tailwind v4 standalone CLI into tools/
curl -L -o tools/tailwindcss.exe https://github.com/tailwindlabs/tailwindcss/releases/download/v4.3.5/tailwindcss-windows-x64.exe

# rebuild the stylesheet (it is embedded into the binary at compile time)
./tools/tailwindcss.exe -i ui/input.css -o ui/dist/app.css --minify
```

```sh
cargo run                 # debug
cargo build --release     # target/release/x-browser.exe
cargo run --release -- example.com   # optional start URL
```

## Keyboard shortcuts

| Keys | Action |
| --- | --- |
| Ctrl+T | new tab |
| Ctrl+W | close the active tab (closing the last one quits) |
| Ctrl+Shift+T | reopen the last closed tab |
| Ctrl+Tab / Ctrl+PgDn | next tab |
| Ctrl+Shift+Tab / Ctrl+PgUp | previous tab |
| Ctrl+1 … Ctrl+8 (also numpad) | go to tab 1–8 |
| Ctrl+9 | go to the last tab |
| Ctrl+Shift+PgUp / Ctrl+Shift+PgDn | move the active tab left / right |
| Ctrl+L | focus the address bar |
| Alt+← / Alt+→ (and the Back/Forward keys) | back / forward |
| F5 / Ctrl+R / Ctrl+F5 | reload |
| Ctrl+F | find in page (WebView2's find bar; also in the menu) |
| Ctrl+P | print preview (WebView2's; also in the menu) |
| Ctrl+= / Ctrl++ / Ctrl+wheel up | zoom in |
| Ctrl+- / Ctrl+wheel down | zoom out |
| Ctrl+0 | reset zoom |
| F11 | fullscreen on/off; Esc also leaves it |
| Ctrl+H | history list |
| Ctrl+D | bookmark the page and open its editor; on a bookmarked page, open the editor with Remove focused (Ctrl+D, Enter removes it) |
| Ctrl+Shift+O | bookmark list, with a filter |
| Ctrl+J | downloads (WebView2's flyout) |
| Alt+F / Alt+E | main menu (also the strip's ⋮ button) |

In the tab strip, with a tab focused: ←/→/Home/End move focus, Enter or Space
selects it, Delete closes it. In the address bar's suggestion list: ↑/↓
choose, Enter opens, Esc closes and restores what was typed. In the menu:
↑/↓/Home/End move, Enter picks, Esc closes.

Shortcuts are Ctrl without Alt only: AltGr (Ctrl+Alt) on e.g. Turkish layouts
always types its character.

## Checks

```sh
cargo build
cargo clippy --all-targets
cargo clippy --all-targets --features selftest
cargo test
```

### In-app self-test

Drives the real app (tabs, navigation, history, blocked schemes, content-IPC
rejection, popups, resize, shortcuts, tab-switch timing) and writes
`selftest.log` plus screenshots to `XB_SELFTEST_DIR` (default: current dir).
Needs network access (example.com, duckduckgo.com).

```sh
XB_SELFTEST_DIR=selftest-out cargo run --release --features selftest
# last line of selftest.log: DONE failures=0 checks=<n>
```

The self-test keeps its saved session and window place in
`<XB_SELFTEST_DIR>\profile` (emptied at the start of each run), never in the
real `%LOCALAPPDATA%\X-Browser`, and its single-instance names follow that
folder, so it runs alongside a normal X-Browser.

A run passes only with `failures=0`. `n` is the number of PASS lines; if it
differs from `EXPECTED_CHECKS` in `src/selftest.rs` (a check was skipped, e.g.
a script callback never came back), an `all-expected-checks-ran` FAIL is
logged first. Update the constant when adding or removing checks.

### Startup timing

`XB_TIMING=<file>` appends `<milestone> <ms since the OS created the process>`
lines (`window-shown`, `ui-webview-created`, `first-tab-created`, `ui-ready`).
With `XB_TIMING_QUIT=1` also set, the app closes itself once the UI is ready:

```sh
for i in 1 2 3 4 5 6 7 8 9 10; do
  XB_TIMING=timing.txt XB_TIMING_QUIT=1 ./target/release/x-browser.exe
  sleep 8
done
```

Leave a few seconds between runs: when runs follow each other directly, the
previous run's WebView2 browser processes are still shutting down and the next
start waits about 6 s for them (measured 2026-11-15 on WebView2 157.0.4341.46:
ui-ready ≈6,450 ms back to back, ≈360–420 ms with an 8 s gap, for v0.1 and
v0.2 alike). To keep a run off the real profile, point `LOCALAPPDATA` **and**
`APPDATA` at a scratch folder for that command.

Release build, 10 runs after one warm-up, 8 s apart, scratch profile, one
blank tab, median (ms since process creation):

| | binary | window-shown | ui-webview-created | first-tab-created | ui-ready |
| --- | --- | --- | --- | --- | --- |
| v0.1 (33b478e) | 684,544 B | 60 | 320 | 354 | 361 |
| v0.2 at task 5 (ae74229) | 1,073,152 B | 65 | 338 | 417 | 418 |
| v0.2 final | | | | | |

<!-- TODO: measure the "v0.2 final" row on the merged release build and fill it in. -->

The first two rows were measured 2026-11-15.

## Fullscreen

F11 and a page's own fullscreen (a video, `requestFullscreen`) hide the strip.
Every entry shows a notice over the top centre for 3.5 s: the site's host (from
the tab's URL, not from the page) and the way out. Known trade-offs:

- For those 3.5 s the notice band (600x40 at the top centre) takes clicks
  there instead of the page.
- Esc in fullscreen belongs to the app: it leaves fullscreen (either kind) and
  does not reach the page, so a page cannot use Esc for itself while
  fullscreen.
- Leaving a page's fullscreen asks the page to call `document.exitFullscreen()`
  in its own world, which its script can ignore. The window is restored
  regardless; a page that stays in element fullscreen then only fills its own
  area below the strip.
- Keyboard Lock (`navigator.keyboard.lock()`) has no browser switch of its
  own. A script removes it from every content frame before the page's scripts
  run (best effort, like any script in the page's world), and
  `--disable-features=SystemKeyboardLock` keeps system keys (Alt+Tab, the
  Windows key) out of its reach.

## Session, window and single instance

The open tabs (order, active tab, titles, zoom) and the window's size,
position and maximized state come back on the next start. F11 fullscreen does
not: the window reopens as it was before F11. Tab history (Back/Forward) is
not kept either. Only the active tab loads at start; the others show their
saved titles and load when first selected. Closing the last tab quits and
keeps that tab for the next start.

Saved when the app closes and about 1.5 s after any change (in the
background), so after a crash the last saved state comes back. A window whose
saved place is on a screen that is no longer there opens on a visible one.

The files are listed under "Data files" below. Each file carries `"v": 1` and is written as a temporary file, flushed, then
renamed over the old one. The first save of a run first copies the previous
good file to `<name>.bak` (best effort; a failed save is retried by the next
one). A file that cannot be read is moved to
`<name>.corrupt` and the `.bak` is used instead, else the defaults. A file
from a newer version is read but never overwritten. What is read back is
treated as untrusted: only `http`/`https` URLs up to 8 KiB, titles cut to 512
characters, zoom 25–500 %, and at most 500 tabs (the rest are dropped).

Only one X-Browser runs per profile. Starting it again, with or without an
address, hands the address (web URLs and search text only) to the running
window, which opens it in a new tab and comes to the front; the second process
then exits. It hands over only to a window whose process is the same
`x-browser.exe`, run by the same user at no lower integrity level; the
instance lock is open to the current user only, and not at all to that user's
lower-integrity processes. If the lock exists but was not made by X-Browser
(another owner, or an integrity label below Medium) or cannot be opened at
all, and no X-Browser window turns up, X-Browser starts anyway rather than not
at all. A normal (Medium) lock is always taken as genuine, so an elevated
start never runs beside a normal one; and a lock that cannot be opened while
an X-Browser window exists (an elevated one, met by a normal start) ends in
the "already running" message, never in a second copy. A window whose process
X-Browser cannot inspect counts only when Windows refuses to deliver messages
to it, which proves it runs at a higher integrity level.

X-Browser is not meant to run elevated (as administrator): the `.tmp`/`.bak`
writes above rely on `%LOCALAPPDATA%` being writable by the current user only.

## History, suggestions and clearing data

Every page that finishes loading is recorded in `history.jsonl` (same folder
as above): one `{"u": address, "t": title, "ts": unix seconds}` line per
visit, appended in the background at most every 2 s and when the app closes.
Only `http`/`https` addresses up to 8 KiB are kept, titles cut to 512
characters; the browser's own pages never are. The file is read as untrusted:
bad or torn lines are skipped (and dropped at the next rewrite), and at most
its last 16 MiB are read. Past 50,000 visits it is compacted to the newest
40,000, written to a temporary file and renamed over the old one.

Typing in the address bar lists up to 8 matching pages under it, by address or
title; addresses that start with the text, and pages visited often and
recently, come first. Only visits the user started count as "often": typed or
picked addresses and link clicks (as WebView2 reports them); a page's own
script navigations only refresh "recently". ↑/↓ choose one (its address goes into the box), Enter
opens it, Esc closes the list and restores what was typed. The list never
takes focus from the address bar, and closes when focus or the window moves,
the window is resized, the tab changes or a page starts loading (a jump to a
`#fragment` within the same page does not close it).

Ctrl+H opens the history list (one row per address, newest first, with a
search box). × then "Sil?" removes every visit to that address; "Tüm geçmişi
sil" removes all history. "Tarama verilerini temizle" removes history plus
WebView2's cookies and all site data (local storage, IndexedDB, cache
storage, service workers…), its disk cache and its own browsing history.
Saved passwords, autofill and the download list are kept. Both also forget
the recently closed tabs (Ctrl+Shift+T), and both ask for confirmation first.

## Data files

All in `%LOCALAPPDATA%\X-Browser`, next to the WebView2 profile
(`EBWebView\`). A self-test build uses `<XB_SELFTEST_DIR>\profile` instead.

| File | Contents | Written |
| --- | --- | --- |
| `session.json` | open tabs (URL, title, zoom) and the active one, at most 500 tabs | ~1.5 s after a change and at exit |
| `window.json` | normal rect (physical px), maximized | same |
| `history.jsonl` | one `{"u","t","ts"}` line per visit; past 50,000 visits compacted to 40,000; at most the last 16 MiB read | appended at most every 2 s and at exit |
| `bookmarks.json` | `{"v":1,"items":[{"url","title","added"}]}`, at most 10,000 items, file at most 4 MiB | on each change |
| `settings.json` | `engine` (duckduckgo, google, bing), `startup` (last session or one blank tab), `autofill` (on/off), `download_dir` (empty: the system Downloads folder); file at most 64 KiB | on each change |

The JSON files carry `"v": 1` and are written as tmp → flush → rename, with a
`.bak` of the previous good file and a `.corrupt` copy of a file that could
not be read. Everything read back is treated as untrusted (http/https only,
size and count limits; a settings value this version does not know reads as
its default). Deleting the folder resets X-Browser to first-start state.

A download folder must be an absolute, existing folder; the Windows Startup
folders (yours and all users', and anything below them) are refused, so a
download cannot land where Windows runs it at sign-in.

## Menu, downloads and settings

The strip's ⋮ button, Alt+F or Alt+E opens the main menu: new tab, find, zoom
(−, +, reset), print, history, bookmarks, downloads and settings. Each item
calls what its own shortcut calls. Downloads use WebView2's own flyout
(progress, pause/cancel, "open file", "open folder"); SmartScreen stays on.
The strip's downloads button appears once the session has had a download.
Settings (search engine, startup, autofill, download folder) are saved on each
change and take effect at once.

## Security indicator and external applications

The start of the address box shows the connection of the page on screen: a
lock for https, "Güvenli değil" for http, "Sertifika hatası" for a host whose
certificate failed in this run (also after the user chose to proceed on
WebView2's warning page), and "Sayfa çöktü" with a reload button for a crashed
tab. Every state has its own icon or text, never colour alone, and is read out
by screen readers when it changes. After more than 64 hosts with certificate
errors in one run, no https page counts as secure until the next start; the
indicator then says "Sertifikalara güvenilmiyor" (told once, not in a dialog).

A page never starts another application by itself. A `mailto:`, `tel:` or
similar link the user clicked shows a question in the overlay (the scheme, the
asking site and the whole address). "Aç" counts only after a short pause
(500 ms) since the question appeared, and anything that closes the question
(Esc, a click elsewhere, a tab switch, a navigation, 60 s) answers "no".
Every other `ms-` scheme is refused without a question.

Known limits, left as they are:

- `ms-windows-store:` is the one `ms-` scheme let through (to the same
  question), because sites link to their Store page and it only shows a page
  of the Store.
- The question appears at a random spot near the middle, so a page cannot
  place a button under "Aç" beforehand. In a narrow or short window there is
  little room around the middle and the spot hardly moves; the 500 ms pause
  still holds.
- WebView2's own find bar (Ctrl+F) is a separate window: whether it follows
  the window when the window is moved has not been verified.

## Icon

`assets/x-browser.ico` (16, 24, 32, 48 and 256 px) is drawn by
`assets/make_icon.py` (pure Python, deterministic; run it after changing the
design). `build.rs` embeds it in the exe as icon resource 1 without any build
dependency, and the window and taskbar icons load that same resource.

## Layout

| Path | Purpose |
| --- | --- |
| `src/main.rs` | window, event loop |
| `src/app.rs` | owns all state; webview creation, layout, IPC handling |
| `src/tabs.rs` | pure tab model (order, active tab, placement, JSON) |
| `src/url.rs` | address bar input → URL or search; scheme blocking |
| `src/commands.rs` | closed IPC command and shortcut sets |
| `src/strip.rs` | tab strip extras: reorder, reopen closed tab, Ctrl+1..9, mute |
| `src/page_tools.rs` | page zoom (keys, indicator) and fullscreen (F11, the page's own) |
| `src/native/mod.rs` | WebView2 features wry does not expose (keys, crashes, window) |
| `src/native/content.rs` | content-tab hooks: URL, history, loading, popups |
| `src/native/favicon.rs` | tab favicons as checked, size-capped PNG `data:` URLs |
| `src/native/audio.rs` | per-tab "playing sound" state and muting |
| `src/native/zoom.rs` | per-tab zoom factor: Ctrl+wheel on, change events, set/read |
| `src/native/fullscreen.rs` | a page's element fullscreen (video, `requestFullscreen`) events |
| `src/native/download.rs` | downloads through WebView2's own flyout; SmartScreen left on |
| `src/native/policy.rs` | content-tab policy hooks: certificate errors, external launches stopped, navigation outcome |
| `src/native/tools.rs` | settings- and menu-driven WebView2 calls: autofill, download folder, Find, Print |
| `src/history.rs` | history file: append, bounded read, compaction, ranking, delete, clear data |
| `src/overlay.rs` | third child webview for popups (suggestions, history, bookmarks, menu, settings, launch question); placement and closing |
| `src/bookmarks.rs` | bookmarks file (re-checked on load, read off the UI thread), star state, Ctrl+D editor, Ctrl+Shift+O list, bookmarks as a suggestion source |
| `src/menu.rs` | main menu (⋮, Alt+F, Alt+E): items call what their shortcuts call |
| `src/downloads.rs` | counts downloads (strip button) and opens/closes the flyout (Ctrl+J) |
| `src/settings.rs` | `settings.json`: engine, startup, autofill, download folder (with its checks) |
| `src/policy.rs` | security indicator, certificate-error memory, external-launch question |
| `src/ui.rs` | embedded UI assets, custom protocol, CSP |
| `src/store.rs` | versioned JSON files: atomic writes, `.bak`, corrupt-file recovery |
| `src/session.rs` | what is saved (tabs, window place), when, and restoring it |
| `src/instance.rs` | single instance: mutex, hand-over to the running window |
| `src/timing.rs` | opt-in startup milestones (`XB_TIMING`) |
| `src/selftest.rs` | in-app scenario test (`--features selftest` only) |
| `ui/` | UI strip (HTML, vanilla JS, Tailwind source + build) |
| `ui/overlay.html`, `ui/js/overlay.js` | overlay page: suggestion rows, history list, launch question |
| `ui/js/suggest.js` | address-bar side of suggestions (keys, selection, restore) |
| `ui/js/bookmarks.js` | the star (UI page), the editor and list (overlay page); keeps no state |
| `ui/js/menu.js`, `ui/js/settings.js` | menu and settings pages in the overlay; draw and report only |
| `ui/js/security.js` | draws the connection indicator Rust decides |
| `docs/` | v0.2 design notes and the manual test checklist |
| `assets/` | app icon and the script that draws it |
| `build.rs` | embeds the icon in the exe |

Profile data lives in `%LOCALAPPDATA%\X-Browser`.
