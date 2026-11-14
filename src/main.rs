// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod native;
mod page_tools;
#[cfg(feature = "selftest")]
mod selftest;
mod strip;
mod tabs;
mod timing;
mod ui;
mod url;

use app::{App, UserEvent};
use tabs::Placement;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;

fn main() {
    if let Err(e) = run() {
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

fn run() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let mut builder = WindowBuilder::new()
        .with_title("X-Browser")
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(LogicalSize::new(480.0, 240.0))
        .with_decorations(false)
        .with_theme(None);
    #[cfg(windows)]
    {
        use tao::platform::windows::WindowBuilderExtWindows;
        // Keeps the native shadow and the invisible left/right/bottom resize
        // borders; the top edge is handled by the UI strip.
        builder = builder.with_undecorated_shadow(true);
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
    timing::mark("ui-webview-created");

    let start = std::env::args()
        .nth(1)
        .and_then(|arg| match url::resolve_input(&arg) {
            url::Resolved::Url(u) => Some(u),
            _ => None,
        })
        .unwrap_or_else(|| url::BLANK.to_string());
    app.open_tab(&start, Placement::End, true)?;
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
                    app.layout();
                    app.mark_dirty(); // maximize state may have changed
                }
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
            *flow = ControlFlow::Exit;
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
