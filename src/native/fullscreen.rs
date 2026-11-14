//! Element fullscreen of a content page (`requestFullscreen`, a video's
//! fullscreen button). WebView2 only reports it; making the window
//! fullscreen around it is up to `page_tools`.

use super::Proxy;
use crate::tabs::TabId;
use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::{PageEvent, UserEvent};
    use webview2_com::*;
    use windows::core::BOOL;
    use wry::WebViewExtWindows;

    pub fn attach_fullscreen(view: &WebView, tab: TabId, proxy: &Proxy) -> windows::core::Result<()> {
        let mut token = 0i64;
        let p = proxy.clone();
        unsafe {
            view.webview().add_ContainsFullScreenElementChanged(
                &ContainsFullScreenElementChangedEventHandler::create(Box::new(move |wv, _| {
                    let Some(wv) = wv else { return Ok(()) };
                    let mut on = BOOL(0);
                    wv.ContainsFullScreenElement(&mut on)?;
                    let _ = p.send_event(UserEvent::Page(tab, PageEvent::Fullscreen(on.as_bool())));
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

    pub fn attach_fullscreen(_: &WebView, _: TabId, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
}

pub use imp::*;
