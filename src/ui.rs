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

const INDEX_HTML: &str = include_str!("../ui/index.html");
const CORE_JS: &str = include_str!("../ui/js/core.js");
const STRIP_JS: &str = include_str!("../ui/js/strip.js");
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
#[cfg(windows)]
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI";

fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    Some(match path {
        "/" | "/index.html" => (INDEX_HTML, "text/html; charset=utf-8"),
        "/js/core.js" => (CORE_JS, "text/javascript; charset=utf-8"),
        "/js/strip.js" => (STRIP_JS, "text/javascript; charset=utf-8"),
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

pub fn build(
    window: &Window,
    context: &mut WebContext,
    proxy: &Proxy,
    bounds: Rect,
    background: (u8, u8, u8, u8),
) -> wry::Result<WebView> {
    let p = proxy.clone();
    let builder = WebViewBuilder::new_with_web_context(context)
        .with_custom_protocol(PROTOCOL.into(), |_, req| serve(req))
        .with_url(START_URL)
        .with_bounds(bounds)
        .with_background_color(background)
        .with_ipc_handler(move |req: Request<String>| {
            // The handler is only installed on this webview; also require the
            // message to come from our own page, in case it was navigated.
            if !is_ui_origin(&req.uri().to_string()) {
                return;
            }
            if let Some(cmd) = UiCommand::parse(req.body()) {
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
            // This builder creates the environment (content tabs reuse it), so
            // its arguments are the ones every webview runs with.
            .with_additional_browser_args(BROWSER_ARGS)
    };
    builder.build_as_child(window)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!INDEX_HTML.contains("<script>"), "no inline script");
        assert!(!INDEX_HTML.contains("<style"), "no inline style blocks");
        assert!(!INDEX_HTML.contains("style="), "no inline styles");
        // No inline event handlers: an attribute ` on<letters>=`.
        let lower = INDEX_HTML.to_ascii_lowercase();
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
