//! Policy hooks of a content tab (see `crate::policy`): certificate errors
//! of the page's own server are reported, launches of external applications
//! are stopped, and what became of each navigation is reported (an error
//! page gets no lock; a loaded page ends a tab's "crashed" state).

use super::Proxy;
use crate::tabs::TabId;
use wry::WebView;

/// Main navigations kept at most: a completion never seen cannot grow the
/// list without end (the oldest goes first).
const MAX_NAVS: usize = 16;

/// Main navigation `id` started (or was redirected) to `uri`.
#[cfg_attr(not(windows), allow(dead_code))]
fn started(navs: &mut Vec<(u64, String)>, id: u64, uri: String) {
    navs.retain(|(n, _)| *n != id);
    if navs.len() >= MAX_NAVS {
        navs.remove(0);
    }
    navs.push((id, uri));
}

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
        // Where the tab's main navigations are going: every one started and
        // not completed yet, by id (a redirect keeps the id and replaces the
        // address). A later one does not end an earlier one: its error may
        // still come. And whether a document has been put on screen since
        // the last one started.
        let nav: Rc<RefCell<Vec<(u64, String)>>> = Rc::new(RefCell::new(Vec::new()));
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
            let p = proxy.clone();
            wv.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |sender, args| {
                    let mut uri = PWSTR::null();
                    let uri = if args.as_ref().is_some_and(|a| a.Uri(&mut uri).is_ok()) { take_pwstr(uri) } else { String::new() };
                    // The Web Store button's sentinel address (`cws`) never
                    // loads: it is cancelled here, and only a request from the
                    // committed detail page of the same id is asked about. A
                    // redirect flag or `Source` that cannot be read counts
                    // against it. Still recorded below as started: WebView2
                    // completes a cancelled navigation like any other.
                    if let Some(a) = &args {
                        let mut redirected = BOOL(1);
                        let redirected = a.IsRedirected(&mut redirected).is_err() || redirected.as_bool();
                        let mut source = PWSTR::null();
                        let source = match &sender {
                            Some(s) if s.Source(&mut source).is_ok() => take_pwstr(source),
                            _ => String::new(),
                        };
                        match crate::cws::sentinel_decision(&source, &uri, redirected) {
                            crate::cws::SentinelDecision::Pass => {}
                            crate::cws::SentinelDecision::Cancel => {
                                let _ = a.SetCancel(true);
                            }
                            crate::cws::SentinelDecision::Ask(action, id) => {
                                let _ = a.SetCancel(true);
                                let _ = p.send_event(UserEvent::Policy(tab, Event::Store { action, id }));
                            }
                        }
                    }
                    let mut id = 0u64;
                    if let Some(a) = &args {
                        let _ = a.NavigationId(&mut id);
                    }
                    super::started(&mut to.borrow_mut(), id, uri);
                    shown.set(false);
                    Ok(())
                })),
                &mut token,
            )?;

            // The action is left alone: the page fails, and the tab gets
            // Taser's error page (`attach_error_page`).
            // Raised for every request of the tab (images, scripts, frames);
            // `policy` keeps the ones for the page itself.
            let p = proxy.clone();
            let to = nav.clone();
            wv.cast::<ICoreWebView2_14>()?.add_ServerCertificateErrorDetected(
                &ServerCertificateErrorDetectedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut uri = PWSTR::null();
                    args.RequestUri(&mut uri)?;
                    let navs = to.borrow().iter().map(|(_, u)| u.clone()).collect();
                    let ev = Event::CertError { uri: take_pwstr(uri), navs };
                    let _ = p.send_event(UserEvent::Policy(tab, ev));
                    Ok(())
                })),
                &mut token,
            )?;

            // A new document is about to be shown: the site's, or an error
            // document in its place (Taser's error page goes into it).
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
            let to = nav.clone();
            wv.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let mut id = 0u64;
                    if args.as_ref().is_some_and(|a| a.NavigationId(&mut id).is_ok()) {
                        to.borrow_mut().retain(|(n, _)| *n != id);
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_main_navigation_not_completed_is_kept() {
        let mut navs = Vec::new();
        started(&mut navs, 1, "https://bad.example/".into());
        started(&mut navs, 2, "https://other.example/".into());
        assert_eq!(navs.len(), 2, "a second navigation does not hide the first");
        started(&mut navs, 1, "https://moved.example/".into());
        assert_eq!(navs, [(2, "https://other.example/".into()), (1, "https://moved.example/".into())], "a redirect replaces its own");
        for id in 10..10 + MAX_NAVS as u64 {
            started(&mut navs, id, format!("https://n{id}.example/"));
        }
        assert_eq!(navs.len(), MAX_NAVS);
        assert_eq!(navs[0].0, 10, "the oldest went first");
    }
}
