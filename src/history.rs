//! Browsing history in `history.jsonl` and the address-bar suggestions drawn
//! from it.
//!
//! - One `{"u","t","ts"}` object per line (address, title, unix seconds),
//!   appended in whole lines; `"k"` marks a visit the user did not start
//!   (1: it does not count toward frequency) or a later title (2). Only
//!   http(s) addresses within 8 KiB, never the UI origin, without user name,
//!   password or `#fragment`; titles are cut to 512 characters.
//! - What is read back is untrusted: every line is checked again, bad lines
//!   and a torn last line are skipped, and only the last 16 MiB are read.
//!   A file that needed any of that is rewritten (atomically), so later
//!   appends never glue onto a torn line.
//! - At most 50,000 lines: more are compacted to the newest 40,000 (and at
//!   most 12 MiB), at load and at run time.
//! - New visits are buffered and appended every 2 s and at exit; deleting
//!   and clearing rewrite the file atomically. All file work happens on one
//!   background thread, started after startup (`preload`). Nothing on the UI
//!   thread waits for the load except the exit flush: until it is merged,
//!   suggestions and the list show what this run recorded, and deletions
//!   made meanwhile are applied to what the load brings (`merge`).
//! - Suggestions rank every source (history now, bookmarks later) through
//!   `rank`, by match kind, frequency and recency.

use crate::store;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const FILE: &str = "history.jsonl";
/// Most lines kept; more are compacted to `COMPACT_TO`.
pub const MAX_ENTRIES: usize = 50_000;
pub const COMPACT_TO: usize = 40_000;
/// Only this much of the file's end is read; a larger file is compacted.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// A compacted file stays below this, whatever the entries' sizes.
const COMPACT_BYTES: usize = 12 * 1024 * 1024;
pub const MAX_TITLE: usize = 512;
const FLUSH_EVERY: Duration = Duration::from_secs(2);
/// Suggestions shown under the address bar.
pub const SUGGESTIONS: usize = 8;
/// Rows the history list shows at once.
pub const LIST_MAX: usize = 200;
/// Titles shorter queries are not matched against (one or two letters
/// match nearly every title).
const TITLE_QUERY_MIN: usize = 3;
/// A title that changes this long after the visit is not written (pages
/// that keep retitling themselves would grow the file); it still shows
/// until the next start.
const RETITLE_WINDOW: u64 = 30;
/// The same address again within this many seconds (an in-page `#anchor`,
/// a page reloading itself) is the same visit. The window runs from the
/// latest repeat, so a page reloading itself every few seconds stays one
/// visit however long it runs.
const REPEAT_WINDOW: u64 = 10;

/// Kinds of line (`Visit::k`).
pub const VISIT_USER: u8 = 0;
pub const VISIT_AUTO: u8 = 1;
pub const VISIT_TITLE: u8 = 2;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Visit {
    pub u: String,
    #[serde(default)]
    pub t: String,
    pub ts: u64,
    /// `VISIT_USER` (omitted), `VISIT_AUTO` or `VISIT_TITLE`.
    #[serde(default, skip_serializing_if = "is_user")]
    pub k: u8,
}

fn is_user(k: &u8) -> bool {
    *k == VISIT_USER
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A title as kept: no control characters, at most `MAX_TITLE` characters.
pub fn clean_title(title: &str) -> String {
    title.chars().filter(|c| !c.is_control()).take(MAX_TITLE).collect::<String>().trim().to_string()
}

/// Whether an address may be kept in history.
pub fn recordable(url: &str) -> bool {
    crate::url::is_tab_url(url) && !crate::ui::is_ui_url(url) && !url.chars().any(char::is_control)
}

/// An address as kept: without `#fragment` (a jump within the page is the
/// same page) and without `user:password@` (never written to disk).
pub fn normalize(url: &str) -> String {
    without_userinfo(url.split('#').next().unwrap_or(url))
}

/// An address without `user:password@` (never written to disk) and without
/// an empty trailing `#`; any other `#fragment` stays (bookmarks keep it).
pub fn without_userinfo(url: &str) -> String {
    let url = url.strip_suffix('#').unwrap_or(url);
    if let Some(i) = url.find("://") {
        let (scheme, rest) = url.split_at(i + 3);
        let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
        if let Some(at) = authority.rfind('@') {
            return format!("{scheme}{}", &rest[at + 1..]);
        }
    }
    url.to_string()
}

/// A visit within the limits, or None if it must not be kept.
fn clean(v: Visit, now: u64) -> Option<Visit> {
    let u = normalize(&v.u);
    (v.k <= VISIT_TITLE && recordable(&u)).then(|| Visit { t: clean_title(&v.t), ts: v.ts.min(now), u, k: v.k })
}

/// Lowercased address without scheme and `www.`: what typed text matches.
pub fn url_key(url: &str) -> String {
    let lower = url.to_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")).unwrap_or(&lower);
    rest.strip_prefix("www.").unwrap_or(rest).to_string()
}

// ---------------------------------------------------------------- ranking

/// One thing a source can suggest. Address and title live as long as the
/// source (`'s`); the match keys only for the call (`'k`), so a source may
/// make them on the fly.
pub struct Candidate<'s, 'k> {
    pub url: &'s str,
    pub title: &'s str,
    /// `url_key(url)`.
    pub key: &'k str,
    /// The title, lowercased.
    pub tkey: &'k str,
    /// Last visit (unix seconds; 0 if never).
    pub last: u64,
    /// Visits the user started.
    pub count: u32,
    /// Source weight (history 1.0).
    pub weight: f32,
}

/// Something suggestions are drawn from (history; bookmarks in task 6).
pub trait SuggestSource {
    fn each<'s>(&'s self, f: &mut dyn FnMut(Candidate<'s, '_>));
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub u: String,
    pub t: String,
}

/// Match score of a candidate for a query key, 0 if it does not match.
/// Titles only count for queries of `TITLE_QUERY_MIN` characters or more.
fn match_score(q: &str, titles: bool, c: &Candidate<'_, '_>) -> f32 {
    if c.key.starts_with(q) {
        // Shorter remainders first: "exa" ranks example.com above example.com/a/b.
        let rest = (c.key.len() - q.len()) as f32;
        return 3.0 * (1.0 + 0.5 / (1.0 + rest / 16.0));
    }
    let mut title_sub = false;
    if titles {
        for (i, _) in c.tkey.match_indices(q) {
            if c.tkey[..i].chars().next_back().is_none_or(|p| !p.is_alphanumeric()) {
                return 2.0;
            }
            title_sub = true;
        }
    }
    if c.key.contains(q) {
        1.0
    } else if title_sub {
        0.8
    } else {
        0.0
    }
}

/// The best `limit` candidates of all sources for typed `text`, each
/// address once.
pub fn rank(text: &str, sources: &[&dyn SuggestSource], now: u64, limit: usize) -> Vec<Suggestion> {
    let q = url_key(text.trim());
    if q.is_empty() || limit == 0 {
        return Vec::new();
    }
    let titles = q.chars().count() >= TITLE_QUERY_MIN;
    let mut hits: Vec<(f32, &str, &str)> = Vec::new();
    for source in sources {
        source.each(&mut |c| {
            let m = match_score(&q, titles, &c);
            if m <= 0.0 {
                return;
            }
            let age_days = now.saturating_sub(c.last) as f32 / 86_400.0;
            let freq = 1.0 + (1.0 + c.count as f32).ln();
            let recency = 0.3 + 1.0 / (1.0 + age_days / 7.0);
            hits.push((m * freq * recency * c.weight, c.url, c.title));
        });
    }
    // Enough of the best that `limit` remain once an address found by
    // several sources is counted once.
    let keep = limit.saturating_mul(sources.len().max(1));
    let by_score = |a: &(f32, &str, &str), b: &(f32, &str, &str)| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1));
    if hits.len() > keep {
        hits.select_nth_unstable_by(keep - 1, by_score);
        hits.truncate(keep);
    }
    hits.sort_unstable_by(by_score);
    let mut out: Vec<Suggestion> = Vec::with_capacity(limit);
    for (_, url, title) in hits {
        if let Some(s) = out.iter_mut().find(|s| s.u == url) {
            if s.t.is_empty() {
                s.t = title.to_string();
            }
        } else if out.len() < limit {
            out.push(Suggestion { u: url.to_string(), t: title.to_string() });
        }
    }
    out
}

/// A web address's host (with its port), `www.` dropped: what counts as
/// one site.
pub(crate) fn site_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    host.strip_prefix("www.").unwrap_or(host)
}

// ---------------------------------------------------------------- index

/// One address with its latest title and visit statistics.
#[derive(Clone, Debug)]
struct Page {
    url: String,
    title: String,
    key: String,
    tkey: String,
    last: u64,
    /// Visits the user started (address bar, a picked suggestion, a click).
    count: u32,
}

#[derive(Default)]
struct Index {
    pages: Vec<Page>,
    map: HashMap<String, usize>,
}

impl Index {
    fn build(visits: &[Visit]) -> Self {
        let mut index = Index::default();
        for v in visits {
            index.add(v);
        }
        index
    }

    fn add(&mut self, v: &Visit) {
        let counted = u32::from(v.k == VISIT_USER);
        match self.map.get(&v.u) {
            Some(&i) => {
                let p = &mut self.pages[i];
                if v.k != VISIT_TITLE {
                    p.count = p.count.saturating_add(counted);
                    p.last = p.last.max(v.ts);
                }
                if !v.t.is_empty() {
                    p.title.clone_from(&v.t);
                    p.tkey = v.t.to_lowercase();
                }
            }
            // A title for an address no longer kept (compacted away).
            None if v.k == VISIT_TITLE => {}
            None => {
                self.map.insert(v.u.clone(), self.pages.len());
                self.pages.push(Page {
                    url: v.u.clone(),
                    title: v.t.clone(),
                    key: url_key(&v.u),
                    tkey: v.t.to_lowercase(),
                    last: v.ts,
                    count: counted,
                });
            }
        }
    }
}

impl SuggestSource for Index {
    fn each<'s>(&'s self, f: &mut dyn FnMut(Candidate<'s, '_>)) {
        for p in &self.pages {
            f(Candidate {
                url: &p.url,
                title: &p.title,
                key: &p.key,
                tkey: &p.tkey,
                last: p.last,
                count: p.count,
                weight: 1.0,
            });
        }
    }
}

// ---------------------------------------------------------------- file

/// One line of the file.
fn line(v: &Visit) -> Vec<u8> {
    let mut out = serde_json::to_vec(v).unwrap_or_default();
    out.push(b'\n');
    out
}

fn encode(visits: &[Visit]) -> Vec<u8> {
    visits.iter().flat_map(line).collect()
}

/// The newest visits, at most `count` of them and `max_bytes` encoded.
fn compact(visits: &[Visit], count: usize, max_bytes: usize) -> Vec<Visit> {
    let mut bytes = 0;
    let mut start = visits.len();
    while start > 0 && visits.len() - start < count {
        let len = line(&visits[start - 1]).len();
        if bytes + len > max_bytes {
            break;
        }
        bytes += len;
        start -= 1;
    }
    visits[start..].to_vec()
}

/// One untrusted line, or None if it is not a visit we may keep.
fn parse_line(raw: &[u8], now: u64) -> Option<Visit> {
    if raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return None;
    }
    clean(serde_json::from_slice::<Visit>(raw).ok()?, now)
}

/// The file's last `limit` bytes; true if that is not the whole file.
fn read_tail(path: &Path, limit: u64) -> std::io::Result<(Vec<u8>, bool)> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let truncated = len > limit;
    if truncated {
        f.seek(SeekFrom::Start(len - limit))?;
    }
    let mut bytes = Vec::with_capacity(len.min(limit) as usize);
    f.take(limit).read_to_end(&mut bytes)?;
    Ok((bytes, truncated))
}

/// What a file holds, after the checks.
#[derive(Default)]
struct Parsed {
    visits: Vec<Visit>,
    /// Lines that were skipped (bad, empty, torn, or cut off by the limit).
    skipped: usize,
    /// More than kept: the file must be rewritten.
    dirty: bool,
}

fn parse(bytes: &[u8], truncated: bool, now: u64) -> Parsed {
    let mut p = Parsed { dirty: truncated, ..Parsed::default() };
    let mut body = bytes;
    if truncated {
        // The first line was cut by the read limit.
        match body.iter().position(|&b| b == b'\n') {
            Some(i) => body = &body[i + 1..],
            None => body = &[],
        }
        p.skipped += 1;
    }
    // After the last '\n' comes either nothing or a torn line.
    let end = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    if end < body.len() {
        p.skipped += 1;
        p.dirty = true;
    }
    // Only the newest MAX_ENTRIES are held while reading, whatever the
    // number of lines (a file of bare newlines costs no memory).
    let mut kept: VecDeque<Visit> = VecDeque::new();
    let mut over = false;
    if end > 0 {
        for raw in body[..end - 1].split(|&b| b == b'\n') {
            let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
            match parse_line(raw, now) {
                Some(v) => {
                    if kept.len() == MAX_ENTRIES {
                        kept.pop_front();
                        over = true;
                    }
                    kept.push_back(v);
                }
                None => {
                    p.skipped += 1;
                    p.dirty = true;
                }
            }
        }
    }
    p.visits = kept.into();
    // Over the cap, or a file past the read limit: down to the compact size.
    if over || truncated {
        p.visits = compact(&p.visits, COMPACT_TO, COMPACT_BYTES);
        p.dirty = true;
    }
    p
}

/// Loaded from disk, ready to merge on the UI thread.
#[derive(Default)]
struct Loaded {
    visits: Vec<Visit>,
    bytes: u64,
    skipped: usize,
}

/// Reads and checks the file, rewriting it if it held anything not kept.
fn load(path: &Path, now: u64) -> Loaded {
    let (bytes, truncated) = match read_tail(path, MAX_FILE_BYTES) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::default(),
        Err(e) => {
            eprintln!("x-browser: cannot read {}: {e}", path.display());
            return Loaded::default();
        }
    };
    let parsed = parse(&bytes, truncated, now);
    let mut size = bytes.len() as u64;
    if parsed.dirty {
        let out = encode(&parsed.visits);
        size = out.len() as u64;
        if parsed.skipped > 0 {
            eprintln!("x-browser: {}: skipped {} unreadable line(s)", path.display(), parsed.skipped);
        }
        if let Err(e) = store::write_atomic(path, &out, false) {
            eprintln!("x-browser: cannot rewrite {}: {e}", path.display());
        }
    }
    Loaded { visits: parsed.visits, bytes: size, skipped: parsed.skipped }
}

enum Io {
    Append(Vec<u8>),
    Rewrite(Vec<u8>),
    Flush(mpsc::Sender<()>),
}

/// Does one piece of file work; a failed write sets `failed`, so the next
/// write replaces the whole file instead of appending to a torn one.
fn run_io(path: &Path, io: Io, failed: &AtomicBool) {
    let result = match io {
        Io::Append(bytes) => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(&bytes)),
        Io::Rewrite(bytes) => store::write_atomic(path, &bytes, false),
        Io::Flush(done) => {
            let _ = done.send(());
            Ok(())
        }
    };
    if let Err(e) = result {
        failed.store(true, Ordering::Relaxed);
        eprintln!("x-browser: cannot write {}: {e}", path.display());
    }
}

// ---------------------------------------------------------------- history

/// A row of the history list.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub u: String,
    pub t: String,
    pub ts: u64,
}

type Waker = Box<dyn Fn() + Send>;

#[derive(Default)]
pub struct History {
    /// None: kept in memory only (no data folder).
    path: Option<PathBuf>,
    /// The file thread; None before `preload`, or if no thread could start
    /// (then file work happens inline).
    tx: Option<mpsc::Sender<Io>>,
    loading: Option<mpsc::Receiver<Loaded>>,
    /// Called (on the file thread) once the load is ready to `poll`.
    waker: Option<Waker>,
    started: bool,
    loaded: bool,
    /// Set by `merge`, taken by `take_merged`.
    merged: bool,
    /// Bumped by every delete and clear.
    generation: u64,
    /// `generation` when the load started.
    load_generation: u64,
    /// `generation` of the last clear.
    cleared: Option<u64>,
    /// Addresses deleted before the load was merged, with their generation.
    deleted: Vec<(u64, String)>,
    /// A write failed: the next one rewrites the whole file.
    write_failed: Arc<AtomicBool>,
    /// Oldest first; `visits[pending..]` are not in the file yet.
    visits: Vec<Visit>,
    pending: usize,
    index: Index,
    /// The address last recorded and when it was last seen (repeats
    /// included): where the repeat window runs from.
    chain: Option<(String, u64)>,
    /// Bytes in the file (as far as this run wrote or read).
    file_bytes: u64,
    /// When the buffered visits are due to be appended.
    pub due: Option<Instant>,
    /// Lines skipped by the last load.
    pub skipped: usize,
}

impl History {
    /// History kept in `dir` (nothing is read until `preload`).
    pub fn new(dir: Option<&Path>) -> Self {
        Self { path: dir.map(|d| d.join(FILE)), ..Self::default() }
    }

    /// Called from the file thread once the load can be `poll`ed (set it
    /// before `preload`).
    pub fn on_loaded(&mut self, waker: impl Fn() + Send + 'static) {
        self.waker = Some(Box::new(waker));
    }

    #[cfg(feature = "selftest")]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether the file has been read (or there is none).
    #[cfg(test)]
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Starts loading the file in the background (once).
    pub fn preload(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        self.load_generation = self.generation;
        let Some(path) = self.path.clone() else {
            self.loaded = true;
            return;
        };
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel::<Io>();
        let thread_path = path.clone();
        let waker = self.waker.take();
        let failed = self.write_failed.clone();
        let spawned = std::thread::Builder::new().name("xb-history".into()).spawn(move || {
            let _ = loaded_tx.send(load(&thread_path, now_secs()));
            if let Some(wake) = waker {
                wake();
            }
            while let Ok(io) = rx.recv() {
                run_io(&thread_path, io, &failed);
            }
        });
        match spawned {
            Ok(_) => {
                self.tx = Some(tx);
                self.loading = Some(loaded_rx);
            }
            Err(e) => {
                eprintln!("x-browser: no history thread ({e}); working inline");
                let loaded = load(&path, now_secs());
                self.merge(loaded);
            }
        }
    }

    /// Takes the loaded file if it is ready (never waits).
    pub fn poll(&mut self) {
        if let Some(rx) = &self.loading {
            match rx.try_recv() {
                Ok(loaded) => {
                    self.loading = None;
                    self.merge(loaded);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    // The thread died loading: what this run has is all there is.
                    self.loading = None;
                    self.merge(Loaded::default());
                }
            }
        }
    }

    /// True once after the load was merged (the list or suggestions shown
    /// meanwhile are out of date).
    pub fn take_merged(&mut self) -> bool {
        std::mem::take(&mut self.merged)
    }

    /// Waits at most `timeout` for the load (the exit flush, and tests).
    pub fn wait_loaded(&mut self, timeout: Duration) -> bool {
        self.preload();
        if let Some(rx) = &self.loading {
            match rx.recv_timeout(timeout) {
                Ok(loaded) => {
                    self.loading = None;
                    self.merge(loaded);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.loading = None;
                    self.merge(Loaded::default());
                }
            }
        }
        self.loaded
    }

    /// Puts the file's visits before the ones this run recorded meanwhile,
    /// minus what was cleared or deleted since the load started.
    fn merge(&mut self, loaded: Loaded) {
        self.loaded = true;
        self.merged = true;
        self.skipped = loaded.skipped;
        let since = self.load_generation;
        if self.cleared.is_some_and(|g| g > since) {
            // Cleared meanwhile: that rewrite already replaced the file
            // (queued behind the load); the old visits stay gone.
            self.deleted.clear();
            return;
        }
        let deleted: Vec<String> = self.deleted.drain(..).filter(|(g, _)| *g > since).map(|(_, u)| u).collect();
        let mut old = loaded.visits;
        let before = old.len();
        if !deleted.is_empty() {
            old.retain(|v| !deleted.contains(&v.u));
        }
        let removed = old.len() < before;
        let buffered = std::mem::replace(&mut self.visits, old);
        self.pending = self.visits.len();
        self.file_bytes = loaded.bytes;
        self.visits.extend(buffered);
        self.index = Index::build(&self.visits);
        if removed {
            // The file still has the deleted visits.
            self.rewrite();
        }
    }

    fn send(&self, io: Io) {
        let io = match &self.tx {
            Some(tx) => match tx.send(io) {
                Ok(()) => return,
                Err(mpsc::SendError(io)) => io,
            },
            None => io,
        };
        if let Some(path) = &self.path {
            run_io(path, io, &self.write_failed);
        } else if let Io::Flush(done) = io {
            let _ = done.send(());
        }
    }

    fn push(&mut self, v: Visit) {
        self.index.add(&v);
        self.visits.push(v);
        self.due.get_or_insert_with(|| Instant::now() + FLUSH_EVERY);
    }

    /// Records a finished visit (ignored unless it is a web address). Only
    /// visits the user started (`user`) count toward frequency; the others
    /// refresh the time and title.
    pub fn record(&mut self, url: &str, title: &str, user: bool) {
        self.record_at(url, title, user, now_secs());
    }

    fn record_at(&mut self, url: &str, title: &str, user: bool, now: u64) {
        let k = if user { VISIT_USER } else { VISIT_AUTO };
        let Some(v) = clean(Visit { u: url.to_string(), t: title.to_string(), ts: now, k }, now) else { return };
        // Twenty clicks down a table of contents are one visit, not twenty
        // (unless the earlier one did not count and this one does). The
        // window runs from the latest repeat, so a chain of them is one.
        let seen = |last: &Visit| match &self.chain {
            Some((u, ts)) if *u == last.u => last.ts.max(*ts),
            _ => last.ts,
        };
        let repeat = self.visits.iter().rev().find(|x| x.k != VISIT_TITLE).is_some_and(|last| {
            last.u == v.u && now.saturating_sub(seen(last)) < REPEAT_WINDOW && (k != VISIT_USER || last.k == VISIT_USER)
        });
        self.chain = Some((v.u.clone(), now));
        if repeat {
            // Still the newest visit to it: the list shows when it was last seen.
            if let Some(&i) = self.index.map.get(&v.u) {
                let page = &mut self.index.pages[i];
                page.last = page.last.max(now);
            }
            self.retitle(&v.u, &v.t);
            return;
        }
        self.push(v);
    }

    /// A page's title arrived (or changed) after its visit was recorded.
    pub fn retitle(&mut self, url: &str, title: &str) {
        let title = clean_title(title);
        let url = normalize(url);
        if title.is_empty() {
            return;
        }
        let Some(&i) = self.index.map.get(&url) else { return };
        let page = &mut self.index.pages[i];
        if page.title == title {
            return;
        }
        page.tkey = title.to_lowercase();
        page.title.clone_from(&title);
        let last = page.last;
        // The newest buffered line for this address takes the title...
        if let Some(v) = self.visits[self.pending..].iter_mut().rev().find(|v| v.u == url) {
            v.t = title;
            return;
        }
        // ...else (already written) a title line, shortly after the visit.
        if now_secs().saturating_sub(last) <= RETITLE_WINDOW {
            self.push(Visit { u: url, t: title, ts: last, k: VISIT_TITLE });
        }
    }

    pub fn flush_if_due(&mut self, now: Instant) {
        self.poll();
        if self.due.is_some_and(|due| due <= now) {
            self.write_pending();
        }
    }

    /// Appends the buffered visits (or compacts, past the cap; or rewrites
    /// the whole file after a failed write).
    fn write_pending(&mut self) {
        if !self.loaded {
            // Never append before the file was checked (a torn last line).
            self.preload();
            self.due = Some(Instant::now() + FLUSH_EVERY);
            return;
        }
        self.due = None;
        if self.write_failed.swap(false, Ordering::Relaxed) {
            if self.visits.len() > MAX_ENTRIES {
                self.visits = compact(&self.visits, COMPACT_TO, COMPACT_BYTES);
                self.index = Index::build(&self.visits);
            }
            self.rewrite();
            return;
        }
        if self.pending >= self.visits.len() {
            return;
        }
        let bytes = encode(&self.visits[self.pending..]);
        if self.visits.len() > MAX_ENTRIES || self.file_bytes + bytes.len() as u64 > MAX_FILE_BYTES {
            self.visits = compact(&self.visits, COMPACT_TO, COMPACT_BYTES);
            self.index = Index::build(&self.visits);
            self.rewrite();
            return;
        }
        self.file_bytes += bytes.len() as u64;
        self.pending = self.visits.len();
        self.send(Io::Append(bytes));
    }

    /// Writes every visit to the file, replacing it atomically.
    fn rewrite(&mut self) {
        let bytes = encode(&self.visits);
        self.file_bytes = bytes.len() as u64;
        self.pending = self.visits.len();
        self.due = None;
        self.send(Io::Rewrite(bytes));
    }

    /// Writes what is buffered and waits (at most `timeout`, the load
    /// included) until it is on disk. False on timeout. With nothing to
    /// write it never starts (or waits for) the load.
    pub fn flush_now(&mut self, timeout: Duration) -> bool {
        let start = Instant::now();
        self.poll();
        if !self.loaded {
            let unwritten = self.pending < self.visits.len() || !self.deleted.is_empty();
            if unwritten {
                if !self.wait_loaded(timeout) {
                    return false;
                }
            } else if self.cleared.is_none() {
                // Nothing recorded, deleted or cleared: nothing to write.
                return true;
            }
            // Else only the clear's rewrite is queued (behind the load).
        }
        if self.loaded {
            self.write_pending();
        }
        if self.tx.is_none() {
            return true;
        }
        let (done_tx, done_rx) = mpsc::channel();
        self.send(Io::Flush(done_tx));
        done_rx.recv_timeout(timeout.saturating_sub(start.elapsed())).is_ok()
    }

    /// Removes every visit to `url` (never waits for the load: the file's
    /// visits to it are dropped when it is merged). False if it is not in
    /// what is known so far.
    pub fn delete_url(&mut self, url: &str) -> bool {
        let url = normalize(url);
        self.preload();
        self.generation += 1;
        let known = self.index.map.contains_key(&url);
        // Before the load nothing was written yet (`pending` is 0).
        self.visits.retain(|v| v.u != url);
        if !self.loaded {
            self.index = Index::build(&self.visits);
            self.deleted.push((self.generation, url));
            return true;
        }
        if !known {
            return false;
        }
        self.index = Index::build(&self.visits);
        self.rewrite();
        true
    }

    /// Removes all history (never waits for the load: the empty file is
    /// written behind it, and what it brings is dropped).
    pub fn clear(&mut self) {
        self.preload();
        self.generation += 1;
        self.cleared = Some(self.generation);
        self.deleted.clear();
        self.visits.clear();
        self.index = Index::default();
        self.rewrite();
    }

    /// Visits kept (buffered ones included).
    #[cfg(test)]
    pub fn visit_count(&self) -> usize {
        self.visits.len()
    }

    /// Addresses kept.
    #[cfg(any(test, feature = "selftest"))]
    pub fn pages(&self) -> usize {
        self.index.pages.len()
    }

    /// Suggestions for typed text, best first, from what is loaded so far.
    #[cfg(test)]
    pub fn suggest(&self, text: &str) -> Vec<Suggestion> {
        rank(text, &[&self.index], now_secs(), SUGGESTIONS)
    }

    /// The most visited sites, for the new tab page: the addresses the user
    /// went to most often (then most recently), one per site (host), best
    /// first.
    pub fn top_sites(&self, limit: usize) -> Vec<Suggestion> {
        let mut pages: Vec<&Page> = self.index.pages.iter().filter(|p| p.url.starts_with("https://") || p.url.starts_with("http://")).collect();
        pages.sort_by(|a, b| b.count.cmp(&a.count).then(b.last.cmp(&a.last)));
        let mut hosts: Vec<&str> = Vec::new();
        let mut out = Vec::new();
        for p in pages {
            if out.len() >= limit {
                break;
            }
            let host = site_of(&p.url);
            if !hosts.contains(&host) {
                hosts.push(host);
                out.push(Suggestion { u: p.url.clone(), t: p.title.clone() });
            }
        }
        out
    }

    /// `suggest` with one more source (the bookmarks) ranked alongside.
    pub fn suggest_with(&self, text: &str, extra: &dyn SuggestSource) -> Vec<Suggestion> {
        rank(text, &[&self.index, extra], now_secs(), SUGGESTIONS)
    }

    /// The history list: one row per address, newest first, those matching
    /// `filter` (all when empty), at most `LIST_MAX`; and how many matched.
    /// From what is loaded so far.
    pub fn list(&self, filter: &str) -> (Vec<Entry>, usize) {
        let q = url_key(filter.trim());
        let mut rows: Vec<&Page> =
            self.index.pages.iter().filter(|p| q.is_empty() || p.key.contains(&q) || p.tkey.contains(&q)).collect();
        let total = rows.len();
        rows.sort_unstable_by(|a, b| b.last.cmp(&a.last).then_with(|| a.url.cmp(&b.url)));
        rows.truncate(LIST_MAX);
        (rows.into_iter().map(|p| Entry { u: p.url.clone(), t: p.title.clone(), ts: p.last }).collect(), total)
    }

    /// The file as a fresh run would load it (after `flush_now`).
    #[cfg(any(test, feature = "selftest"))]
    pub fn reload(&self) -> Vec<Visit> {
        self.path.as_deref().map(|p| load(p, now_secs()).visits).unwrap_or_default()
    }
}

/// Loads `path` the way startup does: the visits kept and the lines skipped.
#[cfg(any(test, feature = "selftest"))]
pub fn load_file(path: &Path) -> (Vec<Visit>, usize) {
    let loaded = load(path, now_secs());
    (loaded.visits, loaded.skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_dir;

    fn visit(u: &str, t: &str, ts: u64) -> Visit {
        Visit { u: u.into(), t: t.into(), ts, k: VISIT_USER }
    }

    fn urls(s: &[Suggestion]) -> Vec<&str> {
        s.iter().map(|s| s.u.as_str()).collect()
    }

    #[test]
    fn top_sites_are_the_most_visited_one_per_site() {
        let link = |u: &str, ts| Visit { u: u.into(), t: String::new(), ts, k: VISIT_AUTO };
        let h = History {
            index: Index::build(&[
                visit("https://a.com/", "A", 10),
                visit("https://www.a.com/x", "A x", 11),
                visit("https://www.a.com/x", "A x", 12),
                visit("https://b.org/", "B", 20),
                visit("https://b.org/", "B", 21),
                visit("https://b.org/", "B", 22),
                visit("http://c.net:8080/", "C", 30),
                link("https://d.dev/", 40),
                visit("https://e.io/", "E", 5),
            ]),
            ..History::default()
        };
        // Most user visits first, then the latest; www.a.com is a.com.
        assert_eq!(urls(&h.top_sites(10)), ["https://b.org/", "https://www.a.com/x", "http://c.net:8080/", "https://e.io/", "https://d.dev/"]);
        assert_eq!(urls(&h.top_sites(2)), ["https://b.org/", "https://www.a.com/x"]);
        assert!(h.top_sites(0).is_empty());
        assert_eq!(site_of("https://user@www.x.com:8443/a?b#c"), "x.com:8443");
    }

    #[test]
    fn only_web_addresses_are_recorded() {
        let mut h = History::new(None);
        h.preload();
        for bad in [
            "http://xb.ui/index.html",
            "https://xb.ui/x",
            "xb://ui/index.html",
            "about:blank",
            "file:///C:/a.txt",
            "javascript:alert(1)",
            "mailto:a@b.c",
            "data:text/html,x",
            "https://a.com/\nx",
        ] {
            h.record(bad, "t", true);
        }
        h.record(&format!("https://a.com/{}", "x".repeat(9000)), "too long", true);
        assert_eq!(h.visit_count(), 0);
        h.record("https://example.com/", &format!("Ti\u{7}tle{}", "y".repeat(600)), true);
        assert_eq!(h.visit_count(), 1);
        assert_eq!(h.visits[0].t.chars().count(), MAX_TITLE);
        assert!(h.visits[0].t.starts_with("Titley"));
    }

    #[test]
    fn visits_survive_a_reload_and_retitle_applies_to_buffered_ones() {
        let dir = temp_dir("history-reload");
        let mut h = History::new(Some(&dir));
        h.preload();
        h.record("https://example.com/", "", true);
        h.retitle("https://example.com/", "Example Domain");
        h.record("https://rust-lang.org/", "Rust", true);
        assert!(h.flush_now(Duration::from_secs(5)));
        let back = h.reload();
        assert_eq!(back.len(), 2);
        assert_eq!((back[0].u.as_str(), back[0].t.as_str()), ("https://example.com/", "Example Domain"));
        // Appends add lines; a fresh History reads them all.
        h.record("https://example.com/", "Example Domain", true);
        assert!(h.flush_now(Duration::from_secs(5)));
        let mut fresh = History::new(Some(&dir));
        fresh.preload();
        assert!(fresh.wait_loaded(Duration::from_secs(5)));
        let (list, total) = fresh.list("");
        assert_eq!(total, 2);
        assert_eq!(list[0].u, "https://example.com/");
        assert_eq!(fresh.visit_count(), 3);
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.lines().all(|l| l.starts_with(r#"{"u":"#)), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_and_torn_lines_are_skipped_and_the_file_rewritten() {
        let dir = temp_dir("history-bad");
        let file = dir.join(FILE);
        let good = |u: &str| String::from_utf8(line(&visit(u, "T", 100))).unwrap();
        let content = [
            good("https://a.com/"),
            "not json\n".into(),
            "[\"https://b.com/\",\"t\",1]\n".into(),
            r#"{"u":"http://xb.ui/index.html","t":"ui","ts":1}"#.to_string() + "\n",
            r#"{"u":"javascript:x","ts":1}"#.to_string() + "\n",
            r#"{"u":"https://c.com/","ts":"soon"}"#.to_string() + "\n",
            "\n".into(),
            "\u{feff}\u{0}\n".into(),
            good("https://d.com/"),
            r#"{"u":"https://e.com/","t":"no ts"}"#.to_string() + "\n",
            r#"{"u":"https://f.com/","t":"future","ts":99999999999}"#.to_string() + "\n",
            r#"{"u":"https://g.com/","t":"unknown kind","ts":1,"k":7}"#.to_string() + "\n",
            r#"{"u":"https://torn.com/","t":"#.to_string(),
        ]
        .concat();
        std::fs::write(&file, content).unwrap();
        let (visits, skipped) = load_file(&file);
        assert_eq!(urls_of(&visits), ["https://a.com/", "https://d.com/", "https://f.com/"]);
        assert!(visits[2].ts <= now_secs(), "future time clamped");
        assert_eq!(skipped, 10);
        // Rewritten: only the good lines, each ending with a newline.
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.ends_with('\n'));
        assert_eq!(load_file(&file).1, 0, "clean after the rewrite");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn urls_of(v: &[Visit]) -> Vec<&str> {
        v.iter().map(|v| v.u.as_str()).collect()
    }

    #[test]
    fn compaction_at_the_cap() {
        let dir = temp_dir("history-cap");
        let file = dir.join(FILE);
        let visits: Vec<Visit> =
            (0..MAX_ENTRIES + 10).map(|i| visit(&format!("https://s{}.com/{i}", i % 300), "", i as u64)).collect();
        std::fs::write(&file, encode(&visits)).unwrap();
        // At load: over the cap, compacted to the newest COMPACT_TO.
        let mut h = History::new(Some(&dir));
        h.preload();
        assert!(h.wait_loaded(Duration::from_secs(10)));
        assert_eq!(h.visit_count(), COMPACT_TO);
        assert_eq!(h.visits.last().unwrap().ts, (MAX_ENTRIES + 9) as u64);
        assert_eq!(h.visits[0].ts, (MAX_ENTRIES + 10 - COMPACT_TO) as u64);
        assert!(h.flush_now(Duration::from_secs(10)));
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), COMPACT_TO);

        // At run time: past the cap, the next flush compacts too.
        for i in 0..(MAX_ENTRIES - COMPACT_TO + 1) {
            h.record(&format!("https://new.com/{i}"), "", true);
        }
        assert_eq!(h.visit_count(), MAX_ENTRIES + 1);
        assert!(h.flush_now(Duration::from_secs(10)));
        assert_eq!(h.visit_count(), COMPACT_TO);
        assert_eq!(load_file(&file).0.len(), COMPACT_TO);
        assert!(h.visits.last().unwrap().u.starts_with("https://new.com/"));

        // The byte cap holds whatever the entries' sizes.
        let big: Vec<Visit> = (0..3000).map(|i| visit(&format!("https://b.com/{i}/{}", "p".repeat(8000)), "", 1)).collect();
        let kept = compact(&big, COMPACT_TO, COMPACT_BYTES);
        assert!(encode(&kept).len() <= COMPACT_BYTES && kept.len() < big.len());
        assert_eq!(kept.last(), big.last());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_last_16_mib_are_read() {
        let dir = temp_dir("history-tail");
        let file = dir.join(FILE);
        let pad = visit(&format!("https://pad.com/{}", "p".repeat(8000)), "", 1);
        let n = (MAX_FILE_BYTES as usize / line(&pad).len()) + 50;
        let mut visits = vec![pad; n];
        visits.push(visit("https://last.com/", "Last", 2));
        std::fs::write(&file, encode(&visits)).unwrap();
        assert!(std::fs::metadata(&file).unwrap().len() > MAX_FILE_BYTES);
        let (back, skipped) = load_file(&file);
        assert_eq!(back.last().unwrap().u, "https://last.com/");
        assert_eq!(skipped, 1, "the line cut by the limit");
        assert!(std::fs::metadata(&file).unwrap().len() <= COMPACT_BYTES as u64 + 9000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_one_and_clear_rewrite_the_file() {
        let dir = temp_dir("history-delete");
        let mut h = History::new(Some(&dir));
        h.preload();
        h.record("https://a.com/", "A", true);
        h.record("https://b.com/", "B", true);
        h.record("https://a.com/", "A", true);
        assert!(h.flush_now(Duration::from_secs(5)));
        assert!(h.delete_url("https://a.com/"));
        assert!(!h.delete_url("https://a.com/"));
        assert!(h.flush_now(Duration::from_secs(5)));
        assert_eq!(urls_of(&h.reload()), ["https://b.com/"]);
        assert!(h.suggest("a.com").is_empty());
        h.clear();
        assert!(h.flush_now(Duration::from_secs(5)));
        assert!(h.reload().is_empty());
        assert_eq!((h.visit_count(), h.pages()), (0, 0));
        assert!(!dir.join(format!("{FILE}.tmp")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ranking_prefers_prefix_frequency_and_recency() {
        let now = 1_000_000_000;
        let day = 86_400;
        let mut h = History::new(None);
        h.preload();
        let mut add = |u: &str, t: &str, ts: u64| h.index.add(&visit(u, t, ts));
        add("https://example.com/", "Example Domain", now - day);
        add("https://www.example.org/long/path/here", "Other", now - day);
        add("https://news.site/example", "Example news", now - day);
        add("https://site.com/about", "An exampleish page", now - day);
        add("https://rust-lang.org/", "Rust", now - 300 * day);
        for _ in 0..5 {
            add("https://rustup.rs/", "Rustup", now - day);
        }
        let s = rank("exa", &[&h.index], now, 8);
        assert_eq!(
            urls(&s),
            ["https://example.com/", "https://www.example.org/long/path/here", "https://news.site/example", "https://site.com/about"]
        );
        // Scheme and www. typed or not: the same matches.
        assert_eq!(urls(&rank("https://www.exa", &[&h.index], now, 8))[0], "https://example.com/");
        // Frequent and recent beats rare and old.
        assert_eq!(urls(&rank("rust", &[&h.index], now, 8)), ["https://rustup.rs/", "https://rust-lang.org/"]);
        assert!(rank("   ", &[&h.index], now, 8).is_empty());
        assert!(rank("zzz", &[&h.index], now, 8).is_empty());
        assert_eq!(rank("e", &[&h.index], now, 2).len(), 2);
    }

    struct Fixed(Vec<(String, String)>);
    impl SuggestSource for Fixed {
        fn each<'s>(&'s self, f: &mut dyn FnMut(Candidate<'s, '_>)) {
            for (u, t) in &self.0 {
                let (key, tkey) = (url_key(u), t.to_lowercase());
                f(Candidate { url: u, title: t, key: &key, tkey: &tkey, last: 0, count: 0, weight: 3.0 });
            }
        }
    }

    #[test]
    fn a_second_source_is_merged_without_duplicates() {
        let now = 1_000_000_000;
        let mut h = History::new(None);
        h.preload();
        h.index.add(&visit("https://example.com/", "Example", now));
        h.index.add(&visit("https://example.net/", "Net", now));
        let marks = Fixed(vec![("https://example.com/".into(), "Mark".into()), ("https://exam.org/".into(), "".into())]);
        let s = rank("exam", &[&h.index, &marks], now, 8);
        assert_eq!(s.len(), 3);
        assert_eq!(s.iter().filter(|s| s.u == "https://example.com/").count(), 1);
    }

    #[test]
    fn ranking_50000_entries_is_fast() {
        let now = 1_000_000_000;
        let visits: Vec<Visit> = (0..MAX_ENTRIES)
            .map(|i| {
                visit(
                    &format!("https://site{}.example.com/path/{i}?q={}", i % 5000, i * 7),
                    &format!("Page {i} about topic{} and more words", i % 97),
                    now - (i as u64 % 1000) * 3600,
                )
            })
            .collect();
        let index = Index::build(&visits);
        assert_eq!(index.pages.len(), MAX_ENTRIES);
        let mut worst = Duration::ZERO;
        for q in ["s", "site12", "topic4", "example.com/path/4999", "nomatch", "p"] {
            let start = Instant::now();
            let s = rank(q, &[&index], now, SUGGESTIONS);
            worst = worst.max(start.elapsed());
            assert!(s.len() <= SUGGESTIONS);
        }
        println!("rank over {MAX_ENTRIES} entries: worst {:.2} ms", worst.as_secs_f64() * 1000.0);
        assert!(worst < Duration::from_millis(if cfg!(debug_assertions) { 1500 } else { 100 }), "{worst:?}");
    }
    /// A history file holding `visits`, and a History on it whose load has
    /// finished on its thread but is not merged yet.
    fn loading(name: &str, visits: &[Visit]) -> (PathBuf, History) {
        let dir = temp_dir(name);
        std::fs::write(dir.join(FILE), encode(visits)).unwrap();
        let mut h = History::new(Some(&dir));
        let (tx, rx) = mpsc::channel();
        h.on_loaded(move || {
            let _ = tx.send(());
        });
        h.preload();
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!h.is_loaded(), "nothing merged before poll");
        (dir, h)
    }

    #[test]
    fn clearing_during_the_load_keeps_old_visits_gone() {
        let (dir, mut h) = loading("history-clear-load", &[visit("https://a.com/", "A", 1), visit("https://b.com/", "B", 2)]);
        h.record("https://before.com/", "", true);
        h.clear();
        h.record("https://c.com/", "C", true);
        assert!(h.wait_loaded(Duration::from_secs(5)));
        assert!(h.take_merged() && !h.take_merged());
        assert_eq!(urls_of(&h.visits), ["https://c.com/"]);
        assert_eq!(h.pages(), 1);
        assert!(h.flush_now(Duration::from_secs(5)));
        assert_eq!(urls_of(&h.reload()), ["https://c.com/"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_during_the_load_drops_the_files_visits_too() {
        let (dir, mut h) = loading(
            "history-delete-load",
            &[visit("https://a.com/", "A", 1), visit("https://b.com/", "B", 2), visit("https://a.com/", "A", 3)],
        );
        h.record("https://a.com/", "A", true);
        assert!(h.delete_url("https://a.com/"));
        assert!(h.suggest("a.com").is_empty(), "gone from what is shown at once");
        // Visited again after the delete: that visit stays.
        h.record("https://d.com/", "D", true);
        h.record("https://a.com/#top", "A again", true);
        h.poll();
        assert!(h.is_loaded());
        assert_eq!(urls_of(&h.visits), ["https://b.com/", "https://d.com/", "https://a.com/"]);
        assert!(h.flush_now(Duration::from_secs(5)));
        assert_eq!(urls_of(&h.reload()), ["https://b.com/", "https://d.com/", "https://a.com/"]);
        assert_eq!(h.reload()[2].t, "A again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggest_and_list_never_wait_for_the_load() {
        let (dir, mut h) = loading("history-nowait", &[visit("https://old.com/", "Old", 1)]);
        h.record("https://new.com/", "New", true);
        let start = Instant::now();
        assert!(h.suggest("old").is_empty());
        assert_eq!(urls(&h.suggest("new")), ["https://new.com/"]);
        assert_eq!(h.list("").1, 1);
        assert!(start.elapsed() < Duration::from_millis(500));
        h.poll();
        assert_eq!(urls(&h.suggest("old")), ["https://old.com/"]);
        assert_eq!(h.list("").1, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_visits_the_user_started_count() {
        let now = now_secs();
        let mut h = History::new(None);
        h.preload();
        // Spaced out, so none is a repeat of the one before.
        let at = |i: u64| now - 20_000 + i * REPEAT_WINDOW;
        for i in 0..1000 {
            h.record_at("https://aaa.com/", "Ads", false, at(i));
        }
        for i in 0..10 {
            h.record_at("https://aab.com/", "Mine", true, at(1000 + i));
        }
        assert_eq!(urls(&rank("aa", &[&h.index], now, 8)), ["https://aab.com/", "https://aaa.com/"]);
        let page = |u: &str| h.index.pages[h.index.map[u]].count;
        assert_eq!((page("https://aaa.com/"), page("https://aab.com/")), (0, 10));
        // Written with their kind; read back the same.
        let auto = String::from_utf8(line(&h.visits[0])).unwrap();
        let user = String::from_utf8(line(&h.visits[1000])).unwrap();
        assert!(auto.contains(r#""k":1"#) && !user.contains(r#""k""#), "{auto}{user}");
        let back = Index::build(&parse(&encode(&h.visits), false, now).visits);
        assert_eq!(back.pages[back.map["https://aaa.com/"]].count, 0);
        assert_eq!(back.pages[back.map["https://aab.com/"]].count, 10);
    }

    #[test]
    fn titles_match_only_from_three_characters() {
        let now = 1_000_000_000;
        let index = Index::build(&[visit("https://zzz.com/", "Example", now)]);
        assert!(rank("ex", &[&index], now, 8).is_empty());
        assert_eq!(urls(&rank("exa", &[&index], now, 8)), ["https://zzz.com/"]);
        assert_eq!(urls(&rank("zz", &[&index], now, 8)), ["https://zzz.com/"], "addresses still match");
    }

    #[test]
    fn a_file_of_bare_newlines_is_cheap() {
        let dir = temp_dir("history-newlines");
        let file = dir.join(FILE);
        std::fs::write(&file, vec![b'\n'; MAX_FILE_BYTES as usize]).unwrap();
        let start = Instant::now();
        let (visits, skipped) = load_file(&file);
        let took = start.elapsed();
        assert!(visits.is_empty());
        assert_eq!(skipped, MAX_FILE_BYTES as usize);
        assert_eq!(std::fs::metadata(&file).unwrap().len(), 0, "rewritten empty");
        assert!(took < Duration::from_secs(if cfg!(debug_assertions) { 10 } else { 2 }), "{took:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_title_after_the_flush_survives_a_reload() {
        let dir = temp_dir("history-retitle");
        let mut h = History::new(Some(&dir));
        h.preload();
        h.record("https://example.com/", "Loading…", true);
        assert!(h.flush_now(Duration::from_secs(5)));
        h.retitle("https://example.com/#section", "Example Domain");
        assert!(h.due.is_some());
        // A second change before the next flush rides in the same line.
        h.retitle("https://example.com/", "Example Domain!");
        assert!(h.flush_now(Duration::from_secs(5)));
        let back = h.reload();
        assert_eq!(back.len(), 2);
        assert_eq!(back[1].k, VISIT_TITLE);
        let mut fresh = History::new(Some(&dir));
        fresh.preload();
        assert!(fresh.wait_loaded(Duration::from_secs(5)));
        let (list, _) = fresh.list("");
        assert_eq!(list[0].t, "Example Domain!");
        assert_eq!(fresh.index.pages[0].count, 1, "a title is not a visit");
        // A title for an address that is no longer kept is ignored.
        let index = Index::build(&[Visit { k: VISIT_TITLE, ..visit("https://gone.com/", "T", 1) }]);
        assert!(index.pages.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_address_moments_later_is_one_visit() {
        let t = now_secs() - 1000;
        let mut h = History::new(None);
        h.preload();
        let count = |h: &History| h.index.pages[h.index.map["https://a.com/doc"]].count;
        h.record_at("https://a.com/doc", "Doc", true, t);
        h.record_at("https://a.com/doc#two", "Doc", true, t + 1);
        h.record_at("https://a.com/doc", "Doc, reloaded", false, t + 2);
        assert_eq!((h.visit_count(), count(&h)), (1, 1), "anchor click and self-reload are the same visit");
        assert_eq!(h.list("").0[0].t, "Doc, reloaded", "but the newer title is kept");
        h.record_at("https://a.com/doc", "Doc", true, t + 2 + REPEAT_WINDOW);
        assert_eq!((h.visit_count(), count(&h)), (2, 2), "after the window (from the latest repeat) it is a new visit");
        h.record_at("https://b.com/", "B", false, t + REPEAT_WINDOW + 3);
        h.record_at("https://b.com/", "B", true, t + REPEAT_WINDOW + 4);
        assert_eq!(h.visit_count(), 4, "a visit that counts is never folded into one that did not");
    }

    #[test]
    fn with_nothing_to_write_the_exit_flush_reads_nothing() {
        let dir = temp_dir("history-exit-idle");
        let mut h = History::new(Some(&dir));
        assert!(h.flush_now(Duration::from_secs(2)), "never loaded, nothing recorded: all written");
        assert!(!h.started && !dir.join(FILE).exists(), "and no load started for it");
        // A visit recorded before the load still waits for it.
        h.record("https://a.com/", "A", true);
        assert!(h.flush_now(Duration::from_secs(5)));
        assert_eq!(urls_of(&h.reload()), ["https://a.com/"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_page_reloading_itself_for_minutes_is_one_visit() {
        let t = now_secs() - 1000;
        let mut h = History::new(None);
        h.preload();
        let page = |h: &History| h.index.pages[h.index.map["https://a.com/live"]].clone();
        h.record_at("https://a.com/live", "Live", true, t);
        // Every 9 s for three minutes: each within the window of the one before.
        for i in 1..=20 {
            h.record_at("https://a.com/live", "Live", false, t + i * (REPEAT_WINDOW - 1));
        }
        assert_eq!((h.visit_count(), page(&h).count), (1, 1));
        assert_eq!(page(&h).last, t + 20 * (REPEAT_WINDOW - 1), "shown as seen last at the latest repeat");
        // Opened again by the user right after the chain: still the same visit.
        h.record_at("https://a.com/live", "Live", true, t + 20 * (REPEAT_WINDOW - 1) + 2);
        assert_eq!((h.visit_count(), page(&h).count), (1, 1));
        // A pause longer than the window ends the chain.
        h.record_at("https://a.com/live", "Live", true, t + 20 * (REPEAT_WINDOW - 1) + 2 + REPEAT_WINDOW);
        assert_eq!((h.visit_count(), page(&h).count), (2, 2));
    }

    #[test]
    fn fragments_and_user_info_are_not_kept() {
        assert_eq!(normalize("https://user:pw@example.com/a?q=1#frag"), "https://example.com/a?q=1");
        assert_eq!(normalize("https://example.com/p@x?m=a@b"), "https://example.com/p@x?m=a@b");
        assert_eq!(normalize("http://a@b@host.com"), "http://host.com");
        assert_eq!(normalize("https://example.com/#"), "https://example.com/");
        assert_eq!(without_userinfo("https://user:pw@example.com/app#/inbox"), "https://example.com/app#/inbox");
        assert_eq!(without_userinfo("https://example.com#x@evil"), "https://example.com#x@evil", "an @ in the fragment is no userinfo");
        assert_eq!(without_userinfo("https://example.com/#"), "https://example.com/");
        let mut h = History::new(None);
        h.preload();
        h.record("https://user:secret@example.com/doc", "Doc", true);
        h.record("https://example.com/doc#part-2", "Doc", true);
        h.record("https://xb.ui@evil.com/", "", true);
        h.record("https://u@xb.ui/index.html", "", true);
        // The #part-2 click right after is the same visit.
        assert_eq!(urls_of(&h.visits), ["https://example.com/doc", "https://evil.com/"]);
        assert_eq!(h.pages(), 2);
        assert!(h.visits.iter().all(|v| !v.u.contains("secret")));
    }

    #[test]
    fn after_a_failed_write_the_whole_file_is_rewritten() {
        let dir = temp_dir("history-failed");
        let file = dir.join(FILE);
        let mut h = History::new(Some(&dir));
        h.preload();
        h.record("https://a.com/", "A", true);
        assert!(h.flush_now(Duration::from_secs(5)));
        // What a failed append can leave behind.
        std::fs::OpenOptions::new().append(true).open(&file).unwrap().write_all(br#"{"u":"https://b"#).unwrap();
        h.write_failed.store(true, Ordering::Relaxed);
        h.record("https://c.com/", "C", true);
        assert!(h.flush_now(Duration::from_secs(5)));
        let (visits, skipped) = load_file(&file);
        assert_eq!((urls_of(&visits), skipped), (vec!["https://a.com/", "https://c.com/"], 0));
        assert!(!h.write_failed.load(Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
