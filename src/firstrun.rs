//! First run (v0.3 task 3): uBlock Origin Lite from the Chrome Web Store,
//! installed once without asking (docs/v0.3-design.md §7, B.4).
//!
//! - The state is `first_run_ubol` in `extensions.json` (`State`), so it
//!   shares that file's `.bak` and its reading rules. Missing (a new
//!   profile, or a v0.2 one) reads as `pending`; a value this version does
//!   not know reads as `done`: never install when in doubt.
//! - It starts once the start-up reconcile is over (`on_reconcile_done`),
//!   which itself waits for ui-ready and a content tab, so start-up time is
//!   not touched. It never runs without a data folder (nothing would be
//!   kept, so every start would install again).
//! - At most one try per start (`Session`), with two exceptions: busy (one
//!   more try after the next store install finishes) and no content
//!   webview (one more try once a content tab is made). Any other failure
//!   stays `pending` for the next start.
//! - Removing uBOL by hand (a `Removed` that WebView2 answered for
//!   `extensions::remove`; not Taser's own `remove_quietly`) makes it
//!   `user_removed`, which is never installed again. Removed while this
//!   start's install was running, it still lands once and is taken back out.
//! - The install itself is task 4's (`webstore::install_from_store`): the
//!   seam is `start_install` out and `on_install_result` back
//!   (`webstore::on_installed`). The self-test answers with planned results
//!   instead (`fake`), so it never reaches the network.

use crate::app::App;
use crate::extensions::Extensions;
use serde::{Deserialize, Deserializer, Serialize};

/// uBlock Origin Lite's Chrome Web Store id.
pub const UBOL_ID: &str = "ddkjiahejlhfcafbddmgiahcphecmpfh";

/// Saved as `"first_run_ubol"` in `extensions.json`.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Not installed yet: the next start tries.
    #[default]
    Pending,
    /// Installed once (or found installed): never tried again.
    Done,
    /// The user removed it: never installed again.
    UserRemoved,
}

impl<'de> Deserialize<'de> for State {
    /// Never fails, so a bad value cannot cost the rest of the file: an
    /// unknown value (a newer version's, or damage) is `Done`.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        Ok(match v.as_str() {
            Some("pending") => State::Pending,
            Some("user_removed") => State::UserRemoved,
            _ => State::Done,
        })
    }
}

/// Why an install failed (`webstore::Failure`).
pub type FailureKind = crate::webstore::Failure;

/// What this start has done so far (not saved).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Run {
    #[default]
    NotYet,
    Installing,
    /// Answered busy: one more try after the next store install's result.
    AfterBusy,
    /// Answered no webview: one more try once a content tab is made.
    AfterNoView,
    /// No more tries this start.
    Over,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Session {
    pub run: Run,
    busy_retried: bool,
    view_retried: bool,
    /// See `take_out`.
    take_out: bool,
}

// ------------------------------------------------------------ pure logic

/// Whether to start the install now; if so it is marked started. Pending
/// with uBOL already recorded or already in the profile (installed some
/// other way) becomes done.
pub fn begin(x: &mut Extensions) -> bool {
    if !x.persists() || x.first_run_ubol() != State::Pending {
        return false;
    }
    if x.has_record(UBOL_ID) || x.is_live(UBOL_ID) {
        x.set_first_run_ubol(State::Done);
        return false;
    }
    start_if(x, Run::NotYet)
}

/// Starts again if this start is waiting in `from`.
fn start_if(x: &mut Extensions, from: Run) -> bool {
    let go = x.first_run.run == from && x.first_run_ubol() == State::Pending;
    if go {
        x.first_run.run = Run::Installing;
    }
    go
}

/// Any store install's result. Returns whether to start the install now
/// (the one more try after busy).
pub fn on_result(x: &mut Extensions, id: &str, result: &Result<String, FailureKind>) -> bool {
    if id == UBOL_ID && x.first_run.run == Run::Installing {
        finish(x, result);
        return false;
    }
    // uBOL installed some other way (from its store page) also counts.
    if id == UBOL_ID && result.is_ok() && x.first_run_ubol() == State::Pending {
        x.set_first_run_ubol(State::Done);
    }
    // A busy refusal did not finish an install: still waiting.
    if matches!(result, Err(FailureKind::Busy)) {
        return false;
    }
    start_if(x, Run::AfterBusy)
}

/// Whether this start's own install came in after the user had removed
/// uBOL meanwhile, so it must be taken back out; true once.
pub fn take_out(x: &mut Extensions) -> bool {
    std::mem::take(&mut x.first_run.take_out)
}

/// The result of this start's own install.
fn finish(x: &mut Extensions, result: &Result<String, FailureKind>) {
    let s = &mut x.first_run;
    s.run = Run::Over;
    match result {
        // Removed while it was installing: the state is already
        // user_removed, but this install still lands once, so it is taken
        // back out (`take_out`).
        Ok(_) => {}
        Err(FailureKind::Busy) if !s.busy_retried => {
            s.busy_retried = true;
            s.run = Run::AfterBusy;
        }
        Err(FailureKind::NoView) if !s.view_retried => {
            s.view_retried = true;
            s.run = Run::AfterNoView;
        }
        // Stays pending: the next start tries again.
        Err(e) => eprintln!("taser: uBlock Origin Lite not installed, tried again at the next start: {e:?}"),
    }
    match (result.is_ok(), x.first_run_ubol()) {
        (true, State::Pending) => x.set_first_run_ubol(State::Done),
        (true, State::UserRemoved) => x.first_run.take_out = true,
        _ => {}
    }
}

/// A content webview was made: the one more try after no webview.
pub fn view_created(x: &mut Extensions) -> bool {
    start_if(x, Run::AfterNoView)
}

/// WebView2 answered a removal of `id`.
pub fn removed(x: &mut Extensions, id: &str) {
    if id == UBOL_ID {
        x.set_first_run_ubol(State::UserRemoved);
    }
}

// ------------------------------------------------------------- the app

/// The start-up reconcile is over (`extensions::on_event`).
pub fn on_reconcile_done(app: &mut App) {
    if begin(&mut app.extensions) {
        start_install(app, UBOL_ID);
    }
}

/// A content webview was made (`extensions::on_view_created`).
pub fn on_view_created(app: &mut App) {
    if view_created(&mut app.extensions) {
        start_install(app, UBOL_ID);
    }
}

/// The seam, out: installs `id` from the Chrome Web Store without asking.
/// Its result must always come back, asynchronously, as `on_install_result`.
pub fn start_install(app: &mut App, id: &str) {
    #[cfg(feature = "selftest")]
    fake::start(app, id);
    #[cfg(not(feature = "selftest"))]
    crate::webstore::install_from_store(app, id, false);
}

/// The seam, back: called with every store install's result
/// (`webstore::on_installed`, for each `UserEvent::StoreInstalled`), not
/// only this module's own: a busy first-run install waits for the next one.
/// `Ok` holds the version installed.
pub fn on_install_result(app: &mut App, id: &str, result: Result<String, FailureKind>) {
    if on_result(&mut app.extensions, id, &result) {
        start_install(app, UBOL_ID);
    }
    if take_out(&mut app.extensions) {
        crate::extensions::remove_quietly(app, UBOL_ID);
    }
}

/// WebView2 answered the user's removal of `id` (`extensions::on_event`).
pub fn on_removed(app: &mut App, id: &str) {
    removed(&mut app.extensions, id);
}

/// The self-test's stand-in for the store: each start takes the next
/// planned result, or offline when none is planned, and sends it back as
/// the real install would.
#[cfg(feature = "selftest")]
pub mod fake {
    use super::*;
    use crate::extensions::Event;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    static PLANNED: Mutex<VecDeque<Result<String, FailureKind>>> = Mutex::new(VecDeque::new());
    static STARTS: AtomicU32 = AtomicU32::new(0);

    /// The result the next start gets.
    pub fn plan(result: Result<String, FailureKind>) {
        PLANNED.lock().unwrap_or_else(|e| e.into_inner()).push_back(result);
    }

    /// How many installs were started.
    pub fn starts() -> u32 {
        STARTS.load(Ordering::Relaxed)
    }

    pub(super) fn start(app: &App, id: &str) {
        STARTS.fetch_add(1, Ordering::Relaxed);
        let next = PLANNED.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
        let result = next.unwrap_or_else(|| Err(FailureKind::Net("offline (self-test)".into())));
        send(app, id, result);
    }

    /// Sends a store install's result, as the real install would.
    pub fn send(app: &App, id: &str, result: Result<String, FailureKind>) {
        let _ = app.proxy().send_event(crate::app::UserEvent::Extension(Event::StoreInstalled { id: id.into(), result }));
    }

    /// As if Taser had started again: this start's tries are forgotten,
    /// the saved state is not.
    pub fn next_start(app: &mut App) {
        app.extensions.first_run = Session::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::{ExtFile, FILE};
    use crate::store::tests::temp_dir;
    use crate::store::{Store, flush};
    use std::path::Path;
    use std::time::Duration;

    fn ok() -> Result<String, FailureKind> {
        Ok("2026.1116.1650".into())
    }

    /// The saved state, as the next start reads it.
    fn on_disk(dir: &Path) -> State {
        assert!(flush(Duration::from_secs(2)));
        Store::<ExtFile>::new(dir).load().value.first_run_ubol
    }

    #[test]
    fn the_state_is_read_leniently() {
        let read = |json: &str| serde_json::from_str::<ExtFile>(json).unwrap().first_run_ubol;
        assert_eq!(read(r#"{"items":[]}"#), State::Pending, "missing: a new or a v0.2 profile");
        assert_eq!(read(r#"{"first_run_ubol":"pending"}"#), State::Pending);
        assert_eq!(read(r#"{"first_run_ubol":"done"}"#), State::Done);
        assert_eq!(read(r#"{"first_run_ubol":"user_removed"}"#), State::UserRemoved);
        for unknown in [r#""later""#, "7", "null", "{}", r#""Pending""#] {
            assert_eq!(read(&format!(r#"{{"first_run_ubol":{unknown}}}"#)), State::Done, "{unknown}");
        }
        // A bad state costs only itself.
        let f: ExtFile = serde_json::from_str(r#"{"items":[{"id":7}],"first_run_ubol":[1]}"#).unwrap();
        assert_eq!(f.first_run_ubol, State::Done);
        let text = serde_json::to_string(&ExtFile { first_run_ubol: State::UserRemoved, ..ExtFile::default() }).unwrap();
        assert_eq!(text, r#"{"items":[],"first_run_ubol":"user_removed"}"#);
    }

    #[test]
    fn offline_stays_pending_and_tries_once_per_start() {
        let dir = temp_dir("firstrun-offline");
        let mut x = Extensions::open(Some(&dir));
        assert!(begin(&mut x), "a new profile installs");
        assert!(!begin(&mut x), "not twice in one start");
        assert!(!on_result(&mut x, UBOL_ID, &Err(FailureKind::Net("offline".into()))));
        assert_eq!((x.first_run.run, x.first_run_ubol()), (Run::Over, State::Pending));
        assert!(!begin(&mut x), "no loop after a failure");
        assert!(!on_result(&mut x, "abcdefghijklmnopabcdefghijklmnop", &ok()), "another install does not retry it");
        drop(x);
        assert_eq!(on_disk(&dir), State::Pending);
        // The next start tries again, and succeeds.
        let mut x = Extensions::open(Some(&dir));
        assert!(begin(&mut x));
        on_result(&mut x, UBOL_ID, &ok());
        assert_eq!(x.first_run_ubol(), State::Done);
        drop(x);
        assert_eq!(on_disk(&dir), State::Done);
        let mut x = Extensions::open(Some(&dir));
        assert!(!begin(&mut x), "done: a second start does not download");
    }

    #[test]
    fn every_failure_but_busy_and_no_view_waits_for_the_next_start() {
        for f in [
            FailureKind::Cancelled,
            FailureKind::BadId,
            FailureKind::Net("x".into()),
            FailureKind::Package("x".into()),
            FailureKind::Install("x".into()),
        ] {
            let dir = temp_dir("firstrun-fail");
            let mut x = Extensions::open(Some(&dir));
            assert!(begin(&mut x));
            assert!(!on_result(&mut x, UBOL_ID, &Err(f.clone())), "{f:?}");
            assert_eq!((x.first_run.run, x.first_run_ubol()), (Run::Over, State::Pending), "{f:?}");
            assert!(!on_result(&mut x, "abcdefghijklmnopabcdefghijklmnop", &ok()), "{f:?}");
            assert!(!view_created(&mut x), "{f:?}");
        }
    }

    #[test]
    fn busy_tries_once_more_after_the_next_install() {
        let dir = temp_dir("firstrun-busy");
        let mut x = Extensions::open(Some(&dir));
        assert!(begin(&mut x));
        assert!(!on_result(&mut x, UBOL_ID, &Err(FailureKind::Busy)));
        assert_eq!(x.first_run.run, Run::AfterBusy);
        assert!(!view_created(&mut x), "a new tab is not the signal");
        let other = "abcdefghijklmnopabcdefghijklmnop";
        assert!(on_result(&mut x, other, &Err(FailureKind::Net("x".into()))), "the next result, of any kind");
        assert!(!on_result(&mut x, UBOL_ID, &Err(FailureKind::Busy)), "busy again: no third try");
        assert_eq!((x.first_run.run, x.first_run_ubol()), (Run::Over, State::Pending));
        assert!(!on_result(&mut x, other, &ok()));
    }

    #[test]
    fn no_view_tries_once_more_when_a_tab_is_made() {
        let dir = temp_dir("firstrun-noview");
        let mut x = Extensions::open(Some(&dir));
        assert!(begin(&mut x));
        on_result(&mut x, UBOL_ID, &Err(FailureKind::NoView));
        assert_eq!(x.first_run.run, Run::AfterNoView);
        assert!(!on_result(&mut x, "abcdefghijklmnopabcdefghijklmnop", &ok()), "another install is not the signal");
        assert!(view_created(&mut x));
        assert!(!view_created(&mut x), "already running");
        on_result(&mut x, UBOL_ID, &Err(FailureKind::NoView));
        assert!(!view_created(&mut x), "no third try");
        assert_eq!(x.first_run_ubol(), State::Pending);
    }

    #[test]
    fn a_removed_ubol_is_never_installed_again() {
        let dir = temp_dir("firstrun-removed");
        let mut x = Extensions::open(Some(&dir));
        assert!(begin(&mut x));
        on_result(&mut x, UBOL_ID, &ok());
        removed(&mut x, "abcdefghijklmnopabcdefghijklmnop");
        assert_eq!(x.first_run_ubol(), State::Done, "another extension's removal");
        removed(&mut x, UBOL_ID);
        assert_eq!(x.first_run_ubol(), State::UserRemoved);
        drop(x);
        assert_eq!(on_disk(&dir), State::UserRemoved);
        let mut x = Extensions::open(Some(&dir));
        assert!(!begin(&mut x));
        on_result(&mut x, UBOL_ID, &ok());
        assert_eq!(x.first_run_ubol(), State::UserRemoved, "installed again by hand: still never reinstalled");
        assert!(!take_out(&mut x), "and the user's own install is left alone");
        // Removed while still pending (it never got in): also final.
        let dir = temp_dir("firstrun-removed-pending");
        let mut x = Extensions::open(Some(&dir));
        removed(&mut x, UBOL_ID);
        assert!(!begin(&mut x));
    }

    #[test]
    fn already_recorded_or_installed_by_hand_counts_as_done() {
        let dir = temp_dir("firstrun-recorded");
        let items = serde_json::json!({ "v": 1, "items": [
            { "id": UBOL_ID, "dir": dir.join("extensions").join(UBOL_ID).join("1.0"), "source": "cws", "version": "1.0" },
        ]});
        std::fs::write(dir.join(FILE), items.to_string()).unwrap();
        let mut x = Extensions::open(Some(&dir));
        assert!(!begin(&mut x), "recorded: nothing to download");
        assert_eq!(x.first_run_ubol(), State::Done);
        // Pending, not started this run, and uBOL comes in from its store page.
        let dir = temp_dir("firstrun-by-hand");
        let mut x = Extensions::open(Some(&dir));
        on_result(&mut x, UBOL_ID, &ok());
        assert_eq!(x.first_run_ubol(), State::Done);
        assert!(!begin(&mut x));
        // A failed one from its store page changes nothing.
        let dir = temp_dir("firstrun-by-hand-failed");
        let mut x = Extensions::open(Some(&dir));
        on_result(&mut x, UBOL_ID, &Err(FailureKind::Net("x".into())));
        assert!(begin(&mut x), "still this start's one try");
    }

    #[test]
    fn another_installs_busy_refusal_does_not_use_up_the_retry() {
        let dir = temp_dir("firstrun-busy-other");
        let mut x = Extensions::open(Some(&dir));
        assert!(begin(&mut x));
        assert!(!on_result(&mut x, UBOL_ID, &Err(FailureKind::Busy)));
        let other = "abcdefghijklmnopabcdefghijklmnop";
        assert!(!on_result(&mut x, other, &Err(FailureKind::Busy)), "another install refused: nothing finished");
        assert!(!on_result(&mut x, UBOL_ID, &Err(FailureKind::Busy)), "nor a store-page uBOL refused as busy");
        assert_eq!(x.first_run.run, Run::AfterBusy, "still waiting");
        assert!(on_result(&mut x, other, &ok()), "the install that held it finished");
    }

    #[test]
    fn already_in_the_profile_counts_as_done() {
        let dir = temp_dir("firstrun-live");
        let mut x = Extensions::open(Some(&dir));
        x.set_live_for_test(UBOL_ID);
        assert!(!begin(&mut x), "in the profile, though not recorded: nothing to download");
        assert_eq!(x.first_run_ubol(), State::Done);
    }

    #[test]
    fn removed_while_installing_is_taken_back_out_once() {
        let dir = temp_dir("firstrun-removed-installing");
        let mut x = Extensions::open(Some(&dir));
        assert!(begin(&mut x));
        removed(&mut x, UBOL_ID);
        assert!(!on_result(&mut x, UBOL_ID, &ok()));
        assert_eq!(x.first_run_ubol(), State::UserRemoved, "the removal wins");
        assert!(take_out(&mut x), "but the install landed: take it out");
        assert!(!take_out(&mut x), "once");
        // A failed install after the removal leaves nothing to take out.
        let dir = temp_dir("firstrun-removed-installing-failed");
        let mut x = Extensions::open(Some(&dir));
        assert!(begin(&mut x));
        removed(&mut x, UBOL_ID);
        on_result(&mut x, UBOL_ID, &Err(FailureKind::Net("x".into())));
        assert!(!take_out(&mut x));
    }

    #[test]
    fn nothing_without_a_data_folder() {
        let mut x = Extensions::open(None);
        assert!(!begin(&mut x));
    }
}
