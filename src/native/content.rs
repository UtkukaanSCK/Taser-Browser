//! Content-tab hooks: the page gets no host bridge, and its URL, history,
//! loading, popup and close events are forwarded to the app.

use super::Proxy;
use crate::tabs::TabId;
use std::time::{Duration, Instant};
use wry::WebView;

/// A middle press this recent before a new-window request counts as the
/// middle click that asked for it. Chromium opens a middle-clicked link on
/// button *up*, so the button state itself is already gone by then.
pub const MIDDLE_CLICK_WINDOW: Duration = Duration::from_secs(2);

/// Whether a page's (user-initiated) new-window request opens as a
/// background tab: Ctrl+click or middle click, as in Chrome; adding Shift
/// opens it in front. `last_press` is the last mouse button pressed over the
/// page (`true` = middle) and when.
pub fn opens_in_background(ctrl: bool, shift: bool, last_press: Option<(bool, Instant)>, now: Instant) -> bool {
    let middle = matches!(last_press, Some((true, at)) if now.saturating_duration_since(at) < MIDDLE_CLICK_WINDOW);
    !shift && (ctrl || middle)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::{PageEvent, UserEvent};
    use crate::native::attach_process_failed;
    use crate::url::is_tab_url;
    use std::cell::Cell;
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, VK_CONTROL, VK_SHIFT};
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_NCDESTROY, WM_PARENTNOTIFY, WM_RBUTTONDOWN, WM_XBUTTONDOWN,
    };
    use windows::core::{BOOL, Interface, PWSTR};
    use wry::WebViewExtWindows;

    /// The last mouse button pressed over the page (`true` = middle), and when.
    type LastPress = Cell<Option<(bool, Instant)>>;

    const PRESS_SUBCLASS_ID: usize = 0x5842_5052; // "XBPR"

    /// wry's per-webview host window; the page's own windows are its children.
    fn host(view: &WebView) -> Option<HWND> {
        let mut hwnd = HWND::default();
        unsafe { view.controller().ParentWindow(&mut hwnd).ok()? };
        (!hwnd.is_invalid()).then_some(hwnd)
    }

    /// Button presses on a child window reach every ancestor as
    /// WM_PARENTNOTIFY (the page's windows live in the WebView2 browser
    /// process; the message still arrives here). Only which button and when
    /// is kept, nothing about the page.
    unsafe extern "system" fn press_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        data: usize,
    ) -> LRESULT {
        unsafe {
            let last = &*(data as *const LastPress);
            match msg {
                WM_PARENTNOTIFY => match (wparam.0 & 0xFFFF) as u32 {
                    WM_MBUTTONDOWN => last.set(Some((true, Instant::now()))),
                    WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_XBUTTONDOWN => last.set(Some((false, Instant::now()))),
                    _ => {}
                },
                WM_NCDESTROY => {
                    let _ = RemoveWindowSubclass(hwnd, Some(press_proc), id);
                    drop(Rc::from_raw(data as *const LastPress));
                }
                _ => {}
            }
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
    }

    /// Starts recording button presses over the tab's page. Without it (no
    /// host window) middle-clicked links simply open in front.
    fn watch_presses(view: &WebView) -> Rc<LastPress> {
        let last = Rc::new(LastPress::new(None));
        if let Some(hwnd) = host(view) {
            let data = Rc::into_raw(last.clone());
            if !unsafe { SetWindowSubclass(hwnd, Some(press_proc), PRESS_SUBCLASS_ID, data as usize) }.as_bool() {
                drop(unsafe { Rc::from_raw(data) });
            }
            #[cfg(feature = "selftest")]
            WATCHED.with_borrow_mut(|w| {
                w.retain(|(_, l)| l.strong_count() > 0);
                w.push((hwnd.0 as isize, Rc::downgrade(&last)));
            });
        }
        last
    }

    /// Self-test only. Sends the host window the WM_PARENTNOTIFY a real press
    /// over the page would cause (CDP input reaches the renderer directly and
    /// causes none), then reads back what the press subclass recorded:
    /// Some(middle) when it recorded a press within the last second.
    #[cfg(feature = "selftest")]
    pub fn simulate_press(view: &WebView, middle: bool) -> Option<bool> {
        use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
        let hwnd = host(view)?;
        let button = if middle { WM_MBUTTONDOWN } else { WM_LBUTTONDOWN };
        unsafe { SendMessageW(hwnd, WM_PARENTNOTIFY, Some(WPARAM(button as usize)), Some(LPARAM(0))) };
        let last = WATCHED.with_borrow(|w| w.iter().find(|(h, _)| *h == hwnd.0 as isize).and_then(|(_, l)| l.upgrade()))?;
        last.get().filter(|(_, at)| at.elapsed() < Duration::from_secs(1)).map(|(m, _)| m)
    }

    // Self-test only: the press record of each watched host window, to read
    // back what the subclass recorded (GetWindowSubclass is not exported by
    // name from the comctl32 the app loads).
    #[cfg(feature = "selftest")]
    thread_local! {
        static WATCHED: std::cell::RefCell<Vec<(isize, std::rc::Weak<LastPress>)>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Self-test only: every window under the host, as (class name, has
    /// WS_EX_NOPARENTNOTIFY itself, it or an ancestor below the host has it).
    /// A real press over a window whose path is quiet never reaches the host.
    #[cfg(feature = "selftest")]
    pub fn parent_notify_probe(view: &WebView) -> Vec<(String, bool, bool)> {
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumChildWindows, GWL_EXSTYLE, GetClassNameW, GetParent, GetWindowLongPtrW, WS_EX_NOPARENTNOTIFY,
        };
        let quiet = |hwnd: HWND| unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_NOPARENTNOTIFY.0 != 0;
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            unsafe { (*(data.0 as *mut Vec<HWND>)).push(hwnd) };
            BOOL(1)
        }
        let Some(top) = host(view) else { return Vec::new() };
        let mut all: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumChildWindows(Some(top), Some(each), LPARAM(&mut all as *mut _ as isize));
        }
        all.into_iter()
            .map(|hwnd| {
                let mut name = [0u16; 128];
                let n = unsafe { GetClassNameW(hwnd, &mut name) } as usize;
                let class = String::from_utf16_lossy(&name[..n.min(name.len())]);
                let mut path = false;
                let mut w = hwnd;
                while w != top && !w.is_invalid() {
                    path |= quiet(w);
                    w = unsafe { GetParent(w) }.unwrap_or_default();
                }
                (class, quiet(hwnd), path)
            })
            .collect()
    }

    fn source(wv: &ICoreWebView2) -> Option<String> {
        let mut uri = PWSTR::null();
        unsafe { wv.Source(&mut uri).ok()? };
        Some(take_pwstr(uri))
    }

    /// Content pages get no host bridge at all: no `chrome.webview.postMessage`
    /// and no host objects. Nor WebView2's own (Edge-branded) error page: a
    /// failed load gets X-Browser's (`attach_error_page`). Call before the
    /// first navigation.
    pub fn harden_content(view: &WebView) -> windows::core::Result<()> {
        unsafe {
            let settings = view.webview().Settings()?;
            settings.SetIsWebMessageEnabled(false)?;
            settings.SetAreHostObjectsAllowed(false)?;
            settings.SetIsBuiltInErrorPageEnabled(false)?;
        }
        Ok(())
    }

    /// `(IsWebMessageEnabled, AreHostObjectsAllowed)` as WebView2 has them
    /// for `view`: what `harden_content` turned off. (A disabled
    /// `postMessage` still exists and does not throw; it posts nothing.)
    #[cfg(feature = "selftest")]
    pub fn host_bridge(view: &WebView) -> Option<(bool, bool)> {
        let (mut messages, mut objects) = (BOOL(0), BOOL(0));
        unsafe {
            let settings = view.webview().Settings().ok()?;
            settings.IsWebMessageEnabled(&mut messages).ok()?;
            settings.AreHostObjectsAllowed(&mut objects).ok()?;
        }
        Some((messages.as_bool(), objects.as_bool()))
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
        crate::native::attach_policy(view, tab, proxy)?;
        if let Err(e) = crate::native::attach_error_page(view, tab) {
            eprintln!("x-browser: tab {tab}: no error page: {e}");
        }
        // Icons and the sound mark are extras: a tab without them still works.
        if let Err(e) = crate::native::attach_favicon(view, tab, proxy) {
            eprintln!("x-browser: tab {tab}: no favicon events: {e}");
        }
        if let Err(e) = crate::native::attach_audio(view, tab, proxy) {
            eprintln!("x-browser: tab {tab}: no audio events: {e}");
        }
        // Zoom and page fullscreen too: without them the page just cannot
        // zoom, or fills only its own area in fullscreen.
        if let Err(e) = crate::native::attach_zoom(view, tab, proxy) {
            eprintln!("x-browser: tab {tab}: no zoom events: {e}");
        }
        if let Err(e) = crate::native::attach_fullscreen(view, tab, proxy) {
            eprintln!("x-browser: tab {tab}: no fullscreen events: {e}");
        }
        let last_press = watch_presses(view);
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
                    // Who started it decides whether the visit counts toward
                    // the address's frequency (see `history`). Sent first, so
                    // a navigation refused below still uses up the address
                    // bar's "the user asked" mark.
                    if let Some(args) = &args {
                        let (mut user, mut nav) = (BOOL(0), 0u64);
                        let _ = args.IsUserInitiated(&mut user);
                        let _ = args.NavigationId(&mut nav);
                        let _ = p.send_event(UserEvent::Page(tab, PageEvent::Navigating { nav, user: user.as_bool() }));
                    }
                    // An address that cannot be read is refused like one
                    // that may not be loaded.
                    // Nor an extension's page unless it is enabled (`ext_ui`).
                    if let Some(args) = &args
                        && nav_uri(args).map_or(true, |uri| {
                            crate::policy::refuses_navigation(&uri) || crate::ext_ui::refuses_extension_url(&uri)
                        })
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
                    // A jump to a #fragment of the same page loads nothing:
                    // no spinner, and the popups stay open (`Loading(true)`
                    // closes them as a real navigation would). Nor does the
                    // Web Store sentinel, which the policy handler always
                    // cancels: the question it may open must stay open.
                    let same_page = args.as_ref().and_then(|a| nav_uri(a).ok()).is_some_and(|to| {
                        crate::cws::is_sentinel(&to)
                            || wv.as_ref().and_then(source).is_some_and(|from| crate::url::same_document(&from, &to))
                    });
                    if same_page {
                        return Ok(());
                    }
                    // HTTPS-only and tracking parameters (`privacy`): this
                    // navigation stops and the app loads the address meant.
                    if let Some(args) = &args
                        && let Ok(uri) = nav_uri(args)
                    {
                        let mut redirected = BOOL(0);
                        let _ = args.IsRedirected(&mut redirected);
                        // (Unknown on an older runtime: taken as a new page.)
                        let mut kind = COREWEBVIEW2_NAVIGATION_KIND_NEW_DOCUMENT;
                        if let Ok(a3) = args.cast::<ICoreWebView2NavigationStartingEventArgs3>() {
                            let _ = a3.NavigationKind(&mut kind);
                        }
                        let from = wv.as_ref().and_then(source).unwrap_or_default();
                        let nav = crate::privacy::Nav {
                            uri: &uri,
                            redirected: redirected.as_bool(),
                            new_document: kind == COREWEBVIEW2_NAVIGATION_KIND_NEW_DOCUMENT,
                            source: &from,
                        };
                        match crate::privacy::on_start(tab, nav) {
                            crate::privacy::Start::Go => {}
                            crate::privacy::Start::Load(url) => {
                                args.SetCancel(true)?;
                                let mut id = 0u64;
                                let _ = args.NavigationId(&mut id);
                                let _ = p.send_event(UserEvent::Page(tab, PageEvent::Rewrite { url, nav: id }));
                                return Ok(());
                            }
                            crate::privacy::Start::Refuse => {
                                args.SetCancel(true)?;
                                let _ = p.send_event(UserEvent::Page(tab, PageEvent::Url(from)));
                                let _ = p.send_event(UserEvent::Page(tab, PageEvent::Loading(false)));
                                return Ok(());
                            }
                        }
                    }
                    let _ = p.send_event(UserEvent::Page(tab, PageEvent::Loading(true)));
                    Ok(())
                })),
                &mut token,
            )?;
            wv.add_FrameNavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    // Nor local files (`policy::refuses_navigation`), nor
                    // pages of an extension that is not enabled (`ext_ui`).
                    if let Some(args) = args
                        && nav_uri(&args).map_or(true, |uri| {
                            crate::policy::refuses_navigation(&uri) || crate::ext_ui::refuses_extension_url(&uri)
                        })
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
                &NavigationCompletedEventHandler::create(Box::new(move |wv, args| {
                    // A page that really loaded goes into history (checked
                    // there: web addresses only); its title so far comes along.
                    let mut ok = BOOL(0);
                    let ok = args.as_ref().is_some_and(|a| a.IsSuccess(&mut ok).is_ok()) && ok.as_bool();
                    // A navigation that ended without a page of its own
                    // (failed, cancelled, a download): the address bar goes
                    // back from the address asked for to the page shown.
                    if !ok && let Some(url) = wv.as_ref().and_then(source) {
                        let _ = p.send_event(UserEvent::Page(tab, PageEvent::Url(url)));
                    }
                    if ok
                        && let Some(args) = args
                        && let Some(wv) = wv
                        && let Some(url) = source(&wv)
                    {
                        let mut title = PWSTR::null();
                        let title = if wv.DocumentTitle(&mut title).is_ok() { take_pwstr(title) } else { String::new() };
                        let mut nav = 0u64;
                        let _ = args.NavigationId(&mut nav);
                        let _ = p.send_event(UserEvent::Page(tab, PageEvent::Visited { url, title, nav }));
                    }
                    let _ = p.send_event(UserEvent::Page(tab, PageEvent::Loading(false)));
                    Ok(())
                })),
                &mut token,
            )?;

            // wry (no new-window handler given) already marks every request
            // handled, so no popup window ever appears. Here we only decide
            // whether to open the URL as a tab: user-initiated, http(s) only, at
            // most MAX_URL_LEN bytes (longer is refused, not cut);
            // in the background for Ctrl+click and middle click. WebView2
            // does not say which it was, so the modifier state (shared with
            // the page's input queue) and the last press are asked instead.
            let p = proxy.clone();
            wv.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut user = BOOL(0);
                    args.IsUserInitiated(&mut user)?;
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let url = take_pwstr(uri);
                    if user.as_bool() && is_tab_url(&url) {
                        // The thread's key state, or the key physically down now
                        // (the press may not have reached this thread's queue yet).
                        let down = |vk: u16| GetKeyState(vk as i32) < 0 || GetAsyncKeyState(vk as i32) < 0;
                        let background =
                            opens_in_background(down(VK_CONTROL.0), down(VK_SHIFT.0), last_press.get(), Instant::now());
                        let _ = p.send_event(UserEvent::OpenTab { opener: tab, url, background });
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
    #[cfg(feature = "selftest")]
    pub fn host_bridge(_: &WebView) -> Option<(bool, bool)> {
        None
    }
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_only_for_ctrl_or_a_recent_middle_press() {
        let now = Instant::now();
        let ago = |ms: u64| now.checked_sub(Duration::from_millis(ms)).unwrap_or(now);
        let left = |ms: u64| Some((false, ago(ms)));
        let middle = |ms: u64| Some((true, ago(ms)));
        assert!(!opens_in_background(false, false, None, now), "plain target=_blank click");
        assert!(!opens_in_background(false, false, left(10), now));
        assert!(opens_in_background(true, false, left(10), now), "Ctrl+click");
        assert!(!opens_in_background(true, true, left(10), now), "Ctrl+Shift+click opens in front");
        assert!(opens_in_background(false, false, middle(300), now), "middle click (opens on button up)");
        assert!(!opens_in_background(false, true, middle(300), now), "Shift+middle click opens in front");
        assert!(!opens_in_background(false, false, middle(2500), now), "a stale middle press does not count");
    }
}
