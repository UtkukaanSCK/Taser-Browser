//! Policy hooks of a content tab (see `crate::policy`): certificate errors
//! are reported, launches of external applications are stopped, and a page
//! that loaded is reported (which ends a tab's "crashed" state).

use super::Proxy;
use crate::tabs::TabId;
use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::UserEvent;
    use crate::policy::Event;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::core::{BOOL, Interface, PWSTR};
    use wry::WebViewExtWindows;

    /// Not an extra: a tab without these hooks would let a page start other
    /// applications unasked, so a tab whose hooks cannot be set is not made.
    pub fn attach_policy(view: &WebView, tab: TabId, proxy: &Proxy) -> windows::core::Result<()> {
        let wv = view.webview();
        let mut token = 0i64;
        unsafe {
            // WebView2 never starts another application by itself: every
            // launch is cancelled, before anything else can fail. One the
            // user asked for is handed to the app, which may start it after
            // its own confirmation (`policy::ExternalLaunch`).
            let p = proxy.clone();
            wv.cast::<ICoreWebView2_18>()?.add_LaunchingExternalUriScheme(
                &LaunchingExternalUriSchemeEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    args.SetCancel(true)?;
                    let mut user = BOOL(0);
                    args.IsUserInitiated(&mut user)?;
                    let (mut uri, mut origin) = (PWSTR::null(), PWSTR::null());
                    args.Uri(&mut uri)?;
                    let uri = take_pwstr(uri);
                    // Empty for an opaque origin.
                    let origin = if args.InitiatingOrigin(&mut origin).is_ok() { take_pwstr(origin) } else { String::new() };
                    let _ = p.send_event(UserEvent::Policy(tab, Event::ExternalLaunch { uri, origin, user: user.as_bool() }));
                    Ok(())
                })),
                &mut token,
            )?;

            // The action is left alone: WebView2 shows its own interstitial.
            let p = proxy.clone();
            wv.cast::<ICoreWebView2_14>()?.add_ServerCertificateErrorDetected(
                &ServerCertificateErrorDetectedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut uri = PWSTR::null();
                    args.RequestUri(&mut uri)?;
                    let _ = p.send_event(UserEvent::Policy(tab, Event::CertError(take_pwstr(uri))));
                    Ok(())
                })),
                &mut token,
            )?;

            let p = proxy.clone();
            wv.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let mut ok = BOOL(0);
                    if let Some(args) = args
                        && args.IsSuccess(&mut ok).is_ok()
                        && ok.as_bool()
                    {
                        let _ = p.send_event(UserEvent::Policy(tab, Event::Loaded));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    /// Hands a URI to the system to open in its registered application.
    /// Only `policy::ExternalLaunch::confirm` calls this, after its checks.
    #[cfg(not(any(test, feature = "selftest")))]
    pub fn open_external(uri: &str) -> bool {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, PCWSTR, w};
        let r = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
        // "If the function succeeds, it returns a value greater than 32."
        r.0 as usize > 32
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attach_policy(_: &WebView, _: TabId, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
    #[cfg(not(any(test, feature = "selftest")))]
    pub fn open_external(_: &str) -> bool {
        false
    }
}

pub use imp::*;
