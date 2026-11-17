//! Downloads (design doc, "Not verified": probed). WebView2 saves the file and
//! shows its own flyout: progress, pause/cancel and, once done, "open file"
//! and "open folder" (`native::download`). SmartScreen stays on. This module
//! only counts what happened, so the strip's downloads button appears once
//! the session has had a download, and opens or closes the flyout (Ctrl+J,
//! the button, the menu).
//!
//! The flyout is the page area's (its top-right corner), so it opens on the
//! active tab's webview.

use crate::app::App;
use crate::native;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Event {
    /// A download began (WebView2 shows its flyout by itself).
    Started,
    /// It ended: completed (`ok`) or interrupted. `file` is where it was saved.
    Finished { ok: bool, file: Option<PathBuf> },
}

#[derive(Debug, Default)]
pub struct Downloads {
    /// Downloads begun this session.
    pub started: u32,
    /// Running right now.
    pub active: u32,
    pub completed: u32,
    pub failed: u32,
    /// The file of the last completed one.
    #[cfg_attr(not(feature = "selftest"), allow(dead_code))]
    pub last_file: Option<PathBuf>,
}

pub fn on_event(app: &mut App, ev: Event) {
    let d = &mut app.downloads;
    match ev {
        Event::Started => {
            d.started += 1;
            d.active += 1;
        }
        Event::Finished { ok, file } => {
            d.active = d.active.saturating_sub(1);
            if ok {
                d.completed += 1;
                d.last_file = file;
            } else {
                d.failed += 1;
            }
        }
    }
    sync_ui(app);
}

/// Tells the strip whether to show its downloads button, and whether a
/// download is running.
pub fn sync_ui(app: &App) {
    let d = &app.downloads;
    if d.started > 0 {
        let _ = app.ui.evaluate_script(&format!("xb.downloads(true,{})", d.active > 0));
    }
}

/// Ctrl+J, the strip button, the menu: opens WebView2's download flyout, or
/// closes it when it is open. False when there is no page to hang it on.
pub fn toggle(app: &mut App) -> bool {
    crate::overlay::close(app);
    // The flyout hangs under the strip's corner, so fullscreen gives way (as
    // for the address bar and the history list).
    if app.fullscreen.on() {
        crate::page_tools::leave_fullscreen(app);
    }
    let Some(view) = app.active_view() else { return false };
    native::toggle_downloads(view)
}
