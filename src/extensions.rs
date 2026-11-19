//! Browser extensions (v0.3 task 2): X-Browser's record of what it
//! installed, the start-up reconcile, and add / list / enable / remove for
//! Rust callers. The WebView2 calls are `native::extensions`; the Uzantılar
//! page and the popups are `ext_ui` and `popup`.
//!
//! - Extensions live only in the default WebView2 profile, the one content
//!   tabs use. The UI strip and the overlay run in their own profile
//!   (`ui::UI_PROFILE`), where nothing is ever added: an `<all_urls>`
//!   content script would otherwise run in the UI page and reach its IPC.
//! - WebView2's profile is the source of truth for what is installed and
//!   enabled. `extensions.json` (`{"v":1,"items":[...]}`, see `store`) adds
//!   what WebView2 does not keep: the folder, version and source. It is read
//!   back as untrusted: each entry is read and checked on its own, so a bad
//!   one is dropped and the rest kept (`ExtFile::sanitize`); the store keeps
//!   a `.bak` of the last good file for a file that does not parse at all.
//! - Unpacked extensions X-Browser owns sit in `<data>\extensions\<id>\<version>`.
//!   Half-written folders there (`.<name>.unpack-<pid>-<n>` from
//!   `crx::unpack`, `<version>.tmp`, `.<id>.removed-<pid>`) are deleted at
//!   start (`sweep`). Nothing is deleted under `<data>\extensions` if it is
//!   itself a junction or link (`is_plain_folder`).
//! - Removing deletes the folder only once WebView2 confirms the removal. If
//!   the folder cannot be deleted then, the entry stays (hidden, `removed`)
//!   and the next start tries again.
//! - Start-up reconcile, once, after the UI is up and a content webview
//!   exists (`on_ui_ready`, `on_view_created`): only recorded ids are ever
//!   loaded. A recorded one WebView2 no longer knows is added again, only
//!   from a folder X-Browser owns, else dropped; if its folder now holds
//!   another id, that one is taken out again and the entry dropped. One
//!   WebView2 has but the record does not (Edge's own install dialog, say)
//!   is listed with an unknown source and turned off.
//! - The two extensions every WebView2 profile has built in are never
//!   listed or recorded (`BUILTIN`).
//! - Operations complete asynchronously: each returns an `Op` whose result
//!   `Extensions::outcome` gives once WebView2 has answered.
//! - The file also keeps the first-run uBOL state (`first_run_ubol`, see
//!   `firstrun`); the reconcile's end starts that job.

use crate::app::{App, UserEvent};
use crate::native::{self, ExtensionInfo};
use crate::store::{Store, Stored};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

pub const FILE: &str = "extensions.json";
/// The folder under the data folder that holds X-Browser's own unpacked
/// extensions.
pub const DIR: &str = "extensions";
/// Larger files are not read and not written.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Most extensions recorded.
pub const MAX_ITEMS: usize = 500;
/// Largest manifest.json read (bytes).
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Longest name or version kept (characters).
pub const MAX_TEXT: usize = 200;
/// Longest folder path kept (characters).
pub const MAX_PATH: usize = 4096;
/// Outcomes kept for `Extensions::outcome`.
const OUTCOMES: usize = 32;

/// Built into every WebView2 profile; never listed, recorded or removed.
pub const BUILTIN: [&str; 2] = [
    "dgiklkfkllikcanfonkcabmbdfmgleag", // Microsoft Clipboard Extension
    "mhjfbmdgcfjbbpaeojofohoefgiehjai", // Microsoft Edge PDF Viewer
];

pub fn is_builtin(id: &str) -> bool {
    BUILTIN.contains(&id)
}

// ------------------------------------------------------------------ file

/// Where an extension came from.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A folder given by a caller (`add`).
    Unpacked,
    /// The Chrome Web Store (task 4).
    Cws,
}

/// One recorded extension.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    /// The unpacked folder WebView2 loads it from (absolute).
    pub dir: String,
    pub source: Source,
    #[serde(default)]
    pub version: String,
    /// The name WebView2 gave (its locale's), for listing it before
    /// WebView2 has answered.
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Unix seconds.
    #[serde(default)]
    pub installed: u64,
    /// Removed from WebView2, but its folder could not be deleted yet: kept,
    /// never listed or loaded, until a start deletes the folder
    /// (`Extensions::open`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub removed: bool,
}

fn yes() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
pub struct ExtFile {
    #[serde(deserialize_with = "lenient")]
    pub items: Vec<Entry>,
    /// Missing: pending (see `firstrun::State`).
    pub first_run_ubol: crate::firstrun::State,
    /// When the last Web Store update check started, in seconds since the
    /// epoch (`webstore::update_check_due`). Missing or unreadable: never.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient_secs")]
    pub last_update_check: Option<u64>,
    /// The last install error nobody saw (`webstore::LastError`), one
    /// record. Missing or unreadable: none.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "crate::webstore::LastError::lenient")]
    pub last_error: Option<crate::webstore::LastError>,
}

/// A time in seconds, or None for anything else (a string, a negative or
/// fractional number): one bad field does not cost the file.
fn lenient_secs<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_u64())
}

/// The items of an array, each read on its own: one that is not an entry
/// (a number, `null`, a missing or wrong field, an unknown source) is
/// skipped, not fatal to the file.
fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Entry>, D::Error> {
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

/// Text as kept: no control characters, no bidi marks, embeddings,
/// overrides or isolates (a name that turns the text around it), at most
/// `MAX_TEXT` characters.
pub fn clean_text(s: &str) -> String {
    let bidi = |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    s.chars().filter(|&c| !c.is_control() && !bidi(c)).take(MAX_TEXT).collect::<String>().trim().to_string()
}

/// An absolute folder path without `.`/`..` parts or control characters.
pub fn valid_dir(dir: &str) -> bool {
    let p = Path::new(dir);
    !dir.is_empty()
        && dir.chars().count() <= MAX_PATH
        && !dir.chars().any(char::is_control)
        && p.is_absolute()
        && p.components().all(|c| !matches!(c, Component::CurDir | Component::ParentDir))
}

/// An entry within the limits, or None if it must not be kept.
fn clean(e: Entry, now: u64) -> Option<Entry> {
    (crate::crx::is_extension_id(&e.id) && !is_builtin(&e.id) && valid_dir(&e.dir)).then(|| Entry {
        version: clean_text(&e.version),
        name: clean_text(&e.name),
        installed: e.installed.min(now),
        ..e
    })
}

impl Stored for ExtFile {
    const NAME: &'static str = FILE;
    const MAX_BYTES: u64 = MAX_FILE_BYTES;

    fn sanitize(&mut self) {
        let now = crate::history::now_secs();
        let mut seen = HashSet::new();
        let items = std::mem::take(&mut self.items);
        self.items = items
            .into_iter()
            .filter_map(|e| clean(e, now))
            .filter(|e| seen.insert(e.id.clone()))
            .take(MAX_ITEMS)
            .collect();
    }
}

// ------------------------------------------------------- folders, manifest

/// A name left by an install or removal that did not finish.
pub fn is_leftover(name: &str) -> bool {
    (name.starts_with('.') && (name.contains(".unpack-") || name.contains(".removed-"))) || name.ends_with(".tmp")
}

/// False when `root` is a junction, symbolic link or any other reparse point
/// (deleting under it would delete somewhere else), or cannot be looked at.
/// A missing folder is fine: there is nothing under it to delete.
pub fn is_plain_folder(root: &Path) -> bool {
    match std::fs::symlink_metadata(root) {
        Ok(m) => !m.file_type().is_symlink() && !is_reparse_point(&m),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// The metadata of `path` if it is a plain file: not a link, junction or
/// other reparse point (a manifest swapped for a link to a file outside
/// the extension's folder is not read).
pub fn plain_file_metadata(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(path).ok().filter(|m| m.is_file() && !is_reparse_point(m))
}

#[cfg(windows)]
fn is_reparse_point(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

/// Deletes the leftovers (`is_leftover`) in `root` and in each
/// `root\<id>`, never following a link, and nothing at all if `root` is not
/// a plain folder. Returns how many were deleted.
pub fn sweep(root: &Path) -> usize {
    if !is_plain_folder(root) {
        eprintln!("x-browser: {} is a link or cannot be read; not cleaned up", root.display());
        return 0;
    }
    fn sweep_level(dir: &Path, deeper: bool) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
        let mut n = 0;
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            let name = e.file_name();
            let path = e.path();
            if is_leftover(&name.to_string_lossy()) {
                let gone = if kind.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
                match gone {
                    Ok(()) => n += 1,
                    Err(err) => eprintln!("x-browser: cannot delete {}: {err}", path.display()),
                }
            } else if deeper && kind.is_dir() {
                n += sweep_level(&path, false);
            }
        }
        n
    }
    sweep_level(root, true)
}

/// The folder `remove` deletes for an extension recorded at `dir`: its
/// `root\<id>` folder when `dir` is a version folder in it, or `dir` itself
/// when it is `root\<id>`. Folders anywhere else are not X-Browser's and are
/// left alone.
pub fn owned_folder(root: &Path, id: &str, dir: &Path) -> Option<PathBuf> {
    if !valid_dir(&dir.to_string_lossy()) || !crate::crx::is_extension_id(id) {
        return None;
    }
    let mine = root.join(id);
    if dir == mine || dir.parent() == Some(mine.as_path()) { Some(mine) } else { None }
}

/// What X-Browser reads from a manifest.json.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    /// The id the manifest's `"key"` gives, if it has one.
    pub key_id: Option<String>,
}

/// Reads `dir\manifest.json`: a JSON object of at most
/// `MAX_MANIFEST_BYTES` with a non-empty `"version"`.
pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join("manifest.json");
    let meta = plain_file_metadata(&path).ok_or("manifest.json is missing, a link or not a file")?;
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err("manifest.json is not a file within the size limit".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("manifest.json: {e}"))?;
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let value: serde_json::Value = serde_json::from_slice(text).map_err(|_| "manifest.json is not valid JSON")?;
    let version = value.get("version").and_then(|v| v.as_str()).map(clean_text).unwrap_or_default();
    if version.is_empty() {
        return Err("manifest.json has no version".into());
    }
    let key_id = value.get("key").and_then(|k| k.as_str()).and_then(id_from_key);
    Ok(Manifest { version, key_id })
}

/// The id Chrome gives an extension whose manifest `"key"` is `key`
/// (base64 of its DER public key): SHA-256 of the key, as `crx::id_from_hash`.
pub fn id_from_key(key: &str) -> Option<String> {
    let der = base64_decode(key)?;
    if der.is_empty() {
        return None;
    }
    crate::crx::sha256(&[&der]).map(|h| crate::crx::id_from_hash(&h))
}

/// Standard base64 (padding optional, whitespace ignored), or None.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut padding = false;
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            padding = true;
            continue;
        }
        if padding {
            return None;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // Leftover bits must be zero padding of the last byte.
    (bits < 6 && acc == 0).then_some(out)
}

// ------------------------------------------------------------- reconcile

/// What the start-up reconcile does with each id.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Recorded and known to WebView2.
    pub keep: Vec<String>,
    /// Recorded, unknown to WebView2, `can_readd`: add it again.
    pub readd: Vec<String>,
    /// Recorded, unknown to WebView2, not `can_readd` (folder gone, or not
    /// X-Browser's): forget it.
    pub drop: Vec<String>,
    /// Known to WebView2 only (not built in): listed as unknown source and
    /// turned off.
    pub unknown: Vec<String>,
}

/// Entries marked `removed` are left out: they are neither loaded nor
/// count as recorded.
pub fn plan(records: &[Entry], live: &[ExtensionInfo], can_readd: impl Fn(&Entry) -> bool) -> Plan {
    let mut p = Plan::default();
    let records: Vec<&Entry> = records.iter().filter(|e| !e.removed).collect();
    for e in &records {
        if live.iter().any(|l| l.id == e.id) {
            p.keep.push(e.id.clone());
        } else if can_readd(e) {
            p.readd.push(e.id.clone());
        } else {
            p.drop.push(e.id.clone());
        }
    }
    for l in live {
        if !is_builtin(&l.id) && !records.iter().any(|e| e.id == l.id) {
            p.unknown.push(l.id.clone());
        }
    }
    p
}

// ----------------------------------------------------------------- state

/// An operation's handle, for `Extensions::outcome`.
pub type Op = u64;

/// The start-up reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reconcile {
    #[default]
    NotYet,
    Running,
    Done,
}

/// One row of `Extensions::list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    /// None: WebView2 has it, but X-Browser did not record installing it.
    pub source: Option<Source>,
    /// WebView2 has it (false only before the reconcile has run).
    pub installed: bool,
}

/// The id an add must come back as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// A reconcile re-add of a recorded entry: another id means its folder
    /// no longer holds it, so the entry is dropped.
    Readd(String),
    /// A store install or update (`webstore`): another id is refused, and
    /// what is recorded for the id (the version installed before, its
    /// folder) is left as it was.
    Install(String),
}

impl Expect {
    pub fn id(&self) -> &str {
        match self {
            Self::Readd(id) | Self::Install(id) => id,
        }
    }
}

/// Answers from `native::extensions`, delivered as `UserEvent::Extension`.
#[derive(Debug)]
pub enum Event {
    /// The reconcile's look at the profile.
    Listed(Result<Vec<ExtensionInfo>, String>),
    /// `expect`: the id the add must come back as (`Expect`).
    Added {
        op: Op,
        dir: String,
        source: Source,
        enabled: bool,
        manifest: Manifest,
        expect: Option<Expect>,
        result: Result<ExtensionInfo, String>,
    },
    Enabled { op: Op, id: String, on: bool, result: Result<bool, String> },
    Removed { op: Op, id: String, result: Result<bool, String> },
    /// The self-test's fake store result (`firstrun::fake`), kept apart from
    /// `UserEvent::StoreInstalled` so the fake never touches `webstore`.
    #[cfg_attr(not(feature = "selftest"), allow(dead_code))]
    StoreInstalled { id: String, result: Result<String, crate::firstrun::FailureKind> },
}

#[derive(Default)]
pub struct Extensions {
    store: Option<Store<ExtFile>>,
    file: ExtFile,
    /// `<data>\extensions`.
    root: Option<PathBuf>,
    /// The content profile's extensions as last seen (no built-ins).
    live: Vec<ExtensionInfo>,
    pub reconcile: Reconcile,
    next_op: Op,
    outcomes: Vec<(Op, Result<String, String>)>,
    /// What this start's first-run job has done (not saved).
    pub first_run: crate::firstrun::Session,
    /// Re-adds of the start-up reconcile sent to WebView2 and not answered
    /// yet (`settled`).
    readding: usize,
}

impl Extensions {
    /// Reads `extensions.json` and deletes the leftovers in the
    /// extensions folder. Without a data folder nothing is kept.
    pub fn open(dir: Option<&Path>) -> Self {
        let Some(dir) = dir else { return Self::default() };
        let mut store = Store::new(dir);
        let file = store.load().value;
        let root = dir.join(DIR);
        let swept = sweep(&root);
        if swept > 0 {
            eprintln!("x-browser: deleted {swept} unfinished extension folder(s)");
        }
        let mut x = Self { store: Some(store), file, root: Some(root), ..Self::default() };
        x.finish_removals();
        x
    }

    /// Entries removed from WebView2 whose folder could not be deleted then:
    /// tries again, and forgets each whose folder is gone now (or was never
    /// X-Browser's to delete).
    fn finish_removals(&mut self) {
        if !self.file.items.iter().any(|e| e.removed) {
            return;
        }
        let root = self.root.clone();
        self.file.items.retain(|e| {
            if !e.removed {
                return true;
            }
            let Some(root) = root.as_deref() else { return false };
            let Some(path) = owned_folder(root, &e.id, Path::new(&e.dir)) else { return false };
            match delete_folder(root, &path) {
                Ok(()) => false,
                Err(err) => {
                    eprintln!("x-browser: extension {}: {err}", e.id);
                    true
                }
            }
        });
        self.save();
    }

    /// Whether the reconcile may load `e` again: only from a folder
    /// X-Browser owns (`owned_folder`) that still has a manifest.
    fn can_readd(&self, e: &Entry) -> bool {
        let dir = Path::new(&e.dir);
        self.root.as_deref().and_then(|r| owned_folder(r, &e.id, dir)).is_some() && dir.join("manifest.json").is_file()
    }

    /// Records what WebView2 installed from `dir`, replacing the entry with
    /// the same id. Refused (nothing recorded) when it is not the id
    /// `expect`ed (for a reconcile re-add that entry is dropped; for a store
    /// install the record is left as it was) or when the list is full;
    /// `undo` then says WebView2 did not have it before, so it is to be
    /// taken out again.
    fn record_added(
        &mut self,
        info: &ExtensionInfo,
        dir: String,
        source: Source,
        manifest: Manifest,
        expect: Option<&Expect>,
        now: u64,
    ) -> Result<(), Refused> {
        let had = self.live.iter().any(|l| l.id == info.id) || self.file.items.iter().any(|e| e.id == info.id && !e.removed);
        if let Some(want) = expect
            && want.id() != info.id
        {
            let mut why = format!("{dir} holds extension {} instead of {}", info.id, want.id());
            if let Expect::Readd(id) = want {
                self.file.items.retain(|e| e.id != *id);
                self.save();
                why.push_str("; forgotten");
            }
            return Err(Refused { why, undo: !had });
        }
        let old = self.file.items.iter().position(|e| e.id == info.id);
        if old.is_none() && self.file.items.len() >= MAX_ITEMS {
            return Err(Refused { why: format!("no room: {MAX_ITEMS} extensions already"), undo: !had });
        }
        let entry = Entry {
            id: info.id.clone(),
            dir,
            source,
            version: manifest.version,
            name: clean_text(&info.name),
            enabled: info.enabled,
            installed: old.map_or(now, |i| self.file.items[i].installed),
            removed: false,
        };
        match old {
            Some(i) => self.file.items[i] = entry,
            None => self.file.items.push(entry),
        }
        self.live.retain(|l| l.id != info.id);
        self.live.push(info.clone());
        self.save();
        Ok(())
    }

    /// Drops `id` after a removal. `confirmed`: WebView2 removed it, so its
    /// folder, if X-Browser owns it, is deleted too; else (it was not
    /// installed) the folder is left alone. A folder that cannot be deleted
    /// keeps the entry, marked `removed`, for the next start.
    fn record_removed(&mut self, id: &str, confirmed: bool) -> Result<String, String> {
        self.live.retain(|l| l.id != id);
        let Some(i) = self.file.items.iter().position(|e| e.id == id) else { return Ok(id.to_owned()) };
        let mut result = Ok(());
        if confirmed
            && let Some(root) = self.root.as_deref()
            && let Some(path) = owned_folder(root, id, Path::new(&self.file.items[i].dir))
        {
            result = delete_folder(root, &path);
        }
        match result {
            Ok(()) => {
                self.file.items.remove(i);
            }
            Err(_) => self.file.items[i].removed = true,
        }
        self.save();
        result.map(|()| id.to_owned()).map_err(|e| format!("{e} (tried again at the next start)"))
    }

    /// Whether there is a file to keep anything in.
    pub fn persists(&self) -> bool {
        self.store.is_some()
    }

    /// Whether `id` is recorded (and not on its way out).
    pub fn has_record(&self, id: &str) -> bool {
        self.file.items.iter().any(|e| e.id == id && !e.removed)
    }

    /// Whether WebView2 had `id` installed at the last listing.
    pub fn is_live(&self, id: &str) -> bool {
        self.live.iter().any(|l| l.id == id)
    }

    /// As if WebView2 had listed just `id`.
    #[cfg(test)]
    pub fn set_live_for_test(&mut self, id: &str) {
        self.set_live(vec![ExtensionInfo { id: id.into(), name: "n".into(), enabled: true }]);
    }

    pub fn first_run_ubol(&self) -> crate::firstrun::State {
        self.file.first_run_ubol
    }

    /// Sets and saves the first-run uBOL state.
    pub fn set_first_run_ubol(&mut self, state: crate::firstrun::State) {
        if self.file.first_run_ubol != state {
            self.file.first_run_ubol = state;
            self.save();
        }
    }

    /// When the last update check started (`ExtFile::last_update_check`).
    pub fn last_update_check(&self) -> Option<u64> {
        self.file.last_update_check
    }

    /// Sets and saves when the last update check started.
    pub fn set_last_update_check(&mut self, secs: u64) {
        self.file.last_update_check = Some(secs);
        self.save();
    }

    /// The last install error nobody saw, for the Extensions page.
    pub fn last_error(&self) -> Option<&crate::webstore::LastError> {
        self.file.last_error.as_ref()
    }

    /// Keeps and saves `e` in place of the one before.
    pub fn set_last_error(&mut self, e: crate::webstore::LastError) {
        self.file.last_error = Some(e);
        self.save();
    }

    /// The Extensions page showed the last error: it is seen, and goes.
    pub fn clear_last_error(&mut self) {
        if self.file.last_error.take().is_some() {
            self.save();
        }
    }

    /// The start-up reconcile is over and WebView2 has answered each of its
    /// re-adds: the record says what the profile has.
    pub fn settled(&self) -> bool {
        self.reconcile == Reconcile::Done && self.readding == 0
    }

    /// Where X-Browser keeps its own unpacked extensions.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The recorded entries.
    pub fn records(&self) -> &[Entry] {
        &self.file.items
    }

    /// Every extension, built-ins left out: the recorded ones in order, then
    /// any WebView2 has that X-Browser did not record.
    pub fn list(&self) -> Vec<Listed> {
        let mut out: Vec<Listed> = self
            .file
            .items
            .iter()
            .filter(|e| !e.removed)
            .map(|e| {
                let live = self.live.iter().find(|l| l.id == e.id);
                Listed {
                    id: e.id.clone(),
                    name: live.map_or_else(|| e.name.clone(), |l| clean_text(&l.name)),
                    version: e.version.clone(),
                    enabled: live.map_or(e.enabled, |l| l.enabled),
                    source: Some(e.source),
                    installed: live.is_some(),
                }
            })
            .collect();
        for l in &self.live {
            if !out.iter().any(|o| o.id == l.id) {
                out.push(Listed {
                    id: l.id.clone(),
                    name: clean_text(&l.name),
                    version: String::new(),
                    enabled: l.enabled,
                    source: None,
                    installed: true,
                });
            }
        }
        out
    }

    /// How operation `op` ended, once it has: `Ok(id)` or `Err(why)`.
    pub fn outcome(&self, op: Op) -> Option<&Result<String, String>> {
        self.outcomes.iter().rev().find(|(o, _)| *o == op).map(|(_, r)| r)
    }

    fn start_op(&mut self) -> Op {
        self.next_op += 1;
        self.next_op
    }

    fn finish(&mut self, op: Op, result: Result<String, String>) {
        if let Err(e) = &result {
            eprintln!("x-browser: extension operation failed: {e}");
        }
        if op == 0 {
            return; // internal (the reconcile)
        }
        if self.outcomes.len() >= OUTCOMES {
            self.outcomes.drain(..1);
        }
        self.outcomes.push((op, result));
    }

    fn save(&mut self) {
        if let Some(s) = &mut self.store {
            s.save(&self.file);
        }
    }

    fn set_live(&mut self, all: Vec<ExtensionInfo>) {
        self.live = all.into_iter().filter(|l| !is_builtin(&l.id)).collect();
    }
}

/// Why `Extensions::record_added` refused an install.
#[derive(Debug, PartialEq, Eq)]
struct Refused {
    why: String,
    /// WebView2 did not have it before: take it out again.
    undo: bool,
}

/// The webview whose profile holds the extensions: any content tab.
fn content_view(app: &App) -> Option<&wry::WebView> {
    app.content_view()
}

/// Takes an extension just installed back out of WebView2 (its folder stays).
fn undo_add(app: &App, id: &str) {
    let Some(view) = content_view(app) else { return };
    let owned = id.to_owned();
    native::remove_extension(view, id, move |r| match r {
        Ok(_) => eprintln!("x-browser: extension {owned} taken out again"),
        Err(e) => eprintln!("x-browser: extension {owned} could not be taken out again: {e}"),
    });
}

fn send(proxy: &crate::native::Proxy, ev: Event) {
    let _ = proxy.send_event(UserEvent::Extension(ev));
}

/// Installs the unpacked extension in `dir` (an absolute folder with a
/// manifest.json) and records it, replacing an older install of the same id
/// in place. The folder is used where it is; `remove` deletes it only if it
/// is in `Extensions::root`. Only the self-test adds a folder directly;
/// the Web Store installs through `add_from_store`.
#[cfg_attr(not(feature = "selftest"), allow(dead_code))]
pub fn add(app: &mut App, dir: &Path, source: Source) -> Op {
    let op = app.extensions.start_op();
    add_as(app, op, dir, source, true, None);
    op
}

/// Installs the unpacked store package in `dir` (see `webstore`), which
/// must come back as extension `id`. Nothing is recorded unless it does; a
/// version recorded before is then left as it was. Once it does, `webstore`
/// deletes the folder of the version replaced (`webstore::retire`).
pub fn add_from_store(app: &mut App, dir: &Path, id: &str) -> Op {
    let op = app.extensions.start_op();
    // An update keeps an extension the user turned off, off.
    let enabled = app.extensions.list().iter().find(|l| l.id == id).is_none_or(|l| l.enabled);
    add_as(app, op, dir, Source::Cws, enabled, Some(Expect::Install(id.to_owned())));
    op
}

/// Returns whether WebView2 was asked: if not, `op` has ended already.
fn add_as(app: &mut App, op: Op, dir: &Path, source: Source, enabled: bool, expect: Option<Expect>) -> bool {
    let checked = match dir.to_str() {
        Some(d) if valid_dir(d) => read_manifest(dir).map(|m| (d.to_owned(), m)),
        _ => Err("not an absolute folder path".into()),
    };
    let (dir_text, manifest) = match checked {
        Ok(c) => c,
        Err(e) => {
            app.extensions.finish(op, Err(e));
            return false;
        }
    };
    let Some(view) = content_view(app) else {
        app.extensions.finish(op, Err("no content webview".into()));
        return false;
    };
    let proxy = app.proxy().clone();
    native::add_extension(view, dir, move |result| {
        send(&proxy, Event::Added { op, dir: dir_text, source, enabled, manifest, expect, result });
    });
    true
}

/// Turns extension `id` on or off.
pub fn set_enabled(app: &mut App, id: &str, on: bool) -> Op {
    let op = app.extensions.start_op();
    set_enabled_as(app, op, id, on);
    op
}

fn set_enabled_as(app: &mut App, op: Op, id: &str, on: bool) {
    if is_builtin(id) {
        return app.extensions.finish(op, Err("built-in extension".into()));
    }
    let Some(view) = content_view(app) else {
        return app.extensions.finish(op, Err("no content webview".into()));
    };
    let (proxy, owned) = (app.proxy().clone(), id.to_owned());
    native::enable_extension(view, id, on, move |result| send(&proxy, Event::Enabled { op, id: owned, on, result }));
}

/// Uninstalls extension `id` from WebView2, drops it from the record and,
/// once WebView2 confirms, deletes its folder if X-Browser owns it
/// (`owned_folder`; see `Extensions::record_removed`).
/// This is the user's removal: a removed uBOL is never installed again by
/// the first run (`firstrun::on_removed`).
pub fn remove(app: &mut App, id: &str) -> Op {
    let op = app.extensions.start_op();
    remove_as(app, op, id);
    op
}

/// `remove` for X-Browser's own reasons (op 0: an install taken back out):
/// the record and folder are handled the same, but it is not the user's
/// choice, so it does not count as removing uBOL by hand.
pub fn remove_quietly(app: &mut App, id: &str) {
    remove_as(app, 0, id);
}

fn remove_as(app: &mut App, op: Op, id: &str) {
    if is_builtin(id) {
        return app.extensions.finish(op, Err("built-in extension".into()));
    }
    let Some(view) = content_view(app) else {
        return app.extensions.finish(op, Err("no content webview".into()));
    };
    let (proxy, owned) = (app.proxy().clone(), id.to_owned());
    native::remove_extension(view, id, move |result| send(&proxy, Event::Removed { op, id: owned, result }));
}

/// A content webview was made: runs the reconcile if the UI came up while
/// there was none (the restored tab's webview could not be made).
pub fn on_view_created(app: &mut App) {
    if app.ui_ready {
        on_ui_ready(app);
    }
    crate::firstrun::on_view_created(app);
}

/// The start-up reconcile, once the UI is up and a content tab exists (else
/// `on_view_created` starts it later; a reloaded UI does not run it again).
pub fn on_ui_ready(app: &mut App) {
    if app.extensions.reconcile != Reconcile::NotYet || app.extensions.store.is_none() {
        return;
    }
    let proxy = app.proxy().clone();
    let Some(view) = content_view(app) else { return };
    native::list_extensions(view, move |r| send(&proxy, Event::Listed(r)));
    app.extensions.reconcile = Reconcile::Running;
}

/// Deletes an owned extension folder in `root`; if that fails (a file still
/// open), renames it so the next start's `sweep` finishes the job. Nothing
/// is deleted if `root` is not a plain folder (`is_plain_folder`).
pub(crate) fn delete_folder(root: &Path, path: &Path) -> Result<(), String> {
    if !is_plain_folder(root) {
        return Err(format!("{} is a link or cannot be read; folder not deleted", root.display()));
    }
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let aside = path.with_file_name(format!(".{name}.removed-{}", std::process::id()));
            match std::fs::rename(path, &aside) {
                Ok(()) => {
                    let _ = std::fs::remove_dir_all(&aside);
                    Ok(())
                }
                Err(_) => Err(format!("folder not deleted: {e}")),
            }
        }
    }
}

pub fn on_event(app: &mut App, ev: Event) {
    if let Event::Added { expect: Some(Expect::Readd(_)), .. } = &ev {
        app.extensions.readding = app.extensions.readding.saturating_sub(1);
    }
    match ev {
        Event::Listed(Err(e)) => {
            eprintln!("x-browser: cannot list extensions: {e}");
            app.extensions.reconcile = Reconcile::Done;
            crate::firstrun::on_reconcile_done(app);
        }
        Event::Listed(Ok(all)) => {
            let x = &mut app.extensions;
            x.set_live(all);
            let p = plan(&x.file.items, &x.live, |e| x.can_readd(e));
            for e in &mut x.file.items {
                if let Some(l) = x.live.iter().find(|l| l.id == e.id) {
                    e.enabled = l.enabled;
                }
            }
            if !p.drop.is_empty() {
                eprintln!("x-browser: {} extension(s) without a folder of X-Browser's were forgotten", p.drop.len());
                x.file.items.retain(|e| !p.drop.contains(&e.id));
            }
            x.save();
            let again: Vec<Entry> = x.file.items.iter().filter(|e| p.readd.contains(&e.id)).cloned().collect();
            let unknown_on: Vec<String> =
                p.unknown.into_iter().filter(|id| x.live.iter().any(|l| l.id == *id && l.enabled)).collect();
            x.reconcile = Reconcile::Done;
            for e in again {
                if add_as(app, 0, Path::new(&e.dir), e.source, e.enabled, Some(Expect::Readd(e.id))) {
                    app.extensions.readding += 1;
                }
            }
            // The Uzantılar page lists them; the user may turn them on there.
            for id in unknown_on {
                eprintln!("x-browser: extension {id} was not installed by X-Browser; turned off");
                set_enabled_as(app, 0, &id, false);
            }
            crate::firstrun::on_reconcile_done(app);
        }
        Event::Added { op, result: Err(e), .. } => app.extensions.finish(op, Err(e)),
        Event::Added { op, dir, source, enabled, manifest, expect, result: Ok(info) } => {
            if manifest.key_id.as_ref().is_some_and(|k| *k != info.id) {
                eprintln!("x-browser: extension id {} differs from its manifest key's", info.id);
            }
            let now = crate::history::now_secs();
            match app.extensions.record_added(&info, dir, source, manifest, expect.as_ref(), now) {
                Ok(()) => {
                    app.extensions.finish(op, Ok(info.id.clone()));
                    // A re-added extension that was off stays off.
                    if !enabled && info.enabled {
                        set_enabled_as(app, 0, &info.id, false);
                    }
                }
                Err(Refused { why, undo }) => {
                    if undo {
                        undo_add(app, &info.id);
                    }
                    app.extensions.finish(op, Err(why));
                }
            }
        }
        Event::Enabled { op, id, on, result } => {
            let x = &mut app.extensions;
            match result {
                Ok(true) => {
                    if let Some(e) = x.file.items.iter_mut().find(|e| e.id == id) {
                        e.enabled = on;
                    }
                    if let Some(l) = x.live.iter_mut().find(|l| l.id == id) {
                        l.enabled = on;
                    }
                    x.save();
                    x.finish(op, Ok(id));
                }
                Ok(false) => x.finish(op, Err(format!("{id} is not installed"))),
                Err(e) => x.finish(op, Err(e)),
            }
        }
        Event::Removed { op, result: Err(e), .. } => app.extensions.finish(op, Err(e)),
        Event::Removed { op, id, result: Ok(confirmed) } => {
            // Only the user's own removals (op 0 is X-Browser's).
            if op != 0 {
                crate::firstrun::on_removed(app, &id);
            }
            let x = &mut app.extensions;
            let result = x.record_removed(&id, confirmed);
            x.finish(op, result);
        }
        Event::StoreInstalled { id, result } => crate::firstrun::on_install_result(app, &id, result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_dir;

    #[test]
    fn kept_text_has_no_controls_or_bidi_overrides() {
        assert_eq!(clean_text("  uBlock\u{0007} Origin\n Lite "), "uBlock Origin Lite");
        let spoof = "\u{202E}fdp.exe\u{202C} \u{200E}a\u{200F}b \u{2066}c\u{2069} \u{202A}d\u{202B}e\u{202D}";
        assert_eq!(clean_text(spoof), "fdp.exe ab c de");
        // Other text, right-to-left scripts too, stays.
        assert_eq!(clean_text("Uzantı \u{05D0}\u{05D1} 1.0"), "Uzantı \u{05D0}\u{05D1} 1.0");
        assert_eq!(clean_text(&"x".repeat(MAX_TEXT + 5)).len(), MAX_TEXT);
    }

    const ID: &str = "mmbkafeccnpmgicaldnppdljpaogpjdm";
    const ID2: &str = "abcdefghijklmnopabcdefghijklmnop";

    fn entry(id: &str, dir: &str) -> Entry {
        Entry {
            id: id.into(),
            dir: dir.into(),
            source: Source::Unpacked,
            version: "1.0".into(),
            name: "X".into(),
            enabled: true,
            installed: 1,
            removed: false,
        }
    }

    fn info(id: &str) -> ExtensionInfo {
        ExtensionInfo { id: id.into(), name: "n".into(), enabled: true }
    }

    #[test]
    fn key_gives_the_chrome_id() {
        // The self-test extension's key (test-assets/selftest-ext).
        let key = include_str!("../test-assets/selftest-ext/manifest.json");
        let v: serde_json::Value = serde_json::from_str(key).unwrap();
        assert_eq!(id_from_key(v["key"].as_str().unwrap()).as_deref(), Some(ID));
        assert_eq!(id_from_key(""), None);
        assert_eq!(id_from_key("not base64!"), None);
    }

    #[test]
    fn base64_round_trips_and_rejects_junk() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVs\nbG8h").unwrap(), b"hell\x6f!");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert!(base64_decode("aGVsbG8=x").is_none(), "data after padding");
        assert!(base64_decode("aGVsbG9").is_none(), "non-zero trailing bits");
        assert!(base64_decode("a-b_").is_none(), "url alphabet");
    }

    #[test]
    fn a_bad_entry_loses_only_itself() {
        let dir = temp_dir("ext-lenient");
        let good = r"C:\\x\\extensions\\mmbkafeccnpmgicaldnppdljpaogpjdm\\1.0";
        let text = format!(
            r#"{{"v":1,"items":[
                5, null, "x",
                {{"id":"{ID}","dir":"{good}","source":"unpacked","version":"1.0"}},
                {{"id":"{ID2}","dir":"{good}","source":"martian"}},
                {{"id":"UPPERCASEUPPERCASEUPPERCASEUPPER","dir":"{good}","source":"cws"}},
                {{"id":"{ID2}","dir":"relative\\path","source":"cws"}},
                {{"id":"{ID2}","dir":"C:\\a\\..\\b","source":"cws"}},
                {{"id":"{ID2}","source":"cws"}},
                {{"id":"dgiklkfkllikcanfonkcabmbdfmgleag","dir":"{good}","source":"cws"}},
                {{"id":"{ID}","dir":"C:\\dup","source":"cws"}},
                {{"id":"{ID2}","dir":"C:\\y","source":"cws","enabled":false,"installed":99999999999999,"name":"a\u0007b"}}
            ]}}"#
        );
        std::fs::write(dir.join(FILE), text).unwrap();
        let loaded = Store::<ExtFile>::new(&dir).load();
        assert_eq!(loaded.source, crate::store::Source::File, "the file itself is fine");
        let items = loaded.value.items;
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!((items[0].id.as_str(), items[0].source, items[0].enabled), (ID, Source::Unpacked, true));
        assert_eq!((items[1].id.as_str(), items[1].dir.as_str(), items[1].enabled), (ID2, r"C:\y", false));
        assert!(items[1].installed <= crate::history::now_secs());
        assert_eq!(items[1].name, "ab");
        assert!(!dir.join(format!("{FILE}.corrupt")).exists());
    }

    #[test]
    fn an_unreadable_file_falls_back_to_the_backup() {
        let dir = temp_dir("ext-bak");
        let mut store = Store::<ExtFile>::new(&dir);
        let good = ExtFile { items: vec![entry(ID, r"C:\e\1.0")], ..ExtFile::default() };
        store.save_now(&good).unwrap();
        // A second run: its first save keeps the previous file as `.bak`.
        let mut store = Store::<ExtFile>::new(&dir);
        assert_eq!(store.load().value, good);
        store.save_now(&ExtFile { items: vec![entry(ID, r"C:\e\1.1")], ..ExtFile::default() }).unwrap();
        assert!(dir.join(format!("{FILE}.bak")).exists());
        std::fs::write(dir.join(FILE), b"{\"v\":1,\"items\":[{\"id\"").unwrap();
        let loaded = Store::<ExtFile>::new(&dir).load();
        assert_eq!((loaded.source, loaded.value), (crate::store::Source::Backup, good));
    }

    #[test]
    fn reconcile_plan() {
        let records = [entry(ID, r"C:\has"), entry(ID2, r"C:\gone"), entry("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", r"C:\has2")];
        let live = [info(ID), info("cccccccccccccccccccccccccccccccc"), info(BUILTIN[0]), info(BUILTIN[1])];
        let p = plan(&records, &live, |e| e.dir.starts_with(r"C:\has"));
        assert_eq!(p.keep, [ID]);
        assert_eq!(p.readd, ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]);
        assert_eq!(p.drop, [ID2]);
        assert_eq!(p.unknown, ["cccccccccccccccccccccccccccccccc"], "built-ins are never unknown");
        // A removed entry is neither loaded nor counts as recorded.
        let records = [Entry { removed: true, ..entry(ID, r"C:\has") }, Entry { removed: true, ..entry(ID2, r"C:\has") }];
        let p = plan(&records, &[info(ID)], |_| true);
        assert_eq!((p.keep.len(), p.readd.len(), p.drop.len()), (0, 0, 0));
        assert_eq!(p.unknown, [ID]);
    }

    /// An `Extensions` on a fresh data folder, with `items` recorded.
    fn opened(name: &str, items: Vec<Entry>) -> (PathBuf, Extensions) {
        let dir = temp_dir(name);
        let mut x = Extensions::open(Some(&dir));
        x.file.items = items;
        (dir, x)
    }

    #[test]
    fn readd_only_from_owned_folders_with_a_manifest() {
        let (dir, x) = opened("ext-readd", vec![]);
        let root = dir.join(DIR);
        let mine = root.join(ID).join("1.0");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::write(mine.join("manifest.json"), "{}").unwrap();
        let outside = dir.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("manifest.json"), "{}").unwrap();
        assert!(x.can_readd(&entry(ID, &mine.to_string_lossy())));
        assert!(!x.can_readd(&entry(ID, &outside.to_string_lossy())), "not X-Browser's folder");
        assert!(!x.can_readd(&entry(ID2, &mine.to_string_lossy())), "another id's folder");
        assert!(!x.can_readd(&entry(ID, &root.join(ID).join("2.0").to_string_lossy())), "no manifest");
        assert!(!x.can_readd(&entry(ID, r"\\server\share\ext")), "UNC");
    }

    #[test]
    fn removal_deletes_the_folder_only_when_confirmed() {
        let (dir, mut x) = opened("ext-remove", vec![]);
        let root = dir.join(DIR);
        let (a, b) = (root.join(ID).join("1.0"), root.join(ID2).join("1.0"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        x.file.items = vec![entry(ID, &a.to_string_lossy()), entry(ID2, &b.to_string_lossy())];
        assert_eq!(x.record_removed(ID, true), Ok(ID.to_string()));
        assert!(!root.join(ID).exists(), "confirmed: the whole id folder goes");
        assert_eq!(x.record_removed(ID2, false), Ok(ID2.to_string()));
        assert!(b.exists(), "not installed: the folder stays");
        assert!(x.records().is_empty());
        assert_eq!(x.record_removed(ID, true), Ok(ID.to_string()), "unknown id");
    }

    #[test]
    fn a_folder_left_by_a_removal_is_deleted_at_the_next_start() {
        let dir = temp_dir("ext-retry");
        let root = dir.join(DIR);
        let (a, other) = (root.join(ID).join("1.0"), dir.join("theirs"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let items = vec![
            Entry { removed: true, ..entry(ID, &a.to_string_lossy()) },
            Entry { removed: true, ..entry(ID2, &other.to_string_lossy()) },
            entry("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", r"C:\kept"),
        ];
        let mut store = Store::<ExtFile>::new(&dir);
        store.save_now(&ExtFile { items, ..ExtFile::default() }).unwrap();
        let x = Extensions::open(Some(&dir));
        assert!(!root.join(ID).exists(), "owned: deleted");
        assert!(other.exists(), "not X-Browser's: left alone");
        let ids: Vec<&str> = x.records().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]);
        assert!(x.list().iter().all(|l| l.id != ID));
    }

    fn manifest() -> Manifest {
        Manifest { version: "2.0".into(), key_id: None }
    }

    #[test]
    fn adds_match_by_id_and_are_refused_when_wrong_or_full() {
        let (_dir, mut x) = opened("ext-added", vec![entry(ID, r"C:\a")]);
        // Same folder, another id: a new entry, the old one untouched.
        x.record_added(&info(ID2), r"C:\a".into(), Source::Unpacked, manifest(), None, 50).unwrap();
        assert_eq!(x.records().len(), 2);
        assert_eq!((x.records()[0].id.as_str(), x.records()[0].version.as_str()), (ID, "1.0"));
        // Same id again: replaced in place, install time kept.
        x.record_added(&info(ID2), r"C:\b".into(), Source::Cws, manifest(), None, 60).unwrap();
        assert_eq!((x.records()[1].dir.as_str(), x.records()[1].installed), (r"C:\b", 50));
        // A re-add that came back as another id: the entry is dropped.
        let new = "cccccccccccccccccccccccccccccccc";
        let r = x.record_added(&info(new), r"C:\a".into(), Source::Unpacked, manifest(), Some(&Expect::Readd(ID.into())), 70);
        assert!(r.as_ref().is_err_and(|r| r.undo), "{r:?}");
        let ids: Vec<&str> = x.records().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, [ID2]);
        // ... but what WebView2 had already is not taken out.
        let r = x.record_added(&info(ID2), r"C:\a".into(), Source::Unpacked, manifest(), Some(&Expect::Readd(new.into())), 70);
        assert!(r.as_ref().is_err_and(|r| !r.undo), "{r:?}");
        // A store install that came back as another id: refused, and the
        // version recorded before keeps its entry and folder.
        let before = x.records().to_vec();
        let r = x.record_added(&info(new), r"C:\z".into(), Source::Cws, manifest(), Some(&Expect::Install(ID2.into())), 75);
        assert!(r.as_ref().is_err_and(|r| r.undo), "{r:?}");
        assert_eq!(x.records(), &before[..]);
        // Full: refused, nothing dropped to make room.
        x.file.items = (0..MAX_ITEMS).map(|i| entry(&format!("{i:0>32}"), r"C:\x")).collect();
        let r = x.record_added(&info(new), r"C:\n".into(), Source::Unpacked, manifest(), None, 80);
        assert!(r.as_ref().is_err_and(|r| r.undo), "{r:?}");
        assert_eq!(x.records().len(), MAX_ITEMS);
        assert!(x.records().iter().all(|e| e.id != new));
    }

    #[test]
    fn plain_folders_only() {
        let dir = temp_dir("ext-plain");
        assert!(is_plain_folder(&dir));
        assert!(is_plain_folder(&dir.join("missing")));
        let target = dir.join("target");
        std::fs::create_dir_all(target.join(ID)).unwrap();
        std::fs::create_dir_all(target.join(".x.unpack-1-0")).unwrap();
        let link = dir.join("link");
        // Needs Developer Mode or elevation; without it there is nothing to test.
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&target, &link).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&target, &link).is_ok();
        if made {
            assert!(!is_plain_folder(&link));
            assert_eq!(sweep(&link), 0);
            assert!(target.join(".x.unpack-1-0").exists(), "nothing deleted through the link");
            assert!(delete_folder(&link, &link.join(ID)).is_err());
            assert!(target.join(ID).exists());
        }
    }

    #[test]
    fn list_hides_builtins_and_marks_unknown_ones() {
        let mut x = Extensions::default();
        x.file.items = vec![entry(ID, r"C:\a"), entry(ID2, r"C:\b")];
        x.set_live(vec![
            ExtensionInfo { id: ID.into(), name: "Live\u{0}Name".into(), enabled: false },
            info(BUILTIN[0]),
            info(BUILTIN[1]),
            info("cccccccccccccccccccccccccccccccc"),
        ]);
        let l = x.list();
        let ids: Vec<&str> = l.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, [ID, ID2, "cccccccccccccccccccccccccccccccc"]);
        assert_eq!((l[0].name.as_str(), l[0].enabled, l[0].installed), ("LiveName", false, true));
        assert_eq!((l[1].enabled, l[1].installed, l[1].source), (true, false, Some(Source::Unpacked)));
        assert_eq!((l[2].source, l[2].installed), (None, true));
    }

    #[test]
    fn sweep_deletes_only_leftovers() {
        let root = temp_dir("ext-sweep");
        let keep = root.join(ID).join("1.0");
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::write(keep.join("manifest.json"), "{}").unwrap();
        for d in [
            root.join(format!(".{ID}.unpack-12-0")),
            root.join(ID).join(".1.1.unpack-12-3"),
            root.join(ID).join("1.1.tmp"),
            root.join(format!(".{ID}.removed-7")),
        ] {
            std::fs::create_dir_all(d.join("sub")).unwrap();
            std::fs::write(d.join("sub").join("f"), "x").unwrap();
        }
        std::fs::write(root.join("stray.tmp"), "x").unwrap();
        // Not ours to judge: deeper than one id folder.
        std::fs::create_dir_all(keep.join(".x.unpack-1-1")).unwrap();
        assert_eq!(sweep(&root), 5);
        let mut left: Vec<String> = walk(&root);
        left.sort();
        assert_eq!(left, [ID.to_string(), format!("{ID}/1.0"), format!("{ID}/1.0/.x.unpack-1-1"), format!("{ID}/1.0/manifest.json")]);
        assert_eq!(sweep(&root.join("missing")), 0);
    }

    fn walk(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo = vec![root.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
                if p.is_dir() {
                    todo.push(p);
                }
            }
        }
        out
    }

    #[test]
    fn leftover_names() {
        assert!(is_leftover(".abc.unpack-1-0") && is_leftover("1.0.tmp") && is_leftover(".abc.removed-9"));
        assert!(!is_leftover("1.0") && !is_leftover(ID) && !is_leftover("abc.unpack-1-0") && !is_leftover(".git"));
    }

    #[test]
    fn only_folders_under_the_root_are_owned() {
        let root = Path::new(r"C:\data\extensions");
        let mine = root.join(ID);
        assert_eq!(owned_folder(root, ID, &mine.join("1.0")).as_deref(), Some(mine.as_path()));
        assert_eq!(owned_folder(root, ID, &mine).as_deref(), Some(mine.as_path()));
        assert_eq!(owned_folder(root, ID, Path::new(r"C:\Users\me\ext")), None);
        assert_eq!(owned_folder(root, ID, &root.join(ID2).join("1.0")), None, "another id's folder");
        assert_eq!(owned_folder(root, ID, root), None);
        assert_eq!(owned_folder(root, ID, &mine.join("1.0").join("deeper")), None);
        assert_eq!(owned_folder(root, ID, Path::new(r"C:\data\extensions\x\..\mmbkafeccnpmgicaldnppdljpaogpjdm\1.0")), None);
        assert_eq!(owned_folder(root, "../..", &root.join("..").join("..")), None);
    }

    #[test]
    fn manifest_reading() {
        let dir = temp_dir("ext-manifest");
        assert!(read_manifest(&dir).is_err(), "missing");
        std::fs::write(dir.join("manifest.json"), "\u{feff}{\"version\":\"2.1\",\"key\":\"aGVsbG8=\"}").unwrap();
        let m = read_manifest(&dir).unwrap();
        assert_eq!(m.version, "2.1");
        assert_eq!(m.key_id.unwrap().len(), 32);
        std::fs::write(dir.join("manifest.json"), "{\"name\":\"x\"}").unwrap();
        assert!(read_manifest(&dir).is_err(), "no version");
        std::fs::write(dir.join("manifest.json"), "[1]").unwrap();
        assert!(read_manifest(&dir).is_err(), "not an object");
        let big = format!("{{\"version\":\"1\",\"pad\":\"{}\"}}", "x".repeat(MAX_MANIFEST_BYTES as usize));
        std::fs::write(dir.join("manifest.json"), big).unwrap();
        assert!(read_manifest(&dir).is_err(), "too big");
    }

    #[test]
    #[cfg(windows)]
    fn manifest_behind_a_link_is_not_read() {
        let dir = temp_dir("ext-manifest-link");
        let outside = dir.join("outside.json");
        std::fs::write(&outside, "{\"version\":\"9\",\"name\":\"Spoof\"}").unwrap();
        let ext = dir.join("ext");
        std::fs::create_dir_all(&ext).unwrap();
        // Creating a link needs Developer Mode or elevation; without it there
        // is nothing to test.
        if std::os::windows::fs::symlink_file(&outside, ext.join("manifest.json")).is_err() {
            return;
        }
        assert!(read_manifest(&ext).is_err());
        assert!(plain_file_metadata(&ext.join("manifest.json")).is_none());
        assert!(plain_file_metadata(&outside).is_some());
    }

    #[test]
    fn valid_dirs() {
        assert!(valid_dir(r"C:\a\b") && valid_dir(r"\\server\share\x"));
        assert!(!valid_dir("") && !valid_dir(r"a\b") && !valid_dir(r"C:\a\..\b") && !valid_dir("C:\\a\u{1}"));
    }
}
