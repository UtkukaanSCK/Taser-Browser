//! Settings-driven and menu-driven WebView2 calls: autofill, the download
//! folder, Find and Print. Elsewhere (no WebView2) they do nothing.

use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::cell::RefCell;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::core::{HSTRING, Interface, PWSTR};
    use wry::WebViewExtWindows;

    thread_local! {
        /// The running find session: dropping it would end the search.
        static FIND: RefCell<Option<ICoreWebView2Find>> = const { RefCell::new(None) };
    }

    /// HRESULT the last `Find::Start` completed with (1: not completed yet).
    #[cfg(feature = "selftest")]
    pub static FIND_START: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

    /// Autofill of addresses and the like follows the setting. Saving
    /// passwords is always off: this browser never offers to.
    pub fn apply_content_settings(view: &WebView, autofill: bool) -> windows::core::Result<()> {
        unsafe {
            let s = view.webview().Settings()?.cast::<ICoreWebView2Settings4>()?;
            s.SetIsGeneralAutofillEnabled(autofill)?;
            s.SetIsPasswordAutosaveEnabled(false)?;
        }
        Ok(())
    }

    /// (general autofill, password autosave) as the webview has them.
    #[cfg(feature = "selftest")]
    pub fn content_settings(view: &WebView) -> Option<(bool, bool)> {
        use windows::core::BOOL;
        unsafe {
            let s = view.webview().Settings().ok()?.cast::<ICoreWebView2Settings4>().ok()?;
            let (mut a, mut p) = (BOOL(0), BOOL(0));
            s.IsGeneralAutofillEnabled(&mut a).ok()?;
            s.IsPasswordAutosaveEnabled(&mut p).ok()?;
            Some((a.as_bool(), p.as_bool()))
        }
    }

    fn profile(view: &WebView) -> windows::core::Result<ICoreWebView2Profile> {
        unsafe { view.webview().cast::<ICoreWebView2_13>()?.Profile() }
    }

    /// Where the profile saves downloads right now (the selftest reads it).
    #[cfg_attr(not(feature = "selftest"), allow(dead_code))]
    pub fn download_folder(view: &WebView) -> Option<String> {
        unsafe {
            let mut path = PWSTR::null();
            profile(view).ok()?.DefaultDownloadFolderPath(&mut path).ok()?;
            Some(take_pwstr(path))
        }
    }

    /// Makes `dir` (an existing folder, checked by the caller) the profile's
    /// download folder.
    pub fn set_download_folder(view: &WebView, dir: &str) -> bool {
        unsafe { profile(view).and_then(|p| p.SetDefaultDownloadFolderPath(&HSTRING::from(dir))).is_ok() }
    }

    /// Opens WebView2's find bar on `view` (what Ctrl+F does in a page).
    pub fn find(view: &WebView) -> bool {
        let started = (|| unsafe {
            let find = view.webview().cast::<ICoreWebView2_28>()?.Find()?;
            let options = view.environment().cast::<ICoreWebView2Environment15>()?.CreateFindOptions()?;
            options.SetFindTerm(&HSTRING::new())?;
            options.SetSuppressDefaultFindDialog(false)?;
            find.Start(
                &options,
                &FindStartCompletedHandler::create(Box::new(|_hr| {
                    #[cfg(feature = "selftest")]
                    FIND_START.store(_hr.err().map_or(0, |e| e.code().0), std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                })),
            )?;
            Ok::<_, windows::core::Error>(find)
        })();
        match started {
            Ok(find) => {
                FIND.with(|f| {
                    if let Some(old) = f.borrow_mut().replace(find) {
                        // A search of another page ends.
                        unsafe {
                            let _ = old.Stop();
                        }
                    }
                });
                true
            }
            Err(e) => {
                eprintln!("x-browser: cannot start find: {e}");
                false
            }
        }
    }

    /// Opens WebView2's print preview for `view` (what Ctrl+P does in a page).
    pub fn print(view: &WebView) -> bool {
        unsafe {
            view.webview()
                .cast::<ICoreWebView2_16>()
                .and_then(|w| w.ShowPrintUI(COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER))
                .is_ok()
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn apply_content_settings(_: &WebView, _: bool) -> Result<(), ()> {
        Ok(())
    }
    pub fn download_folder(_: &WebView) -> Option<String> {
        None
    }
    pub fn set_download_folder(_: &WebView, _: &str) -> bool {
        false
    }
    pub fn find(_: &WebView) -> bool {
        false
    }
    pub fn print(view: &WebView) -> bool {
        view.print().is_ok()
    }
}

pub use imp::*;
