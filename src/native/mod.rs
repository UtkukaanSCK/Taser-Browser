//! WebView2 features wry does not expose. On other platforms these are no-ops
//! (and shortcuts only work while the UI webview has focus).

use crate::app::UserEvent;
use crate::tabs::TabId;
use tao::event_loop::EventLoopProxy;
use wry::WebView;

pub type Proxy = EventLoopProxy<UserEvent>;

/// Why the WebView2 profile folder cannot be opened right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileBusy {
    /// A previous browser process still holds it while shutting down.
    ShuttingDown,
    /// A browser process started with other options (another build) runs on it.
    OtherOptions,
}

mod audio;
mod content;
mod download;
mod error_page;
mod extensions;
mod favicon;
mod fullscreen;
mod policy;
mod popup;
mod privacy;
mod tools;
mod zoom;
pub use audio::*;
pub use content::*;
pub use download::*;
pub use error_page::*;
pub use extensions::*;
pub use favicon::*;
pub use fullscreen::*;
pub use policy::*;
pub use popup::*;
pub use privacy::*;
pub use tools::*;
pub use zoom::*;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::Failure;
    use crate::commands::{Mods, Shortcut};
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
    use wry::WebViewExtWindows;

    /// Browser shortcuts, read from the native key event before the page sees
    /// it. Pages cannot synthesize these and the channel carries no data.
    pub fn attach_accelerators(view: &WebView, proxy: &Proxy) -> windows::core::Result<()> {
        let controller = view.controller();
        let mut token = 0i64;
        let p = proxy.clone();
        unsafe {
            controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                    args.KeyEventKind(&mut kind)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                    {
                        return Ok(());
                    }
                    let mut key = 0u32;
                    args.VirtualKey(&mut key)?;
                    let mut status = COREWEBVIEW2_PHYSICAL_KEY_STATUS::default();
                    args.PhysicalKeyStatus(&mut status)?;
                    let down = |vk: u16| GetKeyState(vk as i32) < 0;
                    let mods = Mods {
                        ctrl: down(VK_CONTROL.0),
                        shift: down(VK_SHIFT.0),
                        alt: status.IsMenuKeyDown.as_bool() || down(VK_MENU.0),
                    };
                    let esc_exits = crate::page_tools::esc_exits_fullscreen();
                    if let Some(sc) = Shortcut::from_key_in(key, mods, esc_exits) {
                        // Swallow it either way so the page / WebView defaults
                        // (e.g. its own Ctrl+R) never run twice.
                        args.SetHandled(true)?;
                        if !status.WasKeyDown.as_bool() || sc.repeats() {
                            let _ = p.send_event(UserEvent::Shortcut(sc));
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    /// Reports WebView2 process failures. `tab` is `None` for the UI webview.
    pub fn attach_process_failed(view: &WebView, tab: Option<TabId>, proxy: &Proxy) -> windows::core::Result<()> {
        let mut token = 0i64;
        let p = proxy.clone();
        unsafe {
            view.webview().add_ProcessFailed(
                &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                    args.ProcessFailedKind(&mut kind)?;
                    let failure = match kind {
                        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => Failure::Browser,
                        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED => Failure::RendererExited,
                        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => Failure::RendererUnresponsive,
                        // GPU/utility/frame processes: WebView2 recovers or
                        // only a subframe is affected.
                        _ => return Ok(()),
                    };
                    let _ = p.send_event(UserEvent::ProcessFailed { tab, failure });
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    pub fn stop(view: &WebView) {
        unsafe {
            let _ = view.webview().Stop();
        }
    }

    /// Puts the webview's host window above its siblings, so it can be shown
    /// before the previous tab is hidden (hiding is the slow part).
    pub fn raise(view: &WebView) {
        use windows::Win32::UI::WindowsAndMessaging::{
            HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
        };
        unsafe {
            // The controller is parented to wry's per-webview host window.
            let mut host = windows::Win32::Foundation::HWND::default();
            if view.controller().ParentWindow(&mut host).is_ok() && !host.is_invalid() {
                let _ = SetWindowPos(host, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    /// Whether, and why, another WebView2 browser process holds the
    /// user-data folder: locked while it shuts down
    /// (HRESULT_FROM_WIN32(ERROR_BUSY)), or running with other environment
    /// options, e.g. those of another build
    /// (HRESULT_FROM_WIN32(ERROR_INVALID_STATE)).
    pub fn profile_busy(e: &wry::Error) -> Option<ProfileBusy> {
        const BUSY: windows::core::HRESULT = windows::core::HRESULT(0x8007_00AA_u32 as i32);
        const INVALID_STATE: windows::core::HRESULT = windows::core::HRESULT(0x8007_139F_u32 as i32);
        let wry::Error::WebView2Error(webview2_com::Error::WindowsError(w)) = e else { return None };
        match w.code() {
            BUSY => Some(ProfileBusy::ShuttingDown),
            INVALID_STATE => Some(ProfileBusy::OtherOptions),
            _ => None,
        }
    }

    /// Destroys every child window of `window`. Used only before the first
    /// webview exists: a failed wry build leaves its empty host window behind.
    pub fn destroy_children(window: &tao::window::Window) {
        use tao::platform::windows::WindowExtWindows;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, GW_CHILD, GetWindow};
        let parent = HWND(window.hwnd() as _);
        for _ in 0..64 {
            let Ok(child) = (unsafe { GetWindow(parent, GW_CHILD) }) else { break };
            if child.is_invalid() || unsafe { DestroyWindow(child) }.is_err() {
                break;
            }
        }
    }

    /// Startup failure dialog (release builds have no console).
    pub fn fatal(message: &str) {
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        use windows::core::HSTRING;
        unsafe {
            MessageBoxW(None, &HSTRING::from(message), &HSTRING::from("X-Browser"), MB_OK | MB_ICONERROR);
        }
    }

    /// Time since this process was created, from the OS (not from `main`).
    pub fn process_uptime() -> Option<std::time::Duration> {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        let (mut created, mut exit, mut kernel, mut user) = Default::default();
        unsafe {
            GetProcessTimes(GetCurrentProcess(), &mut created, &mut exit, &mut kernel, &mut user).ok()?;
            let now = GetSystemTimeAsFileTime();
            Some(std::time::Duration::from_nanos(ft(now).checked_sub(ft(created))? * 100))
        }
    }

    /// Whether `window` is the foreground window (not some other program's).
    pub fn is_foreground(window: &tao::window::Window) -> bool {
        use tao::platform::windows::WindowExtWindows;
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        unsafe { GetForegroundWindow().0 as isize == window.hwnd() }
    }

    /// Clears the profile's cookies and DOM storage (ALL_DOM_STORAGE: local
    /// storage, IndexedDB, Web SQL, cache storage, file systems), the disk
    /// cache and WebView2's own browsing history; `done(true)` once that
    /// succeeded, `done(false)` if it failed or could not start. Passwords,
    /// autofill and download history are kept.
    ///
    /// Named kinds, not ALL_SITE: Microsoft may add new kinds to ALL_SITE
    /// later, and the extensions' own storage (uBOL's settings and rules)
    /// must survive a clear. Never ALL_PROFILE or ClearBrowsingDataAll.
    pub fn clear_browsing_data(view: &WebView, done: impl FnOnce(bool) + 'static) {
        use std::{cell::Cell, rc::Rc};
        use windows::core::Interface;
        let kinds = COREWEBVIEW2_BROWSING_DATA_KINDS(
            COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_DOM_STORAGE.0
                | COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES.0
                | COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE.0
                | COREWEBVIEW2_BROWSING_DATA_KINDS_BROWSING_HISTORY.0,
        );
        let done = Rc::new(Cell::new(Some(done)));
        let later = done.clone();
        let started = unsafe {
            view.webview().cast::<ICoreWebView2_13>().and_then(|w| w.Profile()).and_then(|p| p.cast::<ICoreWebView2Profile2>()).and_then(
                |p| {
                    p.ClearBrowsingData(
                        kinds,
                        &ClearBrowsingDataCompletedHandler::create(Box::new(move |r| {
                            if let Some(done) = later.take() {
                                done(r.is_ok());
                            }
                            Ok(())
                        })),
                    )
                },
            )
        };
        if let Err(e) = started {
            eprintln!("x-browser: cannot clear browsing data: {e}");
            if let Some(done) = done.take() {
                done(false);
            }
        }
    }

    /// Saves a PNG of what the webview currently shows (self-test only).
    #[cfg(feature = "selftest")]
    pub fn capture_png(view: &WebView, path: &std::path::Path) -> windows::core::Result<()> {
        use windows::Win32::System::Com::{STGM_CREATE, STGM_WRITE};
        use windows::Win32::UI::Shell::SHCreateStreamOnFileEx;
        use windows::core::HSTRING;
        unsafe {
            let stream = SHCreateStreamOnFileEx(
                &HSTRING::from(path.as_os_str()),
                (STGM_CREATE | STGM_WRITE).0,
                0x80, // FILE_ATTRIBUTE_NORMAL
                true,
                None,
            )?;
            let keep = stream.clone();
            view.webview().CapturePreview(
                COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                &stream,
                &CapturePreviewCompletedHandler::create(Box::new(move |_| {
                    drop(keep); // flush + close the file
                    Ok(())
                })),
            )
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attach_accelerators(_: &WebView, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
    pub fn attach_process_failed(_: &WebView, _: Option<TabId>, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
    pub fn stop(view: &WebView) {
        let _ = view.evaluate_script("window.stop()");
    }
    pub fn raise(_: &WebView) {}
    pub fn profile_busy(_: &wry::Error) -> Option<ProfileBusy> {
        None
    }
    pub fn destroy_children(_: &tao::window::Window) {}
    pub fn fatal(_: &str) {}
    pub fn process_uptime() -> Option<std::time::Duration> {
        None
    }
    pub fn is_foreground(_: &tao::window::Window) -> bool {
        true
    }
    pub fn clear_browsing_data(_: &WebView, done: impl FnOnce(bool) + 'static) {
        done(false);
    }
    #[cfg(feature = "selftest")]
    pub fn capture_png(_: &WebView, _: &std::path::Path) -> Result<(), ()> {
        Err(())
    }
}

pub use imp::*;
