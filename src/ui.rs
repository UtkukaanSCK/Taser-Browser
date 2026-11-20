//! The UI webview: embedded assets served over a private `xb://` protocol,
//! a strict CSP, and the only IPC channel the app listens to.

use crate::app::UserEvent;
use crate::commands::UiCommand;
use crate::native::Proxy;
use std::borrow::Cow;
use tao::window::Window;
use wry::http::{Request, Response, header};
use wry::{Rect, WebContext, WebView, WebViewBuilder};

const PROTOCOL: &str = "xb";
pub const START_URL: &str = "xb://ui/index.html";
/// The popup surface's page (see `overlay`).
pub const OVERLAY_URL: &str = "xb://ui/overlay.html";

const INDEX_HTML: &str = include_str!("../ui/index.html");
const CORE_JS: &str = include_str!("../ui/js/core.js");
const STRIP_JS: &str = include_str!("../ui/js/strip.js");
const SUGGEST_JS: &str = include_str!("../ui/js/suggest.js");
const OVERLAY_HTML: &str = include_str!("../ui/overlay.html");
const OVERLAY_JS: &str = include_str!("../ui/js/overlay.js");
const BOOKMARKS_JS: &str = include_str!("../ui/js/bookmarks.js");
const MENU_JS: &str = include_str!("../ui/js/menu.js");
const SETTINGS_JS: &str = include_str!("../ui/js/settings.js");
const SECURITY_JS: &str = include_str!("../ui/js/security.js");
const WEBSTORE_JS: &str = include_str!("../ui/js/webstore.js");
const EXTASK_JS: &str = include_str!("../ui/js/extask.js");
const EXTENSIONS_JS: &str = include_str!("../ui/js/extensions.js");
const EXT_BUTTON_JS: &str = include_str!("../ui/js/ext-button.js");
const APP_CSS: &str = include_str!("../ui/dist/app.css");

/// No network, no inline script, no eval, no frames, no forms, no plugins.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                       connect-src 'none'; font-src 'none'; media-src 'none'; object-src 'none'; \
                       frame-src 'none'; worker-src 'none'; base-uri 'none'; form-action 'none'; \
                       frame-ancestors 'none'";

/// The UI page's real origin. wry maps `xb://ui/` to `http://xb.ui/` on
/// Windows and Android (`https` only with `with_https_scheme(true)`, which we
/// do not use); elsewhere it stays as is.
#[cfg(any(windows, target_os = "android"))]
const UI_ORIGIN: &str = "http://xb.ui/";
#[cfg(not(any(windows, target_os = "android")))]
const UI_ORIGIN: &str = "xb://ui/";

/// Whether `url` is on the UI page's origin (exactly; no other scheme).
pub fn is_ui_origin(url: &str) -> bool {
    url.starts_with(UI_ORIGIN)
}

/// URLs a content tab must never load: the UI origin, and host `xb.ui` under
/// any scheme (in a content webview that is a real network request, in the
/// profile the UI shares).
pub fn is_ui_url(url: &str) -> bool {
    is_ui_origin(url) || crate::url::host_of(url).as_deref() == Some("xb.ui")
}

/// WebView2 browser arguments for the one environment every webview shares.
/// wry's default is `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`
/// plus `--autoplay-policy=no-user-gesture-required`; we keep the two UI
/// tweaks (no "mini menu" on text selection) but leave SmartScreen on and
/// Chromium's default autoplay policy in place.
///
/// Keyboard Lock (`navigator.keyboard.lock()`) would let a fullscreen page
/// take Esc and F11 from the app, and Alt+Tab or the Windows key from the
/// system, while the strip is hidden. `SystemKeyboardLock` off keeps the
/// system keys out of its reach (Chromium then uses no OS keyboard hook);
/// the API itself has no switch (it is not a Blink runtime feature, so
/// `--disable-blink-features=KeyboardLock` does nothing), and content tabs
/// remove it with a script instead (`page_tools::NO_KEYBOARD_LOCK`).
#[cfg(windows)]
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,SystemKeyboardLock";

/// The WebView2 profile of the UI strip and the overlay. Content tabs use the
/// default profile, the only one extensions are ever added to: an extension
/// in the UI's profile could run a content script in the UI page and reach
/// its IPC (see `extensions`).
#[cfg(windows)]
pub const UI_PROFILE: &str = "XbUi";

fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    Some(match path {
        "/" | "/index.html" => (INDEX_HTML, "text/html; charset=utf-8"),
        "/js/core.js" => (CORE_JS, "text/javascript; charset=utf-8"),
        "/js/strip.js" => (STRIP_JS, "text/javascript; charset=utf-8"),
        "/js/suggest.js" => (SUGGEST_JS, "text/javascript; charset=utf-8"),
        "/overlay.html" => (OVERLAY_HTML, "text/html; charset=utf-8"),
        "/js/overlay.js" => (OVERLAY_JS, "text/javascript; charset=utf-8"),
        "/js/bookmarks.js" => (BOOKMARKS_JS, "text/javascript; charset=utf-8"),
        "/js/menu.js" => (MENU_JS, "text/javascript; charset=utf-8"),
        "/js/settings.js" => (SETTINGS_JS, "text/javascript; charset=utf-8"),
        "/js/security.js" => (SECURITY_JS, "text/javascript; charset=utf-8"),
        "/js/webstore.js" => (WEBSTORE_JS, "text/javascript; charset=utf-8"),
        "/js/extask.js" => (EXTASK_JS, "text/javascript; charset=utf-8"),
        "/js/extensions.js" => (EXTENSIONS_JS, "text/javascript; charset=utf-8"),
        "/js/ext-button.js" => (EXT_BUTTON_JS, "text/javascript; charset=utf-8"),
        "/app.css" => (APP_CSS, "text/css; charset=utf-8"),
        _ => return None,
    })
}

fn serve(req: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let builder = Response::builder()
        .header(header::CONTENT_SECURITY_POLICY, CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header(header::CACHE_CONTROL, "no-store");
    let found = (req.method() == "GET").then(|| asset(req.uri().path())).flatten();
    match found {
        Some((body, mime)) => builder
            .header(header::CONTENT_TYPE, mime)
            .body(Cow::Borrowed(body.as_bytes())),
        None => builder.status(404).body(Cow::Borrowed(&b""[..])),
    }
    .unwrap_or_else(|_| Response::new(Cow::Borrowed(&b""[..])))
}

/// The command an IPC message from page `origin` asks for, if it may. The
/// handler is only installed on our own webviews; the message must also
/// come from our own page, in case it was navigated. The launch question is
/// answered from the overlay (`overlay`) only.
fn ipc_command(origin: &str, body: &str, overlay: bool) -> Option<UiCommand> {
    if !is_ui_origin(origin) {
        return None;
    }
    // The launch answer, the Web Store question's answers and the
    // extensions page's own commands come from the overlay only; the
    // strip sends the store button's and its extensions button's.
    UiCommand::parse(body).filter(|cmd| match cmd {
        UiCommand::Launch(_) => overlay,
        UiCommand::Store(c) => overlay == c.is_answer(),
        UiCommand::Ext(x) => overlay || x.strip_may_send(),
        _ => true,
    })
}

/// What the UI and overlay webviews share: our assets on the `xb` protocol,
/// the one IPC handler, and no way off the UI origin. `overlay` says which of
/// the two this is: an answer to the launch question is taken from the
/// overlay only, where the question is.
fn ui_builder<'a>(
    context: &'a mut WebContext,
    proxy: &Proxy,
    url: &str,
    bounds: Rect,
    background: (u8, u8, u8, u8),
    overlay: bool,
) -> WebViewBuilder<'a> {
    let p = proxy.clone();
    let builder = WebViewBuilder::new_with_web_context(context)
        .with_custom_protocol(PROTOCOL.into(), |_, req| serve(req))
        .with_url(url)
        .with_bounds(bounds)
        .with_background_color(background)
        .with_ipc_handler(move |req: Request<String>| {
            if let Some(cmd) = ipc_command(&req.uri().to_string(), req.body(), overlay) {
                let _ = p.send_event(UserEvent::Ui(cmd));
            }
        })
        // The UI never leaves its own origin; links can't open windows.
        .with_navigation_handler(|url| is_ui_origin(&url))
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_hotkeys_zoom(false);
    #[cfg(windows)]
    let builder = {
        use wry::WebViewBuilderExtWindows;
        // F5/Ctrl+R etc. must never reload the UI itself; our accelerator
        // handler still sees every key.
        builder
            .with_browser_accelerator_keys(false)
            .with_default_context_menus(cfg!(debug_assertions))
            .with_profile_name(UI_PROFILE)
    };
    builder
}

pub fn build(
    window: &Window,
    context: &mut WebContext,
    proxy: &Proxy,
    bounds: Rect,
    background: (u8, u8, u8, u8),
) -> wry::Result<WebView> {
    let builder = ui_builder(context, proxy, START_URL, bounds, background, false);
    #[cfg(windows)]
    let builder = {
        use wry::WebViewBuilderExtWindows;
        // This builder creates the environment (content tabs reuse it), so
        // its arguments are the ones every webview runs with. Extensions are
        // an environment option too; only the content profile gets any.
        builder.with_additional_browser_args(BROWSER_ARGS).with_browser_extensions_enabled(true)
    };
    builder.build_as_child(window)
}

/// The overlay webview: the UI's settings on its own page, created hidden
/// and without taking focus (it is shown by `overlay`).
pub fn build_overlay(
    window: &Window,
    context: &mut WebContext,
    #[cfg(windows)] environment: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment,
    proxy: &Proxy,
    bounds: Rect,
    background: (u8, u8, u8, u8),
) -> wry::Result<WebView> {
    let builder = ui_builder(context, proxy, OVERLAY_URL, bounds, background, true).with_visible(false).with_focused(false);
    #[cfg(windows)]
    let builder = {
        use wry::WebViewBuilderExtWindows;
        builder.with_environment(environment.clone())
    };
    builder.build_as_child(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `id="..."` in a page is there once: `getElementById` returns
    /// the first, so a second one is silently left out.
    #[test]
    fn page_ids_are_unique() {
        for (page, html) in [("index.html", INDEX_HTML), ("overlay.html", OVERLAY_HTML)] {
            let mut seen = std::collections::HashSet::new();
            let mut rest = html;
            while let Some(at) = rest.find("id=\"") {
                let before = rest[..at].chars().next_back();
                rest = &rest[at + 4..];
                // `data-id`, `aria-...`: not an id.
                if !before.is_some_and(char::is_whitespace) {
                    continue;
                }
                let id = &rest[..rest.find('"').expect("closed")];
                assert!(seen.insert(id), "{page}: id \"{id}\" more than once");
            }
            assert!(seen.len() > 10, "{page}: {} ids", seen.len());
        }
    }

    #[test]
    fn ui_origin_check() {
        #[cfg(windows)]
        {
            assert!(is_ui_origin("http://xb.ui/index.html"));
            assert!(!is_ui_origin("xb://ui/index.html"), "not the origin on Windows");
        }
        #[cfg(not(any(windows, target_os = "android")))]
        {
            assert!(is_ui_origin("xb://ui/index.html"));
            assert!(!is_ui_origin("http://xb.ui/index.html"));
        }
        assert!(!is_ui_origin("https://xb.ui/index.html"), "https is never the UI origin");
        assert!(!is_ui_origin("http://xb.ui.evil.com/"));
        assert!(!is_ui_origin("https://example.com/?http://xb.ui/"));
        assert!(!is_ui_origin("http://xb.uix/"));
        assert!(!is_ui_origin(""));
    }

    #[test]
    fn only_the_ui_origin_reaches_bookmarks_and_the_launch_question() {
        // How WebView2 reports our pages (custom protocols as http://<name>.<host>).
        let ui = if cfg!(windows) { "http://xb.ui/index.html" } else { START_URL };
        let bm = ["bm-remove", "bm-save:x", "bm-star:1:1", "bm-open:1:0:t", "bm-delete:1:0", "bm-filter:a"];
        for body in bm {
            assert!(matches!(ipc_command(ui, body, false), Some(UiCommand::Bookmark(_))), "{body}");
            for page in ["https://example.com/", "http://xb.ui.evil.com/", "https://xb.ui/", "about:blank", ""] {
                assert_eq!(ipc_command(page, body, false), None, "{page} {body}");
                assert_eq!(ipc_command(page, body, true), None, "{page} {body}");
            }
        }
        assert!(ipc_command(ui, "launch-open", true).is_some(), "the overlay answers the question");
        assert_eq!(ipc_command(ui, "launch-open", false), None, "the strip does not");
        assert_eq!(ipc_command("https://example.com/", "launch-open", true), None);
        for answer in ["ext-yes", "ext-no", "ext-press"] {
            assert!(matches!(ipc_command(ui, answer, true), Some(UiCommand::Store(_))), "{answer}");
            assert_eq!(ipc_command(ui, answer, false), None, "the strip does not answer: {answer}");
            assert_eq!(ipc_command("https://chromewebstore.google.com/", answer, true), None);
        }
        assert!(matches!(ipc_command(ui, "ext-store", false), Some(UiCommand::Store(_))), "the strip's button");
        assert_eq!(ipc_command(ui, "ext-store", true), None, "not from the overlay");
        assert_eq!(ipc_command(ui, "ext-yes:1", true), None, "no argument");
        // The extensions page's commands: the overlay only; the button's: both.
        let id = "mmbkafeccnpmgicaldnppdljpaogpjdm";
        for body in [format!("ext-remove:{id}"), format!("ext-enable:{id}:0"), format!("ext-options:{id}"), format!("ext-popup:{id}")] {
            assert!(ipc_command(ui, &body, true).is_some(), "{body}");
            assert_eq!(ipc_command(ui, &body, false), None, "{body}");
            assert_eq!(ipc_command("https://example.com/", &body, true), None, "{body}");
        }
        // The page's update check: the overlay only.
        assert!(matches!(ipc_command(ui, "ext-check-updates", true), Some(UiCommand::Ext(crate::ext_ui::ExtCmd::CheckUpdates))));
        assert_eq!(ipc_command(ui, "ext-check-updates", false), None, "not from the strip");
        assert_eq!(ipc_command("https://example.com/", "ext-check-updates", true), None);
        // The page's store link: the overlay only.
        assert!(matches!(ipc_command(ui, "ext-open-store", true), Some(UiCommand::Ext(crate::ext_ui::ExtCmd::Store))));
        assert_eq!(ipc_command(ui, "ext-open-store", false), None, "not from the strip");
        assert_eq!(ipc_command("https://example.com/", "ext-open-store", true), None);
        assert!(ipc_command(ui, "ext-actions:900", false).is_some());
        assert!(ipc_command(ui, "ext-actions:900", true).is_some());
        assert_eq!(ipc_command("chrome-extension://mmbkafeccnpmgicaldnppdljpaogpjdm/", "ext-actions:9", false), None);
    }

    #[test]
    fn content_may_not_load_ui_host() {
        assert!(is_ui_url("http://xb.ui/index.html"));
        assert!(is_ui_url("https://xb.ui/"));
        assert!(is_ui_url("http://XB.UI./js/core.js"));
        assert!(is_ui_url("http://user@xb.ui:8080/"));
        assert!(is_ui_url("ws://xb.ui/"));
        assert!(!is_ui_url("http://xb.ui.evil.com/"));
        assert!(!is_ui_url("https://example.com/?http://xb.ui/"));
        assert!(!is_ui_url("about:blank"));
        assert!(!is_ui_url("https://example.com/"));
    }

    #[cfg(windows)]
    #[test]
    fn browser_args_keep_smartscreen_and_default_autoplay() {
        assert!(BROWSER_ARGS.contains("msWebOOUI") && BROWSER_ARGS.contains("msPdfOOUI"));
        assert!(!BROWSER_ARGS.contains("SmartScreen"));
        assert!(!BROWSER_ARGS.contains("autoplay"));
        assert!(BROWSER_ARGS.contains("SystemKeyboardLock"));
    }

    #[test]
    fn serves_assets_with_csp() {
        let get = |p: &str| serve(Request::get(format!("xb://ui{p}")).body(Vec::new()).unwrap());
        let r = get("/index.html");
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()[header::CONTENT_SECURITY_POLICY], CSP);
        assert!(r.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
        assert_eq!(get("/js/core.js").status(), 200);
        assert_eq!(get("/js/strip.js").status(), 200);
        assert_eq!(get("/js/suggest.js").status(), 200);
        assert_eq!(get("/js/bookmarks.js").status(), 200);
        assert_eq!(get("/js/overlay.js").status(), 200);
        assert_eq!(get("/js/menu.js").status(), 200);
        assert_eq!(get("/js/settings.js").status(), 200);
        assert_eq!(get("/js/security.js").status(), 200);
        assert_eq!(get("/js/extensions.js").status(), 200);
        assert_eq!(get("/js/ext-button.js").status(), 200);
        let r = get("/overlay.html");
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()[header::CONTENT_SECURITY_POLICY], CSP);
        assert_eq!(get("/app.js").status(), 404);
        assert_eq!(get("/app.css").status(), 200);
        assert_eq!(get("/../Cargo.toml").status(), 404);
        assert_eq!(get("/secret").status(), 404);
        let post = serve(Request::post("xb://ui/index.html").body(Vec::new()).unwrap());
        assert_eq!(post.status(), 404);
    }

    #[test]
    fn csp_forbids_inline_and_network() {
        assert!(!CSP.contains("unsafe-inline"));
        assert!(!CSP.contains("unsafe-eval"));
        assert!(CSP.contains("default-src 'none'"));
        assert!(CSP.contains("connect-src 'none'"));
    }

    #[test]
    fn html_has_no_inline_script_or_handlers() {
        for html in [INDEX_HTML, OVERLAY_HTML] {
            assert!(!html.contains("<script>"), "no inline script");
            assert!(!html.contains("<style"), "no inline style blocks");
            assert!(!html.contains("style="), "no inline styles");
            // No inline event handlers: an attribute ` on<letters>=`.
            let lower = html.to_ascii_lowercase();
            for (i, _) in lower.match_indices(" on") {
                let rest = &lower[i + 3..];
                let name_len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
                assert!(
                    name_len == 0 || !rest[name_len..].trim_start().starts_with('='),
                    "inline event handler near byte {i}"
                );
            }
        }
    }

    #[test]
    fn scripts_never_write_html() {
        for js in [
            CORE_JS, STRIP_JS, SUGGEST_JS, OVERLAY_JS, BOOKMARKS_JS, MENU_JS, SETTINGS_JS, SECURITY_JS, WEBSTORE_JS,
            EXTASK_JS, EXTENSIONS_JS, EXT_BUTTON_JS,
        ] {
            for bad in [".innerHTML", ".outerHTML", "insertAdjacentHTML", "document.write", "eval("] {
                assert!(!js.contains(bad), "{bad}");
            }
        }
    }
}
