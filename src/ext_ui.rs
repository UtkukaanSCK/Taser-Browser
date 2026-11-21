//! The Uzantılar page and the strip's extensions button (v0.3 task 5).
//!
//! - The page (menu > Uzantılar) is an overlay page like the settings: it
//!   lists what `extensions::list` gives (built-ins are never in it), with
//!   each one's icon, name, version, an on/off switch, "Seçenekler" and
//!   "Kaldır". Removing is asked in the page first. Every operation shows as
//!   pending on its row until WebView2 answers; one it never answers turns
//!   into an error after `OP_TIMEOUT`.
//! - The extensions button (a puzzle piece in the strip) shows only while an
//!   enabled extension declares a popup (`action.default_popup`); it lists
//!   them, and a pick opens that popup (`popup`).
//! - Everything read from an extension's folder is untrusted: the manifest
//!   is read within `extensions::MAX_MANIFEST_BYTES`, a page path must stay
//!   inside the extension (`page_path`), and an icon is a PNG within
//!   `native::MAX_ICON_BYTES` whose real path is inside the folder
//!   (`icon_data_url`), sent to the page as a `data:` URL. An extension
//!   Taser did not record (no folder known) gets no icon, options or
//!   popup.
//! - An extension WebView2 has but Taser did not record installing
//!   ("unknown") comes in off: the start's reconcile turns it off. It is
//!   listed too, marked "Taser dışında kurulmuş", and can be turned on
//!   and removed here. Turned on by the user, it enters `ALLOWED`: so an
//!   extension (uBlock Origin Lite) whose record was lost with
//!   extensions.json does not stay off for good. The cost: someone who can
//!   write to the profile can put an extension there, but it does not run
//!   until the user turns it on. Turning it on does not record it (v0.3
//!   task 5B decides that), so the next start's reconcile turns it off
//!   again.
//!
//! `chrome-extension://` in content tabs (`refuses_extension`): the main
//! frame and frames load only pages of an extension that is listed (recorded,
//! or unknown and turned on in the page), installed and enabled
//! (`ALLOWED`), not built in, or of the two built-in ones (the PDF
//! viewer draws a PDF in a frame of its own). Typed into the address bar,
//! such an address opens only for such an extension, never a built-in
//! (`typed_url`); anything else under that scheme is refused, never
//! searched for. The UI and the overlay never load it at all
//! (`ui::is_ui_origin` is their only allowed origin). Extension tabs are
//! not web pages to `url::is_tab_url`: they are neither saved in the
//! session nor written to the history, and show no security indicator.

use crate::app::{App, UI_HEIGHT, UserEvent};
use crate::commands::UiCommand;
use crate::crx::is_extension_id;
use crate::extensions::{self, Op};
use crate::native;
use crate::overlay::{self, Mode};
use crate::tabs::Placement;
use serde::Serialize;
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::sync::RwLock;
use std::time::Duration;

/// How long a row waits for WebView2's answer before it says it failed.
const OP_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest page path taken from a manifest (bytes).
pub const MAX_PAGE_PATH: usize = 512;
/// The button's list: width, row height and padding (logical px); must
/// match overlay.html. The padding is the list's 4 px above and below plus
/// the overlay's 1 px border at the top and the bottom.
const ACTIONS_W: f64 = 280.0;
const ACTIONS_ROW_H: f64 = 36.0;
const ACTIONS_PAD: f64 = 10.0;
/// Operations kept after their row gave up waiting.
const MAX_LATE: usize = 16;
/// Gap to the window's edges.
const EDGE: f64 = 8.0;

const SCHEME: &str = "chrome-extension";

/// Ids whose pages content tabs may load: listed, installed in WebView2
/// (after the start's reconcile), enabled and not built in. An unknown
/// extension comes in off and enters only when the user turns it on in the
/// page. Read by the navigation handlers (`native::content`, `native::popup`).
static ALLOWED: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Messages from the strip's button and the extensions page, plus some of
/// the app's own (never parsed from IPC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtCmd {
    /// The strip's button; its right edge in CSS px.
    Actions(u16),
    /// The page's switch.
    Enable { id: String, on: bool },
    /// The page's "Kaldır", after its question.
    Remove(String),
    /// The page's "Seçenekler".
    Options(String),
    /// A pick in the button's list.
    Popup(String),
    /// An operation's wait is over (`OP_TIMEOUT`).
    Timeout(Op),
    /// The popup asked to close (Esc, `window.close()`: `refocus`) or lost
    /// the focus. `generation` names the popup it came from.
    PopupClosed { generation: u32, refocus: bool },
    /// Time to measure the popup's page again (`popup::measure`).
    PopupMeasure { generation: u32 },
    /// The popup's page measured: its content's size in CSS px.
    PopupSize { generation: u32, w: u32, h: u32 },
    /// The popup picked in the list opens now (`open_popup`), the list
    /// being hidden.
    OpenPopup,
    /// The page's "Güncellemeleri denetle".
    CheckUpdates,
    /// The page's "Chrome Web Mağazası": the store, in a new tab.
    Store,
    /// An update's question, asked over the page, is gone
    /// (`webstore::finish`): the page comes back.
    BackToPage,
}

impl ExtCmd {
    pub const NAMES: &[&str] = &["ext-actions", "ext-enable", "ext-remove", "ext-options", "ext-popup", "ext-check-updates", "ext-open-store"];

    pub fn parse(name: &str, arg: Option<&str>) -> Option<Self> {
        let id = |a: &str| (is_extension_id(a) && !extensions::is_builtin(a)).then(|| a.to_owned());
        if name == "ext-check-updates" {
            return arg.is_none().then_some(Self::CheckUpdates);
        }
        if name == "ext-open-store" {
            return arg.is_none().then_some(Self::Store);
        }
        let arg = arg?;
        match name {
            "ext-actions" if arg.bytes().all(|b| b.is_ascii_digit()) => arg.parse().ok().map(Self::Actions),
            "ext-enable" => {
                let (i, on) = arg.split_once(':')?;
                let on = match on {
                    "1" => true,
                    "0" => false,
                    _ => return None,
                };
                Some(Self::Enable { id: id(i)?, on })
            }
            "ext-remove" => id(arg).map(Self::Remove),
            "ext-options" => id(arg).map(Self::Options),
            "ext-popup" => id(arg).map(Self::Popup),
            _ => None,
        }
    }

    /// Whether the strip may send it; the rest come from the overlay only.
    pub fn strip_may_send(&self) -> bool {
        matches!(self, Self::Actions(_))
    }
}

// ------------------------------------------------------------ manifest

/// A page path from a manifest, as a path inside the extension: relative,
/// no `..`, no drive, no backslash, no escapes; `./` and a query or
/// fragment are dropped.
pub fn page_path(raw: &str) -> Option<String> {
    if raw.len() > MAX_PAGE_PATH {
        return None;
    }
    let path = raw.split(['?', '#']).next().unwrap_or("");
    if path.starts_with('/') || path.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '%')) {
        return None;
    }
    let mut parts = Vec::new();
    for seg in path.split('/') {
        match seg {
            "." if parts.is_empty() => {}
            "" | "." | ".." => return None,
            s => parts.push(s),
        }
    }
    if parts.is_empty() || parts.iter().any(|s| s.trim() != *s) {
        return None;
    }
    Some(parts.join("/"))
}

/// The popup a manifest declares: MV3 `action`, else MV2 `browser_action`
/// or `page_action`.
pub fn action_popup(m: &Value) -> Option<String> {
    ["action", "browser_action", "page_action"]
        .iter()
        .find_map(|k| m.get(k)?.get("default_popup")?.as_str().and_then(page_path))
}

/// The options page a manifest declares: `options_page`, else `options_ui.page`.
pub fn options_path(m: &Value) -> Option<String> {
    let page = m.get("options_page").and_then(Value::as_str).and_then(page_path);
    page.or_else(|| m.get("options_ui")?.get("page")?.as_str().and_then(page_path))
}

/// The icon to show (its path): `icons`, at the size nearest a 32 px row
/// first, else the action's `default_icon`.
pub fn icon_path(m: &Value) -> Option<String> {
    fn pick(v: &Value) -> Option<String> {
        if let Some(s) = v.as_str() {
            return page_path(s);
        }
        let map = v.as_object()?;
        let sized = ["32", "48", "16", "64", "128"].iter().find_map(|k| map.get(*k)?.as_str().and_then(page_path));
        sized.or_else(|| map.values().find_map(|v| v.as_str().and_then(page_path)))
    }
    let icons = m.get("icons").and_then(pick);
    icons.or_else(|| {
        ["action", "browser_action", "page_action"].iter().find_map(|k| m.get(k)?.get("default_icon").and_then(pick))
    })
}

/// The icon at `rel` in `dir` as a `data:` URL: a regular file (no link)
/// within `native::MAX_ICON_BYTES`, really inside `dir`, and a PNG.
pub fn icon_data_url(dir: &Path, rel: &str) -> Option<String> {
    let rel = page_path(rel)?;
    let path = dir.join(&rel);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > native::MAX_ICON_BYTES as u64 {
        return None;
    }
    let (root, real) = (dir.canonicalize().ok()?, path.canonicalize().ok()?);
    if !real.starts_with(&root) {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&real).ok()?.take(native::MAX_ICON_BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
    native::png_data_url(&bytes)
}

fn read_manifest(dir: &Path) -> Option<Value> {
    let path = dir.join("manifest.json");
    let meta = extensions::plain_file_metadata(&path)?;
    if meta.len() > extensions::MAX_MANIFEST_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    serde_json::from_slice(text).ok()
}

/// What the page and the button need from one manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Info {
    popup: Option<String>,
    options: Option<String>,
    icon: Option<String>,
}

impl Info {
    fn read(dir: &Path) -> Self {
        read_manifest(dir).map_or_else(Info::default, |m| Info {
            popup: action_popup(&m),
            options: options_path(&m),
            icon: icon_path(&m),
        })
    }
}

// ------------------------------------------------------------ policy

/// The extension id of a `chrome-extension://<id>[/?#...]` address; None
/// for any other address or a malformed id.
pub fn extension_id_of(url: &str) -> Option<&str> {
    let rest = url.get(SCHEME.len()..)?;
    if !url[..SCHEME.len()].eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    let rest = rest.strip_prefix("://")?;
    let id = rest.get(..32)?;
    let next = rest[32..].chars().next();
    (is_extension_id(id) && matches!(next, None | Some('/' | '?' | '#'))).then_some(id)
}

fn is_extension_scheme(url: &str) -> bool {
    url.split_once(':').is_some_and(|(s, _)| s.trim().eq_ignore_ascii_case(SCHEME))
}

/// Whether a content tab must not load `url` (main frame or frame): an
/// extension page of an id not in `allowed` and not built in. Addresses of
/// other schemes are not this check's business.
pub fn refuses_extension(url: &str, allowed: &[String]) -> bool {
    if !is_extension_scheme(url) {
        return false;
    }
    extension_id_of(url).is_none_or(|id| !extensions::is_builtin(id) && !allowed.iter().any(|a| a == id))
}

/// `refuses_extension` against the extensions enabled now.
pub fn refuses_extension_url(url: &str) -> bool {
    refuses_extension(url, &ALLOWED.read().unwrap_or_else(|e| e.into_inner()))
}

/// Address-bar input under the `chrome-extension` scheme: Some(Some(url))
/// to open (an enabled extension's page), Some(None) to refuse; None for
/// any other input (`url::resolve_input` decides). Built-ins are refused
/// here: nothing the user would type.
pub fn typed_in(text: &str, allowed: &[String]) -> Option<Option<String>> {
    let cleaned: String = text.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let input = cleaned.trim_matches(|c: char| c <= ' ');
    if !is_extension_scheme(input) {
        return None;
    }
    let ok = !input.chars().any(|c| c.is_whitespace() || c.is_control())
        && extension_id_of(input).is_some_and(|id| allowed.iter().any(|a| a == id));
    Some(ok.then(|| input.to_string()))
}

/// `typed_in` against the extensions enabled now.
pub fn typed_url(text: &str) -> Option<Option<String>> {
    typed_in(text, &ALLOWED.read().unwrap_or_else(|e| e.into_inner()))
}

// ------------------------------------------------------------ state

struct Pending {
    id: String,
    op: Op,
    remove: bool,
    name: String,
}

#[derive(Default)]
pub struct ExtUi {
    pending: Vec<Pending>,
    /// Rows whose last operation failed: (id, "error" | "timeout").
    failed: Vec<(String, &'static str)>,
    /// Operations whose row gave up waiting (`ExtCmd::Timeout`): WebView2
    /// may still answer, and that answer is the true one. `MAX_LATE` at
    /// most (the oldest goes first).
    late: Vec<Pending>,
    /// The page's status line: ("removed" | "remove-failed", name), or
    /// how the update check the page asked for ended: ("up-to-date" |
    /// "found" | "check-failed" | "nothing-to-check", the count found).
    note: Option<(&'static str, String)>,
    /// The page's button asked for an update check, and its end is to be
    /// said on the page.
    check_asked: bool,
    /// The last install error nobody had seen (`Extensions::last_error`),
    /// taken for the page when it was shown: it stays until the page is
    /// opened again.
    error: Option<crate::webstore::LastError>,
    /// Manifest facts by folder (a new version is a new folder).
    cache: Vec<(String, Info)>,
    /// What the strip's button was last told (None: nothing yet).
    button: Option<bool>,
    /// The button's right edge when its list was last opened.
    anchor: f64,
    /// The popup to open at the next `ExtCmd::OpenPopup`: (id, page).
    popup_next: Option<(String, String)>,
}

impl ExtUi {
    /// The page has a switch or removal of `id` that WebView2 has not
    /// answered yet, including one whose row gave up waiting. The Web Store
    /// leaves such an id alone (as `start` leaves the store's job alone), so
    /// a late removal and an update cannot cross.
    pub fn is_busy_with(&self, id: &str) -> bool {
        self.pending.iter().chain(&self.late).any(|p| p.id == id)
    }
}

/// The manifest facts of recorded extension `id` (None: not recorded or
/// removed, so its folder is not known).
fn info(app: &mut App, id: &str) -> Option<Info> {
    let e = app.extensions.records().iter().find(|e| e.id == id && !e.removed)?;
    let dir = e.dir.clone();
    let cache = &mut app.ext_ui.cache;
    if let Some((_, i)) = cache.iter().find(|(d, _)| *d == dir) {
        return Some(i.clone());
    }
    let i = Info::read(Path::new(&dir));
    if cache.len() >= extensions::MAX_ITEMS {
        cache.clear();
    }
    cache.push((dir, i.clone()));
    Some(i)
}

fn dir_of(app: &App, id: &str) -> Option<String> {
    app.extensions.records().iter().find(|e| e.id == id && !e.removed).map(|e| e.dir.clone())
}

#[derive(Serialize)]
struct Row {
    id: String,
    name: String,
    version: String,
    enabled: bool,
    installed: bool,
    icon: Option<String>,
    options: bool,
    state: &'static str,
    /// Not recorded: installed outside Taser (or its record was lost).
    unknown: bool,
}

#[derive(Serialize)]
struct Note<'a> {
    code: &'a str,
    name: &'a str,
}

#[derive(Serialize)]
struct ErrorView {
    id: String,
    message: String,
}

#[derive(Serialize)]
struct PageView<'a> {
    items: Vec<Row>,
    note: Option<Note<'a>>,
    /// An update check is running: the button waits.
    checking: bool,
    /// The last install error: under its row, or on top when its id is not
    /// in the list.
    error: Option<ErrorView>,
}

#[derive(Serialize)]
struct ActionRow {
    id: String,
    name: String,
    icon: Option<String>,
}

/// Enabled, installed extensions that have a popup, with their popup page.
fn with_popups(app: &mut App) -> Vec<(extensions::Listed, String)> {
    let mut out = Vec::new();
    for l in app.extensions.list() {
        if l.enabled
            && l.installed
            && let Some(popup) = info(app, &l.id).and_then(|i| i.popup)
        {
            out.push((l, popup));
        }
    }
    out
}

fn icon_of(app: &mut App, id: &str) -> Option<String> {
    let rel = info(app, id)?.icon?;
    icon_data_url(Path::new(&dir_of(app, id)?), &rel)
}

fn page_json(app: &mut App) -> String {
    let mut items = Vec::new();
    for l in app.extensions.list() {
        let i = info(app, &l.id).unwrap_or_default();
        let x = &app.ext_ui;
        let state = if x.pending.iter().any(|p| p.id == l.id) {
            "pending"
        } else {
            x.failed.iter().find(|(f, _)| *f == l.id).map_or("", |(_, s)| *s)
        };
        items.push(Row {
            icon: icon_of(app, &l.id),
            options: i.options.is_some(),
            state,
            unknown: l.source.is_none(),
            id: l.id,
            name: l.name,
            version: l.version,
            enabled: l.enabled,
            installed: l.installed,
        });
    }
    let note = app.ext_ui.note.as_ref().map(|(code, name)| Note { code, name });
    let error = app.ext_ui.error.as_ref().map(|e| ErrorView { id: e.id.clone(), message: extensions::clean_text(&e.message) });
    let checking = app.webstore.is_checking();
    serde_json::to_string(&PageView { items, note, checking, error }).unwrap_or_else(|_| "{}".into())
}

// ------------------------------------------------------------ page

/// Menu > Uzantılar: opens the page.
pub fn open(app: &mut App) {
    overlay::hand_over(app);
    if app.fullscreen.on() {
        crate::page_tools::leave_fullscreen(app);
    }
    app.ext_ui.failed.clear();
    app.ext_ui.note = None;
    app.ext_ui.check_asked = false;
    app.ext_ui.error = None;
    render(app, false);
}

fn render(app: &mut App, keep: bool) {
    // An install error nobody saw: shown now, so seen (it goes from the
    // file).
    if let Some(e) = app.extensions.last_error().cloned() {
        app.ext_ui.error = Some(e);
        app.extensions.clear_last_error();
    }
    let json = page_json(app);
    let bounds = overlay::panel_rect(overlay::win_size(app));
    overlay::show(app, Mode::Extensions, format!("xbx.show({json},{keep})"), bounds);
}

/// The button's list: under the strip, its right edge at `right`.
fn actions_rect(size: tao::dpi::LogicalSize<f64>, right: f64, rows: usize) -> wry::Rect {
    let w = ACTIONS_W.min((size.width - 2.0 * EDGE).max(1.0));
    let x = (right - w).min(size.width - w - EDGE).max(0.0);
    let y = UI_HEIGHT + 2.0;
    let h = (rows as f64 * ACTIONS_ROW_H + ACTIONS_PAD).min((size.height - y - EDGE).max(1.0));
    overlay::rect(x, y, w, h)
}

/// The strip's button: opens its list, or closes it.
fn toggle_actions(app: &mut App, right: f64) {
    if app.overlay.mode == Mode::ExtActions {
        overlay::close(app);
        app.focus_after_switch();
        return;
    }
    crate::popup::close(app, false);
    let mut rows = Vec::new();
    for (l, _) in with_popups(app) {
        rows.push(ActionRow { icon: icon_of(app, &l.id), id: l.id, name: l.name });
    }
    if rows.is_empty() {
        sync_button(app);
        return;
    }
    overlay::hand_over_from_strip(app);
    if app.fullscreen.on() {
        crate::page_tools::leave_fullscreen(app);
    }
    app.ext_ui.anchor = right;
    let bounds = actions_rect(overlay::win_size(app), right, rows.len());
    let json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into());
    overlay::show(app, Mode::ExtActions, format!("xbx.actions({json})"), bounds);
    if app.overlay.mode == Mode::ExtActions {
        let _ = app.ui.evaluate_script("xbExtBtn.expanded(true)");
    }
}

fn start(app: &mut App, id: String, remove: bool, on: bool) {
    if app.overlay.mode != Mode::Extensions || app.ext_ui.pending.iter().any(|p| p.id == id) {
        return;
    }
    let Some(name) = app.extensions.list().into_iter().find(|l| l.id == id).map(|l| l.name) else { return };
    // The store is installing, updating or removing it: not under that.
    if app.webstore.busy_with() == Some(id.as_str()) {
        app.ext_ui.note = Some(("store-busy", name));
        render(app, true);
        return;
    }
    app.ext_ui.failed.retain(|(f, _)| *f != id);
    app.ext_ui.note = None;
    if remove {
        close_pages(app, &id);
    }
    let op = if remove { extensions::remove(app, &id) } else { extensions::set_enabled(app, &id, on) };
    app.ext_ui.pending.push(Pending { id, op, remove, name });
    // A callback WebView2 never calls must not leave the row waiting.
    let proxy = app.proxy().clone();
    std::thread::spawn(move || {
        std::thread::sleep(OP_TIMEOUT);
        let _ = proxy.send_event(UserEvent::Ui(UiCommand::Ext(ExtCmd::Timeout(op))));
    });
    // (It may have failed already: no content webview.)
    on_changed(app);
}

/// Before extension `id` is removed, its pages go: a tab in the background
/// that shows one is closed; the active tab (under the page) shows the
/// blank page instead, so the page stays. (Left to WebView2, they would
/// turn into error pages.)
fn close_pages(app: &mut App, id: &str) {
    let of = |t: &crate::tabs::Tab| extension_id_of(&t.url) == Some(id) || extension_id_of(&t.page_url) == Some(id);
    let tabs: Vec<_> = (0..app.tabs.len()).filter_map(|i| app.tabs.nth(i)).filter(|t| app.tabs.get(*t).is_some_and(of)).collect();
    let active = app.tabs.active();
    for t in tabs {
        if Some(t) != active {
            app.close_tab(t, false);
            continue;
        }
        if let Some(v) = app.view(t) {
            let _ = v.load_url(crate::url::BLANK);
        }
        if let Some(tab) = app.tabs.get_mut(t) {
            tab.url = crate::url::BLANK.to_owned();
            tab.page_url = crate::url::BLANK.to_owned();
        }
        app.mark_dirty();
    }
}

fn open_options(app: &mut App, id: &str) {
    if app.overlay.mode != Mode::Extensions {
        return;
    }
    let on = app.extensions.list().iter().any(|l| l.id == id && l.enabled && l.installed);
    let Some(path) = info(app, id).and_then(|i| i.options).filter(|_| on) else { return };
    overlay::close(app);
    if app.fullscreen.on() {
        crate::page_tools::leave_fullscreen(app);
    }
    if app.open_tab(&format!("{SCHEME}://{id}/{path}"), Placement::End, true).is_ok()
        && let Some(v) = app.active_view()
    {
        let _ = v.focus();
    }
}

fn open_popup(app: &mut App, id: &str) {
    if app.overlay.mode != Mode::ExtActions {
        return;
    }
    let found = with_popups(app).into_iter().find(|(l, _)| l.id == id).map(|(_, p)| p);
    // The list is hidden first; the popup opens on the loop's next pass,
    // and takes the keyboard itself. The focus is not handed back to the
    // page in between: WebView2 moves it asynchronously, so the page would
    // get it after the popup had, and the popup would close for it (a
    // focus loss it did not cause). The hidden list's focus is no click
    // outside (`popup::on_closed`).
    overlay::close(app);
    app.ext_ui.popup_next = found.map(|p| (id.to_owned(), p));
    if app.ext_ui.popup_next.is_some() {
        let _ = app.proxy().send_event(UserEvent::Ui(UiCommand::Ext(ExtCmd::OpenPopup)));
    } else {
        app.focus_after_switch();
    }
}

fn open_next_popup(app: &mut App) {
    let Some((id, path)) = app.ext_ui.popup_next.take() else { return };
    // Still enabled with that popup, and nothing else came up meanwhile.
    let still = with_popups(app).into_iter().any(|(l, p)| l.id == id && p == path);
    if !still || app.overlay.mode != Mode::Hidden {
        if app.overlay.mode == Mode::Hidden {
            app.focus_after_switch();
        }
        return;
    }
    crate::popup::open(app, &id, &path, app.ext_ui.anchor);
}

pub fn on_command(app: &mut App, cmd: ExtCmd) {
    match cmd {
        ExtCmd::Actions(right) => toggle_actions(app, f64::from(right)),
        ExtCmd::Enable { id, on } => start(app, id, false, on),
        ExtCmd::Remove(id) => start(app, id, true, false),
        ExtCmd::Options(id) => open_options(app, &id),
        ExtCmd::Popup(id) => open_popup(app, &id),
        ExtCmd::Timeout(op) => {
            let x = &mut app.ext_ui;
            if let Some(i) = x.pending.iter().position(|p| p.op == op) {
                let p = x.pending.remove(i);
                x.failed.retain(|(f, _)| *f != p.id);
                x.failed.push((p.id.clone(), "timeout"));
                if x.late.len() >= MAX_LATE {
                    x.late.remove(0);
                }
                x.late.push(p);
                if app.overlay.mode == Mode::Extensions {
                    render(app, true);
                }
            }
        }
        ExtCmd::PopupClosed { generation, refocus } => crate::popup::on_closed(app, generation, refocus),
        ExtCmd::PopupMeasure { generation } => crate::popup::measure(app, generation),
        ExtCmd::PopupSize { generation, w, h } => crate::popup::on_size(app, generation, w, h),
        ExtCmd::OpenPopup => open_next_popup(app),
        ExtCmd::CheckUpdates => check_updates(app),
        ExtCmd::Store => open_store(app),
        ExtCmd::BackToPage => back_to_page(app),
    }
}

/// The page's "Chrome Web Mağazası": the store's front page in a new tab,
/// where an extension's "Chrome'a ekle" installs it (`webstore`).
fn open_store(app: &mut App) {
    if app.overlay.mode != Mode::Extensions {
        return;
    }
    overlay::close(app);
    if app.fullscreen.on() {
        crate::page_tools::leave_fullscreen(app);
    }
    if app.open_tab(crate::cws::STORE_URL, Placement::End, true).is_ok()
        && let Some(v) = app.active_view()
    {
        let _ = v.focus();
    }
}

/// Puts the page back after an update's question asked over it, if nothing
/// took its place meanwhile and Taser is still in front. Its note about
/// the check ("found") is over: the question answered it.
fn back_to_page(app: &mut App) {
    if app.overlay.mode != Mode::Hidden || app.fullscreen.on() || crate::popup::extension(app).is_some() || !app.in_front() {
        return;
    }
    app.ext_ui.note = None;
    render(app, false);
}

/// The page's "Güncellemeleri denetle": the same check as the daily one.
/// A check already running (the timer's) counts as this one.
fn check_updates(app: &mut App) {
    if app.overlay.mode != Mode::Extensions {
        return;
    }
    crate::webstore::check_updates(app);
    let checking = app.webstore.is_checking();
    // Not running: nothing to ask about, or the check could not start.
    let checkable = checking || crate::webstore::has_checkable(app);
    let x = &mut app.ext_ui;
    x.check_asked = checking;
    x.note = match (checking, checkable) {
        (true, _) => None,
        (false, true) => Some(("check-failed", String::new())),
        (false, false) => Some(("nothing-to-check", String::new())),
    };
    render(app, true);
}

/// An update check ended: `found` updates to install, one at a time
/// (`None`: it failed). Said on the page if its button asked.
pub fn on_update_checked(app: &mut App, found: Option<usize>) {
    if std::mem::take(&mut app.ext_ui.check_asked) {
        app.ext_ui.note = Some(match found {
            Some(0) => ("up-to-date", String::new()),
            Some(n) => ("found", n.to_string()),
            None => ("check-failed", String::new()),
        });
    }
    if app.overlay.mode == Mode::Extensions {
        render(app, true);
    }
}

/// The UI (re)loaded: its button starts hidden.
pub fn on_ui_ready(app: &mut App) {
    app.ext_ui.button = None;
    on_changed(app);
}

fn sync_button(app: &mut App) {
    if !app.ui_ready {
        return;
    }
    let show = !with_popups(app).is_empty();
    if app.ext_ui.button != Some(show) {
        app.ext_ui.button = Some(show);
        let _ = app.ui.evaluate_script(&format!("xbExtBtn.show({show})"));
    }
}

/// Gives the keyboard to the strip's button, as a menu button gets it back
/// when what it opened closes. False when the button is not there (no
/// extension with a popup left, or the strip hidden in fullscreen).
pub fn focus_button(app: &App) -> bool {
    if app.ext_ui.button != Some(true) || app.fullscreen.on() {
        return false;
    }
    let _ = app.ui.focus();
    let _ = app.ui.evaluate_script("xbExtBtn.focus()");
    true
}

/// Something about the extensions changed (an `extensions` event, an
/// operation started): finishes the rows that have an answer, and brings
/// the allowed ids, the button, the open popup and the page up to date.
pub fn on_changed(app: &mut App) {
    let mut i = 0;
    while i < app.ext_ui.pending.len() {
        let Some(result) = app.extensions.outcome(app.ext_ui.pending[i].op) else {
            i += 1;
            continue;
        };
        let ok = result.is_ok();
        let p = app.ext_ui.pending.remove(i);
        let listed = app.extensions.list().iter().any(|l| l.id == p.id);
        let x = &mut app.ext_ui;
        if p.remove {
            // Gone from the list (its folder may go at the next start): removed.
            x.note = Some((if listed { "remove-failed" } else { "removed" }, p.name));
        }
        if !ok && listed {
            x.failed.push((p.id, "error"));
        }
    }
    let listed: Vec<String> = app.extensions.list().into_iter().map(|l| l.id).collect();
    let ext = &app.extensions;
    settle_late(&mut app.ext_ui, |op| ext.outcome(op).map(Result::is_ok), &listed);
    let allowed: Vec<String> =
        app.extensions.list().into_iter().filter(|l| l.enabled && l.installed && !extensions::is_builtin(&l.id)).map(|l| l.id).collect();
    *ALLOWED.write().unwrap_or_else(|e| e.into_inner()) = allowed;
    sync_button(app);
    if let Some(ext) = crate::popup::extension(app).map(str::to_owned)
        && !with_popups(app).iter().any(|(l, _)| l.id == ext)
    {
        crate::popup::close(app, true);
    }
    if app.overlay.mode == Mode::Extensions {
        render(app, true);
    }
}

#[cfg(feature = "selftest")]
pub fn allowed() -> Vec<String> {
    ALLOWED.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Answers that came after their row gave up waiting: one that succeeded
/// takes back the row's "timeout", one that failed says "error" (unless a
/// newer operation on that row is under way). Then rows no longer in the
/// list (`listed`) lose their marks.
fn settle_late(x: &mut ExtUi, outcome: impl Fn(Op) -> Option<bool>, listed: &[String]) {
    let (late, pending, failed) = (&mut x.late, &x.pending, &mut x.failed);
    late.retain(|p| {
        let Some(ok) = outcome(p.op) else { return true };
        if !pending.iter().any(|q| q.id == p.id) {
            failed.retain(|(f, s)| !(*f == p.id && *s == "timeout"));
            if !ok && !failed.iter().any(|(f, _)| *f == p.id) {
                failed.push((p.id.clone(), "error"));
            }
        }
        false
    });
    failed.retain(|(f, _)| listed.contains(f));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "mmbkafeccnpmgicaldnppdljpaogpjdm";
    const PDF: &str = "mhjfbmdgcfjbbpaeojofohoefgiehjai";

    #[test]
    fn parses_extension_messages() {
        let p = ExtCmd::parse;
        assert_eq!(p("ext-actions", Some("812")), Some(ExtCmd::Actions(812)));
        assert_eq!(p("ext-enable", Some(&format!("{ID}:1"))), Some(ExtCmd::Enable { id: ID.into(), on: true }));
        assert_eq!(p("ext-enable", Some(&format!("{ID}:0"))), Some(ExtCmd::Enable { id: ID.into(), on: false }));
        assert_eq!(p("ext-remove", Some(ID)), Some(ExtCmd::Remove(ID.into())));
        assert_eq!(p("ext-options", Some(ID)), Some(ExtCmd::Options(ID.into())));
        assert_eq!(p("ext-popup", Some(ID)), Some(ExtCmd::Popup(ID.into())));
        assert_eq!(p("ext-check-updates", None), Some(ExtCmd::CheckUpdates));
        assert_eq!(p("ext-open-store", None), Some(ExtCmd::Store));
        for (n, a) in [
            ("ext-open-store", Some("")),
            ("ext-open-store", Some("https://example.com/")),
            ("ext-check-updates", Some("")),
            ("ext-check-updates", Some("1")),
            ("ext-actions", None),
            ("ext-actions", Some("-1")),
            ("ext-actions", Some("+5")),
            ("ext-actions", Some("70000")),
            ("ext-enable", Some(ID)),
            ("ext-enable", Some(&format!("{ID}:2") as &str)),
            ("ext-enable", Some(&format!("{ID}:1:1") as &str)),
            ("ext-remove", None),
            ("ext-remove", Some("MMBKAFECCNPMGICALDNPPDLJPAOGPJDM")),
            ("ext-remove", Some("abc")),
            ("ext-remove", Some(&format!("{ID}/") as &str)),
            ("ext-remove", Some(PDF)),
            ("ext-options", Some(PDF)),
            ("ext-popup", Some("")),
            ("ext-timeout", Some("1")),
        ] {
            assert_eq!(p(n, a), None, "{n} {a:?}");
        }
        assert!(p("ext-actions", Some("1")).unwrap().strip_may_send());
        assert!(!p("ext-remove", Some(ID)).unwrap().strip_may_send());
    }

    #[test]
    fn page_paths_stay_inside_the_extension() {
        assert_eq!(page_path("popup.html").as_deref(), Some("popup.html"));
        assert_eq!(page_path("ui/options.html?tab=2#x").as_deref(), Some("ui/options.html"));
        assert_eq!(page_path("./popup.html").as_deref(), Some("popup.html"));
        assert_eq!(page_path("a b/ç.html").as_deref(), Some("a b/ç.html"));
        for bad in [
            "", "/popup.html", "../x.html", "a/../../x.html", "a/./b.html", "a//b.html", "a\\b.html",
            "C:/x.html", "c:x.html", "https://evil.example/", "%2e%2e/x.html", "a/\u{0}.html", "a/", ".", "?x",
            " popup.html", "popup.html ",
        ] {
            assert_eq!(page_path(bad), None, "{bad:?}");
        }
        assert_eq!(page_path(&"a".repeat(MAX_PAGE_PATH + 1)), None);
        assert!(page_path(&"a".repeat(MAX_PAGE_PATH)).is_some());
    }

    #[test]
    fn finds_the_popup_the_options_and_the_icon() {
        let m = json!({"action": {"default_popup": "popup.html", "default_icon": {"16": "a16.png"}},
                       "options_page": "options.html"});
        assert_eq!(action_popup(&m).as_deref(), Some("popup.html"));
        assert_eq!(options_path(&m).as_deref(), Some("options.html"));
        assert_eq!(icon_path(&m).as_deref(), Some("a16.png"));
        // MV2, options_ui, sized icons.
        let m = json!({"browser_action": {"default_popup": "p.html"}, "options_ui": {"page": "o/o.html"},
                       "icons": {"128": "i128.png", "48": "i48.png", "16": "i16.png"}});
        assert_eq!(action_popup(&m).as_deref(), Some("p.html"));
        assert_eq!(options_path(&m).as_deref(), Some("o/o.html"));
        assert_eq!(icon_path(&m).as_deref(), Some("i48.png"));
        assert_eq!(action_popup(&json!({"page_action": {"default_popup": "q.html"}})).as_deref(), Some("q.html"));
        assert_eq!(icon_path(&json!({"action": {"default_icon": "one.png"}})).as_deref(), Some("one.png"));
        assert_eq!(icon_path(&json!({"icons": {"19": "odd.png"}})).as_deref(), Some("odd.png"));
        // An action without a popup (it would want onClicked), none, bad paths.
        for m in [
            json!({"action": {}}),
            json!({"action": {"default_popup": ""}}),
            json!({"action": {"default_popup": "../p.html"}}),
            json!({"action": {"default_popup": 5}}),
            json!({"action": "popup.html"}),
            json!({}),
        ] {
            assert_eq!(action_popup(&m), None, "{m}");
        }
        assert_eq!(options_path(&json!({"options_page": "/abs.html"})), None);
        assert_eq!(options_path(&json!({"options_ui": {"page": "..\\x.html"}})), None);
        assert_eq!(options_path(&json!({"options_page": "C:/x.html", "options_ui": {"page": "ok.html"}})).as_deref(), Some("ok.html"));
        assert_eq!(icon_path(&json!({"icons": {"32": "../x.png"}})), None);
    }

    /// The smallest valid PNG header (`native::png_data_url` reads IHDR).
    fn png(side: u32, extra: usize) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&side.to_be_bytes());
        b.extend_from_slice(&side.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b.resize(b.len() + extra, 0);
        b
    }

    #[test]
    fn icons_are_small_pngs_inside_the_folder() {
        let root = crate::store::tests::temp_dir("ext-ui-icons");
        let dir = root.join("ext");
        std::fs::create_dir_all(dir.join("img")).unwrap();
        std::fs::write(dir.join("img/ok.png"), png(32, 100)).unwrap();
        std::fs::write(dir.join("big.png"), png(32, native::MAX_ICON_BYTES)).unwrap();
        std::fs::write(dir.join("max.png"), png(32, native::MAX_ICON_BYTES - png(32, 0).len())).unwrap();
        std::fs::write(dir.join("huge-side.png"), png(4096, 10)).unwrap();
        std::fs::write(dir.join("fake.png"), b"GIF89a not a png").unwrap();
        std::fs::write(root.join("outside.png"), png(32, 10)).unwrap();
        let url = icon_data_url(&dir, "img/ok.png").unwrap();
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
        assert!(icon_data_url(&dir, "max.png").is_some(), "exactly the limit");
        for bad in ["big.png", "huge-side.png", "fake.png", "../outside.png", "missing.png", "img", "/img/ok.png"] {
            assert_eq!(icon_data_url(&dir, bad), None, "{bad}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_the_manifest_of_a_folder() {
        let root = crate::store::tests::temp_dir("ext-ui-manifest");
        std::fs::write(
            root.join("manifest.json"),
            "\u{feff}{\"action\":{\"default_popup\":\"popup.html\"},\"options_page\":\"options.html\"}",
        )
        .unwrap();
        let i = Info::read(&root);
        assert_eq!((i.popup.as_deref(), i.options.as_deref(), i.icon), (Some("popup.html"), Some("options.html"), None));
        assert_eq!(Info::read(&root.join("none")), Info::default());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extension_ids_come_from_the_host_only() {
        assert_eq!(extension_id_of(&format!("chrome-extension://{ID}/options.html")), Some(ID));
        assert_eq!(extension_id_of(&format!("Chrome-Extension://{ID}")), Some(ID));
        assert_eq!(extension_id_of(&format!("chrome-extension://{ID}?a")), Some(ID));
        for bad in [
            format!("chrome-extension://{ID}x/"),
            format!("chrome-extension://{}/", ID.to_uppercase()),
            format!("chrome-extension:{ID}/"),
            format!("chrome-extension://user@{ID}/"),
            format!("https://{ID}/"),
            format!("https://example.com/chrome-extension://{ID}/"),
            "chrome-extension://abc/".to_string(),
            "chrome-extension://".to_string(),
            "chrome-extensio".to_string(),
            "çççççç".to_string(),
        ] {
            assert_eq!(extension_id_of(&bad), None, "{bad}");
        }
    }

    #[test]
    fn content_tabs_load_only_enabled_extensions_pages() {
        let allowed = vec![ID.to_string()];
        let other = "abcdefghijklmnopabcdefghijklmnop";
        assert!(!refuses_extension(&format!("chrome-extension://{ID}/options.html"), &allowed));
        assert!(refuses_extension(&format!("chrome-extension://{ID}/options.html"), &[]), "disabled or removed");
        assert!(refuses_extension(&format!("chrome-extension://{other}/x.html"), &allowed));
        assert!(!refuses_extension(&format!("chrome-extension://{PDF}/index.html"), &[]), "the PDF viewer");
        for bad in ["chrome-extension://abc/", "CHROME-EXTENSION://x", "chrome-extension:"] {
            assert!(refuses_extension(bad, &allowed), "{bad}");
        }
        for other in ["https://example.com/", "about:blank", "file:///C:/a", "data:text/html,x", ""] {
            assert!(!refuses_extension(other, &[]), "not this check's: {other}");
        }
    }

    #[test]
    fn typed_extension_addresses_open_only_when_enabled() {
        let allowed = vec![ID.to_string()];
        let page = format!("chrome-extension://{ID}/options.html");
        assert_eq!(typed_in(&page, &allowed), Some(Some(page.clone())));
        assert_eq!(typed_in(&format!("  {page}\t "), &allowed), Some(Some(page.clone())));
        assert_eq!(typed_in(&page, &[]), Some(None));
        assert_eq!(typed_in(&format!("chrome-extension://{PDF}/index.html"), &allowed), Some(None));
        assert_eq!(typed_in("chrome-extension://abc", &allowed), Some(None));
        assert_eq!(typed_in(&format!("{page} x"), &allowed), Some(None));
        assert_eq!(typed_in("example.com", &allowed), None);
        assert_eq!(typed_in("chrome extension", &allowed), None);
    }

    #[test]
    fn the_button_list_hangs_under_the_button_inside_the_window() {
        let parts = |r: wry::Rect| {
            let (p, s) = (r.position.to_logical::<f64>(1.0), r.size.to_logical::<f64>(1.0));
            (p.x, p.y, s.width, s.height)
        };
        let size = tao::dpi::LogicalSize::new(1200.0, 800.0);
        assert_eq!(parts(actions_rect(size, 1100.0, 2)), (820.0, 78.0, 280.0, 82.0));
        // Never off the left or right edge, nor below the window.
        assert_eq!(parts(actions_rect(size, 100.0, 1)).0, 0.0);
        assert_eq!(parts(actions_rect(size, 1200.0, 1)).0, 912.0);
        let small = tao::dpi::LogicalSize::new(480.0, 240.0);
        let (x, y, w, h) = parts(actions_rect(small, 400.0, 50));
        assert!(x >= 0.0 && x + w <= 480.0 && y + h <= 240.0);
    }

    #[test]
    fn an_answer_after_the_timeout_still_counts() {
        let late = |id: &str, op: Op| Pending { id: id.into(), op, remove: false, name: String::new() };
        let mut x = ExtUi::default();
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for (id, op) in [("a", 1), ("b", 2), ("c", 3), ("d", 4)] {
            x.failed.push((id.into(), "timeout"));
            x.late.push(late(id, op));
        }
        // "d" is being tried again: its old answer changes nothing.
        x.pending.push(late("d", 5));
        let answers = |op: Op| match op {
            1 => Some(true),
            2 => Some(false),
            4 => Some(false),
            _ => None,
        };
        settle_late(&mut x, answers, &ids(&["a", "b", "c", "d"]));
        assert_eq!(x.failed, vec![("c".to_string(), "timeout"), ("d".into(), "timeout"), ("b".into(), "error")]);
        // "c" has no answer yet: it stays late. Then it is no longer listed.
        assert_eq!(x.late.iter().map(|p| p.op).collect::<Vec<_>>(), vec![3]);
        settle_late(&mut x, |_| None, &ids(&["b"]));
        assert_eq!(x.failed, vec![("b".to_string(), "error")]);
    }

    #[test]
    fn busy_covers_pending_and_late_operations() {
        let op = |id: &str, op: Op| Pending { id: id.into(), op, remove: true, name: String::new() };
        let mut x = ExtUi::default();
        assert!(!x.is_busy_with("a"));
        x.pending.push(op("a", 1));
        x.late.push(op("b", 2));
        assert!(x.is_busy_with("a") && x.is_busy_with("b") && !x.is_busy_with("c"));
    }
}
