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
//! Updates (`check_updates`, task 5B): one update check for every extension
//! installed from the store and turned on, then each newer version offered,
//! one job at a time (the queue moves on whenever a job ends, `job_ended`).
//! An update job fetches the `codebase` the check named, capped at the size
//! it stated, and checks the download's SHA-256 against the check's before
//! the CRX checks above; its manifest must carry the version offered
//! (`verify_update`). It asks only when the new version wants a permission
//! the installed one did not have ("Güncelle", listing only what is new),
//! and only when nothing else is on screen: with the overlay open, in
//! fullscreen or in the background it is left for the next check. Else it
//! goes on unseen. Once WebView2 has the new version, the folder of the old
//! one is deleted (`retire`). A version whose package or install failed is
//! not fetched again this start (`State::failed`).
//!
//! The check runs at most once a day (`update_check_due`, the time kept in
//! `extensions.json`), asked for by a timer (`start_update_timer`) and run
//! once the start-up reconcile and its re-adds are over and no job runs
//! (`run_pending_check`).
//!
//! The store page's own buttons reach here through the hook script and its
//! sentinel address (`cws`, `policy::Event::Store`); the strip's button does
//! the same for the page shown (`StoreCmd::Button`).

use crate::app::{App, UserEvent};
use crate::crx::{self, Policy};
use crate::cws::{self, Action};
use crate::overlay::Mode;
use crate::tabs::TabId;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long the question waits for an answer once it is on screen.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(60);

/// "Ekle" / "Kaldır" does nothing until the question has been on screen this
/// long, and this long after the last press that came too early (as
/// `policy::ANSWER_DELAY`).
pub const ANSWER_DELAY: Duration = crate::policy::ANSWER_DELAY;

/// `prodversion` when the WebView2 runtime's version cannot be read.
const FALLBACK_PRODVERSION: &str = "157.0.0.0";

/// Downloads that may run at once, cancelled ones included.
const MAX_FETCHES: usize = 2;
/// How many folders of one version `unpack_to` tries before giving up.
const MAX_VERSION_FOLDERS: u32 = 9;
/// Extensions asked about in one update-check request (its URL stays well
/// under `net::MAX_URL`).
const CHECK_BATCH: usize = 50;

/// The packages this build accepts: the Web Store's only. The self-test
/// build also takes the fake store's self-signed ones; it is never
/// distributed (README).
#[cfg(not(feature = "selftest"))]
const STORE_POLICY: Policy = Policy::WebStore;
#[cfg(feature = "selftest")]
const STORE_POLICY: Policy = Policy::SelfSigned;

/// The longest name the question shows, in characters.
const MAX_NAME_CHARS: usize = 75;

/// Seconds between two update checks.
pub const UPDATE_CHECK_EVERY: u64 = 24 * 60 * 60;
/// How often the timer asks whether a check is due.
#[cfg(not(feature = "selftest"))]
const UPDATE_TIMER_TICK: Duration = Duration::from_secs(60);

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

/// Longest `LastError::message` kept, in characters.
const MAX_ERROR_TEXT: usize = 300;

/// The last install that failed after its question was gone, so nobody saw
/// why (`finish`). Kept in `extensions.json` (`ExtFile::last_error`, one
/// record) for the Extensions page to show at the next start.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LastError {
    pub id: String,
    /// `Failure::message`: Turkish, without control characters.
    pub message: String,
    /// Seconds since the epoch.
    pub at: u64,
}

impl LastError {
    /// `None` unless `id` is an extension id and some text is left once
    /// control characters are taken out.
    pub fn new(id: &str, message: &str, at: u64) -> Option<Self> {
        let message: String = message.chars().filter(|c| !c.is_control()).take(MAX_ERROR_TEXT).collect();
        let message = message.trim();
        (crx::is_extension_id(id) && !message.is_empty()).then(|| Self { id: id.to_owned(), message: message.to_owned(), at })
    }

    /// Reads the field leniently: anything but a valid record (a wrong type,
    /// a missing part, a bad id) is `None`, never an error that would cost
    /// the rest of the file.
    pub fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Self>, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let (Some(id), Some(message), Some(at)) =
            (v.get("id").and_then(|x| x.as_str()), v.get("message").and_then(|x| x.as_str()), v.get("at").and_then(|x| x.as_u64()))
        else {
            return Ok(None);
        };
        Ok(Self::new(id, message, at))
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

/// A checked download. `widened`: an update that wants a permission the
/// installed version did not have, so the user is asked.
#[derive(Debug)]
pub struct Fetched {
    bytes: Vec<u8>,
    info: Info,
    widened: bool,
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
    /// The update check's answer (`check_updates`).
    Checked { result: Result<Vec<cws::Update>, Failure> },
}

/// Where a job's package comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// The update service's redirect to the current CRX (`cws::crx_url`).
    Store,
    /// A newer version the update check offered: fetched from `codebase`,
    /// at most `size` bytes (`body_cap`), with this SHA-256 and version.
    Update { codebase: String, sha256: [u8; 32], version: String, size: Option<usize> },
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
    /// An update the update check offered, at this version: the question
    /// says "Güncelle".
    offered: Option<String>,
    /// The folder recorded for the id when unpacking began, deleted once the
    /// new version is in (`retire`).
    replaced: Option<PathBuf>,
    /// Set when the job ends (`finish`): its download, if still running,
    /// stops at its next read (`net::Limits::cancel`).
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct State {
    job: Option<Job>,
    next: u64,
    /// Downloads still running, cancelled ones included: a cancelled one
    /// stops at its next read, but a read already waiting runs to its
    /// timeout, so a page that keeps asking cannot stack them up meanwhile.
    fetching: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Updates offered and not started yet, oldest first.
    queue: VecDeque<(String, Source)>,
    /// An update check is running.
    checking: bool,
    /// The timer asked for a check (`run_pending_check`).
    check_pending: bool,
    /// `(id, version)` of updates whose package or install failed this
    /// start, one per id: not fetched again until a newer one is offered.
    failed: Vec<(String, String)>,
}

impl State {
    /// An update check is running (the Extensions page's button waits).
    pub fn is_checking(&self) -> bool {
        self.checking
    }

    /// The id of the extension being installed, updated or removed.
    pub fn busy_with(&self) -> Option<&str> {
        self.job.as_ref().map(|j| j.id.as_str())
    }

    fn remember_failed(&mut self, id: &str, version: &str) {
        self.failed.retain(|(i, _)| i != id);
        self.failed.push((id.to_owned(), version.to_owned()));
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
/// (`firstrun`) and by the self-test, against its fake store (`fake`).
pub fn install_from_store(app: &mut App, id: &str, confirm: bool) {
    start_install(app, id, confirm, None);
}

fn start_install(app: &mut App, id: &str, confirm: bool, tab: Option<TabId>) {
    start_job(app, id, confirm, tab, Source::Store);
}

/// The WebView2 runtime's version, as the update service wants it.
fn prodversion() -> String {
    crate::native::runtime_version().filter(|v| cws::is_version(v)).unwrap_or_else(|| FALLBACK_PRODVERSION.to_owned())
}

/// The folder recorded for `id` now, if any.
fn recorded_dir(app: &App, id: &str) -> Option<PathBuf> {
    app.extensions.records().iter().find(|e| e.id == id && !e.removed).map(|e| PathBuf::from(&e.dir))
}

fn start_job(app: &mut App, id: &str, confirm: bool, tab: Option<TabId>, source: Source) {
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
    let offered = match &source {
        Source::Update { version, .. } => Some(version.clone()),
        Source::Store => None,
    };
    // An update compares its permissions with the version installed now.
    let installed = if offered.is_some() { recorded_dir(app, id) } else { None };
    let serial = new_job(app, id, Action::Install, confirm, tab, Stage::Fetching);
    if let Some(job) = app.webstore.job.as_mut() {
        job.offered = offered;
    }
    let prodversion = prodversion();
    let (proxy, owned) = (app.proxy().clone(), id.to_owned());
    let running = app.webstore.fetching.clone();
    let cancel = app.webstore.job.as_ref().map(|j| j.cancel.clone()).unwrap_or_default();
    running.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let spawned = std::thread::Builder::new().name("xb-store-fetch".into()).spawn(move || {
        let result = fetch(&owned, &prodversion, &source, installed.as_deref(), STORE_POLICY, cancel);
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
        offered: None,
        replaced: None,
        cancel: Arc::default(),
    });
    w.next
}

/// The store page's button, through the hook script and its sentinel
/// (`policy::Event::Store`), from the page the user is looking at.
pub fn on_store_button(app: &mut App, tab: TabId, action: Action, id: &str) {
    if app.webstore.job.is_some() || app.ext_ui.is_busy_with(id) {
        // One at a time; the strip's button shows that one is under way.
        // The Uzantılar page's own switch or removal of it ends first.
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

/// Downloads and checks the package (worker thread). For an update,
/// `installed` is the folder of the version installed now, whose manifest
/// the new one's permissions are compared with. `cancel`: the job's flag,
/// set once it ends; the download then stops at its next read.
fn fetch(id: &str, prodversion: &str, source: &Source, installed: Option<&Path>, policy: Policy, cancel: Arc<AtomicBool>) -> Result<Fetched, Failure> {
    let (url, max_body) = match source {
        Source::Store => (cws::crx_url(id, prodversion).ok_or(Failure::BadId)?, crx::MAX_CRX_BYTES),
        Source::Update { codebase, size, .. } => (codebase.clone(), body_cap(*size)),
    };
    let limits = crate::net::Limits { max_body, cancel: Some(cancel), ..Default::default() };
    let resp = http_get(&url, &limits).map_err(Failure::Net)?;
    if resp.status != 200 {
        return Err(Failure::Net(format!("HTTP {}", resp.status)));
    }
    let old = installed.and_then(read_manifest_bytes);
    verify_update(resp.body, id, source, old.as_deref(), policy)
}

/// Checks a download for `source` (worker thread). From the store: a
/// package of `id`. An update, in this order: no larger than the check
/// stated (`body_cap`), the check's SHA-256 (nothing else is read from a
/// download that is not the one offered), a package of `id`, the version
/// offered. `installed_manifest`: the manifest of the version installed
/// now; when it cannot be read, the update counts as widening, and the
/// question lists every permission instead of only the new ones.
fn verify_update(bytes: Vec<u8>, id: &str, source: &Source, installed_manifest: Option<&[u8]>, policy: Policy) -> Result<Fetched, Failure> {
    let Source::Update { sha256, version, size, .. } = source else {
        let (info, _) = read_package(&bytes, id, policy)?;
        return Ok(Fetched { bytes, info, widened: false });
    };
    if bytes.len() > body_cap(*size) {
        return Err(Failure::Package("boyut uyuşmuyor".into()));
    }
    if crx::sha256(&[&bytes]).as_ref() != Some(sha256) {
        return Err(Failure::Package("özet uyuşmuyor".into()));
    }
    let (mut info, manifest) = read_package(&bytes, id, policy)?;
    if !cws::same_version(&info.version, version) {
        return Err(Failure::Package("sürüm uyuşmuyor".into()));
    }
    let widened = installed_manifest.is_none_or(|old| cws::widens_permissions(old, &manifest));
    if widened && let Some(old) = installed_manifest {
        info.permissions = cws::added_permissions(old, &manifest);
    }
    Ok(Fetched { bytes, info, widened })
}

/// The download cap for an update: the size the check stated, never above
/// `crx::MAX_CRX_BYTES`; the cap alone when it stated none.
fn body_cap(size: Option<usize>) -> usize {
    size.map_or(crx::MAX_CRX_BYTES, |n| n.min(crx::MAX_CRX_BYTES))
}

/// `dir\manifest.json`, if it is a file of at most
/// `extensions::MAX_MANIFEST_BYTES` (worker thread).
fn read_manifest_bytes(dir: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let path = dir.join("manifest.json");
    crate::extensions::plain_file_metadata(&path)?;
    let mut out = Vec::new();
    std::fs::File::open(&path).ok()?.take(crate::extensions::MAX_MANIFEST_BYTES + 1).read_to_end(&mut out).ok()?;
    (out.len() as u64 <= crate::extensions::MAX_MANIFEST_BYTES).then_some(out)
}

/// A GET through `net` (worker thread); the self-test build asks its fake
/// store instead, so it never reaches the network.
fn http_get(url: &str, limits: &crate::net::Limits) -> Result<crate::net::Response, String> {
    #[cfg(feature = "selftest")]
    return fake::get(url, limits);
    #[cfg(not(feature = "selftest"))]
    return crate::net::get(url, limits).map_err(|e| e.to_string());
}

fn package_error(e: crx::CrxError) -> Failure {
    Failure::Package(e.to_string())
}

/// Checks `bytes` as a package of `id` under `policy` and reads what the
/// question shows.
#[cfg(test)]
fn inspect(bytes: &[u8], id: &str, policy: Policy) -> Result<Info, Failure> {
    read_package(bytes, id, policy).map(|(info, _)| info)
}

/// `inspect`, and the package's manifest as it is.
fn read_package(bytes: &[u8], id: &str, policy: Policy) -> Result<(Info, Vec<u8>), Failure> {
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
    let permissions = cws::describe_permissions(&manifest);
    Ok((Info { name, version: version.to_owned(), permissions }, manifest))
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
        Event::Checked { result } => checked(app, result),
        // The download of a job cancelled meanwhile: one fewer running,
        // which may let the next update start.
        Event::Fetched { .. } => job_ended(app),
        // The unpacking of a job cancelled meanwhile.
        Event::Unpacked { .. } => {}
    }
    app.mark_dirty();
}

fn fetched(app: &mut App, result: Result<Fetched, Failure>) {
    let Some(job) = app.webstore.job.as_mut() else { return };
    if !matches!(job.stage, Stage::Fetching) {
        return;
    }
    let (confirm, update) = (job.confirm, job.offered.is_some());
    match result {
        Err(f) => end(app, Err(f)),
        // New permissions: the update waits for the user's yes, from now on
        // like an install that asks. Never over something else on screen
        // (a menu, a question, an extension's popup, a page in fullscreen)
        // or while X-Browser is in the background: then it is left for the
        // next check (Cancelled is not remembered as failed).
        Ok(Fetched { bytes, info, widened: true }) if update => {
            let free = app.overlay.mode == Mode::Hidden
                && crate::popup::extension(app).is_none()
                && !app.fullscreen.on()
                && app.in_front();
            if !free {
                return end(app, Err(Failure::Cancelled));
            }
            let Some(job) = app.webstore.job.as_mut() else { return };
            job.confirm = true;
            job.stage = Stage::Asking { bytes: Some(bytes), info };
            show(app);
        }
        Ok(Fetched { bytes, info, .. }) if confirm => {
            job.stage = Stage::Asking { bytes: Some(bytes), info };
            if app.overlay.mode == Mode::Store {
                show(app);
            } else {
                // The question went away while the package came in.
                end(app, Err(Failure::Cancelled));
            }
        }
        Ok(Fetched { bytes, info, .. }) => start_unpack(app, bytes, info),
    }
}

fn start_unpack(app: &mut App, bytes: Vec<u8>, info: Info) {
    let Some(root) = app.extensions.root().map(Path::to_path_buf) else { return end(app, Err(Failure::NoView)) };
    let Some(id) = app.webstore.job.as_ref().map(|j| j.id.clone()) else { return };
    let recorded = recorded_dir(app, &id);
    let Some(job) = app.webstore.job.as_mut() else { return };
    let (serial, version) = (job.serial, info.version.clone());
    job.stage = Stage::Unpacking { info };
    job.replaced = recorded.clone();
    let proxy = app.proxy().clone();
    let spawned = std::thread::Builder::new().name("xb-store-unpack".into()).spawn(move || {
        let result = unpack_to(&root, &bytes, &id, &version, recorded.as_deref(), STORE_POLICY);
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
    let (id, update) = (job.id.clone(), job.offered.is_some());
    // An update of an extension removed meanwhile, or one the Uzantılar
    // page is switching or removing, does not install it again.
    if update && (recorded_dir(app, &id).is_none() || app.ext_ui.is_busy_with(&id)) {
        if unpacked.fresh {
            discard(app, unpacked.dir);
        }
        return end(app, Err(Failure::Cancelled));
    }
    let op = crate::extensions::add_from_store(app, &unpacked.dir, &id);
    if let Some(job) = app.webstore.job.as_mut() {
        job.stage = Stage::Adding { op, unpacked, info };
    }
    // It may have ended already (no content view).
    after_extension_event(app);
}

/// Called after every `UserEvent::Extension`: an add or removal of the job
/// may have ended (`extensions` answers through `Extensions::outcome`), or
/// the start-up reconcile and its re-adds, which a check waits for.
pub fn after_extension_event(app: &mut App) {
    job_progress(app);
    run_pending_check(app);
}

fn job_progress(app: &mut App) {
    let Some(job) = app.webstore.job.as_ref() else { return };
    let op = match &job.stage {
        Stage::Adding { op, .. } | Stage::Removing { op } => *op,
        _ => return,
    };
    let Some(result) = app.extensions.outcome(op).cloned() else { return };
    let Some(mut job) = app.webstore.job.take() else { return };
    let removal = job.action == Action::Remove;
    match std::mem::replace(&mut job.stage, Stage::Fetching) {
        Stage::Adding { unpacked, info, .. } => match result {
            Ok(_) => {
                reload_store_tab(app, job.tab, &job.id);
                if let Some(old) = job.replaced.take() {
                    retire(app, &job.id, old, &unpacked.dir);
                }
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
    // A removal has no `on_installed`; an add reaches it through `finish`.
    if removal {
        job_ended(app);
    }
    app.mark_dirty();
}

/// A job ended, or a download of a cancelled one: the next queued update,
/// else a check the timer asked for. Installs come here from
/// `on_installed`, after first-run has had its turn at the free slot (its
/// one retry after a busy answer), removals and stale downloads directly.
fn job_ended(app: &mut App) {
    next_update(app);
    run_pending_check(app);
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

/// Deletes the version folder a newer one replaced, off the UI thread, now
/// that WebView2 loads the new one. Only `root\<id>\<name>`, never the
/// folder now recorded, and only while `root` and `root\<id>` are plain
/// folders; one that cannot be deleted yet (a file still open) is renamed
/// aside for the next start's sweep (`extensions::delete_folder`).
fn retire(app: &App, id: &str, old: PathBuf, new: &Path) {
    let Some(root) = app.extensions.root().map(Path::to_path_buf) else { return };
    if !retirable(&root, id, &old, new) {
        return;
    }
    let home = root.join(id);
    let _ = std::thread::Builder::new().name("xb-store-retire".into()).spawn(move || {
        // Checked again here: the folders may have changed since.
        if !crate::extensions::is_plain_folder(&home) || !crate::extensions::is_plain_folder(&old) {
            return eprintln!("x-browser: old version {} not deleted: a link or unreadable", old.display());
        }
        if let Err(e) = crate::extensions::delete_folder(&root, &old) {
            eprintln!("x-browser: old version {} not deleted: {e}", old.display());
        }
    });
}

/// Whether `old` may be deleted once `new` is in: a version folder
/// (`<version>` or `<version>_<n>`, as `unpack_to` names them) right inside
/// `root\<id>`, and not `new`.
fn retirable(root: &Path, id: &str, old: &Path, new: &Path) -> bool {
    let same = old.components().eq(new.components());
    let Some(name) = old.file_name().and_then(|n| n.to_str()) else { return false };
    let version_name = cws::is_version(name)
        || name.rsplit_once('_').is_some_and(|(v, n)| cws::is_version(v) && (2..=MAX_VERSION_FOLDERS).any(|k| k.to_string() == n));
    let home = root.join(id);
    !same && version_name && old.parent().is_some_and(|p| p.components().eq(home.components()))
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
    // A download still running for this job is no longer wanted.
    job.cancel.store(true, Ordering::Relaxed);
    match &result {
        Ok(_) => {
            if app.overlay.mode == Mode::Store {
                crate::overlay::dismiss(app);
            }
        }
        Err(Failure::Cancelled) => {}
        Err(f) => {
            eprintln!("x-browser: extension {} not installed: {f:?}", job.id);
            // The question is gone: kept for the Extensions page to show at
            // the next start.
            if !show_error(app, &f.message(), job.spot)
                && let Some(e) = LastError::new(&job.id, &f.message(), crate::history::now_secs())
            {
                app.extensions.set_last_error(e);
            }
        }
    }
    // A package or install that failed fails the same way again: that
    // version is not fetched again this start. A network failure, a "no"
    // or a busy answer may go another way next time.
    if let (Some(version), Err(Failure::Package(_) | Failure::Install(_))) = (&job.offered, &result) {
        app.webstore.remember_failed(&job.id, version);
    }
    match job.action {
        // `on_installed` moves the queue on, after first-run.
        Action::Install => report(app, &job.id, result),
        Action::Remove => job_ended(app),
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
    // Then the next update, or a check the timer asked for, if first-run
    // did not start an install.
    job_ended(app);
    app.mark_dirty();
}

// --- updates ----------------------------------------------------------------------

/// Asks the update service whether the extensions installed from the store
/// and turned on have newer versions, and updates them one at a time
/// (module notes). Nothing shows unless an update wants new permissions.
/// Does nothing while a check is running. The daily check comes here
/// (`run_pending_check`); so may a button that asks for one now.
pub fn check_updates(app: &mut App) {
    if app.webstore.checking {
        return;
    }
    let installed = store_installed(app.extensions.records(), &app.extensions.list());
    if installed.is_empty() {
        return;
    }
    let prodversion = prodversion();
    let proxy = app.proxy().clone();
    let spawned = std::thread::Builder::new().name("xb-store-check".into()).spawn(move || {
        let result = check(&installed, &prodversion);
        let _ = proxy.send_event(UserEvent::Store(Event::Checked { result }));
    });
    match spawned {
        Ok(_) => app.webstore.checking = true,
        Err(e) => eprintln!("x-browser: update check not started: {e}"),
    }
}

/// Whether `check_updates` has anything to ask about: an extension
/// installed from the store and turned on.
pub fn has_checkable(app: &App) -> bool {
    !store_installed(app.extensions.records(), &app.extensions.list()).is_empty()
}

/// Sends `UserEvent::UpdateTimer` every `UPDATE_TIMER_TICK` until the event
/// loop is gone. The self-test build has no timer: it sends the event
/// itself.
#[cfg(not(feature = "selftest"))]
pub fn start_update_timer(proxy: crate::native::Proxy) {
    let spawned = std::thread::Builder::new().name("xb-store-timer".into()).spawn(move || {
        loop {
            std::thread::sleep(UPDATE_TIMER_TICK);
            if proxy.send_event(UserEvent::UpdateTimer).is_err() {
                break;
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("x-browser: update timer not started: {e}");
    }
}

/// The timer's tick: asks for a check, which runs once nothing is in its
/// way (`run_pending_check`). Nothing while X-Browser is closing.
pub fn on_update_timer(app: &mut App) {
    if app.quit {
        return;
    }
    app.webstore.check_pending = true;
    run_pending_check(app);
}

/// Runs the check the timer asked for once the start-up reconcile and its
/// re-adds are over (`Extensions::settled`), no job or check runs and
/// first-run is not installing; then only if one is due
/// (`update_check_due`). The time is kept as the check starts, whatever
/// comes of it.
fn run_pending_check(app: &mut App) {
    let w = &app.webstore;
    let blocked = w.checking
        || w.job.is_some()
        || app.quit
        || !app.extensions.settled()
        || app.extensions.first_run.run == crate::firstrun::Run::Installing;
    if !w.check_pending || blocked {
        return;
    }
    app.webstore.check_pending = false;
    let now = crate::history::now_secs();
    if !update_check_due(app.extensions.last_update_check(), now) {
        return;
    }
    app.extensions.set_last_update_check(now);
    check_updates(app);
}

/// Whether a check is due at `now` (seconds) after the last one at `last`:
/// never checked, a day or more since, or the clock went back.
pub fn update_check_due(last: Option<u64>, now: u64) -> bool {
    last.is_none_or(|last| now < last || now - last >= UPDATE_CHECK_EVERY)
}

/// `(id, version)` of every extension recorded as from the store, with a
/// folder and a valid version, and turned on (in the record and, in
/// `listed`, in WebView2): one turned off is not changed behind its back.
fn store_installed(records: &[crate::extensions::Entry], listed: &[crate::extensions::Listed]) -> Vec<(String, String)> {
    records
        .iter()
        .filter(|e| {
            matches!(e.source, crate::extensions::Source::Cws)
                && !e.removed
                && e.enabled
                && listed.iter().find(|l| l.id == e.id).is_some_and(|l| l.enabled)
                && !e.dir.is_empty()
                && crx::is_extension_id(&e.id)
                && !crate::extensions::is_builtin(&e.id)
                && cws::is_version(&e.version)
        })
        .map(|e| (e.id.clone(), e.version.clone()))
        .collect()
}

/// The update check (worker thread): one request per `CHECK_BATCH`. A batch
/// that fails is left out of the answer; only when every batch failed is
/// the check a failure.
fn check(installed: &[(String, String)], prodversion: &str) -> Result<Vec<cws::Update>, Failure> {
    let (mut out, mut answered, mut failure) = (Vec::new(), false, None);
    for batch in installed.chunks(CHECK_BATCH) {
        match check_batch(batch, prodversion) {
            Ok(updates) => {
                answered = true;
                out.extend(updates);
            }
            Err(f) => {
                eprintln!("x-browser: update check of {} extension(s) failed: {f:?}", batch.len());
                failure = Some(f);
            }
        }
    }
    match failure {
        Some(f) if !answered => Err(f),
        _ => Ok(out),
    }
}

fn check_batch(batch: &[(String, String)], prodversion: &str) -> Result<Vec<cws::Update>, Failure> {
    let pairs: Vec<(&str, &str)> = batch.iter().map(|(i, v)| (i.as_str(), v.as_str())).collect();
    let url = cws::update_check_url(&pairs, prodversion).ok_or_else(|| Failure::Net("adres çok uzun".into()))?;
    let limits = crate::net::Limits { max_body: cws::MAX_UPDATE_RESPONSE, ..Default::default() };
    let resp = http_get(&url, &limits).map_err(Failure::Net)?;
    if resp.status != 200 {
        return Err(Failure::Net(format!("HTTP {}", resp.status)));
    }
    Ok(cws::parse_update_response(&resp.body))
}

/// Whether the update of `id` to `version` failed this start
/// (`State::failed`).
fn failed_before(failed: &[(String, String)], id: &str, version: &str) -> bool {
    failed.iter().any(|(i, v)| i == id && cws::same_version(v, version))
}

/// The updates worth fetching: offered (`ok`, with version, URL and hash)
/// for an extension `store_installed` keeps, at a version newer than the
/// one recorded, that did not fail this start.
fn wanted(
    records: &[crate::extensions::Entry],
    listed: &[crate::extensions::Listed],
    offered: &[cws::Update],
    failed: &[(String, String)],
) -> Vec<(String, Source)> {
    let installed = store_installed(records, listed);
    offered
        .iter()
        .filter(|u| u.status == "ok")
        .filter_map(|u| {
            let (version, codebase, sha256) = (u.version.as_ref()?, u.codebase.as_ref()?, u.hash_sha256?);
            let (_, have) = installed.iter().find(|(id, _)| *id == u.id)?;
            (cws::is_newer(version, have) && !failed_before(failed, &u.id, version)).then(|| {
                let source = Source::Update { codebase: codebase.clone(), sha256, version: version.clone(), size: u.size };
                (u.id.clone(), source)
            })
        })
        .collect()
}

fn checked(app: &mut App, result: Result<Vec<cws::Update>, Failure>) {
    app.webstore.checking = false;
    // How many updates this check found to install (None: it failed), for
    // the Extensions page.
    let mut found = None;
    match result {
        Ok(offered) => {
            let listed = app.extensions.list();
            let wanted = wanted(app.extensions.records(), &listed, &offered, &app.webstore.failed);
            found = Some(wanted.len());
            for (id, source) in wanted {
                let w = &mut app.webstore;
                // An update of it under way already; a removal is not one.
                if w.job.as_ref().is_some_and(|j| j.id == id && j.offered.is_some()) {
                    continue;
                }
                match w.queue.iter_mut().find(|(q, _)| *q == id) {
                    Some(queued) => queued.1 = source,
                    None => w.queue.push_back((id, source)),
                }
            }
        }
        Err(f) => eprintln!("x-browser: update check failed: {f:?}"),
    }
    job_ended(app);
    crate::ext_ui::on_update_checked(app, found);
}

/// Starts the next queued update that is still wanted, unless a job is
/// under way or too many downloads still run: then the next `job_ended`
/// tries again.
fn next_update(app: &mut App) {
    let w = &app.webstore;
    if w.job.is_some() || w.fetching.load(std::sync::atomic::Ordering::Relaxed) >= MAX_FETCHES {
        return;
    }
    while let Some((id, source)) = app.webstore.queue.pop_front() {
        // The record may have changed since the check (removed, updated,
        // turned off).
        let listed = app.extensions.list();
        // One the Uzantılar page is switching or removing waits for the
        // next check.
        let still = matches!(&source, Source::Update { version, .. }
            if !failed_before(&app.webstore.failed, &id, version)
                && !app.ext_ui.is_busy_with(&id)
                && store_installed(app.extensions.records(), &listed).iter().any(|(i, have)| *i == id && cws::is_newer(version, have)));
        if still {
            return start_job(app, &id, false, None, source);
        }
    }
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
    /// An update: "Güncelle" instead of "Ekle".
    update: bool,
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
        update: job.offered.is_some(),
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
/// Shows `text` in the question, if it is still on screen. `false`: it was
/// gone, and nobody saw the error.
fn show_error(app: &mut App, text: &str, spot: (f64, f64)) -> bool {
    if app.overlay.mode != Mode::Store {
        return false;
    }
    let view = View { stage: "error", action: "", id: "", name: "", version: "", permissions: &[], update: false, text, ms: 0, guard: 0 };
    let Ok(json) = serde_json::to_string(&view) else { return false };
    crate::overlay::show_store(app, format!("xbExt.show({json})"), spot);
    true
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

/// The self-test's fake store: answers `http_get` from a table instead of
/// the network, and notes every URL asked for.
#[cfg(feature = "selftest")]
pub mod fake {
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    type Answer = (String, u16, Vec<u8>);
    static ANSWERS: Mutex<Vec<Answer>> = Mutex::new(Vec::new());
    static ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// URLs starting with `prefix` get `status` and `body` (the latest
    /// `serve` for a prefix wins); any other URL fails as offline.
    pub fn serve(prefix: &str, status: u16, body: Vec<u8>) {
        let mut a = ANSWERS.lock().unwrap_or_else(|e| e.into_inner());
        a.retain(|(p, ..)| p != prefix);
        a.insert(0, (prefix.to_owned(), status, body));
    }

    /// Forgets every answer and every URL asked for.
    pub fn clear() {
        ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        ASKED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// The URLs asked for since the last `clear`, in order.
    pub fn asked() -> Vec<String> {
        ASKED.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(super) fn get(url: &str, limits: &crate::net::Limits) -> Result<crate::net::Response, String> {
        // A cancelled call stops before asking, as `net::get` does.
        if limits.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(crate::net::NetError::Cancelled.to_string());
        }
        ASKED.lock().unwrap_or_else(|e| e.into_inner()).push(url.to_owned());
        let answers = ANSWERS.lock().unwrap_or_else(|e| e.into_inner());
        let (_, status, body) = answers.iter().find(|(p, ..)| url.starts_with(p.as_str())).ok_or("fake store: offline")?;
        if body.len() > limits.max_body {
            return Err("fake store: body over the cap".into());
        }
        Ok(crate::net::Response { final_url: url.to_owned(), status: *status, body: body.clone() })
    }
}

#[cfg(test)]
mod tests;
