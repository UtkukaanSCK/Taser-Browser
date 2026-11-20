//! Bookmarks (v0.2 task 6): `bookmarks.json`, the star in the address box,
//! the Ctrl+D editor, the Ctrl+Shift+O list and the bookmarks among the
//! address-bar suggestions.
//!
//! - The file is `{"v":1,"items":[{"url","title","added"}]}` (see `store`).
//!   What is read back is untrusted: every item is re-checked on load and a
//!   bad or repeated one is dropped (`BookmarkFile::sanitize`).
//! - Only the trusted UI (and its overlay) can reach any of this: every
//!   command arrives as a `UiCommand` after the `is_ui_origin` check, and the
//!   address an editor or list row acts on is held here, never taken from the
//!   page that sent the command. Nothing is exposed to content webviews.
//! - The UI webview shows the star (`xbBm.star`); the overlay webview shows
//!   the editor and the list (`xbBm.editor`, `xbBm.list`). Both are drawn by
//!   `ui/js/bookmarks.js`, which keeps no state of its own.

use crate::app::{App, UI_HEIGHT};
use crate::commands::Mods;
use crate::history::{self, Candidate, SuggestSource};
use crate::overlay::{self, Mode, win_size};
use crate::page_tools;
use crate::store::{Store, Stored};
use crate::tabs::Placement;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tao::dpi::LogicalSize;
use wry::Rect;

pub const FILE: &str = "bookmarks.json";
/// Most bookmarks kept.
pub const MAX_ITEMS: usize = 10_000;
/// Larger files are not read and not written.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Longest title (characters); the same limit as history's.
pub const MAX_TITLE: usize = history::MAX_TITLE;
/// Rows the list shows at once (the rest is reached by filtering).
const LIST_MAX: usize = history::LIST_MAX;
/// Room for `{"v":1,"items":[]}` and a margin.
const FILE_OVERHEAD: usize = 64;
/// Source weight in the suggestions (history is 1.0): a bookmarked address
/// ranks above an equally good history match.
const WEIGHT: f32 = 1.5;
/// Longest editor title accepted over IPC (bytes): `MAX_TITLE` characters.
const MAX_TITLE_ARG: usize = MAX_TITLE * 4;
/// The editor popup (logical px); must fit overlay.html's `#bm-edit`.
const EDITOR_W: f64 = 320.0;
const EDITOR_H: f64 = 156.0;
/// The editor closing on blur and the star's press that caused it come
/// within this of each other, whichever is first.
const PRESS_WINDOW: Duration = Duration::from_millis(500);
/// A press and its click (a slow one included).
const CLICK_WINDOW: Duration = Duration::from_secs(5);

// ------------------------------------------------------------------ file

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    #[serde(default)]
    pub title: String,
    /// Unix seconds.
    #[serde(default)]
    pub added: u64,
}

#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct BookmarkFile {
    #[serde(deserialize_with = "lenient")]
    pub items: Vec<Bookmark>,
}

/// The items of an array, each read on its own: one that is not a bookmark
/// (a number, `null`, a wrong field type) is skipped, not fatal to the file.
fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Bookmark>, D::Error> {
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

/// Bytes `b` takes in the file (with its separator).
fn encoded_len(b: &Bookmark) -> usize {
    serde_json::to_string(b).map_or(usize::MAX / 2, |s| s.len() + 1)
}

/// What the items may add up to: `MAX_FILE_BYTES` less the file's frame.
const BUDGET: usize = MAX_FILE_BYTES as usize - FILE_OVERHEAD;

/// A bookmark within the limits, or None if it must not be kept: a web
/// address (no `javascript:`, `file:`, the browser's own pages...) of at most
/// 8 KiB without control characters or `user:password@` (a `#fragment` stays:
/// on many sites it is the page); a title of at most `MAX_TITLE` characters
/// (longer ones are cut, control characters dropped); an `added` that is not
/// in the future. The star looks pages up by the same rule (`page_key`).
fn clean(b: Bookmark, now: u64) -> Option<Bookmark> {
    let url = history::without_userinfo(&b.url);
    history::recordable(&url).then(|| Bookmark { url, title: history::clean_title(&b.title), added: b.added.min(now) })
}

impl Stored for BookmarkFile {
    const NAME: &'static str = FILE;
    const MAX_BYTES: u64 = MAX_FILE_BYTES;

    fn sanitize(&mut self) {
        let now = history::now_secs();
        let mut seen = HashSet::new();
        let mut bytes = 0;
        let mut kept = Vec::new();
        for b in std::mem::take(&mut self.items) {
            let Some(b) = clean(b, now) else { continue };
            if !seen.insert(b.url.clone()) {
                continue;
            }
            let n = encoded_len(&b);
            if kept.len() >= MAX_ITEMS || bytes + n > BUDGET {
                break;
            }
            bytes += n;
            kept.push(b);
        }
        self.items = kept;
    }
}

// ----------------------------------------------------------------- state

/// What the overlay shows while it is in `Mode::Bookmarks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum View {
    #[default]
    None,
    Editor,
    List,
}

#[derive(Default)]
pub struct Bookmarks {
    store: Option<Store<BookmarkFile>>,
    file: BookmarkFile,
    urls: HashSet<String>,
    /// What the items take on disk, the file's frame not counted (see `BUDGET`).
    bytes: usize,
    loaded: bool,
    /// What the star was last told: (can bookmark this page, it is bookmarked).
    star_sent: Option<(bool, bool)>,
    /// The star's left edge and width in the UI webview (CSS px).
    anchor: (u16, u16),
    view: View,
    /// The address the open editor works on.
    editing: Option<String>,
    /// Bumped whenever a list is drawn; picks name the list they were on.
    serial: u32,
    /// The addresses of the list drawn as `serial`.
    shown: Vec<String>,
    filter: String,
    /// The file being read on its own thread (`preload`): the store comes
    /// back with the items.
    loading: Option<mpsc::Receiver<(Store<BookmarkFile>, Vec<Bookmark>)>>,
    /// When the editor was last closed by losing the focus.
    blurred: Option<Instant>,
    /// The star was pressed while the editor was open (or just closed by
    /// that press): its click closes the editor instead of opening it.
    press_closes: Option<Instant>,
}

impl Bookmarks {
    /// Nothing is read until `preload` or first needed (see `ensure_loaded`).
    pub fn new(dir: Option<&Path>) -> Self {
        Self { store: dir.map(Store::new), ..Self::default() }
    }

    /// Starts reading the file on its own thread, off the UI thread (once);
    /// `wake` is called there when it is ready to `poll`.
    pub fn preload(&mut self, wake: impl FnOnce() + Send + 'static) {
        if self.loaded || self.loading.is_some() {
            return;
        }
        let Some(mut store) = self.store.take() else {
            self.loaded = true;
            return;
        };
        let dir = store.path().parent().map(Path::to_path_buf);
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("xb-bookmarks".into()).spawn(move || {
            let items = store.load().value.items;
            let _ = tx.send((store, items));
            wake();
        });
        match spawned {
            Ok(_) => self.loading = Some(rx),
            Err(e) => {
                eprintln!("x-browser: no bookmarks thread ({e}); reading inline");
                self.store = dir.as_deref().map(Store::new);
                self.ensure_loaded();
            }
        }
    }

    /// Takes the file `preload` read, if it is ready (never waits). True
    /// when it was taken now.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.loading else { return false };
        match rx.try_recv() {
            Err(mpsc::TryRecvError::Empty) => false,
            got => {
                self.loading = None;
                self.take_loaded(got.ok());
                true
            }
        }
    }

    fn take_loaded(&mut self, got: Option<(Store<BookmarkFile>, Vec<Bookmark>)>) {
        self.loaded = true;
        match got {
            Some((store, items)) => {
                self.store = Some(store);
                self.set(items);
            }
            // The thread died reading: without the file nothing may
            // overwrite it, so this run keeps bookmarks in memory only.
            None => eprintln!("x-browser: the bookmarks file could not be read; changes are not saved"),
        }
    }

    /// Reads the file once (a few ms at most: 4 MiB is the limit), or
    /// waits for `preload` to finish reading it.
    pub fn ensure_loaded(&mut self) {
        if let Some(rx) = self.loading.take() {
            let got = rx.recv().ok();
            return self.take_loaded(got);
        }
        if std::mem::replace(&mut self.loaded, true) {
            return;
        }
        if let Some(store) = &mut self.store {
            let loaded = store.load();
            self.set(loaded.value.items);
        }
    }

    /// Replaces the items (already within the limits).
    fn set(&mut self, items: Vec<Bookmark>) {
        self.urls = items.iter().map(|b| b.url.clone()).collect();
        self.bytes = items.iter().map(encoded_len).sum();
        self.file.items = items;
    }

    /// The bookmarks, in the order of the list.
    pub fn items(&self) -> &[Bookmark] {
        &self.file.items
    }

    /// The file's path (None without a data folder).
    #[cfg(feature = "selftest")]
    pub fn path(&self) -> Option<&Path> {
        self.store.as_ref().map(Store::path)
    }

    /// Whether `url` (as `history::without_userinfo` leaves it) is bookmarked.
    pub fn contains(&self, url: &str) -> bool {
        self.urls.contains(url)
    }

    fn position(&self, url: &str) -> Option<usize> {
        self.urls.contains(url).then(|| self.file.items.iter().position(|b| b.url == url)).flatten()
    }

    fn save(&mut self) {
        if let Some(store) = &mut self.store {
            store.save(&self.file);
        }
    }

    /// Bookmarks `url` (it is made fit to keep first; an address already
    /// there stays as it is). False when it cannot be kept: not a web
    /// address, or the file is full.
    pub fn add(&mut self, url: &str, title: &str) -> bool {
        self.ensure_loaded();
        let Some(b) = clean(Bookmark { url: url.into(), title: title.into(), added: history::now_secs() }, history::now_secs())
        else {
            return false;
        };
        if self.urls.contains(&b.url) {
            return true;
        }
        let n = encoded_len(&b);
        if self.file.items.len() >= MAX_ITEMS || self.bytes + n > BUDGET {
            return false;
        }
        self.bytes += n;
        self.urls.insert(b.url.clone());
        self.file.items.push(b);
        self.save();
        true
    }

    /// Removes `url`; whether it was there.
    pub fn remove(&mut self, url: &str) -> bool {
        self.ensure_loaded();
        let Some(i) = self.position(url) else { return false };
        let b = self.file.items.remove(i);
        self.urls.remove(&b.url);
        self.bytes = self.bytes.saturating_sub(encoded_len(&b));
        self.save();
        true
    }

    /// Gives `url` a new title; whether that changed anything.
    pub fn rename(&mut self, url: &str, title: &str) -> bool {
        self.ensure_loaded();
        let Some(i) = self.position(url) else { return false };
        let title = history::clean_title(title);
        let old = &self.file.items[i];
        if old.title == title {
            return false;
        }
        let renamed = Bookmark { title, ..old.clone() };
        let bytes = self.bytes - encoded_len(old) + encoded_len(&renamed);
        if bytes > BUDGET {
            return false;
        }
        self.bytes = bytes;
        self.file.items[i] = renamed;
        self.save();
        true
    }

    fn title_of(&self, url: &str) -> &str {
        self.position(url).map_or("", |i| &self.file.items[i].title)
    }

    /// The list: newest first, those matching `filter` (all when empty), at
    /// most `LIST_MAX`; and how many matched.
    fn list(&self, filter: &str) -> (Vec<Bookmark>, usize) {
        let q = history::url_key(filter.trim());
        let mut rows: Vec<&Bookmark> = self
            .file
            .items
            .iter()
            .rev()
            .filter(|b| q.is_empty() || history::url_key(&b.url).contains(&q) || b.title.to_lowercase().contains(&q))
            .collect();
        // Stable: bookmarks added in the same second keep the later first.
        rows.sort_by_key(|b| std::cmp::Reverse(b.added));
        let total = rows.len();
        rows.truncate(LIST_MAX);
        (rows.into_iter().cloned().collect(), total)
    }
}

/// Bookmarks are one more source for the address-bar suggestions. Their
/// "last visit" is now and they count as visited once, so a bookmark never
/// ranks below history that matches as well (weight `WEIGHT` on top).
impl SuggestSource for Bookmarks {
    fn each<'s>(&'s self, f: &mut dyn FnMut(Candidate<'s, '_>)) {
        let now = history::now_secs();
        for b in &self.file.items {
            let key = history::url_key(&b.url);
            let tkey = b.title.to_lowercase();
            f(Candidate { url: &b.url, title: &b.title, key: &key, tkey: &tkey, last: now, count: 1, weight: WEIGHT });
        }
    }
}

// ----------------------------------------------------------- suggestions

/// A suggestion row: `b` marks a bookmarked address (a star in the list).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Marked {
    pub u: String,
    pub t: String,
    pub b: bool,
}

/// Suggestions for typed text: history and bookmarks together, best first,
/// each address once, at most `history::SUGGESTIONS`.
pub fn suggest(app: &mut App, text: &str) -> Vec<Marked> {
    app.bookmarks.ensure_loaded();
    let found = app.history.suggest_with(text, &app.bookmarks);
    found.into_iter().map(|s| Marked { b: app.bookmarks.contains(&s.u), u: s.u, t: s.t }).collect()
}

// -------------------------------------------------------------- commands

/// What the UI and the overlay page may ask for (see `UiCommand::Bookmark`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkCmd {
    /// The star was pressed; `x`, `w`: its left edge and width.
    Star { x: u16, w: u16 },
    /// The mouse went down on the star (its click follows).
    Press,
    /// The star moved or was resized.
    Anchor { x: u16, w: u16 },
    /// The editor's Done / Enter: keep the title and close.
    Save(String),
    /// The editor's Remove.
    Remove,
    Filter(String),
    /// Row `i` of the list drawn as `serial`; `tab`: in a new tab.
    Open { serial: u32, i: usize, tab: bool },
    Delete { serial: u32, i: usize },
}

impl BookmarkCmd {
    pub const NAMES: &[&str] =
        &["bm-star", "bm-star-press", "bm-anchor", "bm-save", "bm-remove", "bm-filter", "bm-open", "bm-delete"];

    pub fn parse(name: &str, arg: Option<&str>) -> Option<Self> {
        let edge = |a: &str| {
            let (x, w) = a.split_once(':')?;
            Some((x.parse::<u16>().ok()?, w.parse::<u16>().ok()?))
        };
        let row = |a: &str| {
            let (g, i) = a.split_once(':')?;
            Some((g.parse::<u32>().ok()?, i.parse::<usize>().ok()?))
        };
        Some(match (name, arg) {
            ("bm-star-press", None) => Self::Press,
            ("bm-star", Some(a)) => {
                let (x, w) = edge(a)?;
                Self::Star { x, w }
            }
            ("bm-anchor", Some(a)) => {
                let (x, w) = edge(a)?;
                Self::Anchor { x, w }
            }
            ("bm-save", None) => Self::Save(String::new()),
            ("bm-save", Some(a)) if a.len() <= MAX_TITLE_ARG => Self::Save(a.to_string()),
            ("bm-remove", None) => Self::Remove,
            ("bm-filter", None) => Self::Filter(String::new()),
            ("bm-filter", Some(a)) if a.len() <= overlay::MAX_FILTER => Self::Filter(a.to_string()),
            ("bm-open", Some(a)) => {
                let (rest, how) = a.rsplit_once(':')?;
                let (serial, i) = row(rest)?;
                let tab = match how {
                    "t" => true,
                    "s" => false,
                    _ => return None,
                };
                Self::Open { serial, i, tab }
            }
            ("bm-delete", Some(a)) => {
                let (serial, i) = row(a)?;
                Self::Delete { serial, i }
            }
            _ => return None,
        })
    }
}

/// Keys: Ctrl+D and Ctrl+Shift+O. Mapped here from a native key event, so
/// content pages cannot inject one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkKey {
    /// Ctrl+D: bookmark this page / edit its bookmark.
    Toggle,
    /// Ctrl+Shift+O: the list.
    List,
}

impl BookmarkKey {
    pub fn from_key(vk: u32, m: Mods) -> Option<Self> {
        const D: u32 = 0x44;
        const O: u32 = 0x4F;
        // Ctrl+Alt is AltGr: never a shortcut.
        let ctrl = m.ctrl && !m.alt;
        match vk {
            D if ctrl && !m.shift => Some(Self::Toggle),
            O if ctrl && m.shift => Some(Self::List),
            _ => None,
        }
    }
}

pub fn on_key(app: &mut App, key: BookmarkKey) {
    match key {
        BookmarkKey::Toggle => star(app, true),
        BookmarkKey::List => {
            if showing(app, View::List) {
                overlay::close(app);
                app.focus_after_switch();
            } else {
                open_list(app);
            }
        }
    }
}

pub fn on_command(app: &mut App, cmd: BookmarkCmd) {
    match cmd {
        BookmarkCmd::Anchor { x, w } => app.bookmarks.anchor = (x, w),
        BookmarkCmd::Press => {
            // Pressing the star moves the focus to the strip, which closes an
            // open editor (on blur) before the click arrives: note here, in
            // whichever order the two came, that this click is to close it.
            let open = showing(app, View::Editor);
            let b = &mut app.bookmarks;
            let blurred = b.blurred.take().is_some_and(|t| t.elapsed() < PRESS_WINDOW);
            b.press_closes = (blurred || open).then(Instant::now);
        }
        BookmarkCmd::Star { x, w } => {
            app.bookmarks.anchor = (x, w);
            // The star toggles the editor: a click while it was open closes it.
            if app.bookmarks.press_closes.take().is_some_and(|t| t.elapsed() < CLICK_WINDOW) {
                if showing(app, View::Editor) {
                    finish(app);
                }
                return;
            }
            star(app, false);
        }
        BookmarkCmd::Save(title) => {
            if let Some(url) = editing(app) {
                app.bookmarks.rename(&url, &title);
                finish(app);
            }
        }
        BookmarkCmd::Remove => {
            if let Some(url) = editing(app) {
                app.bookmarks.remove(&url);
                finish(app);
            }
        }
        BookmarkCmd::Filter(text) => {
            if showing(app, View::List) {
                app.bookmarks.filter = text;
                render_list(app, true);
            }
        }
        BookmarkCmd::Open { serial, i, tab } => {
            if !showing(app, View::List) || serial != app.bookmarks.serial {
                return;
            }
            let Some(url) = app.bookmarks.shown.get(i).cloned() else { return };
            overlay::close(app);
            if tab {
                if app.open_tab(&url, Placement::End, true).is_ok() {
                    app.focus_after_switch();
                }
            } else {
                app.navigate(&url);
            }
        }
        BookmarkCmd::Delete { serial, i } => {
            if showing(app, View::List)
                && serial == app.bookmarks.serial
                && let Some(url) = app.bookmarks.shown.get(i).cloned()
            {
                app.bookmarks.remove(&url);
                render_list(app, true);
            }
        }
    }
}

/// The overlay is closing because it lost the focus: if that is the editor
/// and the star was what took the focus, its click is to close it.
pub fn closing_on_blur(app: &mut App) {
    if showing(app, View::Editor) {
        app.bookmarks.blurred = Some(Instant::now());
    }
}

/// The UI page has (re)loaded: it knows no star state yet. (The file is
/// being read meanwhile, see `Bookmarks::preload`; `sync_star` shows it.)
pub fn on_ui_ready(app: &mut App) {
    app.bookmarks.star_sent = None;
}

fn showing(app: &App, view: View) -> bool {
    app.overlay.mode == Mode::Bookmarks && app.bookmarks.view == view
}

/// The address the open editor works on.
fn editing(app: &App) -> Option<String> {
    if showing(app, View::Editor) { app.bookmarks.editing.clone() } else { None }
}

/// Editor done: closes it and gives the keyboard back, as Esc does.
fn finish(app: &mut App) {
    overlay::close(app);
    app.focus_after_switch();
}

// ------------------------------------------------------------------ star

/// The active tab's committed page address, if it can be bookmarked. Not the
/// typed address: that shows in the address bar before the page commits.
fn page_key(app: &App) -> Option<String> {
    let tab = app.tabs.get(app.tabs.active()?)?;
    let url = history::without_userinfo(&tab.page_url);
    history::recordable(&url).then_some(url)
}

/// Tells the UI's star whether the active page can be bookmarked and is.
/// Called at the end of every event-loop pass: a navigation commits without
/// marking the strip dirty, so this is its own check.
pub fn sync_star(app: &mut App) {
    app.bookmarks.poll();
    // Still loading: an empty star now would flip to filled a moment later
    // (and be read out twice); the load's wake event brings another pass.
    if !app.ui_ready || app.bookmarks.loading.is_some() {
        return;
    }
    let state = page_key(app).map_or((false, false), |u| (true, app.bookmarks.contains(&u)));
    if app.bookmarks.star_sent == Some(state) {
        return;
    }
    let js = format!("xbBm.star({},{})", state.0, state.1);
    if app.ui.evaluate_script(&js).is_ok() {
        app.bookmarks.star_sent = Some(state);
    }
}

/// The star or Ctrl+D: bookmarks the page if it is not yet, then opens the
/// editor. `key`: from Ctrl+D, where an existing bookmark opens with Remove
/// focused (Ctrl+D, Enter removes it).
fn star(app: &mut App, key: bool) {
    // The strip is where the star is: fullscreen gives way first.
    if app.fullscreen.on() {
        page_tools::leave_fullscreen(app);
    }
    let Some(url) = page_key(app) else {
        let _ = app.ui.evaluate_script("xb.blocked()");
        return;
    };
    app.bookmarks.ensure_loaded();
    let added = !app.bookmarks.contains(&url);
    if added {
        let title = app.tabs.active().and_then(|id| app.tabs.get(id)).map(|t| t.title.clone()).unwrap_or_default();
        if !app.bookmarks.add(&url, &title) {
            let _ = app.ui.evaluate_script("xb.blocked()");
            return;
        }
    }
    open_editor(app, url, added, key && !added);
}

#[derive(Serialize)]
struct EditorView<'a> {
    url: &'a str,
    title: &'a str,
    /// Just bookmarked by this press.
    added: bool,
    /// Focus Remove instead of the title.
    remove: bool,
}

fn open_editor(app: &mut App, url: String, added: bool, remove: bool) {
    let b = &mut app.bookmarks;
    let view = EditorView { url: &url, title: b.title_of(&url), added, remove };
    let Ok(json) = serde_json::to_string(&view) else { return };
    let js = format!("xbBm.editor({json})");
    b.editing = Some(url);
    b.view = View::Editor;
    let bounds = editor_rect(win_size(app), app.bookmarks.anchor);
    if app.overlay.mode != Mode::Bookmarks {
        overlay::close(app);
    }
    overlay::show(app, Mode::Bookmarks, js, bounds);
}

/// The editor hangs under the star, its right edge at the star's, kept
/// inside the window.
fn editor_rect(size: LogicalSize<f64>, (x, w): (u16, u16)) -> Rect {
    let width = EDITOR_W.min(size.width - 16.0).max(1.0);
    let right = if w == 0 { size.width - 8.0 } else { f64::from(x) + f64::from(w) };
    let left = (right - width).min(size.width - width - 8.0).max(0.0);
    let y = UI_HEIGHT - 2.0;
    overlay::rect(left, y, width, EDITOR_H.min((size.height - y - 8.0).max(1.0)))
}

// ------------------------------------------------------------------ list

#[derive(Serialize)]
struct Row<'a> {
    u: &'a str,
    t: &'a str,
}

#[derive(Serialize)]
struct ListView<'a> {
    serial: u32,
    items: &'a [Row<'a>],
    total: usize,
    filter: &'a str,
}

/// Opens the bookmarks list (Ctrl+Shift+O; the menu). One already open is
/// drawn afresh.
pub fn open_list(app: &mut App) {
    // The list belongs to the browser frame: fullscreen gives way first.
    if app.fullscreen.on() {
        page_tools::leave_fullscreen(app);
    }
    // (Mode Hidden: nothing to close, and a handed-over webview must stay up.)
    if app.overlay.mode != Mode::Hidden && !showing(app, View::List) {
        overlay::close(app);
    }
    app.bookmarks.ensure_loaded();
    app.bookmarks.filter.clear();
    render_list(app, false);
}

/// `keep`: an update of the open list (the search text and focus stay).
fn render_list(app: &mut App, keep: bool) {
    let b = &mut app.bookmarks;
    let (items, total) = b.list(&b.filter);
    b.serial = b.serial.wrapping_add(1);
    b.shown = items.iter().map(|i| i.url.clone()).collect();
    b.view = View::List;
    b.editing = None;
    let rows: Vec<Row> = items.iter().map(|i| Row { u: &i.url, t: &i.title }).collect();
    let view = ListView { serial: b.serial, items: &rows, total, filter: &b.filter };
    let Ok(json) = serde_json::to_string(&view) else { return };
    let js = format!("xbBm.list({json},{keep})");
    let bounds = overlay::panel_rect(win_size(app));
    overlay::show(app, Mode::Bookmarks, js, bounds);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::UiCommand;
    use crate::store::tests::temp_dir;
    use std::time::Duration;

    const NO: Mods = Mods { ctrl: false, shift: false, alt: false };
    const CTRL: Mods = Mods { ctrl: true, ..NO };

    fn bm(url: &str, title: &str, added: u64) -> Bookmark {
        Bookmark { url: url.into(), title: title.into(), added }
    }

    fn cleaned(items: Vec<Bookmark>) -> Vec<Bookmark> {
        let mut f = BookmarkFile { items };
        f.sanitize();
        f.items
    }

    #[test]
    fn sanitize_keeps_web_addresses_only() {
        let kept = cleaned(vec![
            bm("https://a.example/", "A", 1),
            bm("http://b.example/x?y=1", "B", 2),
            bm("javascript:alert(1)", "J", 3),
            bm("file:///C:/Windows/win.ini", "F", 4),
            bm("data:text/html,<b>x</b>", "D", 5),
            bm("about:blank", "", 6),
            bm("http://xb.ui/", "the browser's own page", 7),
            bm("https://c.example/\n", "control", 8),
            bm("", "", 9),
            bm("HTTPS://UPPER.example/", "scheme case", 10),
        ]);
        let urls: Vec<&str> = kept.iter().map(|b| b.url.as_str()).collect();
        assert_eq!(urls, ["https://a.example/", "http://b.example/x?y=1", "HTTPS://UPPER.example/"]);
    }

    #[test]
    fn sanitize_bounds_every_field() {
        let long_url = format!("https://a.example/{}", "x".repeat(history::MAX_TITLE * 20));
        let ok_url = format!("https://b.example/{}", "x".repeat(8 * 1024 - 30));
        let title = format!("a\u{7}b\n{}", "ü".repeat(MAX_TITLE * 2));
        let future = history::now_secs() + 1_000_000;
        let kept = cleaned(vec![
            bm(&long_url, "too long", 1),
            bm(&ok_url, "fits", 2),
            bm("https://c.example/", &title, future),
            bm("https://user:pw@d.example/page#frag", "  padded  ", 4),
        ]);
        assert_eq!(kept.len(), 3, "{kept:?}");
        assert_eq!(kept[0].url, ok_url);
        assert_eq!(kept[1].title.chars().count(), MAX_TITLE, "cut, not dropped");
        assert!(kept[1].title.starts_with("ab"), "control characters are gone: {}", &kept[1].title[..8]);
        assert!(kept[1].added <= history::now_secs(), "never from the future");
        assert_eq!(kept[2], bm("https://d.example/page#frag", "padded", 4), "no userinfo, the fragment kept, trimmed");
    }

    #[test]
    fn sanitize_drops_repeats_keeping_the_first() {
        let kept = cleaned(vec![
            bm("https://a.example/", "first", 1),
            bm("https://a.example/", "second", 2),
            bm("https://me@a.example/#", "same page", 3),
            bm("https://a.example/other", "other", 4),
            bm("https://a.example/#/inbox", "a route of the page", 5),
        ]);
        assert_eq!(kept.iter().map(|b| b.title.as_str()).collect::<Vec<_>>(), ["first", "other", "a route of the page"]);
    }

    #[test]
    fn sanitize_caps_the_count() {
        let many: Vec<Bookmark> = (0..MAX_ITEMS + 25).map(|i| bm(&format!("https://a.example/{i}"), "", 1)).collect();
        let kept = cleaned(many);
        assert_eq!(kept.len(), MAX_ITEMS);
        assert_eq!(kept[0].url, "https://a.example/0", "the first ones stay");
    }

    #[test]
    fn what_is_kept_always_fits_the_file() {
        // Worst case per item: an 8 KiB address and a full title.
        let items: Vec<Bookmark> = (0..2000)
            .map(|i| bm(&format!("https://a.example/{i}/{}", "x".repeat(8 * 1024 - 40)), &"ü".repeat(MAX_TITLE), 1))
            .collect();
        let kept = cleaned(items);
        assert!(kept.len() < 2000, "the byte budget stops it before the count does");
        let bytes = serde_json::to_vec(&BookmarkFile { items: kept }).unwrap().len();
        assert!((bytes as u64) < MAX_FILE_BYTES, "{bytes}");
    }

    #[test]
    fn adds_removes_renames_and_refuses_what_it_cannot_keep() {
        let mut b = Bookmarks::default();
        assert!(b.add("https://u:secret@a.example/#/inbox", "Inbox"));
        assert!(b.contains("https://a.example/#/inbox"), "stored with its fragment, without the user info");
        assert!(!b.contains("https://a.example/"), "a route is not the bare page");
        assert!(b.remove("https://a.example/#/inbox"));
        assert!(b.add("https://a.example/#", " A  page "));
        assert!(b.contains("https://a.example/"), "an empty fragment is none");
        assert_eq!(b.title_of("https://a.example/"), "A  page");
        assert!(b.add("https://a.example/", "again"), "already there: fine, unchanged");
        assert_eq!(b.items().len(), 1);
        assert_eq!(b.title_of("https://a.example/"), "A  page");
        assert!(!b.add("javascript:alert(1)", "x"));
        assert!(!b.add("about:blank", "x"));
        assert!(b.rename("https://a.example/", "New"));
        assert!(!b.rename("https://a.example/", "New"), "same title: nothing to save");
        assert!(!b.rename("https://nope.example/", "x"));
        assert!(b.remove("https://a.example/"));
        assert!(!b.remove("https://a.example/"));
        assert!(b.items().is_empty() && !b.contains("https://a.example/"));
        assert_eq!(b.bytes, 0, "the size account is back to empty");
    }

    #[test]
    fn a_full_file_takes_no_more() {
        let mut b = Bookmarks { loaded: true, ..Bookmarks::default() };
        b.set((0..MAX_ITEMS).map(|i| bm(&format!("https://a.example/{i}"), "", 1)).collect());
        assert!(!b.add("https://new.example/", "x"));
        assert!(b.remove("https://a.example/5"));
        assert!(b.add("https://new.example/", "x"));
    }

    #[test]
    fn saves_the_documented_format_and_loads_it_back() {
        let dir = temp_dir("bm-roundtrip");
        let mut b = Bookmarks::new(Some(&dir));
        assert!(b.add("https://a.example/", "A"));
        assert!(b.add("https://b.example/x", "B"));
        assert!(b.rename("https://b.example/x", "Bee"));
        assert!(b.remove("https://a.example/"));
        assert!(crate::store::flush(Duration::from_secs(5)));
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
        assert_eq!(v["items"][0]["url"], "https://b.example/x");
        assert_eq!(v["items"][0]["title"], "Bee");
        assert!(v["items"][0]["added"].is_u64());
        let mut again = Bookmarks::new(Some(&dir));
        again.ensure_loaded();
        assert_eq!(again.items(), b.items());
        assert!(again.contains("https://b.example/x"));
    }

    #[test]
    fn the_file_is_read_off_the_calling_thread() {
        let dir = temp_dir("bm-preload");
        let mut first = Bookmarks::new(Some(&dir));
        assert!(first.add("https://a.example/", "A"));
        assert!(crate::store::flush(Duration::from_secs(5)));
        let mut b = Bookmarks::new(Some(&dir));
        let (tx, rx) = mpsc::channel();
        let caller = std::thread::current().id();
        b.preload(move || {
            let _ = tx.send(std::thread::current().id());
        });
        assert_ne!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), caller);
        assert!(!b.contains("https://a.example/"), "not taken before poll");
        assert!(b.poll() && !b.poll());
        assert!(b.contains("https://a.example/"));
        // The store came back with it: changes are saved.
        assert!(b.add("https://b.example/", "B"));
        assert!(crate::store::flush(Duration::from_secs(5)));
        let mut again = Bookmarks::new(Some(&dir));
        again.ensure_loaded();
        assert_eq!(again.items().len(), 2);
        // A change before the read finished waits for it, then applies.
        let mut c = Bookmarks::new(Some(&dir));
        c.preload(|| {});
        assert!(c.remove("https://a.example/"));
        assert_eq!(c.items().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hostile_file_loads_sanitized() {
        let dir = temp_dir("bm-hostile");
        let big_title = "T".repeat(100_000);
        let file = serde_json::json!({
            "v": 1,
            "items": [
                {"url": "https://ok.example/", "title": "ok", "added": 5},
                {"url": "javascript:alert(document.cookie)", "title": "x", "added": 1},
                {"url": "file:///C:/Users/", "title": "x", "added": 1},
                {"url": "https://big.example/", "title": big_title, "added": 1},
                {"url": "https://ok.example/", "title": "duplicate", "added": 9},
                {"url": "https://me:pw@ok.example/", "title": "duplicate too", "added": 9},
                {"url": 5, "title": "wrong type"},
                {"title": "no url"},
                null,
                42,
                "https://string.example/",
                {"url": "https://nots.example/", "added": -1},
                {"url": "https://noextras.example/", "extra": {"deep": [1, 2, 3]}},
            ],
            "unknown": true
        });
        std::fs::write(dir.join(FILE), file.to_string()).unwrap();
        let mut b = Bookmarks::new(Some(&dir));
        b.ensure_loaded();
        let urls: Vec<&str> = b.items().iter().map(|b| b.url.as_str()).collect();
        assert_eq!(urls, ["https://ok.example/", "https://big.example/", "https://noextras.example/"]);
        assert_eq!(b.items()[0].title, "ok", "the first of the repeats stays");
        assert_eq!(b.items()[1].title.chars().count(), MAX_TITLE);
        assert!(b.contains("https://ok.example/") && !b.contains("javascript:alert(document.cookie)"));
    }

    #[test]
    fn a_file_that_is_not_a_bookmark_file_is_set_aside() {
        let dir = temp_dir("bm-garbage");
        for (i, text) in ["not json", "[1,2]", r#"{"v":1,"items":"x"}"#, r#"{"items":[]}"#].into_iter().enumerate() {
            std::fs::write(dir.join(FILE), text).unwrap();
            let mut b = Bookmarks::new(Some(&dir));
            b.ensure_loaded();
            assert!(b.items().is_empty(), "case {i}");
            assert!(dir.join("bookmarks.json.corrupt").exists(), "case {i}");
            let _ = std::fs::remove_file(dir.join("bookmarks.json.corrupt"));
        }
        // Over the size limit: not read at all.
        std::fs::write(dir.join(FILE), vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
        let mut b = Bookmarks::new(Some(&dir));
        b.ensure_loaded();
        assert!(b.items().is_empty());
    }

    #[test]
    fn the_list_filters_and_shows_the_newest_first() {
        let mut b = Bookmarks { loaded: true, ..Bookmarks::default() };
        b.set(vec![
            bm("https://old.example/", "Old stuff", 10),
            bm("https://new.example/", "Fresh", 30),
            bm("https://mid.example/a", "Middle", 20),
        ]);
        let urls = |f: &str| b.list(f).0.into_iter().map(|b| b.url).collect::<Vec<_>>();
        assert_eq!(urls(""), ["https://new.example/", "https://mid.example/a", "https://old.example/"]);
        assert_eq!(urls("mid"), ["https://mid.example/a"], "address");
        assert_eq!(urls("STUFF"), ["https://old.example/"], "title, any case");
        assert_eq!(urls("https://www.new"), ["https://new.example/"], "typed like the address bar");
        assert!(urls("zzz").is_empty());
        assert_eq!(b.list("example").1, 3);
    }

    /// A stand-in for history in the ranking.
    struct Fake(Vec<(&'static str, &'static str, u32)>);

    impl SuggestSource for Fake {
        fn each<'s>(&'s self, f: &mut dyn FnMut(Candidate<'s, '_>)) {
            for (url, title, count) in &self.0 {
                let (key, tkey) = (history::url_key(url), title.to_lowercase());
                f(Candidate { url, title, key: &key, tkey: &tkey, last: history::now_secs(), count: *count, weight: 1.0 });
            }
        }
    }

    #[test]
    fn bookmarks_rank_above_equal_history_and_share_a_row() {
        let mut b = Bookmarks { loaded: true, ..Bookmarks::default() };
        b.set(vec![bm("https://marked.example/", "Marked page", 1), bm("https://both.example/", "Both", 1)]);
        let history = Fake(vec![
            ("https://plain.example/", "Plain page", 1),
            ("https://both.example/", "Both", 1),
            ("https://marked.example/old", "", 0),
        ]);
        let found = history::rank("page", &[&history, &b], history::now_secs(), history::SUGGESTIONS);
        let urls: Vec<&str> = found.iter().map(|s| s.u.as_str()).collect();
        let at = |u: &str| urls.iter().position(|x| *x == u).unwrap_or(usize::MAX);
        assert!(at("https://marked.example/") < at("https://plain.example/"), "{urls:?}");
        // "both" is a prefix-less title match like the others; it is in one row.
        let found = history::rank("both", &[&history, &b], history::now_secs(), history::SUGGESTIONS);
        assert_eq!(found.iter().filter(|s| s.u == "https://both.example/").count(), 1, "{found:?}");
        assert!(found.len() <= history::SUGGESTIONS);
    }

    #[test]
    fn only_eight_suggestions_come_out_in_total() {
        let mut b = Bookmarks { loaded: true, ..Bookmarks::default() };
        b.set((0..20).map(|i| bm(&format!("https://site.example/b{i}"), "", 1)).collect());
        let history = Fake((0..20).map(|_| ("https://site.example/h", "", 1)).collect());
        let found = history::rank("site", &[&history, &b], history::now_secs(), history::SUGGESTIONS);
        assert_eq!(found.len(), history::SUGGESTIONS);
    }

    #[test]
    fn parses_bookmark_messages() {
        let p = |m: &str| UiCommand::parse(m);
        let bm = |c| Some(UiCommand::Bookmark(c));
        assert_eq!(p("bm-star:700:20"), bm(BookmarkCmd::Star { x: 700, w: 20 }));
        assert_eq!(p("bm-star-press"), bm(BookmarkCmd::Press));
        assert_eq!(p("bm-star-press:1"), None);
        assert_eq!(p("bm-anchor:0:0"), bm(BookmarkCmd::Anchor { x: 0, w: 0 }));
        assert_eq!(p("bm-save:My: title"), bm(BookmarkCmd::Save("My: title".into())), "only the first colon splits");
        assert_eq!(p("bm-save"), bm(BookmarkCmd::Save(String::new())));
        assert_eq!(p("bm-remove"), bm(BookmarkCmd::Remove));
        assert_eq!(p("bm-filter"), bm(BookmarkCmd::Filter(String::new())));
        assert_eq!(p("bm-filter:a:b"), bm(BookmarkCmd::Filter("a:b".into())));
        assert_eq!(p("bm-open:3:7:s"), bm(BookmarkCmd::Open { serial: 3, i: 7, tab: false }));
        assert_eq!(p("bm-open:3:7:t"), bm(BookmarkCmd::Open { serial: 3, i: 7, tab: true }));
        assert_eq!(p("bm-delete:3:0"), bm(BookmarkCmd::Delete { serial: 3, i: 0 }));
        for bad in [
            "bm-star", "bm-star:1", "bm-star:-1:2", "bm-star:1:70000", "bm-star:x:y", "bm-anchor", "bm-anchor:1:",
            "bm-remove:1", "bm-open", "bm-open:1:2", "bm-open:1:2:x", "bm-open:1:2:", "bm-open:a:2:s", "bm-open:1:-2:s",
            "bm-delete", "bm-delete:1", "bm-delete:1:-1", "bm-delete:1:2:3", "bm-list", "bm-", "bm-add:https://a.example/",
        ] {
            assert_eq!(p(bad), None, "{bad:?}");
        }
        assert_eq!(p(&format!("bm-save:{}", "a".repeat(MAX_TITLE_ARG + 1))), None);
        assert_eq!(p(&format!("bm-filter:{}", "a".repeat(overlay::MAX_FILTER + 1))), None);
        // Every name that is routed here parses; none is parsed twice.
        for name in BookmarkCmd::NAMES {
            assert!(!crate::overlay::OverlayCmd::NAMES.contains(name), "{name}");
        }
    }

    #[test]
    fn maps_the_bookmark_keys() {
        assert_eq!(BookmarkKey::from_key(0x44, CTRL), Some(BookmarkKey::Toggle));
        assert_eq!(BookmarkKey::from_key(0x4F, Mods { shift: true, ..CTRL }), Some(BookmarkKey::List));
        assert_eq!(BookmarkKey::from_key(0x44, NO), None, "plain D types a letter");
        assert_eq!(BookmarkKey::from_key(0x44, Mods { shift: true, ..CTRL }), None);
        assert_eq!(BookmarkKey::from_key(0x4F, CTRL), None, "Ctrl+O is not ours");
        assert_eq!(BookmarkKey::from_key(0x4F, Mods { shift: true, ..NO }), None);
        for key in [0x44, 0x4F] {
            for shift in [false, true] {
                let altgr = Mods { ctrl: true, shift, alt: true };
                assert_eq!(BookmarkKey::from_key(key, altgr), None, "AltGr is a character, not a shortcut");
            }
        }
    }

    #[test]
    fn the_editor_hangs_under_the_star_inside_the_window() {
        let parts = |r: Rect| {
            let (p, s) = (r.position.to_logical::<f64>(1.0), r.size.to_logical::<f64>(1.0));
            (p.x, p.y, s.width, s.height)
        };
        let size = LogicalSize::new(1200.0, 800.0);
        // Right edge at the star's right edge (700 + 20).
        assert_eq!(parts(editor_rect(size, (700, 20))), (400.0, 38.0, 320.0, 156.0));
        // Star unknown: at the window's right.
        assert_eq!(parts(editor_rect(size, (0, 0))), (872.0, 38.0, 320.0, 156.0));
        // Star near the left edge or the right edge: never outside.
        assert_eq!(parts(editor_rect(size, (100, 20))).0, 0.0);
        assert_eq!(parts(editor_rect(size, (1190, 20))).0, 872.0);
        // A small window: narrower, shorter.
        let small = LogicalSize::new(300.0, 120.0);
        assert_eq!(parts(editor_rect(small, (250, 20))), (0.0, 38.0, 284.0, 74.0));
    }
}
