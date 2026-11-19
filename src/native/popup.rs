//! The events that close an extension's popup (`crate::popup`), and the
//! rules its frames keep (`attach_popup_policy`). Closing: Esc (always: the
//! extension cannot receive Esc), its
//! own `window.close()`, and losing the focus. Each names the popup it came
//! from (`generation`), so one that arrives after that popup is gone, or
//! from the popup a new one replaced, is ignored.

use super::Proxy;
use wry::WebView;

/// External launches the popup's frames tried, all refused (self-test).
#[cfg(feature = "selftest")]
pub static POPUP_LAUNCHES_REFUSED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Esc closes the popup. (A WebView2 accelerator: it types no character.)
/// Esc always closes it: the extension's page never receives Esc.
pub fn is_popup_close_key(vk: u32) -> bool {
    vk == 0x1B
}

/// Where the keyboard focus went when the popup reported losing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusAt {
    /// Back in the popup already.
    Popup,
    /// The window itself, or a hidden webview (the list the popup was
    /// picked from, hidden as it opened): no one clicked anywhere.
    Nowhere,
    /// Another webview of the window (the strip, the page) or another
    /// program: a click outside.
    Elsewhere,
}

/// Classifies the focused window `focus` (0: none of this thread's input
/// queue) for a popup hosted in `host`, in main window `main` that is (or
/// is not) the foreground window. `inside`: `focus` is `host` or one of its
/// descendants; `visible`: `focus` and its ancestors are shown.
pub fn classify_focus(foreground: bool, focus: isize, main: isize, inside: bool, visible: bool) -> FocusAt {
    if !foreground || focus == 0 {
        FocusAt::Elsewhere
    } else if inside {
        FocusAt::Popup
    } else if focus == main || !visible {
        FocusAt::Nowhere
    } else {
        FocusAt::Elsewhere
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::UserEvent;
    use crate::commands::UiCommand;
    use crate::ext_ui::ExtCmd;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsChild, IsWindowVisible};
    use wry::WebViewExtWindows;

    /// Where the focus is now, for the popup `view` in `window`. (The
    /// webviews' own windows are children of this thread's windows, so
    /// their input queue is this thread's and `GetFocus` sees into them.)
    pub fn popup_focus(window: &tao::window::Window, view: &WebView) -> FocusAt {
        use tao::platform::windows::WindowExtWindows;
        let main = HWND(window.hwnd() as _);
        let host = view.hwnd();
        unsafe {
            let focus = GetFocus();
            let inside = !focus.is_invalid() && (focus == host || IsChild(host, focus).as_bool());
            let visible = !focus.is_invalid() && IsWindowVisible(focus).as_bool();
            classify_focus(GetForegroundWindow() == main, focus.0 as isize, main.0 as isize, inside, visible)
        }
    }

    /// The focused window's class, for the self-test's trace.
    #[cfg(feature = "selftest")]
    pub fn focus_class() -> String {
        use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
        let mut name = [0u16; 64];
        let focus = unsafe { GetFocus() };
        let n = unsafe { GetClassNameW(focus, &mut name) } as usize;
        format!("{:?} {}", focus.0, String::from_utf16_lossy(&name[..n.min(64)]))
    }

    /// The content tabs' rules, for the popup's frames too (its own page is
    /// kept in its extension by wry's navigation handler): no other
    /// application is ever started from a popup, asked or not (an extension
    /// that opens `mailto:` cannot, there), and no frame may load a local
    /// file, the app's pages or a page of an extension that is not enabled.
    /// A popup without both is not shown.
    pub fn attach_popup_policy(view: &WebView) -> windows::core::Result<()> {
        use windows::core::{Interface, PWSTR};
        let wv = view.webview();
        let mut token = 0i64;
        unsafe {
            wv.cast::<ICoreWebView2_18>()?.add_LaunchingExternalUriScheme(
                &LaunchingExternalUriSchemeEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                        #[cfg(feature = "selftest")]
                        super::POPUP_LAUNCHES_REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            wv.add_FrameNavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut uri = PWSTR::null();
                    let uri = if args.Uri(&mut uri).is_ok() { Some(take_pwstr(uri)) } else { None };
                    if uri.is_none_or(|u| crate::policy::refuses_navigation(&u) || crate::ext_ui::refuses_extension_url(&u)) {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    pub fn attach_popup_events(view: &WebView, generation: u32, proxy: &Proxy) -> windows::core::Result<()> {
        let closed = move |refocus| UserEvent::Ui(UiCommand::Ext(ExtCmd::PopupClosed { generation, refocus }));
        let controller = view.controller();
        let mut token = 0i64;
        unsafe {
            let p = proxy.clone();
            controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                    args.KeyEventKind(&mut kind)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN {
                        return Ok(());
                    }
                    let mut key = 0u32;
                    args.VirtualKey(&mut key)?;
                    if is_popup_close_key(key) {
                        args.SetHandled(true)?;
                        let _ = p.send_event(closed(true));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            // A click outside (the strip, the page) or another window.
            let p = proxy.clone();
            controller.add_LostFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    let _ = p.send_event(closed(false));
                    Ok(())
                })),
                &mut token,
            )?;
            // `window.close()`: wry's own handler has destroyed the host
            // window already; dropping the view then finishes the job.
            let p = proxy.clone();
            view.webview().add_WindowCloseRequested(
                &WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
                    let _ = p.send_event(closed(true));
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attach_popup_events(_: &WebView, _: u32, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }

    pub fn attach_popup_policy(_: &WebView) -> Result<(), ()> {
        Ok(())
    }

    pub fn popup_focus(_: &tao::window::Window, _: &WebView) -> FocusAt {
        FocusAt::Elsewhere
    }

    #[cfg(feature = "selftest")]
    pub fn focus_class() -> String {
        String::new()
    }
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_esc_closes_the_popup() {
        assert!(is_popup_close_key(0x1B));
        for vk in [0x0D, 0x08, 0x09, 0x20, 0x41, 0x73] {
            assert!(!is_popup_close_key(vk), "{vk:#x}");
        }
    }

    #[test]
    fn only_a_focus_gone_to_another_view_or_program_is_a_click_outside() {
        use FocusAt::*;
        // (foreground, focus, main, inside, visible)
        assert_eq!(classify_focus(true, 5, 1, true, true), Popup);
        assert_eq!(classify_focus(true, 1, 1, false, true), Nowhere);
        assert_eq!(classify_focus(true, 7, 1, false, false), Nowhere);
        assert_eq!(classify_focus(true, 7, 1, false, true), Elsewhere);
        assert_eq!(classify_focus(true, 0, 1, false, false), Elsewhere);
        assert_eq!(classify_focus(false, 5, 1, true, true), Elsewhere);
    }
}
