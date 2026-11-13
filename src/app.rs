//! The browser: one window, one UI webview, one content webview per tab.
//! All state lives here; webviews only send events in and get state pushed out.

use crate::commands::{Shortcut, TopEdge, UiCommand};
use crate::native::{self, Proxy};
use crate::tabs::{Placement, TabId, Tabs};
use crate::ui;
use crate::url::{self, Resolved};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::window::{ResizeDirection, Theme, Window};
use wry::{Rect, WebContext, WebView, WebViewBuilder};

/// Height of the UI strip in logical pixels. Must match `--strip-h` in the CSS.
pub const UI_HEIGHT: f64 = 40.0;

#[derive(Debug)]
pub enum UserEvent {
    /// From the UI webview (already origin-checked and parsed).
    Ui(UiCommand),
    /// From a native key event in any of our webviews.
    Shortcut(Shortcut),
    /// From a content webview. Untrusted: only updates that tab's display state.
    Page(TabId, PageEvent),
    /// A page asked for a new window (user-initiated, http/https only).
    OpenTab { opener: TabId, url: String },
    /// A WebView2 process died or hung. `tab` is `None` for the UI webview.
    ProcessFailed { tab: Option<TabId>, failure: Failure },
    #[cfg(feature = "selftest")]
    Test(u32),
}

#[derive(Debug)]
pub enum PageEvent {
    Title(String),
    Url(String),
    Loading(bool),
    History { back: bool, forward: bool },
    /// The page called `window.close()` (and WebView2 allowed it).
    CloseRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The shared WebView2 browser process exited; every webview is dead.
    Browser,
    RendererExited,
    RendererUnresponsive,
}

/// Shown when the WebView2 browser process dies under us.
const BROWSER_DIED: &str = "X-Browser beklenmedik bir hatayla karşılaştı: Microsoft Edge WebView2 \
                            tarayıcı işlemi kapandı.\n\nUygulama kapanacak. Yeniden başlatmanız yeterlidir.";

/// Shown when the UI renderer keeps dying or hanging, so reloading it is futile.
const UI_FAILED: &str = "X-Browser beklenmedik bir hatayla karşılaştı: arayüz işlemi kısa sürede \
                         defalarca çöktü veya yanıt vermedi.\n\nUygulama kapanacak. Yeniden başlatmanız yeterlidir.";

/// The UI is reloaded after a renderer failure, but not after this many
/// failures within `UI_FAILURE_WINDOW`.
const UI_FAILURE_LIMIT: usize = 3;
const UI_FAILURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// Records a UI renderer failure at `now` in `recent` (forgetting ones older
/// than `UI_FAILURE_WINDOW`). True when this one reaches `UI_FAILURE_LIMIT`.
fn ui_failed_too_often(recent: &mut Vec<std::time::Instant>, now: std::time::Instant) -> bool {
    recent.retain(|t| now.saturating_duration_since(*t) < UI_FAILURE_WINDOW);
    recent.push(now);
    recent.len() >= UI_FAILURE_LIMIT
}

// Field order is drop order: every webview goes before the WebView2
// environment and context, and all of them before the window that hosts them
// (dropping the window first would destroy the webviews' host windows under wry).
pub struct App {
    /// Content webviews, same order as nothing in particular; looked up by id.
    pub views: Vec<(TabId, WebView)>,
    pub ui: WebView,
    #[cfg(windows)]
    environment: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment,
    context: WebContext,
    pub window: Window,
    pub tabs: Tabs,
    proxy: Proxy,
    /// Last caption given to the window, so it is only set when it changes.
    title: String,
    #[cfg(feature = "selftest")]
    pub title_sets: u32,
    /// Set when the app must quit with an error dialog (after the loop ends).
    pub fatal: Option<&'static str>,
    /// Recent UI renderer failures (see `ui_failed_too_often`).
    ui_failures: Vec<std::time::Instant>,
    dirty: bool,
    pub ui_ready: bool,
    /// Tab shown right now (may lag `tabs.active()` until `sync_visibility`).
    shown: Option<TabId>,
    /// Views still visible behind the shown one; hidden by `flush`.
    to_hide: Vec<TabId>,
    /// Keyboard focus follows a tab switch at the end of the pass (`flush`).
    focus_pending: bool,
    pub quit: bool,
}

/// Palette background, so nothing flashes white in dark mode before first paint.
pub fn background(theme: Theme) -> (u8, u8, u8, u8) {
    match theme {
        Theme::Dark => (0x1c, 0x1c, 0x1e, 0xff),
        _ => (0xf4, 0xf4, 0xf5, 0xff),
    }
}

fn data_dir() -> Option<std::path::PathBuf> {
    // Keep profile data out of the exe folder (which may be read-only).
    std::env::var_os("LOCALAPPDATA").map(|d| std::path::Path::new(&d).join("X-Browser"))
}

impl App {
    pub fn new(window: Window, proxy: Proxy) -> wry::Result<Self> {
        let mut context = WebContext::new(data_dir());
        let size = window.inner_size().to_logical::<f64>(window.scale_factor());
        let build = |context: &mut WebContext| {
            ui::build(&window, context, &proxy, ui_rect(size), background(window.theme()))
        };
        // Right after a previous instance exits, its WebView2 browser
        // processes may still hold the profile for a moment (ERROR_BUSY).
        // Retry briefly instead of failing to start.
        let mut ui = build(&mut context);
        let mut tries = 0;
        // (After a crash / hard kill the lock lasts ~30 s; then main() shows
        // an explanatory dialog rather than a blank window.)
        while tries < 25 && ui.as_ref().is_err_and(|e| native::profile_busy(e).is_some()) {
            tries += 1;
            native::destroy_children(&window);
            std::thread::sleep(std::time::Duration::from_millis(100));
            ui = build(&mut context);
        }
        if tries > 0 {
            crate::timing::mark(&format!("profile-busy-retries-{tries}"));
        }
        let ui = ui?;
        #[cfg(windows)]
        {
            native::attach_accelerators(&ui, &proxy)?;
            native::attach_process_failed(&ui, None, &proxy)?;
        }
        Ok(Self {
            #[cfg(windows)]
            environment: wry::WebViewExtWindows::environment(&ui),
            title: window.title(),
            #[cfg(feature = "selftest")]
            title_sets: 0,
            fatal: None,
            ui_failures: Vec::new(),
            window,
            ui,
            views: Vec::new(),
            tabs: Tabs::new(),
            proxy,
            context,
            dirty: true,
            ui_ready: false,
            shown: None,
            to_hide: Vec::new(),
            focus_pending: false,
            quit: false,
        })
    }

    fn size(&self) -> LogicalSize<f64> {
        self.window.inner_size().to_logical(self.window.scale_factor())
    }

    pub fn view(&self, id: TabId) -> Option<&WebView> {
        self.views.iter().find(|(i, _)| *i == id).map(|(_, v)| v)
    }

    fn active_view(&self) -> Option<&WebView> {
        self.view(self.tabs.active()?)
    }

    /// Creates a tab + webview. Returns the new id.
    pub fn open_tab(&mut self, url: &str, placement: Placement, activate: bool) -> wry::Result<TabId> {
        let id = self.tabs.add(url, placement);
        let p = self.proxy.clone();
        let bounds = content_rect(self.size());
        // No `with_url`: the content settings below must be in place before
        // the first navigation starts, so `load_url` comes last.
        let builder = WebViewBuilder::new_with_web_context(&mut self.context)
            .with_bounds(bounds)
            .with_visible(false)
            .with_document_title_changed_handler(move |title| {
                let _ = p.send_event(UserEvent::Page(id, PageEvent::Title(title)));
            });
        #[cfg(windows)]
        let builder = {
            use wry::WebViewBuilderExtWindows;
            // Reusing the UI's environment skips creating a new one per tab.
            builder.with_environment(self.environment.clone())
        };
        #[cfg(not(windows))]
        let builder = {
            let p = self.proxy.clone();
            builder.with_on_page_load_handler(move |ev, url| {
                let loading = matches!(ev, wry::PageLoadEvent::Started);
                let _ = p.send_event(UserEvent::Page(id, PageEvent::Loading(loading)));
                let _ = p.send_event(UserEvent::Page(id, PageEvent::Url(url)));
            })
        };
        let view = match builder.build_as_child(&self.window) {
            Ok(v) => v,
            Err(e) => {
                self.tabs.close(id);
                return Err(e);
            }
        };
        let setup = |view: &WebView| -> wry::Result<()> {
            #[cfg(windows)]
            {
                native::harden_content(view)?;
                native::attach_content_events(view, id, &self.proxy)?;
                native::attach_accelerators(view, &self.proxy)?;
            }
            view.load_url(url)
        };
        if let Err(e) = setup(&view) {
            // No half-wired tab: without its events it would never update.
            drop(view);
            self.tabs.close(id);
            return Err(e);
        }
        self.views.push((id, view));
        if activate {
            self.tabs.activate(id);
        }
        self.sync_visibility();
        self.dirty = true;
        Ok(id)
    }

    pub fn close_tab(&mut self, id: TabId) {
        let was_active = self.tabs.active();
        if !self.tabs.close(id) {
            return;
        }
        if let Some(i) = self.views.iter().position(|(v, _)| *v == id) {
            // Hide first so the removal never flashes; drop destroys the webview.
            let (_, view) = self.views.remove(i);
            let _ = view.set_visible(false);
            drop(view);
        }
        if self.shown == Some(id) {
            self.shown = None;
        }
        if self.tabs.is_empty() {
            self.quit = true;
            return;
        }
        self.sync_visibility();
        // Closing a background tab must not pull focus out of the page.
        if self.tabs.active() != was_active {
            self.focus_pending = true;
        }
        self.dirty = true;
    }

    pub fn activate(&mut self, id: TabId) {
        if self.tabs.active() != Some(id) && self.tabs.activate(id) {
            self.sync_visibility();
            self.focus_pending = true;
            self.dirty = true;
        }
    }

    /// Shows the active tab's webview on top of the previously shown one.
    /// Hiding the old one costs ~12 ms in WebView2, so it is deferred to
    /// `flush`, after the new tab is visible and focused and the UI updated.
    /// Bounds are only kept current for the visible view; a view about to be
    /// shown is resized first, so it never appears at a stale size.
    fn sync_visibility(&mut self) {
        let want = self.tabs.active();
        if want == self.shown {
            return;
        }
        let rect = content_rect(self.size());
        if let Some(v) = want.and_then(|id| self.view(id)) {
            let _ = v.set_bounds(rect);
            native::raise(v);
            let _ = v.set_visible(true);
        }
        if let Some(old) = self.shown {
            self.to_hide.push(old);
        }
        self.to_hide.retain(|id| Some(*id) != want);
        self.shown = want;
    }

    /// Hides views that were left visible behind the active one.
    pub fn hide_stale(&mut self) {
        for id in std::mem::take(&mut self.to_hide) {
            if Some(id) != self.shown
                && let Some(v) = self.view(id)
            {
                let _ = v.set_visible(false);
            }
        }
    }

    fn focus_after_switch(&self) {
        let blank = self
            .tabs
            .active()
            .and_then(|id| self.tabs.get(id))
            .is_some_and(|t| t.url == url::BLANK);
        if blank {
            self.focus_address();
        } else if let Some(v) = self.active_view() {
            let _ = v.focus();
        }
    }

    fn focus_address(&self) {
        let _ = self.ui.focus();
        let _ = self.ui.evaluate_script("xb.focusAddress()");
    }

    pub fn layout(&self) {
        let size = self.size();
        let _ = self.ui.set_bounds(ui_rect(size));
        if let Some(v) = self.active_view() {
            let _ = v.set_bounds(content_rect(size));
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// End of an event-loop pass. A tab switch has already put the new view
    /// on screen; the slower follow-up work happens here, in order of
    /// visibility: push state to the UI, move keyboard focus, then hide the
    /// previously shown view(s).
    pub fn flush(&mut self) {
        if self.dirty {
            self.update_title();
        }
        if self.dirty && self.ui_ready {
            self.dirty = false;
            let js = format!("xb.render({},{})", self.tabs.to_json(), self.window.is_maximized());
            let _ = self.ui.evaluate_script(&js);
        }
        if std::mem::take(&mut self.focus_pending) {
            self.focus_after_switch();
        }
        self.hide_stale();
    }

    /// Window caption (taskbar, Alt+Tab) follows the active tab.
    fn update_title(&mut self) {
        let title = crate::tabs::window_title(self.tabs.active().and_then(|id| self.tabs.get(id)));
        if title != self.title {
            self.window.set_title(&title);
            self.title = title;
            #[cfg(feature = "selftest")]
            {
                self.title_sets += 1;
            }
        }
    }

    fn on_process_failed(&mut self, tab: Option<TabId>, failure: Failure) {
        match (failure, tab) {
            // Every webview reports it; the first report decides.
            (Failure::Browser, _) => {
                if self.fatal.is_none() && !self.quit {
                    self.fatal = Some(BROWSER_DIED);
                }
                self.quit = true;
            }
            // The UI has no state of its own: reload it; it sends `ready`
            // again and gets the current state re-rendered. Unless it keeps
            // failing: then the app gives up instead of reloading forever.
            (_, None) => {
                if ui_failed_too_often(&mut self.ui_failures, std::time::Instant::now()) {
                    if self.fatal.is_none() && !self.quit {
                        self.fatal = Some(UI_FAILED);
                    }
                    self.quit = true;
                    return;
                }
                self.ui_ready = false;
                self.dirty = true;
                let _ = self.ui.reload();
            }
            // A dead page shows WebView2's error page; the tab stays, but
            // must not keep spinning.
            (_, Some(id)) => {
                if let Some(t) = self.tabs.get_mut(id) {
                    t.loading = false;
                    self.dirty = true;
                }
            }
        }
    }

    pub fn handle(&mut self, event: UserEvent) {
        match event {
            UserEvent::Ui(cmd) => self.on_ui(cmd),
            UserEvent::Shortcut(sc) => self.on_shortcut(sc),
            UserEvent::Page(id, ev) => self.on_page(id, ev),
            UserEvent::OpenTab { opener, url } => {
                if self.tabs.get(opener).is_some() {
                    let _ = self.open_tab(&url, Placement::After(opener), true);
                    if let Some(v) = self.active_view() {
                        let _ = v.focus();
                    }
                }
            }
            UserEvent::ProcessFailed { tab, failure } => self.on_process_failed(tab, failure),
            #[cfg(feature = "selftest")]
            UserEvent::Test(step) => crate::selftest::step(self, step),
        }
    }

    fn on_ui(&mut self, cmd: UiCommand) {
        match cmd {
            UiCommand::Ready => {
                self.ui_ready = true;
                self.dirty = true;
                crate::timing::mark("ui-ready");
                if crate::timing::quit_when_ready() {
                    self.quit = true;
                }
                if self.tabs.active().and_then(|id| self.tabs.get(id)).is_some_and(|t| t.url == url::BLANK) {
                    self.focus_address();
                }
            }
            UiCommand::NewTab => self.new_blank_tab(),
            UiCommand::CloseTab(id) => self.close_tab(id),
            UiCommand::Activate(id) => self.activate(id),
            UiCommand::Navigate(text) => self.navigate(&text),
            UiCommand::Back => self.with_active(|v| v.go_back()),
            UiCommand::Forward => self.with_active(|v| v.go_forward()),
            UiCommand::Reload => self.with_active(|v| v.reload()),
            UiCommand::Stop => {
                if let Some(v) = self.active_view() {
                    native::stop(v);
                }
            }
            UiCommand::Minimize => self.window.set_minimized(true),
            UiCommand::ToggleMaximize => {
                self.window.set_maximized(!self.window.is_maximized());
                self.dirty = true;
            }
            UiCommand::CloseWindow => self.quit = true,
            UiCommand::DragWindow => {
                let _ = self.window.drag_window();
            }
            UiCommand::ResizeTop(edge) => {
                if !self.window.is_maximized() {
                    let dir = match edge {
                        TopEdge::Left => ResizeDirection::NorthWest,
                        TopEdge::Middle => ResizeDirection::North,
                        TopEdge::Right => ResizeDirection::NorthEast,
                    };
                    let _ = self.window.drag_resize_window(dir);
                }
            }
            UiCommand::FocusContent => {
                if let Some(v) = self.active_view() {
                    let _ = v.focus();
                }
            }
        }
    }

    fn on_shortcut(&mut self, sc: Shortcut) {
        match sc {
            Shortcut::NewTab => self.new_blank_tab(),
            Shortcut::CloseTab => {
                if let Some(id) = self.tabs.active() {
                    self.close_tab(id);
                }
            }
            Shortcut::FocusAddress => self.focus_address(),
            Shortcut::Reload => self.with_active(|v| v.reload()),
            Shortcut::NextTab | Shortcut::PrevTab => {
                let step = if sc == Shortcut::NextTab { 1 } else { -1 };
                if let Some(id) = self.tabs.cycle(step) {
                    self.activate(id);
                }
            }
            Shortcut::Back => self.with_active(|v| v.go_back()),
            Shortcut::Forward => self.with_active(|v| v.go_forward()),
        }
    }

    fn on_page(&mut self, id: TabId, ev: PageEvent) {
        if let PageEvent::CloseRequested = ev {
            self.close_tab(id);
            return;
        }
        let Some(tab) = self.tabs.get_mut(id) else { return };
        match ev {
            PageEvent::CloseRequested => {}
            PageEvent::Title(t) => tab.title = t.chars().take(512).collect(),
            PageEvent::Url(u) => tab.url = u.chars().take(8192).collect(),
            PageEvent::Loading(l) => tab.loading = l,
            PageEvent::History { back, forward } => {
                tab.can_back = back;
                tab.can_forward = forward;
            }
        }
        self.dirty = true;
    }

    fn new_blank_tab(&mut self) {
        if self.open_tab(url::BLANK, Placement::End, true).is_ok() {
            self.focus_address();
        }
    }

    fn navigate(&mut self, text: &str) {
        match url::resolve_input(text) {
            Resolved::Empty => {}
            Resolved::Blocked => {
                let _ = self.ui.evaluate_script("xb.blocked()");
            }
            Resolved::Url(u) => {
                let Some(id) = self.tabs.active() else { return };
                let Some(v) = self.view(id) else { return };
                // WebView2 rejects some URLs outright (E_INVALIDARG); then the
                // tab keeps what it shows and the UI says so.
                if v.load_url(&u).is_err() {
                    let _ = self.ui.evaluate_script("xb.blocked()");
                    return;
                }
                let _ = v.focus();
                if let Some(t) = self.tabs.get_mut(id) {
                    t.url = u;
                    t.loading = true;
                }
                self.dirty = true;
            }
        }
    }

    fn with_active(&self, f: impl FnOnce(&WebView) -> wry::Result<()>) {
        if let Some(v) = self.active_view() {
            let _ = f(v);
        }
    }

    pub fn on_theme(&self, theme: Theme) {
        self.window.set_background_color(Some(background(theme)));
        let _ = self.ui.set_background_color(background(theme));
    }
}

pub fn ui_rect(size: LogicalSize<f64>) -> Rect {
    Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: LogicalSize::new(size.width, UI_HEIGHT.min(size.height)).into(),
    }
}

pub fn content_rect(size: LogicalSize<f64>) -> Rect {
    Rect {
        position: LogicalPosition::new(0.0, UI_HEIGHT).into(),
        size: LogicalSize::new(size.width, (size.height - UI_HEIGHT).max(0.0)).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn ui_reload_gives_up_on_third_failure_within_ten_seconds() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut recent = Vec::new();
        assert!(!ui_failed_too_often(&mut recent, t0));
        assert!(!ui_failed_too_often(&mut recent, t0 + s(4)));
        assert!(ui_failed_too_often(&mut recent, t0 + s(9)));
    }

    #[test]
    fn ui_failures_spread_out_keep_reloading() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut recent = Vec::new();
        // One failure every 6 s: never three inside a 10 s window.
        for i in 0..10 {
            assert!(!ui_failed_too_often(&mut recent, t0 + s(6 * i)), "failure {i}");
        }
        // Exactly 10 s apart counts as outside the window.
        let mut recent = Vec::new();
        assert!(!ui_failed_too_often(&mut recent, t0));
        assert!(!ui_failed_too_often(&mut recent, t0 + s(5)));
        assert!(!ui_failed_too_often(&mut recent, t0 + s(10)));
        assert!(ui_failed_too_often(&mut recent, t0 + s(11)));
    }
}
