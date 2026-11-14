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
| `src/timing.rs` | opt-in startup milestones (`XB_TIMING`) |
| `src/selftest.rs` | in-app scenario test (`--features selftest` only) |
| `ui/` | UI strip (HTML, vanilla JS, Tailwind source + build) |

Profile data lives in `%LOCALAPPDATA%\X-Browser`.
