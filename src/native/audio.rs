//! Per-tab audio state (ICoreWebView2_8): whether the page is playing sound,
//! and muting it. Runtimes without that interface show no indicator.

use super::Proxy;
use crate::tabs::TabId;
use wry::WebView;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::{PageEvent, UserEvent};
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::core::{BOOL, Interface};
    use wry::WebViewExtWindows;

    fn report(wv: Option<ICoreWebView2>, tab: TabId, proxy: &Proxy) -> windows::core::Result<()> {
        let Some(wv) = wv.and_then(|w| w.cast::<ICoreWebView2_8>().ok()) else { return Ok(()) };
        let (mut playing, mut muted) = (BOOL(0), BOOL(0));
        unsafe {
            wv.IsDocumentPlayingAudio(&mut playing)?;
            wv.IsMuted(&mut muted)?;
        }
        let ev = PageEvent::Audio { playing: playing.as_bool(), muted: muted.as_bool() };
        let _ = proxy.send_event(UserEvent::Page(tab, ev));
        Ok(())
    }

    pub fn attach_audio(view: &WebView, tab: TabId, proxy: &Proxy) -> windows::core::Result<()> {
        let Ok(wv) = view.webview().cast::<ICoreWebView2_8>() else { return Ok(()) };
        let mut token = 0i64;
        unsafe {
            let p = proxy.clone();
            wv.add_IsDocumentPlayingAudioChanged(
                &IsDocumentPlayingAudioChangedEventHandler::create(Box::new(move |wv, _| report(wv, tab, &p))),
                &mut token,
            )?;
            let p = proxy.clone();
            wv.add_IsMutedChanged(
                &IsMutedChangedEventHandler::create(Box::new(move |wv, _| report(wv, tab, &p))),
                &mut token,
            )?;
        }
        Ok(())
    }

    /// Mutes or unmutes the tab. False if this runtime cannot.
    pub fn set_muted(view: &WebView, muted: bool) -> bool {
        let Ok(wv) = view.webview().cast::<ICoreWebView2_8>() else { return false };
        unsafe { wv.SetIsMuted(muted).is_ok() }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attach_audio(_: &WebView, _: TabId, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
    pub fn set_muted(_: &WebView, _: bool) -> bool {
        false
    }
}

pub use imp::*;
