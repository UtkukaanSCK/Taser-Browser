//! Content-tab hooks: the page gets no host bridge, and its URL, history,
//! loading, popup and close events are forwarded to the app.

use super::Proxy;
use crate::tabs::TabId;
use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::{PageEvent, UserEvent};
    use crate::native::attach_process_failed;
    use crate::ui::is_ui_url;
    use crate::url::is_web_url;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::core::{BOOL, PWSTR};
    use wry::WebViewExtWindows;

    fn source(wv: &ICoreWebView2) -> Option<String> {
        let mut uri = PWSTR::null();
        unsafe { wv.Source(&mut uri).ok()? };
        Some(take_pwstr(uri))
    }

    /// Content pages get no host bridge at all: no `chrome.webview.postMessage`
    /// and no host objects. Call before the first navigation.
    pub fn harden_content(view: &WebView) -> windows::core::Result<()> {
        unsafe {
            let settings = view.webview().Settings()?;
            settings.SetIsWebMessageEnabled(false)?;
            settings.SetAreHostObjectsAllowed(false)?;
        }
        Ok(())
    }

    fn nav_uri(args: &ICoreWebView2NavigationStartingEventArgs) -> windows::core::Result<String> {
        let mut uri = PWSTR::null();
        unsafe { args.Uri(&mut uri)? };
        Ok(take_pwstr(uri))
    }

    /// URL, history, loading, popup, close and crash events of one content tab.
    pub fn attach_content_events(view: &WebView, tab: TabId, proxy: &Proxy) -> windows::core::Result<()> {
        let wv = view.webview();
        let mut token = 0i64;
        attach_process_failed(view, Some(tab), proxy)?;
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

            // Host xb.ui is the UI's origin; in a content tab it would be a
            // real network request in the profile the UI shares. Never load it,
            // neither as the page nor in a frame.
            let p = proxy.clone();
            wv.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |wv, args| {
                    if let Some(args) = args
                        && is_ui_url(&nav_uri(&args)?)
                    {
                        args.SetCancel(true)?;
                        // The address bar may already show the refused URL,
                        // and the tab a spinner (NavigationCompleted may never
                        // come for a navigation cancelled here).
                        if let Some(url) = wv.as_ref().and_then(source) {
                            let _ = p.send_event(UserEvent::Page(tab, PageEvent::Url(url)));
                        }
                        let _ = p.send_event(UserEvent::Page(tab, PageEvent::Loading(false)));
                        return Ok(());
                    }
                    let _ = p.send_event(UserEvent::Page(tab, PageEvent::Loading(true)));
                    Ok(())
                })),
                &mut token,
            )?;
            wv.add_FrameNavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args
                        && is_ui_url(&nav_uri(&args)?)
                    {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            // `window.close()` closes the tab through the app. wry's own
            // handler (registered first) has already destroyed the tab's host
            // window; dropping the view then only fails to destroy it again.
            let p = proxy.clone();
            wv.add_WindowCloseRequested(
                &WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
                    let _ = p.send_event(UserEvent::Page(tab, PageEvent::CloseRequested));
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
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attach_content_events(_: &WebView, _: TabId, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
    pub fn harden_content(_: &WebView) -> Result<(), ()> {
        Ok(())
    }
}

pub use imp::*;
