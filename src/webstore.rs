//! Installs and removes Chrome Web Store extensions (v0.3 task 4, design
//! B.3–B.6).
//!
//! An install is one job at a time, in stages:
//!
//! 1. Fetching (a worker thread): the CRX from the update service
//!    (`cws::crx_url`), checked as a Web Store package for the id asked for
//!    (`crx::verify_crx3`), its name, version and permissions read from the
//!    manifest before anything is written.
//! 2. Asking (only when `confirm`): X-Browser's own question in the overlay
//!    (`overlay::Mode::Store`), never the page's. As for the launch question
//!    (`policy`): "Ekle" counts only `ANSWER_DELAY` after the question came on
//!    screen and after the last press that came too early, it is answered from
//!    the overlay page only (`ui::ipc_command`), a lost focus or anything that
//!    closes the overlay is "no", and it waits `ASK_TIMEOUT` at most.
//! 3. Unpacking (a worker thread): the checked bytes into
//!    `<extensions root>\<id>\<version>` (`crx::unpack`, all or nothing).
//! 4. Adding: `extensions::add_from_store`, which records the extension only
//!    once WebView2 has added it under the id asked for; a version recorded
//!    before keeps its record and its folder until then, and after a failure.
//!
//! Whoever called `install_from_store` hears the end of it as
//! `UserEvent::StoreInstalled` (`on_installed`), never during the call.
//!
//! The store page's own buttons reach here through the hook script and its
//! sentinel address (`cws`, `policy::Event::Store`); the strip's button does
//! the same for the page shown (`StoreCmd::Button`).

use crate::app::{App, UserEvent};
use crate::crx::{self, Policy};
use crate::cws::{self, Action};
use crate::overlay::Mode;
use crate::tabs::TabId;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long the question waits for an answer once it is on screen.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(60);

/// "Ekle" / "Kaldır" does nothing until the question has been on screen this
/// long, and this long after the last press that came too early (as
/// `policy::ANSWER_DELAY`).
pub const ANSWER_DELAY: Duration = crate::policy::ANSWER_DELAY;

/// `prodversion` when the WebView2 runtime's version cannot be read.
const FALLBACK_PRODVERSION: &str = "157.0.0.0";

/// The longest name the question shows, in characters.
/// Downloads that may run at once, cancelled ones included.
const MAX_FETCHES: usize = 2;
/// How many folders of one version `unpack_to` tries before giving up.
const MAX_VERSION_FOLDERS: u32 = 9;
const MAX_NAME_CHARS: usize = 75;

/// The largest `_locales/<l>/messages.json` read for the name.
const MAX_MESSAGES_BYTES: u64 = 1024 * 1024;

/// Why an install did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The user said no, or the question went away (closed, timed out).
    Cancelled,
    /// Another install or removal is under way.
    Busy,
    /// Not an extension id.
    BadId,
    /// No content tab, so no profile to add it to.
    NoView,
    /// The download failed.
    Net(String),
    /// The package is not a Web Store package for the id, or is malformed.
    Package(String),
    /// Unpacking or WebView2 failed.
    Install(String),
}

impl Failure {
    /// The sentence the user sees.
    pub fn message(&self) -> String {
        match self {
            Self::Cancelled => "Yükleme iptal edildi.".into(),
            Self::Busy => "Başka bir uzantı işlemi sürüyor. Bitince yeniden deneyin.".into(),
            Self::BadId => "Bu bir uzantı kimliği değil.".into(),
            Self::NoView => "Uzantı eklemek için açık bir sekme gerekiyor.".into(),
            Self::Net(e) => format!("Uzantı indirilemedi ({e})."),
            Self::Package(e) => format!("Uzantı paketi doğrulanamadı ({e})."),
            Self::Install(e) => format!("Uzantı eklenemedi ({e})."),
        }
    }
}

/// What the question shows of a package: read from its manifest, cleaned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Info {
    pub name: String,
    pub version: String,
    /// `cws::describe_permissions`: Turkish lines, never the manifest's text.
    pub permissions: Vec<String>,
}

/// A checked download.
#[derive(Debug)]
pub struct Fetched {
    bytes: Vec<u8>,
    info: Info,
}

/// An unpacked version folder. `fresh`: made by this job, so a failed add
/// deletes it again; else it is the folder recorded for the id already.
#[derive(Debug)]
pub struct Unpacked {
    dir: PathBuf,
    fresh: bool,
}

/// The workers' answers, delivered as `UserEvent::Store`.
#[derive(Debug)]
pub enum Event {
    Fetched { serial: u64, result: Result<Fetched, Failure> },
    Unpacked { serial: u64, result: Result<Unpacked, Failure> },
}

#[derive(Debug)]
enum Stage {
    Fetching,
    /// The question is up (or on its way). `bytes`: None for a removal.
    Asking { bytes: Option<Vec<u8>>, info: Info },
    Unpacking { info: Info },
    Adding { op: crate::extensions::Op, unpacked: Unpacked, info: Info },
    Removing { op: crate::extensions::Op },
}

#[derive(Debug)]
struct Job {
    serial: u64,
    id: String,
    action: Action,
    /// Ask before installing (a removal always asks).
    confirm: bool,
    /// The store tab the request came from, if any.
    tab: Option<TabId>,
    stage: Stage,
    /// When the question first came on screen (`ASK_TIMEOUT`).
    asked: Option<Instant>,
    /// When "Ekle" may count from: the last show of the question, or the last
    /// press that came too early.
    guard: Option<Instant>,
    /// Where the question goes (`overlay::show_store`), the same for every
    /// stage of the job.
    spot: (f64, f64),
}

#[derive(Default)]
pub struct State {
    job: Option<Job>,
    next: u64,
    /// Downloads still running, cancelled ones included (a thread cannot be
    /// stopped): a page that keeps asking cannot stack them up.
    fetching: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl State {
    /// The id of the extension being installed or removed. For the update
    /// check (task 3), which is wired in at the merge.
    #[allow(dead_code)]
    pub fn busy_with(&self) -> Option<&str> {
        self.job.as_ref().map(|j| j.id.as_str())
    }
}

/// The overlay page's answer, or the strip's button. Which page may send
/// which is checked in `ui::ipc_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreCmd {
    /// The strip's button: install or remove the extension of the store
    /// page shown.
    Button,
    /// "Ekle" / "Kaldır".
    Yes,
    /// "İptal", "Kapat", the page's own timeout, or the question lost focus.
    No,
    /// A press inside the question that came too early.
    Press,
}

impl StoreCmd {
    pub const NAMES: [&'static str; 4] = ["ext-store", "ext-yes", "ext-no", "ext-press"];

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "ext-store" => Some(Self::Button),
            "ext-yes" => Some(Self::Yes),
            "ext-no" => Some(Self::No),
            "ext-press" => Some(Self::Press),
            _ => None,
        }
    }

    /// Sent by the question in the overlay, never by the strip.
    pub fn is_answer(self) -> bool {
        self != Self::Button
    }
}

// --- starting ---------------------------------------------------------------------

/// Installs (or updates) Web Store extension `id`. `confirm`: ask the user
/// first, showing the package's name and permissions. The result always
/// comes later, as `UserEvent::StoreInstalled` (`on_installed`): `Ok` with
/// the version installed, or why not. Called by the first-run uBOL install
/// (`firstrun`); the self-test uses a fake store instead.
#[cfg_attr(feature = "selftest", allow(dead_code))]
pub fn install_from_store(app: &mut App, id: &str, confirm: bool) {
    start_install(app, id, confirm, None);
}

fn start_install(app: &mut App, id: &str, confirm: bool, tab: Option<TabId>) {
    let refused = if app.webstore.job.is_some() || app.webstore.fetching.load(std::sync::atomic::Ordering::Relaxed) >= MAX_FETCHES {
        Some(Failure::Busy)
    } else if !crx::is_extension_id(id) || crate::extensions::is_builtin(id) {
        Some(Failure::BadId)
    } else if app.content_view().is_none() || app.extensions.root().is_none() {
        Some(Failure::NoView)
    } else {
        None
    };
    if let Some(f) = refused {
        return report(app, id, Err(f));
    }
    let serial = new_job(app, id, Action::Install, confirm, tab, Stage::Fetching);
    let prodversion = crate::native::runtime_version()
        .filter(|v| cws::is_version(v))
        .unwrap_or_else(|| FALLBACK_PRODVERSION.to_owned());
    let (proxy, owned) = (app.proxy().clone(), id.to_owned());
    let running = app.webstore.fetching.clone();
    running.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let spawned = std::thread::Builder::new().name("xb-store-fetch".into()).spawn(move || {
        let result = fetch(&owned, &prodversion);
        running.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        let _ = proxy.send_event(UserEvent::Store(Event::Fetched { serial, result }));
    });
    // Shown first, so a thread that cannot start still says why.
    if confirm {
        show(app);
    }
    if let Err(e) = spawned {
        app.webstore.fetching.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return end(app, Err(Failure::Install(e.to_string())));
    }
    app.mark_dirty();
}

fn new_job(app: &mut App, id: &str, action: Action, confirm: bool, tab: Option<TabId>, stage: Stage) -> u64 {
    let w = &mut app.webstore;
    w.next += 1;
    w.job = Some(Job {
        serial: w.next,
        id: id.to_owned(),
        action,
        confirm,
        tab,
        stage,
        asked: None,
        guard: None,
        spot: crate::overlay::store_spot(),
    });
    w.next
}

/// The store page's button, through the hook script and its sentinel
/// (`policy::Event::Store`), from the page the user is looking at.
pub fn on_store_button(app: &mut App, tab: TabId, action: Action, id: &str) {
    if app.webstore.job.is_some() {
        // One at a time; the strip's button shows that one is under way.
        return;
    }
    match action {
        Action::Install => start_install(app, id, true, Some(tab)),
        Action::Remove => ask_remove(app, tab, id),
    }
}

/// Asks before removing `id`, if X-Browser has it.
fn ask_remove(app: &mut App, tab: TabId, id: &str) {
    let Some(l) = app.extensions.list().into_iter().find(|l| l.id == id) else { return };
    if crate::extensions::is_builtin(id) {
        return;
    }
    let info = Info { name: clean_name(&l.name), version: l.version, permissions: Vec::new() };
    new_job(app, id, Action::Remove, true, Some(tab), Stage::Asking { bytes: None, info });
    show(app);
    app.mark_dirty();
}

/// The strip's button, for the page shown.
fn on_button(app: &mut App) {
    let Some(tab) = app.tabs.active() else { return };
    let Some(id) = app.tabs.get(tab).and_then(|t| cws::detail_id(&t.url)) else { return };
    let action = if has(app, &id) { Action::Remove } else { Action::Install };
    on_store_button(app, tab, action, &id);
}

/// X-Browser has extension `id` (recorded or in the profile).
fn has(app: &App, id: &str) -> bool {
    app.extensions.list().iter().any(|l| l.id == id)
}

// --- the workers ------------------------------------------------------------------

/// Downloads and checks the package (worker thread).
fn fetch(id: &str, prodversion: &str) -> Result<Fetched, Failure> {
    let url = cws::crx_url(id, prodversion).ok_or(Failure::BadId)?;
    let limits = crate::net::Limits { max_body: crx::MAX_CRX_BYTES, ..Default::default() };
    let resp = crate::net::get(&url, &limits).map_err(|e| Failure::Net(e.to_string()))?;
    if resp.status != 200 {
        return Err(Failure::Net(format!("HTTP {}", resp.status)));
    }
    let info = inspect(&resp.body, id, Policy::WebStore)?;
    Ok(Fetched { bytes: resp.body, info })
}

fn package_error(e: crx::CrxError) -> Failure {
    Failure::Package(e.to_string())
}

/// Checks `bytes` as a package of `id` under `policy` and reads what the
/// question shows.
fn inspect(bytes: &[u8], id: &str, policy: Policy) -> Result<Info, Failure> {
    let payload = crx::verify_crx3(bytes, id, policy).map_err(package_error)?;
    let manifest = payload
        .file("manifest.json", crate::extensions::MAX_MANIFEST_BYTES)
        .map_err(package_error)?
        .ok_or_else(|| Failure::Package("manifest.json yok".into()))?;
    let text = manifest.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&manifest);
    let Ok(serde_json::Value::Object(m)) = serde_json::from_slice::<serde_json::Value>(text) else {
        return Err(Failure::Package("manifest.json okunamadı".into()));
    };
    let version = m.get("version").and_then(|v| v.as_str()).unwrap_or_default();
    if !cws::is_version(version) {
        return Err(Failure::Package("sürüm geçersiz".into()));
    }
    let raw = m.get("name").and_then(|v| v.as_str()).unwrap_or_default();
    let locale = m.get("default_locale").and_then(|v| v.as_str());
    let name = clean_name(&resolve_name(raw, locale, |path| payload.file(path, MAX_MESSAGES_BYTES).ok().flatten()));
    Ok(Info { name, version: version.to_owned(), permissions: cws::describe_permissions(&manifest) })
}

/// A locale folder name: `en`, `pt_BR`, `zh_CN`.
fn valid_locale(l: &str) -> bool {
    (2..=10).contains(&l.len()) && l.bytes().all(|c| c.is_ascii_alphabetic() || c == b'_')
}

/// The manifest's `name`, with a `__MSG_key__` looked up in
/// `_locales/{tr, default_locale, en}/messages.json` (keys match without
/// regard to case, as in Chrome). `read` gives a file of the package.
fn resolve_name(raw: &str, default_locale: Option<&str>, read: impl Fn(&str) -> Option<Vec<u8>>) -> String {
    let Some(key) = raw.strip_prefix("__MSG_").and_then(|r| r.strip_suffix("__")) else { return raw.to_owned() };
    let mut locales = vec!["tr"];
    locales.extend(default_locale.filter(|l| valid_locale(l)));
    locales.push("en");
    for loc in locales {
        let Some(bytes) = read(&format!("_locales/{loc}/messages.json")) else { continue };
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let Ok(serde_json::Value::Object(msgs)) = serde_json::from_slice::<serde_json::Value>(bytes) else { continue };
        let found = msgs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| v.get("message"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty());
        if let Some(s) = found {
            return s.to_owned();
        }
    }
    raw.to_owned()
}

/// Characters that change how text around them reads without showing
/// themselves: bidi controls and marks, zero-width ones, invisible
/// operators, tags.
fn is_invisible(c: char) -> bool {
    matches!(c as u32,
        0xAD | 0x34F | 0x61C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
        | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
        | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFFB | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
}

/// A name as the question shows it: no control or invisible characters,
/// runs of white space as one space, at most `MAX_NAME_CHARS` characters.
fn clean_name(s: &str) -> String {
    let kept: String = s.chars().filter(|c| !c.is_control() && !is_invisible(*c)).collect();
    let joined = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = joined.chars().take(MAX_NAME_CHARS).collect();
    if joined.chars().count() > MAX_NAME_CHARS {
        out.pop();
        out.push('…');
    }
    out
}

/// Unpacks a checked package into `root\<id>\<version>`, or `<version>_<n>`
/// if that is taken by a folder not recorded (worker thread).
/// `recorded`: the folder recorded for the id now (left exactly as it is;
/// when it is that same folder, it is used as it is).
fn unpack_to(root: &Path, bytes: &[u8], id: &str, version: &str, recorded: Option<&Path>, policy: Policy) -> Result<Unpacked, Failure> {
    let io = |e: std::io::Error| Failure::Install(e.to_string());
    if !cws::is_version(version) {
        return Err(Failure::Package("sürüm geçersiz".into()));
    }
    let payload = crx::verify_crx3(bytes, id, policy).map_err(package_error)?;
    let home = root.join(id);
    let dest = home.join(version);
    // The folder recorded now is never written to or deleted here, even
    // when it looks broken: a failed install must leave it as it was.
    if recorded == Some(dest.as_path()) {
        return if dest.join("manifest.json").is_file() {
            Ok(Unpacked { dir: dest, fresh: false })
        } else {
            Err(Failure::Install("kayıtlı sürüm klasörü eksik".into()))
        };
    }
    // Nothing is written under a link: it would land somewhere else.
    if !crate::extensions::is_plain_folder(root) {
        return Err(Failure::Install("uzantı klasörü bir bağlantı".into()));
    }
    std::fs::create_dir_all(root).map_err(io)?;
    if !crate::extensions::is_plain_folder(&home) {
        return Err(Failure::Install("uzantı klasörü bir bağlantı".into()));
    }
    std::fs::create_dir_all(&home).map_err(io)?;
    // Anything already there that is not recorded is left alone: an install
    // that did not finish, or a folder WebView2 may still load from (its
    // record lost). The package goes into the first free name instead.
    let dest = std::iter::once(dest)
        .chain((2..=MAX_VERSION_FOLDERS).map(|n| home.join(format!("{version}_{n}"))))
        .find(|d| matches!(std::fs::symlink_metadata(d), Err(e) if e.kind() == std::io::ErrorKind::NotFound))
        .ok_or_else(|| Failure::Install("sürüm klasörü kullanılamıyor".into()))?;
    crx::unpack(&payload, &dest).map_err(|e| Failure::Install(e.to_string()))?;
    Ok(Unpacked { dir: dest, fresh: true })
}

// --- events -----------------------------------------------------------------------

pub fn on_event(app: &mut App, ev: Event) {
    let current = app.webstore.job.as_ref().map(|j| j.serial);
    match ev {
        Event::Fetched { serial, result } if Some(serial) == current => fetched(app, result),
        Event::Unpacked { serial, result } if Some(serial) == current => unpacked(app, result),
        // A job that was cancelled meanwhile.
        _ => {}
    }
    app.mark_dirty();
}

fn fetched(app: &mut App, result: Result<Fetched, Failure>) {
    let Some(job) = app.webstore.job.as_mut() else { return };
    if !matches!(job.stage, Stage::Fetching) {
        return;
    }
    let confirm = job.confirm;
    match result {
        Err(f) => end(app, Err(f)),
        Ok(Fetched { bytes, info }) if confirm => {
            job.stage = Stage::Asking { bytes: Some(bytes), info };
            if app.overlay.mode == Mode::Store {
                show(app);
            } else {
                // The question went away while the package came in.
                end(app, Err(Failure::Cancelled));
            }
        }
        Ok(Fetched { bytes, info }) => start_unpack(app, bytes, info),
    }
}

fn start_unpack(app: &mut App, bytes: Vec<u8>, info: Info) {
    let Some(root) = app.extensions.root().map(Path::to_path_buf) else { return end(app, Err(Failure::NoView)) };
    let Some(job) = app.webstore.job.as_mut() else { return };
    let (serial, id, version) = (job.serial, job.id.clone(), info.version.clone());
    job.stage = Stage::Unpacking { info };
    let recorded = app
        .extensions
        .records()
        .iter()
        .find(|e| e.id == id && !e.removed)
        .map(|e| PathBuf::from(&e.dir));
    let proxy = app.proxy().clone();
    let spawned = std::thread::Builder::new().name("xb-store-unpack".into()).spawn(move || {
        let result = unpack_to(&root, &bytes, &id, &version, recorded.as_deref(), Policy::WebStore);
        let _ = proxy.send_event(UserEvent::Store(Event::Unpacked { serial, result }));
    });
    if let Err(e) = spawned {
        return end(app, Err(Failure::Install(e.to_string())));
    }
    if app.overlay.mode == Mode::Store {
        show(app);
    }
}

fn unpacked(app: &mut App, result: Result<Unpacked, Failure>) {
    let Some(job) = app.webstore.job.as_mut() else { return };
    let Stage::Unpacking { info } = &job.stage else { return };
    let info = info.clone();
    let unpacked = match result {
        Ok(u) => u,
        Err(f) => return end(app, Err(f)),
    };
    let id = job.id.clone();
    let op = crate::extensions::add_from_store(app, &unpacked.dir, &id);
    if let Some(job) = app.webstore.job.as_mut() {
        job.stage = Stage::Adding { op, unpacked, info };
    }
    // It may have ended already (no content view).
    after_extension_event(app);
}

/// Called after every `UserEvent::Extension`: an add or removal of the job
/// may have ended (`extensions` answers through `Extensions::outcome`).
pub fn after_extension_event(app: &mut App) {
    let Some(job) = app.webstore.job.as_ref() else { return };
    let op = match &job.stage {
        Stage::Adding { op, .. } | Stage::Removing { op } => *op,
        _ => return,
    };
    let Some(result) = app.extensions.outcome(op).cloned() else { return };
    let Some(mut job) = app.webstore.job.take() else { return };
    match std::mem::replace(&mut job.stage, Stage::Fetching) {
        Stage::Adding { unpacked, info, .. } => match result {
            Ok(_) => {
                reload_store_tab(app, job.tab, &job.id);
                finish(app, job, Ok(info.version));
            }
            Err(e) => {
                if unpacked.fresh {
                    discard(app, unpacked.dir);
                }
                finish(app, job, Err(Failure::Install(e)));
            }
        },
        // Gone from WebView2 and the list is removed, even when its folder
        // could not be deleted yet (`extensions` tries again at the next start).
        Stage::Removing { .. } => match result.or_else(|e| if has(app, &job.id) { Err(e) } else { Ok(String::new()) }) {
            Ok(_) => {
                reload_store_tab(app, job.tab, &job.id);
                if app.overlay.mode == Mode::Store {
                    crate::overlay::dismiss(app);
                }
            }
            Err(e) => {
                eprintln!("x-browser: extension {} could not be removed: {e}", job.id);
                show_error(app, &format!("Uzantı kaldırılamadı ({e})."), job.spot);
            }
        },
        _ => {}
    }
    app.mark_dirty();
}

/// Deletes a version folder this job unpacked, after a failed add, off the
/// UI thread. Never the folder recorded for the id: that one was not fresh.
fn discard(app: &App, dir: PathBuf) {
    let Some(root) = app.extensions.root() else { return };
    if !dir.starts_with(root) || !crate::extensions::is_plain_folder(root) {
        return;
    }
    let _ = std::thread::Builder::new().name("xb-store-discard".into()).spawn(move || {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            eprintln!("x-browser: cannot delete {}: {e}", dir.display());
        }
    });
}

/// The store page still shows the extension's detail page: reloads it so its
/// button shows what changed.
fn reload_store_tab(app: &App, tab: Option<TabId>, id: &str) {
    let Some(tab) = tab else { return };
    let on_page = app.tabs.get(tab).is_some_and(|t| cws::detail_id(&t.url).as_deref() == Some(id));
    if on_page && let Some(v) = app.view(tab) {
        let _ = v.reload();
    }
}

/// Ends the job now (before it reached `Adding`).
fn end(app: &mut App, result: Result<String, Failure>) {
    if let Some(job) = app.webstore.job.take() {
        finish(app, job, result);
    }
}

fn finish(app: &mut App, job: Job, result: Result<String, Failure>) {
    match &result {
        Ok(_) => {
            if app.overlay.mode == Mode::Store {
                crate::overlay::dismiss(app);
            }
        }
        Err(Failure::Cancelled) => {}
        Err(f) => {
            eprintln!("x-browser: extension {} not installed: {f:?}", job.id);
            show_error(app, &f.message(), job.spot);
        }
    }
    if job.action == Action::Install {
        report(app, &job.id, result);
    }
    app.mark_dirty();
}

/// Tells whoever asked, later (`UserEvent::StoreInstalled`).
fn report(app: &App, id: &str, result: Result<String, Failure>) {
    let _ = app.proxy().send_event(UserEvent::StoreInstalled { id: id.to_owned(), result });
}

/// An install ended: `Ok(version)` or why not. Every install's end comes
/// here once, whoever started it.
pub fn on_installed(app: &mut App, id: &str, result: &Result<String, Failure>) {
    match result {
        Ok(version) => eprintln!("x-browser: extension {id} {version} installed from the Chrome Web Store"),
        Err(Failure::Cancelled) => {}
        Err(f) => eprintln!("x-browser: extension {id} install ended: {f:?}"),
    }
    // First-run uBOL waits on every install's result, not only its own.
    crate::firstrun::on_install_result(app, id, result.clone());
    // Görev 5 (uzantı yönetimi) buraya bağlanacak:

    app.mark_dirty();
}

// --- the question -----------------------------------------------------------------

#[derive(Serialize)]
struct View<'a> {
    /// "fetch", "ask", "adding", "removing", "error".
    stage: &'static str,
    /// "install" or "remove".
    action: &'static str,
    id: &'a str,
    name: &'a str,
    version: &'a str,
    permissions: &'a [String],
    text: &'a str,
    ms: u64,
    guard: u64,
}

/// Draws the job's stage in the overlay (`overlay::show_store`).
fn show(app: &mut App) {
    let Some(job) = app.webstore.job.as_ref() else { return };
    let empty = Info { name: String::new(), version: String::new(), permissions: Vec::new() };
    let (stage, info) = match &job.stage {
        Stage::Fetching => ("fetch", &empty),
        Stage::Asking { info, .. } => ("ask", info),
        Stage::Unpacking { info } | Stage::Adding { info, .. } => ("adding", info),
        Stage::Removing { .. } => ("removing", &empty),
    };
    let left = job.asked.map_or(ASK_TIMEOUT, |t| ASK_TIMEOUT.saturating_sub(t.elapsed()));
    let view = View {
        stage,
        action: if job.action == Action::Install { "install" } else { "remove" },
        id: &job.id,
        name: &info.name,
        version: &info.version,
        permissions: &info.permissions,
        text: "",
        ms: left.as_millis() as u64,
        guard: ANSWER_DELAY.as_millis() as u64,
    };
    let Ok(json) = serde_json::to_string(&view) else { return };
    let spot = job.spot;
    crate::overlay::show_store(app, format!("xbExt.show({json})"), spot);
}

/// Shows `text` with "Kapat" if the question is still on screen; a question
/// the user already closed does not come back for it.
fn show_error(app: &mut App, text: &str, spot: (f64, f64)) {
    if app.overlay.mode != Mode::Store {
        return;
    }
    let view = View { stage: "error", action: "", id: "", name: "", version: "", permissions: &[], text, ms: 0, guard: 0 };
    let Ok(json) = serde_json::to_string(&view) else { return };
    crate::overlay::show_store(app, format!("xbExt.show({json})"), spot);
}

/// The overlay put a stage of the question on screen (`overlay::presented`).
pub fn on_question_shown(app: &mut App) {
    let now = Instant::now();
    if let Some(job) = app.webstore.job.as_mut()
        && matches!(job.stage, Stage::Asking { .. })
    {
        job.guard = Some(now);
        job.asked.get_or_insert(now);
    }
}

/// The overlay stopped showing the question, whatever the reason. Before
/// the user said yes, that is "no"; after it, the job goes on unseen.
pub fn on_question_closed(app: &mut App) {
    let open = app.webstore.job.as_ref().is_some_and(|j| matches!(j.stage, Stage::Fetching | Stage::Asking { .. }) && j.confirm);
    if open {
        end(app, Err(Failure::Cancelled));
    }
}

/// The shown tab changed: a question about the page left behind goes.
pub fn on_switch(app: &mut App) {
    let asking = app.webstore.job.as_ref().is_some_and(|j| j.tab.is_some() && matches!(j.stage, Stage::Fetching | Stage::Asking { .. }));
    if asking && app.tabs.active() != app.webstore.job.as_ref().and_then(|j| j.tab) {
        if app.overlay.mode == Mode::Store {
            crate::overlay::dismiss(app);
        }
        end(app, Err(Failure::Cancelled));
    }
}

/// A command from the strip's button or the question (`ui` checked which
/// page sent it).
pub fn on_command(app: &mut App, cmd: StoreCmd) {
    if cmd == StoreCmd::Button {
        return on_button(app);
    }
    // An answer to a question that is not shown is no answer.
    if app.overlay.mode != Mode::Store {
        return;
    }
    let now = Instant::now();
    match cmd {
        StoreCmd::Button => {}
        StoreCmd::Press => {
            if let Some(job) = app.webstore.job.as_mut()
                && job.guard.is_some()
            {
                job.guard = Some(now);
            }
        }
        StoreCmd::No => crate::overlay::dismiss(app),
        StoreCmd::Yes => yes(app, now),
    }
}

fn yes(app: &mut App, now: Instant) {
    let Some(job) = app.webstore.job.as_mut() else { return };
    if !matches!(job.stage, Stage::Asking { .. }) {
        return;
    }
    if job.asked.is_some_and(|t| now.saturating_duration_since(t) >= ASK_TIMEOUT) {
        return crate::overlay::dismiss(app);
    }
    let ready = job.guard.is_some_and(|g| now.saturating_duration_since(g) >= ANSWER_DELAY);
    if !ready {
        // Too early: the page shows the button as off again for as long.
        if job.guard.is_some() {
            job.guard = Some(now);
        }
        return crate::overlay::run(app, Mode::Store, &format!("xbExt.guard({})", ANSWER_DELAY.as_millis()));
    }
    let (action, id) = (job.action, job.id.clone());
    let Stage::Asking { bytes, info } = std::mem::replace(&mut job.stage, Stage::Fetching) else { return };
    match (action, bytes) {
        (Action::Install, Some(bytes)) => start_unpack(app, bytes, info),
        (Action::Remove, _) => {
            let op = crate::extensions::remove(app, &id);
            if let Some(job) = app.webstore.job.as_mut() {
                job.stage = Stage::Removing { op };
            }
            show(app);
            after_extension_event(app);
        }
        (Action::Install, None) => end(app, Err(Failure::Cancelled)),
    }
    app.mark_dirty();
}

/// The strip button's state for the page shown, as the script `App::flush`
/// appends to its render call: "add", "remove", "busy", or "" off a store
/// detail page.
pub fn render_js(app: &App) -> String {
    let id = app.tabs.active().and_then(|t| app.tabs.get(t)).and_then(|t| cws::detail_id(&t.url));
    let state = match id {
        None => "",
        Some(_) if app.webstore.job.is_some() => "busy",
        Some(id) if has(app, &id) => "remove",
        Some(_) => "add",
    };
    format!("window.xbStore&&xbStore.render(\"{state}\")")
}

#[cfg(test)]
mod tests;
