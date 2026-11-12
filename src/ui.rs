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
const APP_JS: &str = include_str!("../ui/app.js");
const APP_CSS: &str = include_str!("../ui/dist/app.css");

/// No network, no inline script, no eval, no frames, no forms, no plugins.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                       connect-src 'none'; font-src 'none'; media-src 'none'; object-src 'none'; \
                       frame-src 'none'; worker-src 'none'; base-uri 'none'; form-action 'none'; \
                       frame-ancestors 'none'";

/// Origins the UI page may have. wry maps `xb://ui/` to `http://xb.ui/` on
/// Windows and Android; elsewhere it stays as is.
pub fn is_ui_origin(url: &str) -> bool {
    ["xb://ui/", "http://xb.ui/", "https://xb.ui/"].iter().any(|o| url.starts_with(o))
}

fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    Some(match path {
        "/" | "/index.html" => (INDEX_HTML, "text/html; charset=utf-8"),
        "/app.js" => (APP_JS, "text/javascript; charset=utf-8"),
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
        .with_hotkeys_zoom(false)
        .with_clipboard(true);
    #[cfg(windows)]
    let builder = {
        use wry::WebViewBuilderExtWindows;
        // F5/Ctrl+R etc. must never reload the UI itself; our accelerator
        // handler still sees every key.
        builder
            .with_browser_accelerator_keys(false)
            .with_default_context_menus(cfg!(debug_assertions))
    };
    builder.build_as_child(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_origin_check() {
        assert!(is_ui_origin("http://xb.ui/index.html"));
        assert!(is_ui_origin("xb://ui/index.html"));
        assert!(!is_ui_origin("http://xb.ui.evil.com/"));
        assert!(!is_ui_origin("https://example.com/?http://xb.ui/"));
        assert!(!is_ui_origin("http://xb.uix/"));
        assert!(!is_ui_origin(""));
    }

    #[test]
    fn serves_assets_with_csp() {
        let get = |p: &str| serve(Request::get(format!("xb://ui{p}")).body(Vec::new()).unwrap());
        let r = get("/index.html");
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()[header::CONTENT_SECURITY_POLICY], CSP);
        assert!(r.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
        assert_eq!(get("/app.js").status(), 200);
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
