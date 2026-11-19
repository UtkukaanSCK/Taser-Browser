//! The browser: one window, one UI webview, one content webview per tab.
//! All state lives here; webviews only send events in and get state pushed out.

use crate::commands::{Shortcut, TopEdge, UiCommand};
use crate::native::{self, Proxy};
use crate::page_tools::{self, Fullscreen};
use crate::strip::{self, ClosedTabs};
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
    /// A page asked for a new window (user-initiated, http/https only);
    /// `background` for Ctrl+click and middle click.
    OpenTab { opener: TabId, url: String, background: bool },
    /// A WebView2 process died or hung. `tab` is `None` for the UI webview.
    ProcessFailed { tab: Option<TabId>, failure: Failure },
    /// The fullscreen notice's timer ran out (see `page_tools`).
    NoticeTimeout(u32),
    /// From a content webview's policy hooks (see `policy`). Untrusted.
    Policy(TabId, crate::policy::Event),
    /// X-Browser was started again (see `instance`): open this checked web
    /// address in a new tab, or with None just come to the front.
    OpenFromOtherInstance(Option<String>),
    /// The history file has been read in the background (see `history`).
    HistoryLoaded,
    /// The bookmarks file has been read in the background (see `bookmarks`).
    BookmarksLoaded,
    /// WebView2's download flyout state changed (see `downloads`).
    Download(crate::downloads::Event),
    /// A WebView2 extension call has answered (see `extensions`).
    Extension(crate::extensions::Event),
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
    /// The page's icon as a checked PNG `data:` URL (see `native::png_data_url`);
    /// None while a new document has none yet.
    Favicon(Option<String>),
    Audio { playing: bool, muted: bool },
    /// The tab's zoom factor changed (keys, indicator, Ctrl+wheel).
    Zoom(f64),
    /// A page element entered (true) or left element fullscreen.
    Fullscreen(bool),
    /// A navigation started; `user`: WebView2 says the user started it
    /// (a link click), as opposed to a script or redirect.
    Navigating { nav: u64, user: bool },
    /// Navigation `nav` completed successfully: a history entry (checked there).
    Visited { url: String, title: String, nav: u64 },
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
    /// Suggestions and the history list (see `overlay`), made on first use.
    pub overlay: crate::overlay::Overlay,
    /// An extension's popup, made when it opens (see `popup`).
    pub popup: crate::popup::Popup,
    #[cfg(windows)]
    pub(crate) environment: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment,
    pub(crate) context: WebContext,
    pub window: Window,
    pub tabs: Tabs,
    /// Ctrl+Shift+T stack (see `strip`).
    pub closed: ClosedTabs,
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
    /// F11 / page fullscreen (see `page_tools`).
    pub fullscreen: Fullscreen,
    /// session.json / window.json (see `session`).
    pub persist: crate::session::Persist,
    /// history.jsonl (see `history`).
    pub history: crate::history::History,
    /// bookmarks.json (see `bookmarks`).
    pub bookmarks: crate::bookmarks::Bookmarks,
    /// settings.json (see `settings`).
    pub settings: crate::settings::State,
    /// Downloads of this session (see `downloads`).
    pub downloads: crate::downloads::Downloads,
    /// Certificate errors, crashed tabs, external launches (see `policy`).
    pub policy: crate::policy::State,
    /// extensions.json and the content profile's extensions (see `extensions`).
    pub extensions: crate::extensions::Extensions,
    /// The Uzantılar page and the strip's extensions button (see `ext_ui`).
    pub ext_ui: crate::ext_ui::ExtUi,
    /// Addresses from other starts, to drop a late duplicate.
    handovers: crate::instance::Repeats,
    pub quit: bool,
}

/// Palette background, so nothing flashes white in dark mode before first paint.
pub fn background(theme: Theme) -> (u8, u8, u8, u8) {
    match theme {
        Theme::Dark => (0x1c, 0x1c, 0x1e, 0xff),
        _ => (0xf4, 0xf4, 0xf5, 0xff),
    }
}

impl App {
    pub fn new(window: Window, proxy: Proxy) -> wry::Result<Self> {
        let mut context = WebContext::new(crate::store::data_dir());
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
            closed: ClosedTabs::default(),
            proxy,
            context,
            dirty: true,
            ui_ready: false,
            shown: None,
            to_hide: Vec::new(),
            focus_pending: false,
            handovers: crate::instance::Repeats::default(),
            fullscreen: Fullscreen::default(),
            persist: Default::default(),
            history: Default::default(),
            bookmarks: Default::default(),
            settings: Default::default(),
            downloads: Default::default(),
            overlay: Default::default(),
            popup: Default::default(),
            policy: Default::default(),
            extensions: Default::default(),
            ext_ui: Default::default(),
            quit: false,
        })
    }

    fn size(&self) -> LogicalSize<f64> {
        self.window.inner_size().to_logical(self.window.scale_factor())
    }

    pub fn view(&self, id: TabId) -> Option<&WebView> {
        self.views.iter().find(|(i, _)| *i == id).map(|(_, v)| v)
    }

    pub(crate) fn active_view(&self) -> Option<&WebView> {
        self.view(self.tabs.active()?)
    }

    /// A webview of the content profile (the tabs' one; the UI has its own,
    /// `ui::UI_PROFILE`), for profile-wide calls: the active tab's, else any.
    pub fn content_view(&self) -> Option<&WebView> {
        self.active_view().or_else(|| self.views.first().map(|(_, v)| v))
    }

    pub fn proxy(&self) -> &Proxy {
        &self.proxy
    }

    /// Where the page goes: below the strip, or the whole window in fullscreen.
    pub fn content_bounds(&self) -> Rect {
        if self.fullscreen.on() { full_rect(self.size()) } else { content_rect(self.size()) }
    }

    /// Creates a tab + webview. Returns the new id.
    ///
    /// Every tab opened here was asked for (a link the user clicked, an
    /// address from another program or the command line, a bookmark, a
    /// reopened tab), so its first load counts as the user's visit (see
    /// `history`); the last session's tabs come back through `session` and
    /// do not.
    pub fn open_tab(&mut self, url: &str, placement: Placement, activate: bool) -> wry::Result<TabId> {
        let id = self.tabs.add(url, placement);
        if let Some(t) = self.tabs.get_mut(id) {
            t.user_nav_next = true;
        }
        if let Err(e) = self.create_view(id) {
            self.tabs.close(id);
            return Err(e);
        }
        if activate {
            self.tabs.activate(id);
        }
        self.sync_visibility();
        self.dirty = true;
        Ok(id)
    }

    /// Builds tab `id`'s webview and starts loading the tab's URL at its
    /// zoom. Tabs restored from the last session get theirs here on first
    /// activation (`sync_visibility`).
    fn create_view(&mut self, id: TabId) -> wry::Result<()> {
        let Some((url, zoom)) = self.tabs.get(id).map(|t| (t.url.clone(), t.zoom)) else { return Ok(()) };
        let url = url.as_str();
        let p = self.proxy.clone();
        let bounds = self.content_bounds();
        // No `with_url`: the content settings below must be in place before
        // the first navigation starts, so `load_url` comes last.
        let builder = WebViewBuilder::new_with_web_context(&mut self.context)
            .with_bounds(bounds)
            .with_visible(false)
            .with_initialization_script_for_main_only(page_tools::NO_KEYBOARD_LOCK, false)
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
        // A new webview numbers its navigations afresh: an old id would pass
        // for a redirect. And if it cannot be made, no navigation of it
        // takes the address bar's "the user asked" mark later.
        let fresh = |tabs: &mut crate::tabs::Tabs, asked: bool| {
            if let Some(t) = tabs.get_mut(id) {
                t.user_nav = None;
                t.user_nav_next &= asked;
            }
        };
        let view = match builder.build_as_child(&self.window) {
            Ok(view) => view,
            Err(e) => {
                fresh(&mut self.tabs, false);
                return Err(e);
            }
        };
        fresh(&mut self.tabs, true);
        let setup = |view: &WebView| -> wry::Result<()> {
            #[cfg(windows)]
            {
                native::harden_content(view)?;
                native::attach_content_events(view, id, &self.proxy)?;
                native::attach_accelerators(view, &self.proxy)?;
                native::attach_downloads(view, &self.proxy)?;
                let _ = native::apply_content_settings(view, self.settings.value.autofill);
            }
            view.load_url(url)
        };
        // No half-wired tab: without its events it would never update.
        setup(&view)?;
        self.views.push((id, view));
        // Profile-wide work that found no content webview at start.
        crate::settings::on_view_created(self);
        crate::extensions::on_view_created(self);
        if zoom != 100 {
            page_tools::set_zoom(self, id, f64::from(zoom) / 100.0);
        }
        Ok(())
    }

    /// Shows the active tab (creating its webview if it has none yet).
    pub fn show_active(&mut self) {
        self.sync_visibility();
        self.dirty = true;
    }

    /// Closes tab `id`. `remember`: whether Ctrl+Shift+T may bring it back,
    /// true for the user's own closes (button, Ctrl+W, middle click), false
    /// when the page closed itself (window.close()).
    pub fn close_tab(&mut self, id: TabId, remember: bool) {
        let was_active = self.tabs.active();
        if remember {
            self.closed.remember(&self.tabs, id);
        }
        if self.tabs.len() == 1 && self.tabs.get(id).is_some() {
            // Closing the last tab quits: the next start brings that tab back.
            crate::session::save(self);
        }
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
        page_tools::on_switch(self, want);
        crate::overlay::close(self);
        crate::popup::close(self, false);
        crate::policy::on_switch(self);
        // A restored tab comes alive on first activation. If its webview
        // cannot be made, it is treated like a tab whose renderer failed: it
        // stays in the strip with its saved title and address, not loading,
        // over empty content (not closed, so nothing saved is lost). Leaving
        // the tab and coming back tries again.
        if let Some(id) = want
            && self.view(id).is_none()
            && let Err(e) = self.create_view(id)
        {
            eprintln!("x-browser: cannot create the webview of tab {id}: {e}");
            if let Some(t) = self.tabs.get_mut(id) {
                t.loading = false;
            }
            self.dirty = true;
        }
        let rect = self.content_bounds();
        if let Some(v) = want.and_then(|id| self.view(id)) {
            let _ = v.set_bounds(rect);
            native::raise(v);
            let _ = v.set_visible(true);
        }
        // The fullscreen notice stays above whichever page is shown.
        if self.fullscreen.notice_up() {
            native::raise(&self.ui);
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

    pub fn focus_after_switch(&self) {
        let blank = self
            .tabs
            .active()
            .and_then(|id| self.tabs.get(id))
            .is_some_and(|t| t.url == url::BLANK);
        // In fullscreen the strip is hidden: focus stays with the page.
        if blank && !self.fullscreen.on() {
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
        // While fullscreen the UI webview is hidden, or shown only as the
        // notice band.
        let ui = if self.fullscreen.on() && self.fullscreen.notice_up() { notice_rect(size) } else { ui_rect(size) };
        let _ = self.ui.set_bounds(ui);
        if let Some(v) = self.active_view() {
            let _ = v.set_bounds(self.content_bounds());
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
            // Whatever the strip shows may be session state; identical
            // saves are skipped (see `store`).
            self.persist.changed();
        }
        if self.dirty && self.ui_ready {
            self.dirty = false;
            let js = format!("xb.render({},{});{}", self.tabs.render_json(), self.window.is_maximized(), crate::policy::render_js(self));
            if self.ui.evaluate_script(&js).is_ok() {
                self.tabs.icons_sent();
            }
        }
        if std::mem::take(&mut self.focus_pending) {
            self.focus_after_switch();
        }
        self.hide_stale();
        crate::session::save_if_due(self);
        self.history.flush_if_due(std::time::Instant::now());
        // However the load got merged (`HistoryLoaded`, or a flush that
        // polled first), a list or suggestions shown meanwhile lacked the file.
        if self.history.take_merged() {
            crate::overlay::refresh(self);
        }
        crate::bookmarks::sync_star(self);
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
                crate::overlay::on_ui_failed(self);
                let _ = self.ui.reload();
            }
            // A dead page shows WebView2's error page; the tab stays, but
            // must not keep spinning, nor keep the screen.
            (_, Some(id)) => {
                if let Some(t) = self.tabs.get_mut(id) {
                    t.loading = false;
                    self.dirty = true;
                }
                page_tools::on_page_failed(self, id);
                crate::policy::on_page_failed(self, id, failure);
            }
        }
    }

    pub fn handle(&mut self, event: UserEvent) {
        match event {
            UserEvent::Ui(cmd) => self.on_ui(cmd),
            UserEvent::Shortcut(sc) => self.on_shortcut(sc),
            UserEvent::Page(id, ev) => self.on_page(id, ev),
            UserEvent::OpenTab { opener, url, background } => {
                // Checked where it is raised too; over-long URLs are refused, not cut.
                if self.tabs.get(opener).is_some() && crate::url::is_tab_url(&url) {
                    let _ = self.open_tab(&url, Placement::After(opener), !background);
                    if !background && let Some(v) = self.active_view() {
                        let _ = v.focus();
                    }
                }
            }
            UserEvent::ProcessFailed { tab, failure } => self.on_process_failed(tab, failure),
            UserEvent::NoticeTimeout(generation) => page_tools::on_notice_timeout(self, generation),
            // Only a wake-up: `flush`, at the end of this pass, merges the
            // load and redraws what it changes.
            // (The star the same way: `bookmarks::sync_star` takes the file.)
            UserEvent::HistoryLoaded | UserEvent::BookmarksLoaded => {}
            UserEvent::Download(ev) => crate::downloads::on_event(self, ev),
            UserEvent::Extension(ev) => {
                crate::extensions::on_event(self, ev);
                crate::ext_ui::on_changed(self);
            }
            UserEvent::Policy(tab, ev) => crate::policy::on_event(self, tab, ev),
            UserEvent::OpenFromOtherInstance(url) => {
                if let Some(url) = url
                    && crate::url::is_tab_url(&url)
                    && self.handovers.fresh(&url, std::time::Instant::now())
                {
                    // The strip (and the new tab in it) must be on screen.
                    if self.fullscreen.on() {
                        page_tools::leave_fullscreen(self);
                    }
                    if self.open_tab(&url, Placement::End, true).is_ok() {
                        self.focus_pending = true;
                    }
                }
                crate::instance::bring_forward(&self.window);
            }
            #[cfg(feature = "selftest")]
            UserEvent::Test(step) => crate::selftest::step(self, step),
        }
    }

    fn on_ui(&mut self, cmd: UiCommand) {
        match cmd {
            UiCommand::Ready => {
                self.ui_ready = true;
                // Read in the background, ready before the first suggestion.
                let proxy = self.proxy.clone();
                self.history.on_loaded(move || {
                    let _ = proxy.send_event(UserEvent::HistoryLoaded);
                });
                self.history.preload();
                // The bookmarks too (for the star; a few ms, but off this thread).
                let proxy = self.proxy.clone();
                self.bookmarks.preload(move || {
                    let _ = proxy.send_event(UserEvent::BookmarksLoaded);
                });
                crate::bookmarks::on_ui_ready(self);
                self.dirty = true;
                // A (re)loaded UI has no icons yet.
                self.tabs.resend_icons();
                crate::downloads::sync_ui(self);
                crate::timing::mark("ui-ready");
                crate::extensions::on_ui_ready(self);
                crate::ext_ui::on_ui_ready(self);
                if crate::timing::quit_when_ready() {
                    self.quit = true;
                }
                if !self.fullscreen.on() && self.tabs.active().and_then(|id| self.tabs.get(id)).is_some_and(|t| t.url == url::BLANK) {
                    self.focus_address();
                }
            }
            UiCommand::NewTab => self.new_blank_tab(),
            UiCommand::CloseTab(id) => self.close_tab(id, true),
            UiCommand::Activate(id) => self.activate(id),
            UiCommand::Navigate(text) => self.navigate(&text),
            UiCommand::Back => self.with_active(|v| v.go_back()),
            UiCommand::Forward => self.with_active(|v| v.go_forward()),
            UiCommand::Reload => self.reload_active(),
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
            UiCommand::Strip(cmd) => strip::on_command(self, cmd),
            UiCommand::Page(cmd) => page_tools::on_command(self, cmd),
            UiCommand::Overlay(cmd) => crate::overlay::on_command(self, cmd),
            UiCommand::Bookmark(cmd) => crate::bookmarks::on_command(self, cmd),
            UiCommand::Menu(cmd) => crate::menu::on_command(self, cmd),
            UiCommand::Settings(cmd) => crate::settings::on_command(self, cmd),
            UiCommand::Launch(cmd) => crate::policy::on_command(self, cmd),
            UiCommand::Ext(cmd) => crate::ext_ui::on_command(self, cmd),
        }
    }

    pub(crate) fn on_shortcut(&mut self, sc: Shortcut) {
        match sc {
            Shortcut::NewTab => self.new_blank_tab(),
            Shortcut::CloseTab => {
                if let Some(id) = self.tabs.active() {
                    self.close_tab(id, true);
                }
            }
            Shortcut::FocusAddress => {
                // The address bar is in the strip: fullscreen gives way first.
                if self.fullscreen.on() {
                    page_tools::leave_fullscreen(self);
                }
                self.focus_address();
            }
            Shortcut::Reload => self.reload_active(),
            Shortcut::NextTab | Shortcut::PrevTab => {
                let step = if sc == Shortcut::NextTab { 1 } else { -1 };
                if let Some(id) = self.tabs.cycle(step) {
                    self.activate(id);
                }
            }
            Shortcut::Back => self.with_active(|v| v.go_back()),
            Shortcut::Forward => self.with_active(|v| v.go_forward()),
            Shortcut::Tab(key) => strip::on_key(self, key),
            Shortcut::Page(key) => page_tools::on_key(self, key),
            Shortcut::History => crate::overlay::toggle_history(self),
            Shortcut::Bookmark(key) => crate::bookmarks::on_key(self, key),
            Shortcut::Menu => crate::menu::toggle(self),
            Shortcut::Downloads => {
                // (An overlay page may hold the keyboard: give it back to the page first.)
                self.focus_after_switch();
                crate::downloads::toggle(self);
            }
        }
    }

    fn on_page(&mut self, id: TabId, ev: PageEvent) {
        crate::policy::on_page(self, id, &ev);
        if let PageEvent::CloseRequested = ev {
            // The page's own window.close(): not something to reopen.
            self.close_tab(id, false);
            return;
        }
        match ev {
            PageEvent::Favicon(icon) => {
                if self.tabs.set_favicon(id, icon) {
                    self.dirty = true;
                }
                return;
            }
            PageEvent::Zoom(factor) => return page_tools::on_zoom(self, id, factor),
            PageEvent::Fullscreen(on) => return page_tools::on_page_fullscreen(self, id, on),
            PageEvent::Navigating { nav, user } => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    // Address bar and suggestions go through `load_url`, which
                    // WebView2 never calls user-initiated: `user_nav_next` says so.
                    let asked = std::mem::take(&mut tab.user_nav_next);
                    if user || asked {
                        tab.user_nav = Some(nav);
                    } else if tab.user_nav != Some(nav) {
                        // A redirect keeps its id (and who started it).
                        tab.user_nav = None;
                    }
                }
                return;
            }
            PageEvent::Visited { url, title, nav } => {
                let Some(tab) = self.tabs.get_mut(id) else { return };
                let user = tab.user_nav == Some(nav);
                tab.page_url = crate::url::cap_url(&url);
                return self.history.record(&url, &title, user);
            }
            // (Not the Uzantılar page: it is about the browser, not this
            // page, and its own removal loads pages, see `ext_ui::close_pages`.)
            PageEvent::Loading(true) if self.tabs.active() == Some(id) && self.overlay.mode != crate::overlay::Mode::Extensions => {
                crate::overlay::dismiss(self)
            }
            _ => {}
        }
        let Some(tab) = self.tabs.get_mut(id) else { return };
        match ev {
            PageEvent::CloseRequested
            | PageEvent::Favicon(_)
            | PageEvent::Zoom(_)
            | PageEvent::Fullscreen(_)
            | PageEvent::Navigating { .. }
            | PageEvent::Visited { .. } => {}
            PageEvent::Audio { playing, muted } => {
                tab.audible = playing;
                tab.muted = muted;
            }
            PageEvent::Title(t) => {
                tab.title = t.chars().take(512).collect();
                // Not `tab.url`: a typed address shows there before it
                // commits, while the old page may still retitle itself.
                self.history.retitle(&tab.page_url, &tab.title);
            }
            PageEvent::Url(u) => {
                tab.url = crate::url::cap_url(&u);
                tab.page_url.clone_from(&tab.url);
            }
            PageEvent::Loading(l) => tab.loading = l,
            PageEvent::History { back, forward } => {
                tab.can_back = back;
                tab.can_forward = forward;
            }
        }
        self.dirty = true;
    }

    /// Ctrl+T, the strip's "+": a blank tab with the address bar focused, so
    /// fullscreen gives way first (as Chrome shows its toolbar).
    fn new_blank_tab(&mut self) {
        if self.fullscreen.on() {
            page_tools::leave_fullscreen(self);
        }
        if self.open_tab(url::BLANK, Placement::End, true).is_ok() {
            self.focus_address();
        }
    }

    pub fn navigate(&mut self, text: &str) {
        crate::overlay::close(self);
        // An extension's page opens only while it is enabled, and is never
        // searched for (see `ext_ui`).
        let resolved = match crate::ext_ui::typed_url(text) {
            Some(Some(u)) => Resolved::Url(u),
            Some(None) => Resolved::Blocked,
            None => url::resolve_input(text),
        };
        match resolved {
            Resolved::Empty => {}
            Resolved::Blocked => {
                let _ = self.ui.evaluate_script("xb.blocked()");
            }
            Resolved::Url(u) => {
                let Some(id) = self.tabs.active() else { return };
                if self.view(id).is_none() {
                    // A tab whose webview could not be made: try again, at
                    // the new address.
                    if let Some(t) = self.tabs.get_mut(id) {
                        t.url = u;
                        t.user_nav_next = true;
                    }
                    self.revive(id);
                    return;
                }
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
                    t.user_nav_next = true;
                }
                self.dirty = true;
            }
        }
    }

    /// Reloads the active tab; one without a webview gets one (loading its
    /// address), as its webview could not be made before.
    fn reload_active(&mut self) {
        let Some(id) = self.tabs.active() else { return };
        match self.view(id) {
            Some(v) => {
                let _ = v.reload();
            }
            None => self.revive(id),
        }
    }

    /// Makes tab `id`'s missing webview (it failed before) and, for the
    /// active tab, shows and focuses it. On failure the tab stays as it was.
    fn revive(&mut self, id: TabId) {
        if let Err(e) = self.create_view(id) {
            eprintln!("x-browser: cannot create the webview of tab {id}: {e}");
            self.dirty = true;
            return;
        }
        if let Some(t) = self.tabs.get_mut(id) {
            t.loading = true;
        }
        let rect = self.content_bounds();
        if self.tabs.active() == Some(id)
            && let Some(v) = self.view(id)
        {
            let _ = v.set_bounds(rect);
            native::raise(v);
            let _ = v.set_visible(true);
            let _ = v.focus();
        }
        self.dirty = true;
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

/// The whole window: the page in fullscreen, the strip hidden.
pub fn full_rect(size: LogicalSize<f64>) -> Rect {
    Rect { position: LogicalPosition::new(0.0, 0.0).into(), size: size.into() }
}

/// Width, height and top margin of the fullscreen notice band (logical px).
pub const NOTICE_SIZE: (f64, f64, f64) = (600.0, 40.0, 16.0);

/// The fullscreen notice: a band centred at the top of the window.
pub fn notice_rect(size: LogicalSize<f64>) -> Rect {
    let (w, h, top) = NOTICE_SIZE;
    let w = w.min(size.width);
    Rect {
        position: LogicalPosition::new(((size.width - w) / 2.0).max(0.0), top.min((size.height - h).max(0.0))).into(),
        size: LogicalSize::new(w, h.min(size.height)).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn notice_band_is_centred_at_the_top() {
        let r = notice_rect(LogicalSize::new(1920.0, 1080.0));
        let (p, s) = (r.position.to_logical::<f64>(1.0), r.size.to_logical::<f64>(1.0));
        assert_eq!((p.x, p.y, s.width, s.height), (660.0, 16.0, 600.0, 40.0));
        // Never wider than the window.
        let r = notice_rect(LogicalSize::new(480.0, 240.0));
        let (p, s) = (r.position.to_logical::<f64>(1.0), r.size.to_logical::<f64>(1.0));
        assert_eq!((p.x, s.width), (0.0, 480.0));
    }

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
