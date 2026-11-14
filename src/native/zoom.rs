//! Page zoom of a content tab (ICoreWebView2Controller::ZoomFactor). The
//! zoom keys are mapped in `page_tools` and set the factor here; Ctrl+wheel
//! is WebView2's own (`IsZoomControlEnabled`) and comes back as
//! ZoomFactorChanged, as does a per-site zoom WebView2 restores on
//! navigation. A factor set here raises no event (WebView2 documents this);
//! `page_tools` updates the tab itself.

use super::Proxy;
use crate::tabs::TabId;
use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::{PageEvent, UserEvent};
    use webview2_com::*;
    use wry::WebViewExtWindows;

    /// Turns zoom on for a content tab (Ctrl+wheel; the keys never reach
    /// WebView2's own zoom, the native hook takes them first) and reports
    /// every change of its factor.
    pub fn attach_zoom(view: &WebView, tab: TabId, proxy: &Proxy) -> windows::core::Result<()> {
        let controller = view.controller();
        let mut token = 0i64;
        unsafe {
            view.webview().Settings()?.SetIsZoomControlEnabled(true)?;
            let p = proxy.clone();
            controller.add_ZoomFactorChanged(
                &ZoomFactorChangedEventHandler::create(Box::new(move |c, _| {
                    let Some(c) = c else { return Ok(()) };
                    let mut factor = 1.0;
                    c.ZoomFactor(&mut factor)?;
                    let _ = p.send_event(UserEvent::Page(tab, PageEvent::Zoom(factor)));
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    /// The tab's current zoom factor (1.0 = 100%).
    pub fn zoom_factor(view: &WebView) -> Option<f64> {
        let mut factor = 0.0;
        unsafe { view.controller().ZoomFactor(&mut factor).ok()? };
        Some(factor)
    }

    pub fn set_zoom_factor(view: &WebView, factor: f64) {
        unsafe {
            let _ = view.controller().SetZoomFactor(factor);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attach_zoom(_: &WebView, _: TabId, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
    pub fn zoom_factor(_: &WebView) -> Option<f64> {
        None
    }
    pub fn set_zoom_factor(view: &WebView, factor: f64) {
        let _ = view.zoom(factor);
    }
}

pub use imp::*;
