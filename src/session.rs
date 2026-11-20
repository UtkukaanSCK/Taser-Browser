//! What survives a restart: the open tabs (`session.json`) and the window's
//! place (`window.json`). `store` writes and reads the files; this module
//! decides what goes in them, what is accepted back, and when to save.
//!
//! Saves happen `SAVE_DELAY` after a change (tab opened, closed, moved,
//! navigated, retitled, zoomed; window moved or resized) and once more before
//! the app exits, so after a crash the last saved state comes back.
//! Restoring creates only the active tab's webview; the others keep their
//! saved title in the strip and load on first activation (`App::create_view`).

use crate::app::App;
use crate::store::{self, Store, Stored};
use crate::tabs::{Placement, TabId, Tabs};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// At most this many tabs are saved or restored (the rest are dropped).
pub const MAX_TABS: usize = 500;
/// Titles are kept to this many characters (as in `App::on_page`).
pub const MAX_TITLE: usize = 512;
/// Restored zoom outside this range (whole percent) reads as 100.
const ZOOM_RANGE: std::ops::RangeInclusive<u16> = 25..=500;
/// How long after a change the state is saved (not pushed back by later changes).
pub const SAVE_DELAY: Duration = Duration::from_millis(1500);
/// How long the exit waits for the files to reach the disk.
const EXIT_FLUSH: Duration = Duration::from_secs(2);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct SavedTab {
    pub url: String,
    pub title: String,
    /// Whole percent, 100 = none. A hand-edited value that is no whole
    /// number in range costs only this tab its zoom, not the whole file.
    #[serde(deserialize_with = "lenient_u16")]
    pub zoom: u16,
}

/// A whole number of the target type, or 0 for anything else (out of
/// range, negative, fractional, not a number); `sanitize` then applies its
/// usual fallback.
fn lenient<'de, D: serde::Deserializer<'de>, N: TryFrom<u64> + Default>(d: D) -> Result<N, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_u64().and_then(|n| N::try_from(n).ok()).unwrap_or_default())
}

fn lenient_u16<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    lenient(d)
}

fn lenient_usize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    lenient(d)
}

impl Default for SavedTab {
    fn default() -> Self {
        Self { url: String::new(), title: String::new(), zoom: 100 }
    }
}

/// The tab strip, in order. Blank tabs are not saved: an empty session
/// starts with one blank tab.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<SavedTab>,
    /// Index into `tabs` (anything but a whole number reads as 0).
    #[serde(deserialize_with = "lenient_usize")]
    pub active: usize,
}

/// A tab URL worth saving and safe to load again.
fn restorable(url: &str) -> bool {
    crate::url::is_tab_url(url) && !crate::ui::is_ui_url(url) && !url.chars().any(char::is_control)
}

impl Stored for Session {
    const NAME: &'static str = "session.json";
    /// 500 tabs of 8 KiB URLs and 512-character titles fit.
    const MAX_BYTES: u64 = 8 * 1024 * 1024;

    fn sanitize(&mut self) {
        let mut kept = Vec::new();
        let mut active = 0;
        for (i, mut t) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if kept.len() >= MAX_TABS {
                break;
            }
            if !restorable(&t.url) {
                continue;
            }
            if t.title.chars().count() > MAX_TITLE {
                t.title = t.title.chars().take(MAX_TITLE).collect();
            }
            if !ZOOM_RANGE.contains(&t.zoom) {
                t.zoom = 100;
            }
            // The saved active tab, or if it was dropped the nearest kept
            // one before it (else the first).
            if i <= self.active {
                active = kept.len();
            }
            kept.push(t);
        }
        self.tabs = kept;
        self.active = active;
    }
}

impl Session {
    /// The current strip as it would be restored.
    pub fn capture(tabs: &Tabs) -> Self {
        let mut s = Session::default();
        let active = tabs.active();
        for id in (0..tabs.len()).filter_map(|i| tabs.nth(i)) {
            let Some(t) = tabs.get(id) else { continue };
            let is_active = Some(id) == active;
            if s.tabs.len() >= MAX_TABS || !restorable(&t.url) {
                // An active blank tab (not saved): the nearest saved one before it.
                if is_active {
                    s.active = s.tabs.len().saturating_sub(1);
                }
                continue;
            }
            if is_active {
                s.active = s.tabs.len();
            }
            s.tabs.push(SavedTab { url: t.url.clone(), title: t.title.clone(), zoom: t.zoom });
        }
        s
    }
}

/// The window's normal (not maximized) rect in physical pixels, as tao
/// reports it (outer position, inner size), and whether it was maximized.
/// F11 fullscreen is deliberately not kept: it is left on the next start.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(default)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    /// 0 = nothing saved.
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

/// Saved coordinates beyond this are junk (virtual screens are far smaller).
const COORD_LIMIT: i32 = 100_000;
const SIZE_RANGE: std::ops::RangeInclusive<u32> = 100..=32_768;

impl WindowState {
    pub fn is_set(&self) -> bool {
        self.width > 0
    }

    pub fn rect(&self) -> Rect {
        Rect { x: self.x, y: self.y, w: self.width as i32, h: self.height as i32 }
    }
}

impl Stored for WindowState {
    const NAME: &'static str = "window.json";
    const MAX_BYTES: u64 = 4 * 1024;

    fn sanitize(&mut self) {
        let sane = SIZE_RANGE.contains(&self.width)
            && SIZE_RANGE.contains(&self.height)
            && self.x.unsigned_abs() <= COORD_LIMIT as u32
            && self.y.unsigned_abs() <= COORD_LIMIT as u32;
        if !sane {
            *self = WindowState::default();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn overlap(&self, o: &Rect) -> (i64, i64) {
        let w = (self.x + self.w).min(o.x + o.w) as i64 - self.x.max(o.x) as i64;
        let h = (self.y + self.h).min(o.y + o.h) as i64 - self.y.max(o.y) as i64;
        (w.max(0), h.max(0))
    }
}

/// Height of the window's top band (the tab strip, where it is dragged).
const GRAB_BAND: i32 = 40;

/// Where to open a window saved at `saved`, given the monitors now attached
/// (`monitors[0]` is the primary). It stays put while enough of its top band
/// is on some monitor to grab it; otherwise it moves (and shrinks if need
/// be) onto the monitor it overlaps most, or the primary.
pub fn place(saved: Rect, monitors: &[Rect]) -> Rect {
    let Some(primary) = monitors.first() else { return saved };
    let band = Rect { h: GRAB_BAND.min(saved.h), ..saved };
    let need = (100.min(saved.w) as i64, 20.min(band.h) as i64);
    if monitors.iter().any(|m| {
        let (w, h) = band.overlap(m);
        w >= need.0 && h >= need.1
    }) {
        return saved;
    }
    let target = monitors
        .iter()
        .map(|m| (m, saved.overlap(m)))
        .filter(|(_, (w, h))| w * h > 0)
        .max_by_key(|(_, (w, h))| w * h)
        .map_or(primary, |(m, _)| m);
    let w = saved.w.min(target.w);
    let h = saved.h.min(target.h);
    Rect {
        x: saved.x.clamp(target.x, target.x + target.w - w),
        y: saved.y.clamp(target.y, target.y + target.h - h),
        w,
        h,
    }
}

/// The saving side, owned by `App`.
#[derive(Default)]
pub struct Persist {
    session: Option<Store<Session>>,
    window: Option<Store<WindowState>>,
    /// The window's last normal rect and maximized state.
    pub placement: WindowState,
    /// When the pending save is due.
    pub due: Option<Instant>,
    exit_saved: bool,
}

impl Persist {
    /// Opens the files in `dir` (none: nothing is saved) and returns what
    /// was in them.
    pub fn open(dir: Option<&std::path::Path>) -> (Self, Session, WindowState) {
        let Some(dir) = dir else { return (Self::default(), Session::default(), WindowState::default()) };
        let mut session = Store::<Session>::new(dir);
        let mut window = Store::<WindowState>::new(dir);
        let (tabs, placement) = (session.load(), window.load());
        for (name, source) in [(Session::NAME, tabs.source), (WindowState::NAME, placement.source)] {
            if source == store::Source::Backup {
                eprintln!("x-browser: {name} restored from its backup");
            }
        }
        let (tabs, placement) = (tabs.value, placement.value);
        (Self { session: Some(session), window: Some(window), placement, ..Self::default() }, tabs, placement)
    }

    /// Something worth saving changed: save within `SAVE_DELAY`.
    pub fn changed(&mut self) {
        if self.due.is_none() {
            self.due = Some(Instant::now() + SAVE_DELAY);
        }
    }

    #[cfg(feature = "selftest")]
    pub fn paths(&self) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        Some((self.session.as_ref()?.path().to_path_buf(), self.window.as_ref()?.path().to_path_buf()))
    }
}

/// Saves both files now (in the background; see `store::flush`). A tab list
/// with no tabs at all is the app quitting after its last tab closed; that
/// tab was saved just before (`App::close_tab`) and stays saved.
pub fn save(app: &mut App) {
    app.persist.due = None;
    let placement = app.persist.placement;
    if !app.tabs.is_empty()
        && let Some(s) = &mut app.persist.session
    {
        s.save(&Session::capture(&app.tabs));
    }
    if placement.is_set()
        && let Some(w) = &mut app.persist.window
    {
        w.save(&placement);
    }
}

/// End of an event-loop pass: saves if a save is due.
pub fn save_if_due(app: &mut App) {
    if app.persist.due.is_some_and(|due| Instant::now() >= due) {
        save(app);
    }
}

/// Last save before the app exits; waits (briefly) for the disk. Once.
pub fn save_at_exit(app: &mut App) {
    if std::mem::replace(&mut app.persist.exit_saved, true) {
        return;
    }
    // From here on a second start does not hand over to us (we would drop
    // its address): it waits and becomes the first instance itself.
    crate::instance::set_quitting(true);
    track_window(app);
    save(app);
    if !store::flush(EXIT_FLUSH) {
        eprintln!("x-browser: saving the session took too long; the previous save stays");
    }
}

/// The window moved or was resized: remember its normal rect / maximized
/// state. Minimized and fullscreen states are not what the next start
/// should open with, so they are skipped.
pub fn track_window(app: &mut App) {
    let w = &app.window;
    if w.is_minimized() || w.fullscreen().is_some() || app.fullscreen.on() {
        return;
    }
    let mut p = app.persist.placement;
    p.maximized = w.is_maximized();
    if !p.maximized
        && let Ok(pos) = w.outer_position()
        // Windows parks minimized windows at -32000.
        && pos.x > -COORD_LIMIT
        && pos.y > -COORD_LIMIT
    {
        let size = w.inner_size();
        (p.x, p.y, p.width, p.height) = (pos.x, pos.y, size.width, size.height);
    }
    if p != app.persist.placement {
        app.persist.placement = p;
        app.persist.changed();
    }
}

/// Appends the saved tabs and shows the saved active one; only that one gets
/// a webview now. Returns the active tab.
pub fn restore(app: &mut App, session: &Session) -> Option<TabId> {
    // The setting asks for a blank or home page start: the saved tabs are
    // not brought back (and the next save replaces them).
    if app.settings.value.startup != crate::settings::Startup::Last {
        return None;
    }
    let mut active = None;
    for (i, t) in session.tabs.iter().enumerate() {
        let id = app.tabs.add(&t.url, Placement::End);
        if let Some(tab) = app.tabs.get_mut(id) {
            tab.title.clone_from(&t.title);
            tab.zoom = t.zoom;
            tab.new_tab_page = t.url == crate::url::BLANK;
        }
        if i == session.active || active.is_none() {
            active = Some(id);
        }
    }
    let id = active?;
    app.tabs.activate(id);
    app.show_active();
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(url: &str) -> SavedTab {
        SavedTab { url: url.into(), title: format!("t {url}"), zoom: 100 }
    }

    fn urls(s: &Session) -> Vec<&str> {
        s.tabs.iter().map(|t| t.url.as_str()).collect()
    }

    #[test]
    fn untrusted_tabs_are_filtered_and_active_follows() {
        let long = format!("https://a.example/{}", "x".repeat(crate::url::MAX_URL_LEN));
        let mut s = Session {
            tabs: vec![
                tab("https://a.example/"),
                tab("javascript:alert(1)"),
                tab("file:///C:/Windows/win.ini"),
                tab(&long),
                tab("http://b.example/"),
                tab("about:blank"),
                tab("https://xb.ui/index.html"),
                tab("https://c.example/\nx"),
                tab("HTTPS://D.example/"),
            ],
            active: 4,
        };
        s.sanitize();
        assert_eq!(urls(&s), ["https://a.example/", "http://b.example/", "HTTPS://D.example/"]);
        assert_eq!(s.active, 1);

        // The active tab was dropped: the nearest kept one before it.
        let mut s = Session { tabs: vec![tab("https://a/"), tab("https://b/"), tab("file:///x")], active: 2 };
        s.sanitize();
        assert_eq!(s.active, 1);
        // ... or the first, if none before it; out-of-range indexes too.
        let mut s = Session { tabs: vec![tab("file:///x"), tab("https://b/")], active: 0 };
        s.sanitize();
        assert_eq!(s.active, 0);
        let mut s = Session { tabs: vec![tab("https://a/"), tab("https://b/")], active: usize::MAX };
        s.sanitize();
        assert_eq!(s.active, 1);
        let mut s = Session { tabs: vec![tab("file:///x")], active: 0 };
        s.sanitize();
        assert_eq!((s.tabs.len(), s.active), (0, 0));
    }

    #[test]
    fn titles_zoom_and_tab_count_are_bounded() {
        let mut t = tab("https://a/");
        t.title = "ğ".repeat(MAX_TITLE + 100);
        t.zoom = 9000;
        let mut z = tab("https://b/");
        z.zoom = 150;
        let mut s = Session { tabs: vec![t, z], active: 0 };
        s.sanitize();
        assert_eq!(s.tabs[0].title.chars().count(), MAX_TITLE);
        assert_eq!((s.tabs[0].zoom, s.tabs[1].zoom), (100, 150));

        let mut s = Session { tabs: (0..MAX_TABS + 50).map(|i| tab(&format!("https://t{i}/"))).collect(), active: 520 };
        s.sanitize();
        assert_eq!(s.tabs.len(), MAX_TABS);
        assert_eq!(s.active, MAX_TABS - 1);
    }

    #[test]
    fn capture_skips_blank_tabs() {
        let mut tabs = Tabs::new();
        let a = tabs.add("https://a/", Placement::End);
        let blank = tabs.add("about:blank", Placement::End);
        let b = tabs.add("https://b/", Placement::End);
        tabs.get_mut(b).unwrap().zoom = 125;
        tabs.activate(b);
        let s = Session::capture(&tabs);
        assert_eq!(urls(&s), ["https://a/", "https://b/"]);
        assert_eq!((s.active, s.tabs[1].zoom), (1, 125));
        tabs.activate(blank);
        assert_eq!(Session::capture(&tabs).active, 0);
        tabs.activate(a);
        assert_eq!(Session::capture(&tabs).active, 0);
        let mut only_blank = Tabs::new();
        only_blank.add("about:blank", Placement::End);
        assert_eq!(Session::capture(&only_blank), Session::default());
    }

    #[test]
    fn session_round_trips_through_the_store() {
        let dir = crate::store::tests::temp_dir("session");
        let s = Session { tabs: vec![tab("https://a/"), SavedTab { zoom: 150, ..tab("https://b/") }], active: 1 };
        Store::<Session>::new(&dir).save_now(&s).unwrap();
        let text = std::fs::read_to_string(dir.join("session.json")).unwrap();
        assert!(text.starts_with(r#"{"v":1,"tabs":[{"url":"https://a/""#), "{text}");
        assert_eq!(Store::<Session>::new(&dir).load().value, s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_zoom_or_active_costs_only_itself() {
        let dir = crate::store::tests::temp_dir("lenient");
        let text = r#"{"v":1,"active":-1,"tabs":[
            {"url":"https://a/","zoom":70000},
            {"url":"https://b/","zoom":-5},
            {"url":"https://c/","zoom":"big"},
            {"url":"https://d/","zoom":1.5},
            {"url":"https://e/","zoom":null},
            {"url":"https://f/","zoom":150}]}"#;
        std::fs::write(dir.join("session.json"), text).unwrap();
        let l = Store::<Session>::new(&dir).load();
        assert_eq!(l.source, crate::store::Source::File);
        assert_eq!(l.value.tabs.iter().map(|t| t.zoom).collect::<Vec<_>>(), [100, 100, 100, 100, 100, 150]);
        assert_eq!(l.value.active, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn window_state_out_of_range_is_dropped() {
        let ok = WindowState { x: -1920, y: 10, width: 1200, height: 800, maximized: true };
        let mut w = ok;
        w.sanitize();
        assert_eq!(w, ok);
        for bad in [
            WindowState { width: 10, ..ok },
            WindowState { height: 1_000_000, ..ok },
            WindowState { x: i32::MIN, ..ok },
            WindowState { y: 200_000, ..ok },
        ] {
            let mut w = bad;
            w.sanitize();
            assert!(!w.is_set(), "{bad:?}");
        }
    }

    const PRIMARY: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };
    const LEFT: Rect = Rect { x: -1280, y: 200, w: 1280, h: 1024 };

    #[test]
    fn a_window_on_a_monitor_stays_put() {
        let r = Rect { x: 100, y: 100, w: 1200, h: 800 };
        assert_eq!(place(r, &[PRIMARY]), r);
        let on_left = Rect { x: -1200, y: 300, w: 1000, h: 700 };
        assert_eq!(place(on_left, &[PRIMARY, LEFT]), on_left);
        // Mostly off screen, but its strip can still be grabbed: kept.
        let hanging = Rect { x: 1700, y: 900, w: 1200, h: 800 };
        assert_eq!(place(hanging, &[PRIMARY]), hanging);
    }

    #[test]
    fn a_window_on_a_missing_monitor_moves_to_a_visible_one() {
        // Saved on the left monitor, which is gone now.
        let r = Rect { x: -1200, y: 300, w: 1000, h: 700 };
        assert_eq!(place(r, &[PRIMARY]), Rect { x: 0, y: 300, w: 1000, h: 700 });
        // Far away and larger than the screen: shrunk onto the primary.
        let r = Rect { x: 5000, y: 5000, w: 2500, h: 1500 };
        assert_eq!(place(r, &[PRIMARY, LEFT]), PRIMARY);
        // Strip above the top of the screen (title bar unreachable).
        let r = Rect { x: 200, y: -500, w: 800, h: 600 };
        assert_eq!(place(r, &[PRIMARY]), Rect { x: 200, y: 0, w: 800, h: 600 });
        // Strip off screen, body over both monitors but more over the left
        // one: moved onto the left one.
        let r = Rect { x: -1000, y: -300, w: 1100, h: 600 };
        assert_eq!(place(r, &[PRIMARY, LEFT]), Rect { x: -1100, y: 200, w: 1100, h: 600 });
        // No monitors known: left to Windows as it was.
        assert_eq!(place(r, &[]), r);
    }
}
