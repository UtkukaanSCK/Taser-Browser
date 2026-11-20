//! Puts X-Browser's own error page (`crate::error_page`) into a tab whose
//! load failed. WebView2's built-in page is turned off (`harden_content`),
//! so the failed load leaves an empty error document; once its navigation
//! has completed, the page is written into it. When the address that
//! failed was an `http://` one tried as https (`privacy`), the page is the
//! HTTPS-only one, which offers the http address.

use crate::tabs::TabId;
use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::core::{BOOL, HSTRING, PWSTR};
    use wry::WebViewExtWindows;

    /// A tab without it still never shows WebView2's page, only an empty one.
    pub fn attach_error_page(view: &WebView, tab: TabId) -> windows::core::Result<()> {
        let wv = view.webview();
        let mut token = 0i64;
        // Whether an error document has been put on screen since the last
        // main navigation started: only then is there one to fill (a failure
        // that showed nothing leaves the page as it was).
        let error_shown = Rc::new(Cell::new(false));
        unsafe {
            let shown = error_shown.clone();
            wv.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, _| {
                    shown.set(false);
                    Ok(())
                })),
                &mut token,
            )?;

            let shown = error_shown.clone();
            wv.add_ContentLoading(
                &ContentLoadingEventHandler::create(Box::new(move |_, args| {
                    let mut error = BOOL(0);
                    shown.set(args.is_some_and(|a| a.IsErrorPage(&mut error).is_ok()) && error.as_bool());
                    Ok(())
                })),
                &mut token,
            )?;

            wv.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |wv, args| {
                    let (Some(wv), Some(args)) = (wv, args) else { return Ok(()) };
                    let mut ok = BOOL(0);
                    if args.IsSuccess(&mut ok).is_err() {
                        return Ok(());
                    }
                    if ok.as_bool() {
                        crate::privacy::on_loaded(tab);
                        return Ok(());
                    }
                    if !error_shown.get() {
                        return Ok(());
                    }
                    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN;
                    let _ = args.WebErrorStatus(&mut status);
                    // The failed address: what the error document's history
                    // entry, and so the address bar, has.
                    let mut uri = PWSTR::null();
                    let url = if wv.Source(&mut uri).is_ok() { take_pwstr(uri) } else { String::new() };
                    let js = match crate::privacy::on_failed(tab, &url, status.0) {
                        Some(http) => Some(crate::error_page::https_only_script(&http, crate::privacy::HTTP_SENTINEL)),
                        None => crate::error_page::script(status.0, &url),
                    };
                    if let Some(js) = js {
                        // Scripts run in the order given: one sent for a later
                        // error document replaces this one's page.
                        wv.ExecuteScript(&HSTRING::from(js), None)?;
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

    pub fn attach_error_page(_: &WebView, _: TabId) -> Result<(), ()> {
        Ok(())
    }
}

pub use imp::*;
