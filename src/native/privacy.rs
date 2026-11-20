//! Global Privacy Control in a content tab (see `crate::privacy`): every
//! request it makes carries `Sec-GPC: 1`, and every new document sees
//! `navigator.globalPrivacyControl === true`. Turning it off (or on) takes
//! effect for requests at once and for documents loaded after.

use crate::tabs::TabId;
use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::core::{HSTRING, Interface};
    use wry::WebViewExtWindows;

    /// What GPC has put into a tab.
    #[derive(Default)]
    struct Tab {
        on: bool,
        /// The document script's id, once WebView2 has given it.
        script: Option<String>,
    }

    thread_local! {
        /// Only the UI thread, where WebView2 calls back, uses it.
        static TABS: RefCell<HashMap<TabId, Tab>> = RefCell::new(HashMap::new());
    }

    /// Adds the request hook to tab `tab`'s new webview. Call once per
    /// webview, then `apply_gpc`.
    pub fn attach_privacy(view: &WebView, tab: TabId) -> windows::core::Result<()> {
        // (A tab's webview made again starts with nothing in it.)
        forget_gpc(tab);
        let wv = view.webview();
        let mut token = 0i64;
        unsafe {
            wv.add_WebResourceRequested(
                &WebResourceRequestedEventHandler::create(Box::new(|_, args| {
                    // (The filter is there only while GPC is on; this is
                    // for a request already under way as it went off.)
                    if crate::privacy::gpc()
                        && let Some(args) = args
                    {
                        let headers = args.Request()?.Headers()?;
                        headers.SetHeader(&HSTRING::from("Sec-GPC"), &HSTRING::from("1"))?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    fn filter(wv: &ICoreWebView2, add: bool) -> windows::core::Result<()> {
        let all = HSTRING::from("*");
        unsafe {
            // Workers' requests too where the runtime can say so.
            if let Ok(wv22) = wv.cast::<ICoreWebView2_22>() {
                let (ctx, kinds) =
                    (COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL);
                return if add {
                    wv22.AddWebResourceRequestedFilterWithRequestSourceKinds(&all, ctx, kinds)
                } else {
                    wv22.RemoveWebResourceRequestedFilterWithRequestSourceKinds(&all, ctx, kinds)
                };
            }
            if add {
                wv.AddWebResourceRequestedFilter(&all, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)
            } else {
                wv.RemoveWebResourceRequestedFilter(&all, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)
            }
        }
    }

    /// Puts GPC into tab `tab` (`on`) or takes it out; doing what is
    /// already done does nothing.
    pub fn apply_gpc(view: &WebView, tab: TabId, on: bool) -> windows::core::Result<()> {
        let wv = view.webview();
        let was = TABS.with(|t| std::mem::replace(&mut t.borrow_mut().entry(tab).or_default().on, on));
        if was == on {
            return Ok(());
        }
        if let Err(e) = filter(&wv, on) {
            TABS.with(|t| t.borrow_mut().entry(tab).or_default().on = was);
            return Err(e);
        }
        if !on {
            if let Some(id) = TABS.with(|t| t.borrow_mut().get_mut(&tab).and_then(|s| s.script.take())) {
                unsafe { wv.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(id))? };
            }
            return Ok(());
        }
        let wv2 = wv.clone();
        unsafe {
            wv.AddScriptToExecuteOnDocumentCreated(
                &HSTRING::from(crate::privacy::GPC_SCRIPT),
                &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |result, id| {
                    result?;
                    // Turned off (or the tab closed) while it was being added.
                    let keep = TABS.with(|t| match t.borrow_mut().get_mut(&tab) {
                        Some(s) if s.on && s.script.is_none() => {
                            s.script = Some(id.clone());
                            true
                        }
                        _ => false,
                    });
                    if !keep {
                        wv2.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(id))?;
                    }
                    Ok(())
                })),
            )
        }
    }

    /// The tab closed.
    pub fn forget_gpc(tab: TabId) {
        TABS.with(|t| t.borrow_mut().remove(&tab));
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attach_privacy(_: &WebView, _: TabId) -> Result<(), ()> {
        Ok(())
    }

    pub fn apply_gpc(_: &WebView, _: TabId, _: bool) -> Result<(), ()> {
        Ok(())
    }

    pub fn forget_gpc(_: TabId) {}
}

pub use imp::*;
