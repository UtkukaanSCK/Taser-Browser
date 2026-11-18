//! Browser extensions in a webview's WebView2 profile (`ICoreWebView2Profile7`
//! and `ICoreWebView2BrowserExtension`). Only the calls and their completion
//! handlers live here; what to install, and the record of it, is
//! `crate::extensions`.
//!
//! Every function takes the webview whose profile is meant: a content tab for
//! the default profile (where extensions live), the UI webview for `XbUi`
//! (which must never have any). Each `done` runs exactly once, on the UI
//! thread: with WebView2's answer, or right away when the call could not be
//! started (an older runtime, no such extension).

use std::path::Path;
use wry::WebView;

/// One extension as WebView2 lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::result::Result; // not webview2_com::Result
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::core::{BOOL, HSTRING, Interface, PWSTR};
    use wry::WebViewExtWindows;

    /// A `done` that can be handed to WebView2 and still be called here if
    /// the call is never started: whoever takes it first runs it.
    struct Once<F>(Rc<Cell<Option<F>>>);

    impl<F> Once<F> {
        fn new(f: F) -> Self {
            Self(Rc::new(Cell::new(Some(f))))
        }
        fn share(&self) -> Self {
            Self(self.0.clone())
        }
        fn take(&self) -> Option<F> {
            self.0.take()
        }
    }

    fn profile(view: &WebView) -> windows::core::Result<ICoreWebView2Profile7> {
        unsafe { view.webview().cast::<ICoreWebView2_13>()?.Profile()?.cast::<ICoreWebView2Profile7>() }
    }

    fn describe(ext: &ICoreWebView2BrowserExtension) -> windows::core::Result<ExtensionInfo> {
        let (mut id, mut name, mut enabled) = (PWSTR::null(), PWSTR::null(), BOOL(0));
        unsafe {
            ext.Id(&mut id)?;
            let id = take_pwstr(id);
            ext.Name(&mut name)?;
            let name = take_pwstr(name);
            ext.IsEnabled(&mut enabled)?;
            Ok(ExtensionInfo { id, name, enabled: enabled.as_bool() })
        }
    }

    fn items(list: &ICoreWebView2BrowserExtensionList) -> windows::core::Result<Vec<ICoreWebView2BrowserExtension>> {
        let mut n = 0u32;
        unsafe {
            list.Count(&mut n)?;
            (0..n).map(|i| list.GetValueAtIndex(i)).collect()
        }
    }

    /// The profile's extensions, as WebView2 objects (for `with_extension`).
    fn get_all(
        view: &WebView,
        done: impl FnOnce(Result<Vec<ICoreWebView2BrowserExtension>, String>) + 'static,
    ) {
        let done = Once::new(done);
        let later = done.share();
        let started = profile(view).and_then(|p| unsafe {
            p.GetBrowserExtensions(&ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |hr, list| {
                if let Some(done) = later.take() {
                    done(match (hr, list) {
                        (Ok(()), Some(list)) => items(&list).map_err(|e| e.to_string()),
                        (Ok(()), None) => Ok(Vec::new()),
                        (Err(e), _) => Err(e.to_string()),
                    });
                }
                Ok(())
            })))
        });
        if let Err(e) = started && let Some(done) = done.take() {
            done(Err(e.to_string()));
        }
    }

    /// Every extension in `view`'s profile (built-ins included).
    pub fn list_extensions(view: &WebView, done: impl FnOnce(Result<Vec<ExtensionInfo>, String>) + 'static) {
        get_all(view, move |r| {
            done(r.and_then(|all| all.iter().map(|e| describe(e).map_err(|e| e.to_string())).collect()));
        });
    }

    /// Installs the unpacked extension in `dir` into `view`'s profile (or
    /// replaces the one with the same id in place, keeping its storage).
    pub fn add_extension(view: &WebView, dir: &Path, done: impl FnOnce(Result<ExtensionInfo, String>) + 'static) {
        let done = Once::new(done);
        let later = done.share();
        let path = HSTRING::from(dir.as_os_str());
        let started = profile(view).and_then(|p| unsafe {
            p.AddBrowserExtension(
                &path,
                &ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |hr, ext| {
                    if let Some(done) = later.take() {
                        done(match (hr, ext) {
                            (Ok(()), Some(ext)) => describe(&ext).map_err(|e| e.to_string()),
                            (Ok(()), None) => Err("no extension returned".into()),
                            (Err(e), _) => Err(e.to_string()),
                        });
                    }
                    Ok(())
                })),
            )
        });
        if let Err(e) = started && let Some(done) = done.take() {
            done(Err(e.to_string()));
        }
    }

    /// Finds `id` in `view`'s profile and hands it to `act`; `done(Ok(false))`
    /// when the profile has no such extension.
    fn with_extension(
        view: &WebView,
        id: &str,
        done: impl FnOnce(Result<bool, String>) + 'static,
        act: impl FnOnce(&ICoreWebView2BrowserExtension, Box<dyn FnOnce(Result<bool, String>)>) + 'static,
    ) {
        let id = id.to_owned();
        get_all(view, move |r| {
            let all = match r {
                Ok(all) => all,
                Err(e) => return done(Err(e)),
            };
            match all.iter().find(|e| describe(e).is_ok_and(|d| d.id == id)) {
                Some(ext) => act(ext, Box::new(done)),
                None => done(Ok(false)),
            }
        });
    }

    /// Turns extension `id` on or off. `Ok(false)`: not installed.
    pub fn enable_extension(view: &WebView, id: &str, on: bool, done: impl FnOnce(Result<bool, String>) + 'static) {
        with_extension(view, id, done, move |ext, done| {
            let done = Once::new(done);
            let later = done.share();
            let started = unsafe {
                ext.Enable(
                    on,
                    &BrowserExtensionEnableCompletedHandler::create(Box::new(move |hr| {
                        if let Some(done) = later.take() {
                            done(hr.map(|()| true).map_err(|e| e.to_string()));
                        }
                        Ok(())
                    })),
                )
            };
            if let Err(e) = started && let Some(done) = done.take() {
                done(Err(e.to_string()));
            }
        });
    }

    /// The installed WebView2 runtime's version, e.g. `131.0.2903.70` (a
    /// channel name after it, as in `89.0.774.18 dev`, is left out).
    pub fn runtime_version() -> Option<String> {
        let mut out = PWSTR::null();
        unsafe { GetAvailableCoreWebView2BrowserVersionString(windows::core::PCWSTR::null(), &mut out).ok()? };
        let text = take_pwstr(out);
        text.split_whitespace().next().map(str::to_owned)
    }

    /// Uninstalls extension `id` (WebView2 also drops its storage; the
    /// folder stays). `Ok(false)`: it was not installed.
    pub fn remove_extension(view: &WebView, id: &str, done: impl FnOnce(Result<bool, String>) + 'static) {
        with_extension(view, id, done, move |ext, done| {
            let done = Once::new(done);
            let later = done.share();
            let started = unsafe {
                ext.Remove(&BrowserExtensionRemoveCompletedHandler::create(Box::new(move |hr| {
                    if let Some(done) = later.take() {
                        done(hr.map(|()| true).map_err(|e| e.to_string()));
                    }
                    Ok(())
                })))
            };
            if let Err(e) = started && let Some(done) = done.take() {
                done(Err(e.to_string()));
            }
        });
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    const NONE: &str = "extensions need WebView2";

    pub fn list_extensions(_: &WebView, done: impl FnOnce(Result<Vec<ExtensionInfo>, String>) + 'static) {
        done(Err(NONE.into()));
    }
    pub fn add_extension(_: &WebView, _: &Path, done: impl FnOnce(Result<ExtensionInfo, String>) + 'static) {
        done(Err(NONE.into()));
    }
    pub fn enable_extension(_: &WebView, _: &str, _: bool, done: impl FnOnce(Result<bool, String>) + 'static) {
        done(Err(NONE.into()));
    }
    pub fn remove_extension(_: &WebView, _: &str, done: impl FnOnce(Result<bool, String>) + 'static) {
        done(Err(NONE.into()));
    }
    pub fn runtime_version() -> Option<String> {
        None
    }
}

pub use imp::*;
