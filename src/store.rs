//! Small versioned JSON files in the data folder (session.json, window.json;
//! later tasks add their own through the same `Stored` trait).
//!
//! - Every file is a JSON object carrying `"v": 1` next to the type's fields.
//! - Writes are atomic: a `.tmp` file is written and synced, then renamed over
//!   the real one (retried briefly). The first write of each run first copies
//!   the previous good file to `.bak`, best effort: a failed copy is logged
//!   and the save goes on.
//! - What was written is recorded only once the write succeeded, so a failed
//!   save is retried by the next one, even with the same content.
//! - A file that cannot be read (bad JSON, no or zero `"v"`, wrong shape, over
//!   the type's size limit) is renamed to `.corrupt`; then the `.bak` is
//!   tried, then the type's default. Nothing here panics.
//! - A file from a newer version (`"v"` above `VERSION`) is used as far as it
//!   parses, but never overwritten, so going back a version loses nothing.
//! - What is read back is untrusted: each type's `sanitize` bounds it.
//!
//! Writes go to one background thread (coalesced per file, identical bytes
//! skipped) so the UI thread never waits on the disk; `flush` waits for it,
//! with a time limit, before the app exits.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

/// The format version written into every file.
pub const VERSION: u64 = 1;

/// A type kept in its own file in the data folder.
pub trait Stored: Serialize + DeserializeOwned + Default {
    /// File name, e.g. `session.json`.
    const NAME: &'static str;
    /// Larger files are not read (treated as corrupt) and not written.
    const MAX_BYTES: u64;
    /// Brings a value read from disk within the type's limits.
    fn sanitize(&mut self);
}

/// The self-test's own folder (its log, pictures and profile):
/// `$XB_SELFTEST_DIR`, or `%TEMP%\taser-selftest` when that is unset,
/// as an absolute path.
#[cfg(feature = "selftest")]
pub fn selftest_dir() -> PathBuf {
    let base = std::env::var_os("XB_SELFTEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("taser-selftest"));
    std::path::absolute(&base).unwrap_or(base)
}

/// Where profile data lives: `%LOCALAPPDATA%\Taser`. Self-test builds use
/// `selftest_dir()\profile` instead (an own WebView2 profile, own files and
/// an own single-instance name), and never the real folder.
pub fn data_dir() -> Option<PathBuf> {
    #[cfg(feature = "selftest")]
    {
        Some(selftest_dir().join("profile"))
    }
    #[cfg(not(feature = "selftest"))]
    {
        // Keep profile data out of the exe folder (which may be read-only).
        std::env::var_os("LOCALAPPDATA").map(|d| profile_in(Path::new(&d)))
    }
}

/// The browser was called X-Browser until 2026-11-23. A profile made then
/// stays where it is and keeps being used: extensions.json and WebView2's
/// own profile hold absolute paths into it, so moving it would break the
/// installed extensions. Only a machine without one gets `Taser`.
#[cfg_attr(feature = "selftest", allow(dead_code))]
fn profile_in(base: &Path) -> PathBuf {
    let new = base.join("Taser");
    let old = base.join("X-Browser");
    if !new.exists() && old.is_dir() { old } else { new }
}

/// Where a loaded value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    File,
    Backup,
    Default,
}

#[derive(Debug)]
pub struct Loaded<T> {
    pub value: T,
    pub source: Source,
}

/// `path` with `suffix` appended to the whole name (`a.json` -> `a.json.bak`).
pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Just the version of a stored file (the rest is ignored here).
#[derive(serde::Deserialize)]
struct Header {
    v: Option<u64>,
}

enum Read<T> {
    Ok { value: T, newer: bool },
    Missing,
    /// Written by a newer version and not understood: left alone.
    NewerUnreadable,
    Bad(String),
}

fn read_file<T: Stored>(path: &Path) -> Read<T> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Read::Missing,
        Err(e) => return Read::Bad(e.to_string()),
    };
    if meta.len() > T::MAX_BYTES {
        return Read::Bad(format!("{} bytes, limit {}", meta.len(), T::MAX_BYTES));
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return Read::Bad(e.to_string()),
    };
    // Only an object: serde would also map an array onto a struct.
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Read::Bad("not a JSON object".into());
    }
    let version = match serde_json::from_slice::<Header>(&bytes) {
        Ok(Header { v: Some(v) }) if v >= 1 => v,
        Ok(_) => return Read::Bad("missing or invalid \"v\"".into()),
        Err(e) => return Read::Bad(e.to_string()),
    };
    let newer = version > VERSION;
    match serde_json::from_slice::<T>(&bytes) {
        Ok(mut value) => {
            value.sanitize();
            Read::Ok { value, newer }
        }
        Err(_) if newer => Read::NewerUnreadable,
        Err(e) => Read::Bad(e.to_string()),
    }
}

/// Moves an unreadable file out of the way, keeping it for inspection.
fn set_aside(path: &Path, why: &str) {
    eprintln!("taser: {} is unreadable ({why}); moved to .corrupt", path.display());
    let _ = std::fs::rename(path, sibling(path, ".corrupt"));
}

/// Attempts at the final rename (a virus scanner or indexer may hold the
/// target open for a moment).
const RENAME_TRIES: u32 = 3;

/// Replaces `to` with `from` in one step (MoveFileEx with REPLACE_EXISTING),
/// retrying briefly.
fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 1;
    loop {
        match std::fs::rename(from, to) {
            Err(_) if tries < RENAME_TRIES => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

/// Writes `bytes` to `path` atomically. With `backup`, first copies the
/// current file (if any) to `.bak`, atomically too; that copy is best
/// effort and never stops the save itself.
pub fn write_atomic(path: &Path, bytes: &[u8], backup: bool) -> std::io::Result<()> {
    fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let mut f = std::fs::File::create(path)?;
        f.write_all(bytes)?;
        f.sync_all()
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if backup && let Ok(old) = std::fs::read(path) {
        let tmp = sibling(path, ".bak.tmp");
        if let Err(e) = write_synced(&tmp, &old).and_then(|()| rename_retry(&tmp, &sibling(path, ".bak"))) {
            eprintln!("taser: cannot back up {}: {e}; saving anyway", path.display());
            let _ = std::fs::remove_file(&tmp);
        }
    }
    let tmp = sibling(path, ".tmp");
    write_synced(&tmp, bytes)?;
    rename_retry(&tmp, path)
}

/// What is on disk, and what will be once the queued writes land.
#[derive(Default)]
struct Written {
    /// Bytes of the last successful write.
    last: Option<Vec<u8>>,
    /// Bytes of the last write handed out (queued or written), to skip
    /// identical ones; back to `last` when that write fails.
    queued: Option<Vec<u8>>,
    /// Copy the file to `.bak` before the first successful write of this run
    /// (only when it was read back fine, so a good `.bak` is never replaced
    /// by junk).
    backup_pending: bool,
}

type Shared = std::sync::Arc<std::sync::Mutex<Written>>;

fn lock(state: &Shared) -> std::sync::MutexGuard<'_, Written> {
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Writes `bytes` (with the backup still pending, if any) and records it
/// only once it is on disk; a failed write is retried by the next save,
/// even with the same bytes.
fn write_tracked(path: &Path, bytes: &[u8], state: &Shared) -> std::io::Result<()> {
    let backup = lock(state).backup_pending;
    if let Err(e) = write_atomic(path, bytes, backup) {
        let mut s = lock(state);
        // Unless something newer is queued already, the next save compares
        // against what really is on disk.
        if s.queued.as_deref() == Some(bytes) {
            s.queued = s.last.clone();
        }
        return Err(e);
    }
    let mut s = lock(state);
    s.last = Some(bytes.to_vec());
    s.backup_pending = false;
    Ok(())
}

/// One stored file.
pub struct Store<T> {
    path: PathBuf,
    /// The file is from a newer version: never overwrite it.
    read_only: bool,
    /// Shared with the writer thread.
    written: Shared,
    _type: PhantomData<fn() -> T>,
}

#[derive(Serialize)]
struct Versioned<'a, T> {
    v: u64,
    #[serde(flatten)]
    data: &'a T,
}

impl<T: Stored> Store<T> {
    pub fn new(dir: &Path) -> Self {
        Self { path: dir.join(T::NAME), read_only: false, written: Shared::default(), _type: PhantomData }
    }

    #[cfg_attr(not(feature = "selftest"), allow(dead_code))]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the file, falling back to `.bak`, then to the default.
    pub fn load(&mut self) -> Loaded<T> {
        match read_file::<T>(&self.path) {
            Read::Ok { value, newer } => {
                self.read_only = newer;
                lock(&self.written).backup_pending = !newer;
                return Loaded { value, source: Source::File };
            }
            Read::NewerUnreadable => {
                self.read_only = true;
                return Loaded { value: T::default(), source: Source::Default };
            }
            Read::Bad(why) => set_aside(&self.path, &why),
            Read::Missing => {}
        }
        let bak = sibling(&self.path, ".bak");
        match read_file::<T>(&bak) {
            Read::Ok { value, newer } => {
                self.read_only = newer;
                Loaded { value, source: Source::Backup }
            }
            Read::NewerUnreadable => {
                self.read_only = true;
                Loaded { value: T::default(), source: Source::Default }
            }
            Read::Bad(why) => {
                set_aside(&bak, &why);
                Loaded { value: T::default(), source: Source::Default }
            }
            Read::Missing => Loaded { value: T::default(), source: Source::Default },
        }
    }

    /// The bytes to write for `value`, or None when nothing should be
    /// written (read-only, unchanged, too large, not serializable).
    fn encode(&self, value: &T) -> Option<Vec<u8>> {
        if self.read_only {
            return None;
        }
        let bytes = match serde_json::to_vec(&Versioned { v: VERSION, data: value }) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("taser: cannot encode {}: {e}", T::NAME);
                return None;
            }
        };
        if bytes.len() as u64 > T::MAX_BYTES {
            eprintln!("taser: {} would be {} bytes; not written", T::NAME, bytes.len());
            return None;
        }
        // Against the last queued bytes, not the last written: reverting to
        // what is on disk while another value is still queued must be written.
        let mut s = lock(&self.written);
        if s.queued.as_ref() == Some(&bytes) {
            return None;
        }
        s.queued = Some(bytes.clone());
        Some(bytes)
    }

    /// Saves `value` in the background (see `flush`).
    pub fn save(&mut self, value: &T) {
        let Some(bytes) = self.encode(value) else { return };
        let (path, state) = (self.path.clone(), self.written.clone());
        match writer() {
            Some(tx) => {
                if let Err(mpsc::SendError(Msg::Write { path, bytes, state })) =
                    tx.send(Msg::Write { path, bytes, state })
                {
                    write_or_log(&path, &bytes, &state);
                }
            }
            None => write_or_log(&path, &bytes, &state),
        }
    }

    /// Saves `value` right away on this thread.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn save_now(&mut self, value: &T) -> std::io::Result<()> {
        match self.encode(value) {
            Some(bytes) => write_tracked(&self.path, &bytes, &self.written),
            None => Ok(()),
        }
    }
}

fn write_or_log(path: &Path, bytes: &[u8], state: &Shared) {
    if let Err(e) = write_tracked(path, bytes, state) {
        eprintln!("taser: cannot write {}: {e}", path.display());
    }
}

enum Msg {
    Write { path: PathBuf, bytes: Vec<u8>, state: Shared },
    Flush(mpsc::Sender<()>),
}

/// The background writer, started on first use (None if no thread could be
/// started; then writes happen on the caller's thread).
fn writer() -> Option<&'static mpsc::Sender<Msg>> {
    static TX: OnceLock<Option<mpsc::Sender<Msg>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("xb-store".into()).spawn(move || run_writer(rx)).ok().map(|_| tx)
    })
    .as_ref()
}

fn run_writer(rx: mpsc::Receiver<Msg>) {
    while let Ok(first) = rx.recv() {
        // Everything queued meanwhile: only the newest bytes per file count.
        let mut writes: Vec<(PathBuf, Vec<u8>, Shared)> = Vec::new();
        let mut flushes = Vec::new();
        for msg in std::iter::once(first).chain(rx.try_iter()) {
            match msg {
                Msg::Write { path, bytes, state } => match writes.iter_mut().find(|w| w.0 == path) {
                    Some(w) => *w = (path, bytes, state),
                    None => writes.push((path, bytes, state)),
                },
                Msg::Flush(done) => flushes.push(done),
            }
        }
        for (path, bytes, state) in writes {
            write_or_log(&path, &bytes, &state);
        }
        for done in flushes {
            let _ = done.send(());
        }
    }
}

/// Waits (at most `timeout`) until every save queued so far is on disk.
/// False on timeout.
pub fn flush(timeout: Duration) -> bool {
    let Some(tx) = writer() else { return true };
    let (done_tx, done_rx) = mpsc::channel();
    if tx.send(Msg::Flush(done_tx)).is_err() {
        return false;
    }
    done_rx.recv_timeout(timeout).is_ok()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde::Deserialize;

    /// A fresh, empty folder under the system temp dir.
    pub fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xb-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn profile_keeps_an_old_x_browser_folder() {
        let base = temp_dir("profile-name");
        assert_eq!(profile_in(&base), base.join("Taser"));
        std::fs::create_dir(base.join("X-Browser")).unwrap();
        assert_eq!(profile_in(&base), base.join("X-Browser"));
        std::fs::create_dir(base.join("Taser")).unwrap();
        assert_eq!(profile_in(&base), base.join("Taser"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
    #[serde(default)]
    struct Demo {
        items: Vec<String>,
    }

    impl Stored for Demo {
        const NAME: &'static str = "demo.json";
        const MAX_BYTES: u64 = 256;
        fn sanitize(&mut self) {
            self.items.retain(|s| s.len() <= 8);
            self.items.truncate(3);
        }
    }

    fn demo(items: &[&str]) -> Demo {
        Demo { items: items.iter().map(|s| s.to_string()).collect() }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn saves_atomically_with_version_and_backup() {
        let dir = temp_dir("atomic");
        let mut s = Store::<Demo>::new(&dir);
        assert_eq!(s.load().source, Source::Default);
        s.save_now(&demo(&["a"])).unwrap();
        assert_eq!(read(&dir.join("demo.json")), r#"{"v":1,"items":["a"]}"#);
        assert!(!dir.join("demo.json.tmp").exists());
        // No good file was read this run: no .bak yet.
        assert!(!dir.join("demo.json.bak").exists());

        // Next run: the first write keeps the previous good file as .bak,
        // later writes of that run leave the .bak alone.
        let mut s = Store::<Demo>::new(&dir);
        let l = s.load();
        assert_eq!((l.source, l.value), (Source::File, demo(&["a"])));
        s.save_now(&demo(&["b"])).unwrap();
        s.save_now(&demo(&["c"])).unwrap();
        assert_eq!(read(&dir.join("demo.json")), r#"{"v":1,"items":["c"]}"#);
        assert_eq!(read(&dir.join("demo.json.bak")), r#"{"v":1,"items":["a"]}"#);
        assert!(!dir.join("demo.json.tmp").exists() && !dir.join("demo.json.bak.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identical_saves_are_skipped() {
        let dir = temp_dir("same");
        let mut s = Store::<Demo>::new(&dir);
        s.save_now(&demo(&["a"])).unwrap();
        std::fs::remove_file(dir.join("demo.json")).unwrap();
        s.save_now(&demo(&["a"])).unwrap();
        assert!(!dir.join("demo.json").exists(), "same bytes are not written twice");
        s.save_now(&demo(&["b"])).unwrap();
        assert!(dir.join("demo.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_moves_aside_then_backup_then_default() {
        let dir = temp_dir("corrupt");
        let file = dir.join("demo.json");
        std::fs::write(&file, b"{not json").unwrap();
        std::fs::write(dir.join("demo.json.bak"), br#"{"v":1,"items":["old"]}"#).unwrap();
        let mut s = Store::<Demo>::new(&dir);
        let l = s.load();
        assert_eq!((l.source, l.value), (Source::Backup, demo(&["old"])));
        assert!(!file.exists());
        assert_eq!(read(&dir.join("demo.json.corrupt")), "{not json");
        // Loaded from .bak: the first save must not replace the good .bak.
        s.save_now(&demo(&["new"])).unwrap();
        assert_eq!(read(&dir.join("demo.json.bak")), r#"{"v":1,"items":["old"]}"#);

        // Both bad: default, both set aside.
        std::fs::write(&file, b"[]").unwrap();
        std::fs::write(dir.join("demo.json.bak"), b"").unwrap();
        let l = Store::<Demo>::new(&dir).load();
        assert_eq!((l.source, l.value), (Source::Default, Demo::default()));
        assert!(dir.join("demo.json.bak.corrupt").exists());
        assert!(!file.exists() && !dir.join("demo.json.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversize_and_wrongly_shaped_files_are_corrupt() {
        let dir = temp_dir("shape");
        let file = dir.join("demo.json");
        let big = format!(r#"{{"v":1,"items":["{}"]}}"#, "x".repeat(300));
        for bad in [big.as_str(), r#"{"v":1,"items":"nope"}"#, r#"{"v":1,"items":[1]}"#, "null"] {
            std::fs::write(&file, bad).unwrap();
            let l = Store::<Demo>::new(&dir).load();
            assert_eq!(l.source, Source::Default, "{bad}");
            assert!(!file.exists() && dir.join("demo.json.corrupt").exists(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_must_be_present_and_newer_files_are_kept() {
        let dir = temp_dir("version");
        let file = dir.join("demo.json");
        for bad in [r#"{"items":["a"]}"#, r#"{"v":0,"items":["a"]}"#, r#"{"v":"1","items":["a"]}"#] {
            std::fs::write(&file, bad).unwrap();
            assert_eq!(Store::<Demo>::new(&dir).load().source, Source::Default, "{bad}");
            assert!(!file.exists(), "{bad}");
        }
        std::fs::remove_file(dir.join("demo.json.corrupt")).unwrap();
        // A newer file is read (unknown fields ignored) but never rewritten.
        let newer = r#"{"v":2,"items":["a"],"extra":true}"#;
        std::fs::write(&file, newer).unwrap();
        let mut s = Store::<Demo>::new(&dir);
        let l = s.load();
        assert_eq!((l.source, l.value), (Source::File, demo(&["a"])));
        s.save_now(&demo(&["b"])).unwrap();
        assert_eq!(read(&file), newer);
        // ... also when it does not parse as this version's shape.
        let newer = r#"{"v":3,"items":{"changed":1}}"#;
        std::fs::write(&file, newer).unwrap();
        let mut s = Store::<Demo>::new(&dir);
        assert_eq!(s.load().source, Source::Default);
        s.save_now(&demo(&["b"])).unwrap();
        assert_eq!(read(&file), newer);
        assert!(!dir.join("demo.json.corrupt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_back_data_is_sanitized_and_oversize_writes_refused() {
        let dir = temp_dir("sanitize");
        std::fs::write(dir.join("demo.json"), r#"{"v":1,"items":["ok","much-too-long","b","c","d"]}"#).unwrap();
        let mut s = Store::<Demo>::new(&dir);
        assert_eq!(s.load().value, demo(&["ok", "b", "c"]));
        let huge = Demo { items: vec!["y".repeat(300)] };
        s.save_now(&huge).unwrap();
        assert!(read(&dir.join("demo.json")).contains("much-too-long"), "over MAX_BYTES: not written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_save_is_retried_by_an_identical_one() {
        let dir = temp_dir("retry");
        let file = dir.join("demo.json");
        // A folder where the temp file goes makes the write fail.
        std::fs::create_dir(dir.join("demo.json.tmp")).unwrap();
        let mut s = Store::<Demo>::new(&dir);
        assert!(s.save_now(&demo(&["a"])).is_err());
        assert!(!file.exists());
        std::fs::remove_dir(dir.join("demo.json.tmp")).unwrap();
        s.save_now(&demo(&["a"])).unwrap();
        assert_eq!(read(&file), r#"{"v":1,"items":["a"]}"#, "same bytes, but the first try never landed");

        // The same through the background writer.
        std::fs::create_dir(dir.join("demo.json.tmp")).unwrap();
        s.save(&demo(&["b"]));
        assert!(flush(Duration::from_secs(5)));
        assert_eq!(read(&file), r#"{"v":1,"items":["a"]}"#);
        std::fs::remove_dir(dir.join("demo.json.tmp")).unwrap();
        s.save(&demo(&["b"]));
        assert!(flush(Duration::from_secs(5)));
        assert_eq!(read(&file), r#"{"v":1,"items":["b"]}"#);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_backup_stays_pending_until_a_save_lands() {
        let dir = temp_dir("pending");
        std::fs::write(dir.join("demo.json"), r#"{"v":1,"items":["old"]}"#).unwrap();
        let mut s = Store::<Demo>::new(&dir);
        assert_eq!(s.load().source, Source::File);
        std::fs::create_dir(dir.join("demo.json.tmp")).unwrap();
        assert!(s.save_now(&demo(&["new"])).is_err());
        std::fs::remove_file(dir.join("demo.json.bak")).unwrap();
        std::fs::remove_dir(dir.join("demo.json.tmp")).unwrap();
        s.save_now(&demo(&["new"])).unwrap();
        assert_eq!(read(&dir.join("demo.json.bak")), r#"{"v":1,"items":["old"]}"#);
        assert_eq!(read(&dir.join("demo.json")), r#"{"v":1,"items":["new"]}"#);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_backup_never_stops_the_save() {
        let dir = temp_dir("bakfail");
        std::fs::write(dir.join("demo.json"), r#"{"v":1,"items":["old"]}"#).unwrap();
        let mut s = Store::<Demo>::new(&dir);
        assert_eq!(s.load().source, Source::File);
        std::fs::create_dir(dir.join("demo.json.bak.tmp")).unwrap();
        s.save_now(&demo(&["new"])).unwrap();
        assert_eq!(read(&dir.join("demo.json")), r#"{"v":1,"items":["new"]}"#);
        assert!(!dir.join("demo.json.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reverting_while_a_change_is_queued_is_written() {
        let dir = temp_dir("revert");
        let mut store = Store::<Demo>::new(&dir);
        let x = Demo { items: vec!["x".into()] };
        let a = Demo { items: vec!["a".into()] };
        store.save_now(&x).unwrap();
        // A is handed to the writer but has not landed yet.
        let queued_a = store.encode(&a).expect("A differs from X");
        // Back to X: not on disk once A lands, so it must be written too.
        let back_to_x = store.encode(&x).expect("X differs from the queued A");
        write_tracked(&store.path, &queued_a, &store.written).unwrap();
        write_tracked(&store.path, &back_to_x, &store.written).unwrap();
        assert_eq!(store.load().value.items, ["x"]);
        assert!(store.encode(&x).is_none(), "X again: already queued and written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arrays_are_not_objects() {
        let dir = temp_dir("array");
        std::fs::write(dir.join("demo.json"), r#"[1,["a"]]"#).unwrap();
        assert_eq!(Store::<Demo>::new(&dir).load().source, Source::Default);
        assert!(dir.join("demo.json.corrupt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn background_saves_land_after_flush() {
        let dir = temp_dir("background");
        let mut s = Store::<Demo>::new(&dir);
        for i in 0..20 {
            s.save(&demo(&[&i.to_string()]));
        }
        assert!(flush(Duration::from_secs(5)));
        assert_eq!(read(&dir.join("demo.json")), r#"{"v":1,"items":["19"]}"#);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
