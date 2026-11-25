//! "Kapanışta tarama verilerini temizle": the chosen kinds of browsing data
//! (history, cookies and site data, cache) are cleared when Taser quits.
//!
//! A quit that is not clean (a crash, a killed process, the power going)
//! leaves them for the next start, which clears them before any tab loads.
//! That is what the marker file, `exit-clear.json`, is for: while the
//! setting is on it names the kinds still owed, and a clear that finished
//! takes them off again. A missing marker with the setting on counts as
//! owing everything chosen.
//!
//! Clearing never touches the extensions' own storage or their on/off state,
//! bookmarks, settings, the window's place or the download list (see
//! `native::clear_browsing_data_of`). Each wait is bounded: what did not
//! finish in time is logged and owed to the next start.
//!
//! With history chosen, the open tabs count as history: the last session is
//! not saved, so "Son oturumu aç" starts with one blank tab.

use crate::app::App;
use crate::settings::Settings;
use crate::store::{self, Store, Stored};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

/// Longest a quit waits for WebView2 to clear (the history file has its own
/// bounded write).
pub const EXIT_WAIT: Duration = Duration::from_secs(5);

/// Longest a start waits for what the last run left owed.
pub const START_WAIT: Duration = Duration::from_secs(5);

/// Longest a quit waits for the marker to reach the disk.
const MARKER_FLUSH: Duration = Duration::from_secs(1);

/// The kinds of data to clear (the settings' choices, or what is owed).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Kinds {
    /// Taser's history, the closed-tab list, the last session's tabs and
    /// WebView2's own browsing history.
    #[serde(deserialize_with = "crate::settings::lenient_off")]
    pub history: bool,
    /// Cookies and every kind of DOM storage of the pages' profile.
    #[serde(deserialize_with = "crate::settings::lenient_off")]
    pub cookies: bool,
    /// The disk cache.
    #[serde(deserialize_with = "crate::settings::lenient_off")]
    pub cache: bool,
}

impl Kinds {
    pub fn any(self) -> bool {
        self.history || self.cookies || self.cache
    }

    pub fn union(self, o: Self) -> Self {
        Self { history: self.history || o.history, cookies: self.cookies || o.cookies, cache: self.cache || o.cache }
    }

    /// What of these is WebView2's to clear.
    fn native(self) -> crate::native::ClearKinds {
        crate::native::ClearKinds { site_data: self.cookies, cache: self.cache, history: self.history }
    }

    /// Names for the log.
    fn names(self) -> String {
        let all = [(self.history, "history"), (self.cookies, "cookies and site data"), (self.cache, "cache")];
        all.iter().filter(|(on, _)| *on).map(|(_, n)| *n).collect::<Vec<_>>().join(", ")
    }
}

impl Stored for Kinds {
    const NAME: &'static str = "exit-clear.json";
    const MAX_BYTES: u64 = 1024;

    fn sanitize(&mut self) {}
}

/// What the settings ask to clear at quit (nothing while the switch is off).
pub fn wanted(s: &Settings) -> Kinds {
    if !s.exit_clear {
        return Kinds::default();
    }
    Kinds { history: s.exit_clear_history, cookies: s.exit_clear_cookies, cache: s.exit_clear_cache }
}

/// Whether the open tabs are left out of the saved session.
pub fn forgets_tabs(s: &Settings) -> bool {
    wanted(s).history
}

/// What a start owes: the marker's kinds, or everything chosen when there is
/// no marker to say (it was lost, or the setting was turned on by hand).
fn owed_at_start(marker: Option<Kinds>, wanted: Kinds) -> Kinds {
    marker.unwrap_or(wanted)
}

/// The marker file and what it says (kept in `settings::State`).
#[derive(Default)]
pub struct Marker {
    store: Option<Store<Kinds>>,
    /// What the file said when it was read (None: no readable file).
    found: Option<Kinds>,
    /// Kinds a clear did not finish, owed whatever the setting says now.
    left: Kinds,
    /// The quit's clear ran (the quit is seen on more than one loop pass).
    quit_done: bool,
}

impl Marker {
    /// Reads `exit-clear.json` from `dir` (none: nothing kept).
    pub fn open(dir: Option<&Path>) -> Self {
        let Some(dir) = dir else { return Self::default() };
        let mut store = Store::<Kinds>::new(dir);
        let loaded = store.load();
        let found = (loaded.source != store::Source::Default).then_some(loaded.value);
        Self { store: Some(store), found, ..Self::default() }
    }

    /// Writes what a crash from now on would leave owed.
    fn arm(&mut self, wanted: Kinds) {
        let owed = self.left.union(wanted);
        if let Some(s) = &mut self.store {
            s.save(&owed);
        }
    }

    #[cfg(feature = "selftest")]
    pub fn path(&self) -> Option<&Path> {
        self.store.as_ref().map(|s| s.path())
    }
}

/// Clears `what` now, waiting at most about `wait` for each part. Returns
/// the kinds that did not finish.
pub fn run(app: &mut App, what: Kinds, wait: Duration) -> Kinds {
    let mut failed = Kinds::default();
    if what.history {
        app.closed.clear();
        app.history.clear();
        if !app.history.flush_now(wait) {
            failed.history = true;
        }
    }
    if (what.cookies || what.cache || what.history) && !clear_profile(app, what.native(), wait) {
        failed = failed.union(what);
    }
    failed
}

/// WebView2's part, through any content tab, or a hidden blank one made for
/// it (the pages' profile, never the UI's own).
fn clear_profile(app: &mut App, what: crate::native::ClearKinds, wait: Duration) -> bool {
    let scratch;
    let view = match app.content_view() {
        Some(v) => v,
        None => match scratch_view(app) {
            Some(v) => {
                scratch = v;
                &scratch
            }
            None => return false,
        },
    };
    let result = Rc::new(Cell::new(None));
    let r = result.clone();
    crate::native::clear_browsing_data_of(view, what, move |ok| r.set(Some(ok)));
    crate::native::pump_until(wait, &|| result.get().is_some());
    result.get() == Some(true)
}

/// A hidden content webview with nothing loaded, for when there is no tab.
fn scratch_view(app: &mut App) -> Option<wry::WebView> {
    let builder = wry::WebViewBuilder::new_with_web_context(&mut app.context).with_visible(false);
    #[cfg(windows)]
    let builder = {
        use wry::WebViewBuilderExtWindows;
        builder.with_environment(app.environment.clone())
    };
    match builder.build_as_child(&app.window) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("taser: no webview to clear browsing data through: {e}");
            None
        }
    }
}

/// At start, before any tab is made: clears what the last run left owed
/// and arms the marker for this run. True if the last session's tabs are
/// not to be brought back.
pub fn at_start(app: &mut App) -> bool {
    let wanted = wanted(&app.settings.value);
    let owed = owed_at_start(app.settings.exit_marker.found.take(), wanted);
    if owed.any() {
        if owed.history {
            // `clear` starts the history load: it must wake the app as the
            // UI's own start would have (`UiCommand::Ready`).
            let proxy = app.proxy().clone();
            app.history.on_loaded(move || {
                let _ = proxy.send_event(crate::app::UserEvent::HistoryLoaded);
            });
        }
        let failed = run(app, owed, START_WAIT);
        if failed.any() {
            eprintln!("taser: browsing data not cleared at start ({}); tried again at quit", failed.names());
        }
        app.settings.exit_marker.left = failed;
    }
    app.settings.exit_marker.arm(wanted);
    owed.history || wanted.history
}

/// At quit, after the session's last save: clears what the settings ask
/// with the window already gone, then writes what is still owed.
pub fn at_exit(app: &mut App) {
    if std::mem::replace(&mut app.settings.exit_marker.quit_done, true) {
        return;
    }
    let want = wanted(&app.settings.value).union(app.settings.exit_marker.left);
    if !want.any() {
        return;
    }
    app.window.set_visible(false);
    clear_for_quit(app, want);
}

/// The quit's clear: clears `want`, then writes the marker to say what is
/// still owed (nothing, when it all finished). Returns what did not finish.
pub fn clear_for_quit(app: &mut App, want: Kinds) -> Kinds {
    let failed = run(app, want, EXIT_WAIT);
    if failed.any() {
        eprintln!("taser: browsing data not cleared at quit ({}); cleared at the next start", failed.names());
    }
    let m = &mut app.settings.exit_marker;
    m.left = failed;
    m.arm(Kinds::default());
    if !store::flush(MARKER_FLUSH) {
        eprintln!("taser: {} not written at quit; the next start clears again", Kinds::NAME);
    }
    failed
}

/// The switch or a kind changed: the marker follows, and the session's tabs
/// are saved (or left out) as the history choice now says.
pub fn on_settings_changed(app: &mut App) {
    let wanted = wanted(&app.settings.value);
    app.settings.exit_marker.arm(wanted);
    app.persist.changed();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(history: bool, cookies: bool, cache: bool) -> Settings {
        Settings {
            exit_clear: true,
            exit_clear_history: history,
            exit_clear_cookies: cookies,
            exit_clear_cache: cache,
            ..Settings::default()
        }
    }

    #[test]
    fn nothing_is_wanted_while_the_switch_is_off() {
        let mut s = on(true, true, true);
        s.exit_clear = false;
        assert_eq!(wanted(&s), Kinds::default());
        assert!(!forgets_tabs(&s));
        assert!(!wanted(&Settings::default()).any());
    }

    #[test]
    fn the_chosen_kinds_are_wanted() {
        assert_eq!(wanted(&on(true, false, true)), Kinds { history: true, cookies: false, cache: true });
        assert!(forgets_tabs(&on(true, false, false)));
        assert!(!forgets_tabs(&on(false, true, true)));
        assert!(!wanted(&on(false, false, false)).any());
    }

    #[test]
    fn a_start_owes_the_marker_or_everything_chosen_without_one() {
        let cookies = Kinds { cookies: true, ..Kinds::default() };
        let all = Kinds { history: true, cookies: true, cache: true };
        // A clean quit left nothing owed: nothing to do, whatever is chosen.
        assert!(!owed_at_start(Some(Kinds::default()), all).any());
        // A crash left the kinds armed (even if the setting changed since).
        assert_eq!(owed_at_start(Some(cookies), Kinds::default()), cookies);
        // No marker: what the settings ask.
        assert_eq!(owed_at_start(None, all), all);
        assert!(!owed_at_start(None, Kinds::default()).any());
    }

    #[test]
    fn union_and_names() {
        let a = Kinds { history: true, ..Kinds::default() };
        let b = Kinds { cache: true, ..Kinds::default() };
        assert_eq!(a.union(b), Kinds { history: true, cookies: false, cache: true });
        assert_eq!(a.union(b).names(), "history, cache");
        assert_eq!(Kinds::default().names(), "");
    }

    #[test]
    fn a_hostile_marker_reads_as_owing_nothing_it_does_not_say() {
        let k: Kinds = serde_json::from_str(r#"{"v":1,"history":"yes","cookies":1,"cache":true,"x":[1]}"#).unwrap();
        assert_eq!(k, Kinds { history: false, cookies: false, cache: true });
        let k: Kinds = serde_json::from_str(r#"{"v":1}"#).unwrap();
        assert_eq!(k, Kinds::default());
    }

    #[test]
    fn the_marker_is_armed_and_disarmed_on_disk() {
        let dir = store::tests::temp_dir("exit-clear-marker");
        let m = Marker::open(Some(&dir));
        assert_eq!(m.found, None, "no file yet");

        let mut m = Marker::open(Some(&dir));
        m.arm(Kinds { cookies: true, ..Kinds::default() });
        assert!(store::flush(Duration::from_secs(5)));
        let m2 = Marker::open(Some(&dir));
        assert_eq!(m2.found, Some(Kinds { cookies: true, ..Kinds::default() }));

        // What a clear left is kept armed whatever is wanted now.
        m.left = Kinds { cache: true, ..Kinds::default() };
        m.arm(Kinds::default());
        assert!(store::flush(Duration::from_secs(5)));
        assert_eq!(Marker::open(Some(&dir)).found, Some(Kinds { cache: true, ..Kinds::default() }));

        m.left = Kinds::default();
        m.arm(Kinds::default());
        assert!(store::flush(Duration::from_secs(5)));
        assert_eq!(Marker::open(Some(&dir)).found, Some(Kinds::default()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_marker_counts_as_missing() {
        let dir = store::tests::temp_dir("exit-clear-corrupt");
        std::fs::write(dir.join(Kinds::NAME), b"{not json").unwrap();
        assert_eq!(Marker::open(Some(&dir)).found, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
