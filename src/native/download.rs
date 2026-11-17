//! Downloads through WebView2's own flyout (see `downloads`).
//!
//! `DownloadStarting` arrives with `Handled` already true; setting it back to
//! false is what lets WebView2 show its download flyout (progress, pause,
//! "open file", "open folder"). SmartScreen and the save check are WebView2's
//! and stay on: nothing here touches `SaveFileSecurityCheckStarting`.

use super::Proxy;
use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::UserEvent;
    use crate::downloads::Event;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::Win32::Foundation::POINT;
    use windows::core::{BOOL, Interface};
    use wry::WebViewExtWindows;

    /// Where the flyout sits in the page area: its top-right corner, a little
    /// in from the edges, under the strip's downloads button.
    const MARGIN: POINT = POINT { x: 12, y: 6 };

    pub fn attach_downloads(view: &WebView, proxy: &Proxy) -> windows::core::Result<()> {
        let wv4 = view.webview().cast::<ICoreWebView2_4>()?;
        let wv9 = view.webview().cast::<ICoreWebView2_9>()?;
        let mut token = 0i64;
        let p = proxy.clone();
        unsafe {
            wv9.SetDefaultDownloadDialogCornerAlignment(COREWEBVIEW2_DEFAULT_DOWNLOAD_DIALOG_CORNER_ALIGNMENT_TOP_RIGHT)?;
            wv9.SetDefaultDownloadDialogMargin(MARGIN)?;
            wv4.add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    args.SetHandled(false)?;
                    let _ = p.send_event(UserEvent::Download(Event::Started));
                    if let Ok(op) = args.DownloadOperation() {
                        let done = p.clone();
                        let mut t = 0i64;
                        op.add_StateChanged(
                            &StateChangedEventHandler::create(Box::new(move |sender, _| {
                                let Some(op) = sender else { return Ok(()) };
                                let mut state = COREWEBVIEW2_DOWNLOAD_STATE::default();
                                op.State(&mut state)?;
                                let ok = match state {
                                    COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED => true,
                                    COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED => false,
                                    _ => return Ok(()),
                                };
                                let mut path = windows::core::PWSTR::null();
                                let file = op.ResultFilePath(&mut path).ok().map(|()| {
                                    std::path::PathBuf::from(take_pwstr(path))
                                });
                                let _ = done.send_event(UserEvent::Download(Event::Finished { ok, file }));
                                Ok(())
                            })),
                            &mut t,
                        )?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    /// Opens WebView2's download flyout on `view`, or closes it when it is
    /// open. False if it could not be asked.
    pub fn toggle_downloads(view: &WebView) -> bool {
        let Ok(wv9) = view.webview().cast::<ICoreWebView2_9>() else { return false };
        unsafe {
            let mut open = BOOL(0);
            let _ = wv9.IsDefaultDownloadDialogOpen(&mut open);
            if open.as_bool() { wv9.CloseDefaultDownloadDialog().is_ok() } else { wv9.OpenDefaultDownloadDialog().is_ok() }
        }
    }

    /// Whether the flyout is open on `view`.
    #[cfg(feature = "selftest")]
    pub fn downloads_open(view: &WebView) -> bool {
        let Ok(wv9) = view.webview().cast::<ICoreWebView2_9>() else { return false };
        let mut open = BOOL(0);
        unsafe {
            let _ = wv9.IsDefaultDownloadDialogOpen(&mut open);
        }
        open.as_bool()
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attach_downloads(_: &WebView, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
    pub fn toggle_downloads(_: &WebView) -> bool {
        false
    }
    #[cfg(feature = "selftest")]
    pub fn downloads_open(_: &WebView) -> bool {
        false
    }
}

pub use imp::*;
