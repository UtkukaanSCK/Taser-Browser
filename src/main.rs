// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod native;
#[cfg(feature = "selftest")]
mod selftest;
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
        let message = if native::is_profile_busy(&e) {
            "Önceki X-Browser oturumu henüz tam kapanmadı; profil klasörü hâlâ kullanımda.\n\
             Bu genellikle uygulama zorla kapatıldıktan sonra olur ve yarım dakika içinde geçer.\n\
             Biraz sonra yeniden deneyin."
                .to_string()
        } else {
            format!("X-Browser başlatılamadı.\n\nMicrosoft Edge WebView2 Runtime kurulu olmalıdır.\n\n{e}")
        };
        if !timing::quit_when_ready() {
            native::fatal(&message);
        }
        std::process::exit(1);
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
