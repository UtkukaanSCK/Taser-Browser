//! WebView2 features wry does not expose. On other platforms these are no-ops
//! (and shortcuts only work while the UI webview has focus).

use crate::app::{PageEvent, UserEvent};
use crate::tabs::TabId;
use tao::event_loop::EventLoopProxy;
use wry::WebView;

pub type Proxy = EventLoopProxy<UserEvent>;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::commands::{Mods, Shortcut};
    use crate::url::is_web_url;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
    use windows::core::{BOOL, PWSTR};
    use wry::WebViewExtWindows;

    fn source(wv: &ICoreWebView2) -> Option<String> {
        let mut uri = PWSTR::null();
        unsafe { wv.Source(&mut uri).ok()? };
        Some(take_pwstr(uri))
    }

    /// URL, history, loading and popup events of one content tab.
    pub fn attach_content_events(view: &WebView, tab: TabId, proxy: &Proxy) -> windows::core::Result<()> {
        let wv = view.webview();
        let mut token = 0i64;
        unsafe {
            // Fires for new documents *and* same-document changes (pushState,
            // #fragment), which NavigationStarting/Completed do not report.
            let p = proxy.clone();
            wv.add_SourceChanged(
                &SourceChangedEventHandler::create(Box::new(move |wv, _| {
                    if let Some(url) = wv.as_ref().and_then(source) {
                        let _ = p.send_event(UserEvent::Page(tab, PageEvent::Url(url)));
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let p = proxy.clone();
            wv.add_HistoryChanged(
                &HistoryChangedEventHandler::create(Box::new(move |wv, _| {
                    if let Some(wv) = wv {
                        let (mut back, mut fwd) = (BOOL(0), BOOL(0));
                        let _ = wv.CanGoBack(&mut back);
                        let _ = wv.CanGoForward(&mut fwd);
                        let ev = PageEvent::History { back: back.as_bool(), forward: fwd.as_bool() };
                        let _ = p.send_event(UserEvent::Page(tab, ev));
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let p = proxy.clone();
            wv.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, _| {
                    let _ = p.send_event(UserEvent::Page(tab, PageEvent::Loading(true)));
                    Ok(())
                })),
                &mut token,
            )?;

            let p = proxy.clone();
            wv.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, _| {
                    let _ = p.send_event(UserEvent::Page(tab, PageEvent::Loading(false)));
                    Ok(())
                })),
                &mut token,
            )?;

            // wry (no new-window handler given) already marks every request
            // handled, so no popup window ever appears. Here we only decide
            // whether to open the URL as a tab: user-initiated, http(s) only.
            let p = proxy.clone();
            wv.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut user = BOOL(0);
                    args.IsUserInitiated(&mut user)?;
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let url = take_pwstr(uri);
                    if user.as_bool() && is_web_url(&url) {
                        let _ = p.send_event(UserEvent::OpenTab { opener: tab, url });
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

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
                    if let Some(sc) = Shortcut::from_key(key, mods) {
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

    /// The user-data folder is still locked by a WebView2 browser process
    /// that is shutting down (HRESULT_FROM_WIN32(ERROR_BUSY)).
    pub fn is_profile_busy(e: &wry::Error) -> bool {
        const BUSY: windows::core::HRESULT = windows::core::HRESULT(0x8007_00AA_u32 as i32);
        matches!(e, wry::Error::WebView2Error(webview2_com::Error::WindowsError(w)) if w.code() == BUSY)
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

    pub fn attach_content_events(_: &WebView, _: TabId, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
    pub fn attach_accelerators(_: &WebView, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
    pub fn stop(view: &WebView) {
        let _ = view.evaluate_script("window.stop()");
    }
    pub fn raise(_: &WebView) {}
    pub fn is_profile_busy(_: &wry::Error) -> bool {
        false
    }
    pub fn destroy_children(_: &tao::window::Window) {}
    pub fn fatal(_: &str) {}
    pub fn process_uptime() -> Option<std::time::Duration> {
        None
    }
    #[cfg(feature = "selftest")]
    pub fn capture_png(_: &WebView, _: &std::path::Path) -> Result<(), ()> {
        Err(())
    }
}

pub use imp::*;
