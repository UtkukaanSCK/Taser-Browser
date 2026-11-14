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
done
```

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

Files, in `%LOCALAPPDATA%\X-Browser` (next to the WebView2 profile):

| File | Contents |
| --- | --- |
| `session.json` | tabs: URL, title, zoom; the active tab's index |
| `window.json` | normal rect (physical px) and maximized flag |

Each file carries `"v": 1` and is written as a temporary file, flushed, then
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
| `src/ui.rs` | embedded UI assets, custom protocol, CSP |
| `src/store.rs` | versioned JSON files: atomic writes, `.bak`, corrupt-file recovery |
| `src/session.rs` | what is saved (tabs, window place), when, and restoring it |
| `src/instance.rs` | single instance: mutex, hand-over to the running window |
| `src/timing.rs` | opt-in startup milestones (`XB_TIMING`) |
| `src/selftest.rs` | in-app scenario test (`--features selftest` only) |
| `ui/` | UI strip (HTML, vanilla JS, Tailwind source + build) |
| `assets/` | app icon and the script that draws it |
| `build.rs` | embeds the icon in the exe |

Profile data lives in `%LOCALAPPDATA%\X-Browser`.
