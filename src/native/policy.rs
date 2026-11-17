//! Policy hooks of a content tab (see `crate::policy`): certificate errors
//! of the page's own server are reported, launches of external applications
//! are stopped, and what became of each navigation is reported (an error
//! page gets no lock; a loaded page ends a tab's "crashed" state).

use super::Proxy;
use crate::tabs::TabId;
use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::UserEvent;
    use crate::policy::Event;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::core::{BOOL, Interface, PWSTR};
    use wry::WebViewExtWindows;

    /// Not an extra: a tab without these hooks would let a page start other
    /// applications unasked, so a tab whose hooks cannot be set is not made.
    pub fn attach_policy(view: &WebView, tab: TabId, proxy: &Proxy) -> windows::core::Result<()> {
        let wv = view.webview();
        let mut token = 0i64;
        // Where the tab's main navigation is going (each redirect starts
        // anew), and whether it has put a document on screen yet.
        let nav = Rc::new(RefCell::new(String::new()));
        let committed = Rc::new(Cell::new(false));
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

            let (to, shown) = (nav.clone(), committed.clone());
            wv.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    let mut uri = PWSTR::null();
                    let uri = if args.is_some_and(|a| a.Uri(&mut uri).is_ok()) { take_pwstr(uri) } else { String::new() };
                    to.replace(uri);
                    shown.set(false);
                    Ok(())
                })),
                &mut token,
            )?;

            // The action is left alone: WebView2 shows its own interstitial.
            // Raised for every request of the tab (images, scripts, frames);
            // `policy` keeps the ones for the page itself.
            let p = proxy.clone();
            let to = nav.clone();
            wv.cast::<ICoreWebView2_14>()?.add_ServerCertificateErrorDetected(
                &ServerCertificateErrorDetectedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut uri = PWSTR::null();
                    args.RequestUri(&mut uri)?;
                    let ev = Event::CertError { uri: take_pwstr(uri), nav: to.borrow().clone() };
                    let _ = p.send_event(UserEvent::Policy(tab, ev));
                    Ok(())
                })),
                &mut token,
            )?;

            // A new document is about to be shown: the site's, or WebView2's
            // error page in its place.
            let p = proxy.clone();
            let shown = committed.clone();
            wv.add_ContentLoading(
                &ContentLoadingEventHandler::create(Box::new(move |_, args| {
                    let mut error = BOOL(0);
                    // Unknown counts as an error page: no lock.
                    let error = !args.is_some_and(|a| a.IsErrorPage(&mut error).is_ok()) || error.as_bool();
                    shown.set(true);
                    let _ = p.send_event(UserEvent::Policy(tab, Event::Page { error }));
                    Ok(())
                })),
                &mut token,
            )?;

            let p = proxy.clone();
            wv.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let mut ok = BOOL(0);
                    if args.is_some_and(|a| a.IsSuccess(&mut ok).is_ok()) && ok.as_bool() {
                        let _ = p.send_event(UserEvent::Policy(tab, Event::Loaded));
                    } else if committed.get() {
                        // A failure that put nothing on screen (a download,
                        // a launch that was stopped) leaves the page as it
                        // was, and its mark with it.
                        let _ = p.send_event(UserEvent::Policy(tab, Event::Failed));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    /// Hands a URI to the system to open in the application registered for
    /// its scheme. Only `policy::ExternalLaunch::confirm` calls this, after
    /// its checks; they are made again here.
    ///
    /// The shell finds the handler from the text, the way it does for every
    /// browser (a handler chosen in Windows settings is found no other way).
    /// It would also open a path or run a program, so the text must be what
    /// `policy::launchable_scheme` lets through: a URI with a real scheme of
    /// two characters or more, never a drive letter or a file name.
    #[cfg(not(any(test, feature = "selftest")))]
    pub fn open_external(uri: &str) -> bool {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, PCWSTR, w};
        if crate::policy::launchable_scheme(uri).is_none() {
            return false;
        }
        // "If the function succeeds, it returns a value greater than 32."
        let r = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
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
