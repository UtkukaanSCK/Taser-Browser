// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bookmarks;
mod commands;
mod crx;
mod downloads;
mod extensions;
mod history;
mod instance;
mod menu;
mod native;
mod overlay;
mod page_tools;
mod policy;
#[cfg(feature = "selftest")]
mod selftest;
mod session;
mod settings;
mod store;
mod strip;
mod tabs;
mod timing;
mod ui;
mod url;

use app::{App, UserEvent};
use std::path::Path;
use tabs::Placement;
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
use tao::window::WindowBuilder;

fn main() {
    let dir = store::data_dir();
    // The first argument: an address to open (as typed in the address bar).
    let arg = std::env::args_os().nth(1).and_then(|a| a.into_string().ok());
    // A second start hands its address to the running instance and exits.
    let _instance = match dir.as_deref().map(|d| instance::start(d, arg.as_deref())) {
        Some(instance::Start::HandedOver) => return,
        Some(instance::Start::Unreachable) => std::process::exit(1),
        Some(instance::Start::First(guard)) => Some(guard),
        None => None,
    };
    // The self-test's second process must only ever hand over: if it became
    // the first instance, stop before any window, profile or test code.
    #[cfg(feature = "selftest")]
    if std::env::var_os("XB_SELFTEST_CHILD").is_some() {
        eprintln!("x-browser: self-test child found no instance to hand over to");
        std::process::exit(3);
    }
    if let Err(e) = run(dir.as_deref(), arg.as_deref()) {
        eprintln!("x-browser: {e}");
        if !timing::quit_when_ready() {
            native::fatal(&startup_error_message(&e));
        }
        std::process::exit(1);
    }
}

/// Startup failure dialog text.
fn startup_error_message(e: &wry::Error) -> String {
    match native::profile_busy(e) {
        Some(native::ProfileBusy::ShuttingDown) => {
            "Önceki X-Browser oturumu henüz tam kapanmadı; profil klasörü hâlâ kullanımda.\n\
             Bu genellikle uygulama zorla kapatıldıktan sonra olur ve yarım dakika içinde geçer.\n\
             Biraz sonra yeniden deneyin."
                .to_string()
        }
        Some(native::ProfileBusy::OtherOptions) => {
            "Başka bir X-Browser sürümü çalışıyor ve aynı profil klasörünü farklı ayarlarla kullanıyor.\n\
             Onu kapatıp yeniden deneyin."
                .to_string()
        }
        None => format!("X-Browser başlatılamadı.\n\nMicrosoft Edge WebView2 Runtime kurulu olmalıdır.\n\n{e}"),
    }
}

/// The monitors now attached, the primary first.
fn monitors(event_loop: &EventLoop<UserEvent>) -> Vec<session::Rect> {
    let rect = |m: &tao::monitor::MonitorHandle| {
        let (p, s) = (m.position(), m.size());
        session::Rect { x: p.x, y: p.y, w: s.width as i32, h: s.height as i32 }
    };
    let primary = event_loop.primary_monitor().map(|m| rect(&m));
    let mut all: Vec<_> = event_loop.available_monitors().map(|m| rect(&m)).collect();
    all.sort_by_key(|r| Some(*r) != primary);
    all
}

/// The exe's icon (resource 1, see build.rs) at `size` logical pixels.
#[cfg(windows)]
fn icon(size: f64, scale: f64) -> Option<tao::window::Icon> {
    use tao::platform::windows::IconExtWindows;
    let px = (size * scale).round() as u32;
    tao::window::Icon::from_resource(1, Some(PhysicalSize::new(px, px))).ok()
}

fn run(dir: Option<&Path>, arg: Option<&str>) -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    if let Some(dir) = dir
        && let Err(e) = instance::listen(dir, event_loop.create_proxy())
    {
        eprintln!("x-browser: later starts cannot reach this one: {e}");
    }
    #[cfg(feature = "selftest")]
    selftest::prepare_profile(dir);
    let (mut persist, saved_tabs, saved_window) = session::Persist::open(dir);

    let mut builder = WindowBuilder::new()
        .with_title("X-Browser")
        .with_min_inner_size(LogicalSize::new(570.0, 240.0))
        .with_decorations(false)
        .with_theme(None);
    // The last session's place, moved onto a monitor that is still there.
    builder = if saved_window.is_set() {
        let r = session::place(saved_window.rect(), &monitors(&event_loop));
        persist.placement = session::WindowState { x: r.x, y: r.y, width: r.w as u32, height: r.h as u32, ..saved_window };
        builder
            .with_position(PhysicalPosition::new(r.x, r.y))
            .with_inner_size(PhysicalSize::new(r.w as u32, r.h as u32))
            .with_maximized(saved_window.maximized)
    } else {
        builder.with_inner_size(LogicalSize::new(1200.0, 800.0))
    };
    #[cfg(windows)]
    {
        use tao::platform::windows::WindowBuilderExtWindows;
        // Keeps the native shadow and the invisible left/right/bottom resize
        // borders; the top edge is handled by the UI strip.
        builder = builder.with_undecorated_shadow(true);
        let scale = event_loop.primary_monitor().map_or(1.0, |m| m.scale_factor());
        // Caption/Alt+Tab (small) and taskbar (big) icons.
        builder = builder.with_window_icon(icon(16.0, scale)).with_taskbar_icon(icon(32.0, scale));
    }
    // Created hidden so the first frame already has the theme's background.
    let window = builder
        .with_visible(false)
        .build(&event_loop)
        .map_err(|e| wry::Error::Io(std::io::Error::other(e)))?;
    window.set_background_color(Some(app::background(window.theme())));
    window.set_visible(true);
    timing::mark("window-shown");

    let mut app = App::new(window, event_loop.create_proxy())?;
    app.persist = persist;
    // Nothing read yet: that waits until the UI is up (see `history`).
    app.history = history::History::new(dir);
    app.bookmarks = bookmarks::Bookmarks::new(dir);
    app.settings = settings::State::open(dir);
    app.extensions = extensions::Extensions::open(dir);
    session::track_window(&mut app);
    timing::mark("ui-webview-created");

    // The last session's tabs (only the active one loads now), then the
    // address given on the command line in a new tab; else one blank tab.
    let restored = session::restore(&mut app, &saved_tabs);
    match arg.and_then(instance::url_from_arg) {
        Some(start) => {
            app.open_tab(&start, Placement::End, true)?;
        }
        None if restored.is_none() => {
            app.open_tab(url::BLANK, Placement::End, true)?;
        }
        None => {}
    }
    // The download folder is the content profile's: set through a tab.
    settings::apply(&mut app);
    timing::mark("first-tab-created");

    #[cfg(feature = "selftest")]
    selftest::start(event_loop.create_proxy());

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {}
            Event::UserEvent(ev) => app.handle(ev),
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => app.quit = true,
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    overlay::on_window_changed(&mut app);
                    app.layout();
                    app.mark_dirty(); // maximize state may have changed
                    session::track_window(&mut app);
                }
                WindowEvent::Moved(_) => {
                    overlay::on_window_changed(&mut app);
                    session::track_window(&mut app);
                }
                WindowEvent::Focused(false) => overlay::on_window_unfocused(&mut app),
                WindowEvent::ThemeChanged(theme) => app.on_theme(theme),
                _ => {}
            },
            Event::MainEventsCleared => app.flush(),
            Event::LoopDestroyed => {
                if let Some(message) = app.fatal.take() {
                    app.window.set_visible(false);
                    if !timing::quit_when_ready() {
                        native::fatal(message);
                    }
                }
            }
            _ => {}
        }
        if app.quit {
            // Last save, while every tab is still there.
            session::save_at_exit(&mut app);
            if !app.history.flush_now(std::time::Duration::from_secs(2)) {
                eprintln!("x-browser: history not fully written at exit");
            }
            *flow = ControlFlow::Exit;
        } else if let Some(due) = [app.persist.due, app.history.due].into_iter().flatten().min() {
            // Wake up for the pending saves even if nothing else happens.
            *flow = ControlFlow::WaitUntil(due);
        }
    });
}

#[cfg(all(test, windows))]
mod tests {
    use super::startup_error_message;
    use windows::core::HRESULT;

    fn webview2_error(code: u32) -> wry::Error {
        wry::Error::WebView2Error(webview2_com::Error::WindowsError(HRESULT(code as i32).into()))
    }

    #[test]
    fn profile_errors_get_their_own_message() {
        let busy = startup_error_message(&webview2_error(0x8007_00AA));
        assert!(busy.starts_with("Önceki X-Browser oturumu henüz tam kapanmadı"), "{busy}");
        let other = startup_error_message(&webview2_error(0x8007_139F));
        assert!(other.starts_with("Başka bir X-Browser sürümü çalışıyor"), "{other}");
        assert!(!other.contains("henüz tam kapanmadı") && !other.contains("Runtime"), "{other}");
        let missing = startup_error_message(&webview2_error(0x8007_0002));
        assert!(missing.contains("WebView2 Runtime kurulu olmalıdır"), "{missing}");
        let io = startup_error_message(&wry::Error::Io(std::io::Error::other("x")));
        assert!(io.contains("WebView2 Runtime kurulu olmalıdır"), "{io}");
    }
}
