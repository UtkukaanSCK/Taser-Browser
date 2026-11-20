//! One site's permissions and data, for the site information panel
//! (`site_info`): the profile's non-default permission settings, setting
//! one, and deleting what one origin stored. Elsewhere (no WebView2) the
//! list is empty and every change fails.

use wry::WebView;

/// A permission setting as the profile keeps it: (kind, origin, state),
/// with `COREWEBVIEW2_PERMISSION_KIND` and `COREWEBVIEW2_PERMISSION_STATE`
/// values.
pub type PermissionSetting = (i32, String, i32);

#[cfg(windows)]
mod imp {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::core::{HSTRING, Interface, PWSTR};
    use wry::WebViewExtWindows;

    fn profile4(view: &WebView) -> windows::core::Result<ICoreWebView2Profile4> {
        unsafe { view.webview().cast::<ICoreWebView2_13>()?.Profile()?.cast::<ICoreWebView2Profile4>() }
    }

    /// A completion not called yet; taken by whichever comes first.
    type Pending<T> = Rc<Cell<Option<Box<dyn FnOnce(T)>>>>;

    /// Calls `done` once: from the handler with the outcome WebView2
    /// reports, or through the cell if the call could not even start.
    fn once<T: 'static>(done: impl FnOnce(T) + 'static) -> (Pending<T>, impl Fn(T)) {
        let cell: Pending<T> = Rc::new(Cell::new(Some(Box::new(done))));
        let later = cell.clone();
        (cell, move |v| {
            if let Some(done) = later.take() {
                done(v);
            }
        })
    }

    /// Every permission setting of the profile that is not "ask" (the
    /// default); `None` if WebView2 could not list them.
    pub fn site_permissions(view: &WebView, done: impl FnOnce(Option<Vec<PermissionSetting>>) + 'static) {
        let (cell, finish) = once(done);
        let started = profile4(view).and_then(|p| unsafe {
            p.GetNonDefaultPermissionSettings(&GetNonDefaultPermissionSettingsCompletedHandler::create(Box::new(
                move |r, list| {
                    finish(r.ok().and(list).map(|l| read(&l)));
                    Ok(())
                },
            )))
        });
        if let Err(e) = started {
            eprintln!("x-browser: cannot list site permissions: {e}");
            if let Some(done) = cell.take() {
                done(None);
            }
        }
    }

    fn read(list: &ICoreWebView2PermissionSettingCollectionView) -> Vec<PermissionSetting> {
        let mut out = Vec::new();
        unsafe {
            let mut n = 0u32;
            if list.Count(&mut n).is_err() {
                return out;
            }
            for i in 0..n {
                let Ok(s) = list.GetValueAtIndex(i) else { continue };
                let (mut kind, mut state, mut origin) =
                    (COREWEBVIEW2_PERMISSION_KIND(0), COREWEBVIEW2_PERMISSION_STATE(0), PWSTR::null());
                if s.PermissionKind(&mut kind).is_ok() && s.PermissionState(&mut state).is_ok() && s.PermissionOrigin(&mut origin).is_ok() {
                    out.push((kind.0, take_pwstr(origin), state.0));
                }
            }
        }
        out
    }

    /// Sets permission `kind` of `origin` (`https://host[:port]`) to `state`
    /// (0 ask, 1 allow, 2 block); `done(true)` once WebView2 did.
    pub fn set_site_permission(view: &WebView, kind: i32, origin: &str, state: i32, done: impl FnOnce(bool) + 'static) {
        let (cell, finish) = once(done);
        let started = profile4(view).and_then(|p| unsafe {
            p.SetPermissionState(
                COREWEBVIEW2_PERMISSION_KIND(kind),
                &HSTRING::from(origin),
                COREWEBVIEW2_PERMISSION_STATE(state),
                &SetPermissionStateCompletedHandler::create(Box::new(move |r| {
                    finish(r.is_ok());
                    Ok(())
                })),
            )
        });
        if let Err(e) = started {
            eprintln!("x-browser: cannot set a site permission: {e}");
            if let Some(done) = cell.take() {
                done(false);
            }
        }
    }

    /// Deletes what `origin` stored in the profile: its cookies, local and
    /// session storage, IndexedDB, cache storage, service workers and the
    /// rest (DevTools `Storage.clearDataForOrigin`, every storage type).
    /// Other sites' data and the extensions' storage stay.
    pub fn clear_site_data(view: &WebView, origin: &str, done: impl FnOnce(bool) + 'static) {
        let (cell, finish) = once(done);
        let params = serde_json::json!({ "origin": origin, "storageTypes": "all" }).to_string();
        let started = unsafe {
            view.webview().CallDevToolsProtocolMethod(
                &HSTRING::from("Storage.clearDataForOrigin"),
                &HSTRING::from(params),
                &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |r, _json| {
                    finish(r.is_ok());
                    Ok(())
                })),
            )
        };
        if let Err(e) = started {
            eprintln!("x-browser: cannot clear site data: {e}");
            if let Some(done) = cell.take() {
                done(false);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn site_permissions(_: &WebView, done: impl FnOnce(Option<Vec<PermissionSetting>>) + 'static) {
        done(None);
    }
    pub fn set_site_permission(_: &WebView, _: i32, _: &str, _: i32, done: impl FnOnce(bool) + 'static) {
        done(false);
    }
    pub fn clear_site_data(_: &WebView, _: &str, done: impl FnOnce(bool) + 'static) {
        done(false);
    }
}

pub use imp::*;
