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

The self-test build never reaches the Chrome Web Store: its store and update
service are a fake that answers from a table (`webstore::fake`), and it accepts
packages signed only by their own developer key (`crx::Policy::SelfSigned`),
so it can install and update test extensions. That makes it unsafe as a
browser: **a `--features selftest` build is for testing only and is never
distributed.** A normal build has neither the fake nor that policy.

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
| v0.1 (33b478e) | 684,544 B | 50 | 276 | 302 | 308 |
| v0.2 at task 5 (ae74229) | 1,073,152 B | 51 | 293 | 365 | 366 |
| v0.2 final (e966be7) | 1,260,032 B | 50 | 291 | 361 | 362 |
| v0.3 final, blank profile () | B | | | | |
| v0.3 final, uBOL installed () | B | | | | |

All three rows were measured in one session on 2026-11-17 (absolute numbers
move with machine load: the same builds measured 50–60 ms slower on
2026-11-15, with the same gaps between them). The v0.2 cost at start-up is
creating the first content tab, which grew at task 5
(first-tab-created − ui-webview-created: 26 ms in v0.1, 70 ms in v0.2);
tasks 6–9 added nothing measurable.

v0.3: *to be measured (task 6).*

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
| `settings.json` | `engine` (duckduckgo, google, bing), `startup` (last session or one blank tab), `autofill` (on/off), `download_dir` (empty: the system Downloads folder), `tracking` (off, basic, balanced, strict; default balanced); file at most 64 KiB | on each change |
| `extensions.json` | extensions X-Browser installed (id, name, version, folder, source, on/off), at most 500, `first_run_ubol` (`pending`, `done` or `user_removed`), `last_update_check` (seconds) and `last_error` (the last install error nobody saw: id, message of at most 300 characters, time); file at most 1 MiB | on each install, removal or change |
| `extensions\<id>\<version>\` | unpacked extensions X-Browser owns | on install, update and removal |

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
(−, +, reset), print, history, bookmarks, downloads, settings and "Uzantılar"
(see "Extensions"). Each item calls what its own shortcut calls. Downloads use
WebView2's own flyout (progress, pause/cancel, "open file", "open folder");
SmartScreen stays on. The strip's downloads button appears once the session has
had a download. Settings (search engine, startup, autofill, download folder,
tracker blocking) are saved on each change and take effect at once.

Tracker blocking is WebView2's tracking prevention for the content
profile (the tabs and their extensions, not the UI's own profile): Temel
(known harmful trackers only), Dengeli (the default, also trackers from
sites you have not visited), Katı (most trackers; some sites may break) or
Kapalı, which leaves blocking to the extensions (uBlock Origin Lite) when the
two together break a site. `settings.json` stores them as basic, balanced,
strict and off.

## Security indicator and external applications

The start of the address box shows the connection of the page on screen: a lock
for https, "Güvenli değil" for http, "Sertifika hatası" for a host whose
certificate failed in this run, and "Sayfa çöktü" with a reload button for a
crashed tab (the tab itself stays empty). Every state has its own icon or text,
never colour alone, and is read out by screen readers when it changes. After
more than 64 hosts with certificate errors in one run, no https page counts as
secure until the next start; the indicator then says "Sertifikalara
güvenilmiyor" (told once, not in a dialog).

A page never starts another application by itself. A `mailto:`, `tel:` or
similar link the user clicked shows a question in the overlay (the scheme, the
asking site and the whole address). "Aç" counts only after a short pause
(500 ms) since the question appeared, and anything that closes the question
(Esc, a click elsewhere, a tab switch, a navigation, 60 s) answers "no".
Every other `ms-` scheme is refused without a question.

A page that cannot load (unknown host, no connection, refused connection,
time-out, bad certificate) shows X-Browser's own error page in Turkish, never
WebView2's Edge-branded one: a title for the kind of error, a short
explanation, the failed address as plain text and a "Yeniden dene" button.
The address bar keeps the failed address, the error page never shows the
lock, and Back and Forward step over it like any page. The page is written
only into WebView2's error document (`chrome-error://`), which no site can
load or script, and only as text. A bad certificate has no "proceed anyway".

Known limits, left as they are:

- A real certificate error has not been checked against the error page here;
  the self-test checks unreachable hosts only.
- `ms-windows-store:` is the one `ms-` scheme let through (to the same
  question), because sites link to their Store page and it only shows a page
  of the Store.
- The question appears at a random spot near the middle, so a page cannot
  place a button under "Aç" beforehand. In a narrow or short window there is
  little room around the middle and the spot hardly moves; the 500 ms pause
  still holds.
- WebView2's own find bar (Ctrl+F) is a separate window: whether it follows
  the window when the window is moved has not been verified.

## Extensions (v0.3)

Content tabs can run Chrome extensions (Manifest V3; MV2 ones may stop working
on a future WebView2 runtime). They live in the content tabs' WebView2
profile only: the strip and the overlay run in a separate profile (`XbUi`)
that no extension reaches. There is no password manager: WebView2 never
offers to save passwords, whatever the autofill setting says.

**Ad blocking on first start.** uBlock Origin Lite
(`ddkjiahejlhfcafbddmgiahcphecmpfh`) installs by itself, without a question,
on the first start that has a data folder: X-Browser downloads it from the
Chrome Web Store a few seconds after the window is up. If that fails (no
network, say), it tries again on the next start. It is installed once: after
the user removes it, X-Browser never installs it again on its own (it can
still be installed by hand from the store).

**Installing from the Chrome Web Store.** On an extension's store page the
address box shows "Ekle" (or "Kaldır" when it is installed). The store's own
"Add to Chrome" and "Remove from Chrome" buttons go through the same flow.
Every install asks first, in the overlay: name, version, and its permissions
and site access in Turkish, with "Ekle" or "İptal". Removing asks too.
WebView2's own "Add … to Microsoft Edge?" dialog is not used. The package is
checked before anything is unpacked: it must be a CRX3 signed by the Chrome Web
Store and by the extension's own key, and its id must be the one asked for. The
signature, not the host it came from, is what makes it trusted.

**Uzantılar page.** The menu's "Uzantılar" lists each extension with its icon,
name and version, an on/off switch, "Seçenekler" (opens its options page in a
new tab; only for an extension that has one and is turned on) and "Kaldır"
(asks in the page first; the extension's own settings are deleted with it).
Each operation shows on its row until WebView2 answers, and turns into an
error if it does not answer within 10 s. The two extensions built into
WebView2 (the PDF viewer among them) are not listed.

An extension that got into the profile without going through X-Browser (Edge's
own dialog, say) is turned off at the next start and listed as "X-Browser
dışında kurulmuş", without an icon, options or popup. It can be turned on or
removed there; turned on, it is still not recorded, so the next start turns it
off again.

**Toolbar button and popups.** While a turned-on extension has a popup
(`action.default_popup`), the strip shows a puzzle-piece button. It lists
those extensions, and a pick opens the popup under the button. The popup
stays on its extension's pages: other navigations, new windows and downloads
are refused, and it cannot start another application, not even after a
question. It closes on Esc, on its own `window.close()`, when the focus goes
elsewhere (a click outside, another window), on a tab switch and when the
window moves or is resized.

**Updates.** Extensions installed from the store and turned on are checked for
updates at most once a day, on their own: a timer looks every minute, and the
time of the last check is kept in `extensions.json`. A turned-off extension is
not updated. The Uzantılar page's "Güncellemeleri denetle" runs the same check
by hand (a check already running counts as it); the button stays disabled
meanwhile, and the page then says that everything is current, how many updates
were queued, that there was nothing to check, or that the check failed. A newer
version is checked like an install (signature, same id and key, and the size
and SHA-256 the update service gave) and replaces the old one in place, so the
extension keeps its settings; the old folder is deleted afterwards. It asks
only when the new version wants a permission or site the installed one did not
have, or lets sites or other extensions message it, runs its scripts in more
frames, replaces a browser page or changes its manifest version ("Güncelle" or
"İptal", listing only what is new). An update of an extension the Uzantılar
page is still switching or removing waits for the next check. It asks only when nothing
else is on screen: with the overlay open (the Uzantılar page itself included),
an extension's popup open, in fullscreen or with X-Browser in the background it
waits for the next check. So an update that wants more is never asked about
from the page's button; it is counted as queued and asked about at a later
daily check. Otherwise it goes in without a question. A version whose download
or install failed is not fetched again until the next start.

An install or update that fails with no question of its own on screen (its
question already gone, or an update that never asked), so nobody saw why, is
kept as the last error in `extensions.json`. The Uzantılar page shows it once,
under its extension's row (or on a line at the top if the extension is gone),
and then clears it.

Clearing browsing data leaves extensions and their settings alone.

**Network.** Installs and update checks are plain HTTPS GETs over WinHTTP
(`src/net.rs`), from a worker thread:

- The first request always goes to the Google update service,
  `https://clients2.google.com/service/update2/crx`, with the extension's id
  and the WebView2 runtime version (`prodversion`). For an install it asks for
  a redirect to the package; for an update check, for an XML answer listing
  each installed store extension's id and version.
- The package itself comes from wherever that answer points: the install's
  redirect, or the update answer's `codebase`. In practice that is
  `clients2.googleusercontent.com` (seen in the v0.3 spike), but the code does
  not pin the host. It accepts any public DNS name over HTTPS: at most 5
  redirects, each `Location` checked by hand; no `http`, IP addresses,
  single-label names or user names in URLs; TLS 1.2 or later.
- No cookies, no stored credentials and no automatic sign-in are sent. The
  User-Agent is `X-Browser` and nothing else. The system proxy settings are
  used.

The store page itself is an ordinary page in a content tab, like any other
site.

**Licence and credit.** uBlock Origin Lite is by Raymond Hill and
contributors, under GPLv3 ([source](https://github.com/uBlockOrigin/uBOL-home)).
X-Browser (MIT) does not ship it, copy it or change it: it downloads the
publisher's own package from the Chrome Web Store at run time. See
`docs/v0.3-licensing.md` (not legal advice).

Extensions live in `%LOCALAPPDATA%\X-Browser\extensions\<id>\<version>`
(uBlock Origin Lite takes about 47 MB once WebView2 has indexed its rules),
recorded in `extensions.json` (see "Data files"). Nothing is deleted under
`extensions` if that folder is itself a junction or link.

Known limits, open until checked by hand (`docs/v0.3-manual-checklist.md`):

- Optional permissions an extension asks for at run time: whether a question
  appears, and whose.
- Whether WebView2 accepts a newer version of an installed id from the store
  (an update in place worked with a local test extension).
- After `extensions.json` and its `.bak` are both lost, whether uBlock Origin
  Lite stays turned off (unknown to the record) and whether the first-run
  install runs again. The "removed by the user" choice is lost in that case,
  and also when an older v0.3 build without that field rewrites the file, or
  the main file is damaged and the `.bak` predates the removal.
- The popup matches the active tab by URL; two tabs on the same URL can
  confuse an extension's per-tab view. `tabs.getCurrent`,
  `windows.getCurrent` and `lastFocusedWindow` are not covered.
- The store button hook relies on the store's private browser contract
  (`chrome.webstorePrivate`). If Google changes it, the strip's "Ekle" still
  works; the store's own button may then open Edge's dialog, and an extension
  added that way shows up turned off with an unknown source.
- Removing an extension when `extensions` is a junction: nothing is deleted
  through it; what the page shows is not verified yet.

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
| `src/native/error_page.rs` | writes X-Browser's error page into a tab whose load failed |
| `src/native/tools.rs` | settings- and menu-driven WebView2 calls: autofill, download folder, tracking prevention, Find, Print |
| `src/history.rs` | history file: append, bounded read, compaction, ranking, delete, clear data |
| `src/overlay.rs` | third child webview for popups (suggestions, history, bookmarks, menu, settings, launch and store questions, Uzantılar); placement and closing |
| `src/bookmarks.rs` | bookmarks file (re-checked on load, read off the UI thread), star state, Ctrl+D editor, Ctrl+Shift+O list, bookmarks as a suggestion source |
| `src/menu.rs` | main menu (⋮, Alt+F, Alt+E): items call what their shortcuts call |
| `src/downloads.rs` | counts downloads (strip button) and opens/closes the flyout (Ctrl+J) |
| `src/settings.rs` | `settings.json`: engine, startup, autofill, download folder (with its checks), tracking prevention |
| `src/error_page.rs` | error status → Turkish error page text, and the script that shows it |
| `src/policy.rs` | security indicator, certificate-error memory, external-launch question |
| `src/extensions.rs` | `extensions.json`, install/remove/enable, start-up reconcile with WebView2's list |
| `src/native/extensions.rs` | WebView2's extension calls (add, list, enable, remove) on the content profile |
| `src/ext_ui.rs` | the Uzantılar page and the strip's extensions button; which `chrome-extension://` pages a tab may load |
| `src/popup.rs`, `src/native/popup.rs` | an extension's popup: its webview, placement, what closes it, what it may not do |
| `src/webstore.rs` | Web Store install, removal and update jobs, the question, the daily check |
| `src/firstrun.rs` | first-run uBlock Origin Lite: state, one install, never again after removal |
| `src/cws.rs` | Chrome Web Store: detail-page ids, download and update-check URLs, update answers, permission text |
| `src/net.rs` | HTTPS GET over WinHTTP: redirects checked by hand, size and time limits |
| `src/crx.rs` | CRX3 signature check and safe unpacking |
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
| `ui/js/extensions.js`, `ui/js/ext-button.js` | Uzantılar page and the extensions button's list (overlay), the button (strip) |
| `ui/js/extask.js`, `ui/js/webstore.js` | the store question (overlay) and the address box's "Ekle"/"Kaldır" button |
| `docs/` | v0.2 and v0.3 design notes, manual test checklists, v0.3 licensing note |
| `assets/` | app icon and the script that draws it |
| `build.rs` | embeds the icon in the exe |

Profile data lives in `%LOCALAPPDATA%\X-Browser`.
