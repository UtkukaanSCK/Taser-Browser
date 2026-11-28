//! Taser's own page for a load that failed, in place of WebView2's
//! built-in (Edge-branded) one, which every tab turns off
//! (`native::harden_content`).
//!
//! With the built-in page off, a failed load still commits a document: an
//! empty error document (`chrome-error://chromewebdata/`) whose history
//! entry and `Source` keep the failed address. `native::attach_error_page`
//! fills that document with this page once the navigation has failed. The
//! address bar, Back/Forward and the lock rule (an error document never
//! gets one, `policy::State::indicator`) all stay as they were.
//!
//! No site can script or fill this page: the script only writes into an
//! error document (it checks the document's own address first, which no
//! site can have), and the failed address is put in as text, never markup.

/// What kind of failure a page explains, from WebView2's `WebErrorStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// No such host, or it would not take the connection.
    Unreachable,
    /// This machine has no network.
    Offline,
    Timeout,
    Certificate,
    Other,
}

/// `COREWEBVIEW2_WEB_ERROR_STATUS` values (WebView2.h); a test checks them
/// against the bindings.
pub mod status {
    pub const UNKNOWN: i32 = 0;
    pub const CERTIFICATE_COMMON_NAME_IS_INCORRECT: i32 = 1;
    pub const CERTIFICATE_EXPIRED: i32 = 2;
    pub const CLIENT_CERTIFICATE_CONTAINS_ERRORS: i32 = 3;
    pub const CERTIFICATE_REVOKED: i32 = 4;
    pub const CERTIFICATE_IS_INVALID: i32 = 5;
    pub const SERVER_UNREACHABLE: i32 = 6;
    pub const TIMEOUT: i32 = 7;
    pub const ERROR_HTTP_INVALID_SERVER_RESPONSE: i32 = 8;
    pub const CONNECTION_ABORTED: i32 = 9;
    pub const CONNECTION_RESET: i32 = 10;
    pub const DISCONNECTED: i32 = 11;
    pub const CANNOT_CONNECT: i32 = 12;
    pub const HOST_NAME_NOT_RESOLVED: i32 = 13;
    pub const OPERATION_CANCELED: i32 = 14;
    pub const REDIRECT_FAILED: i32 = 15;
    pub const UNEXPECTED_ERROR: i32 = 16;
    pub const VALID_AUTHENTICATION_CREDENTIALS_REQUIRED: i32 = 17;
    pub const VALID_PROXY_AUTHENTICATION_REQUIRED: i32 = 18;
}

/// The kind of page for a failed navigation's status. `None` for a
/// navigation that was cancelled (stopped, refused, replaced by another, a
/// download): nothing failed, so nothing is explained.
pub fn kind(web_error_status: i32) -> Option<Kind> {
    use status::*;
    Some(match web_error_status {
        OPERATION_CANCELED => return None,
        SERVER_UNREACHABLE | CANNOT_CONNECT | HOST_NAME_NOT_RESOLVED | CONNECTION_ABORTED | CONNECTION_RESET => {
            Kind::Unreachable
        }
        DISCONNECTED => Kind::Offline,
        TIMEOUT => Kind::Timeout,
        CERTIFICATE_COMMON_NAME_IS_INCORRECT
        | CERTIFICATE_EXPIRED
        | CLIENT_CERTIFICATE_CONTAINS_ERRORS
        | CERTIFICATE_REVOKED
        | CERTIFICATE_IS_INVALID => Kind::Certificate,
        UNKNOWN
        | ERROR_HTTP_INVALID_SERVER_RESPONSE
        | REDIRECT_FAILED
        | UNEXPECTED_ERROR
        | VALID_AUTHENTICATION_CREDENTIALS_REQUIRED
        | VALID_PROXY_AUTHENTICATION_REQUIRED => Kind::Other,
        // A status newer than this list.
        _ => Kind::Other,
    })
}

/// The page's title and its short explanation.
pub fn texts(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Unreachable => (
            "Bu siteye ulaşılamıyor",
            "Sunucu bulunamadı ya da bağlantıyı kabul etmedi. Adresi denetleyin ya da biraz sonra yeniden deneyin.",
        ),
        Kind::Offline => (
            "İnternet bağlantınız yok",
            "Bilgisayarınız internete bağlı görünmüyor. Ağ bağlantınızı denetleyip yeniden deneyin.",
        ),
        Kind::Timeout => ("Bağlantı zaman aşımına uğradı", "Sunucu zamanında yanıt vermedi. Biraz sonra yeniden deneyin."),
        Kind::Certificate => (
            "Bu sitenin güvenlik sertifikası geçersiz",
            "Bağlantının güvenli olduğu doğrulanamadı; araya biri girmiş olabilir. Sayfa bu yüzden açılmadı.",
        ),
        Kind::Other => ("Bu sayfa açılamadı", "Sayfa yüklenirken bir hata oluştu. Yeniden denemek işe yarayabilir."),
    }
}

/// The retry button's label.
pub const RETRY: &str = "Yeniden dene";

/// Characters of the failed address shown at most (the rest is cut, marked
/// with "…"); the address bar still has all of it.
const MAX_SHOWN_URL: usize = 1000;

/// The page's look: the colors and type of `ui/input.css`, light and dark,
/// on the toolbar's color like the new tab page.
const CSS: &str = ":root { color-scheme: light dark; --page: #f6f7f8; --field: #e9ebee; --ink: #1b1f24; \
  --muted: #575f69; --edge: #868d97; --hover: rgb(24 32 44 / 0.07); --accent: #3d4a5c; --on-accent: #ffffff; } \
  @media (prefers-color-scheme: dark) { :root { --page: #212428; --field: #191b1f; --ink: #e8eaed; \
  --muted: #a4abb4; --edge: #7c838d; --hover: rgb(255 255 255 / 0.08); --accent: #a7b6c8; --on-accent: #16191d; } } \
  html, body { height: 100%; margin: 0; } \
  body { background: var(--page); color: var(--ink); display: flex; justify-content: center; \
  font: 14px/1.5 \"Segoe UI Variable Text\", \"Segoe UI\", system-ui, sans-serif; } \
  main { box-sizing: border-box; width: 100%; max-width: 600px; padding: 16vh 24px 24px; } \
  h1 { font-size: 24px; font-weight: 600; line-height: 1.3; margin: 0 0 10px; } \
  p { margin: 0 0 18px; color: var(--muted); } \
  #xb-error-url { color: var(--ink); overflow-wrap: anywhere; padding: 8px 12px; background: var(--field); \
  border-radius: 8px; user-select: text; } \
  button { font: inherit; font-weight: 500; color: var(--on-accent); background: var(--accent); border: 0; border-radius: 8px; \
  height: 32px; padding: 0 16px; cursor: pointer; } \
  button:hover { filter: brightness(1.08); } \
  button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; } \
  .xb-buttons { display: flex; flex-wrap: wrap; gap: 8px; } \
  button.xb-alt { color: var(--ink); background: transparent; border: 1px solid var(--edge); } \
  button.xb-alt:hover { filter: none; background: var(--hover); }";

/// The script that turns the empty error document into this page, or
/// `None` when there is nothing to explain (`kind`). `url` is the failed
/// address; it and every text go in as JSON strings, and into the page as
/// text only. Run in any other document, the script does nothing and
/// returns `"skip"` (`"shown"` when it filled the page).
///
/// "Yeniden dene" reloads: an error document's reload loads its failed
/// address again, in the same history entry.
pub fn script(web_error_status: i32, url: &str) -> Option<String> {
    let (title, text) = texts(kind(web_error_status)?);
    Some(page(title, text, url, None))
}

/// The HTTPS-only page's title and explanation (`privacy`).
pub const HTTPS_ONLY_TEXTS: (&str, &str) = (
    "Bu site güvenli bağlantı sunmuyor",
    "Sitenin şifreli (https) sürümü açılamadı. Yine de açarsanız gönderdiğiniz ve gördüğünüz her şey yolda \
     okunabilir ya da değiştirilebilir.",
);

/// The HTTPS-only page's button that opens the http address anyway.
pub const OPEN_HTTP: &str = "Yine de http ile aç";

/// The page for an `http://` address whose `https://` form failed to load
/// (`privacy`): the failed https address is the document's, `http_url` is
/// shown. "Yine de http ile aç" goes to `sentinel`, an address the tab's
/// navigation hook stops and answers with `http_url` (only for this error
/// document); "Yeniden dene" tries https again.
pub fn https_only_script(http_url: &str, sentinel: &str) -> String {
    let (title, text) = HTTPS_ONLY_TEXTS;
    page(title, text, http_url, Some((OPEN_HTTP, sentinel)))
}

fn page(title: &str, text: &str, url: &str, open: Option<(&str, &str)>) -> String {
    let shown: String = if url.chars().count() > MAX_SHOWN_URL {
        url.chars().take(MAX_SHOWN_URL).chain(['…']).collect()
    } else {
        url.to_string()
    };
    let lit = |s: &str| {
        let mut out = String::new();
        crate::tabs::push_json_str(&mut out, s);
        out
    };
    // A first button that leaves for `to` (the retry button then steps back).
    let open = open.map_or(String::new(), |(label, to)| {
        format!(
            "const open = make('button', {{ id: 'xb-error-open', type: 'button', textContent: {label} }}); \
             open.addEventListener('click', () => {{ location.href = {to}; }}); \
             retry.className = 'xb-alt'; buttons.prepend(open); ",
            label = lit(label),
            to = lit(to),
        )
    });
    format!(
        "(() => {{ if (!document.URL.startsWith('chrome-error:')) return 'skip'; \
         const make = (tag, props) => Object.assign(document.createElement(tag), props); \
         const sheet = new CSSStyleSheet(); sheet.replaceSync({css}); \
         document.adoptedStyleSheets = [sheet]; \
         const root = document.documentElement; root.lang = 'tr'; \
         const main = make('main', {{ id: 'xb-error' }}); \
         const retry = make('button', {{ id: 'xb-error-retry', type: 'button', textContent: {retry} }}); \
         retry.addEventListener('click', () => location.reload()); \
         const buttons = make('div', {{ className: 'xb-buttons' }}); buttons.append(retry); \
         {open}main.append(make('h1', {{ textContent: {title} }}), make('p', {{ textContent: {text} }}), \
           make('p', {{ id: 'xb-error-url', textContent: {url} }}), buttons); \
         root.replaceChildren(make('head'), make('body')); \
         document.body.append(main); \
         document.title = {title}; \
         return 'shown'; }})()",
        css = lit(CSS),
        retry = lit(RETRY),
        title = lit(title),
        text = lit(text),
        url = lit(&shown),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_has_its_page() {
        use status::*;
        for s in [SERVER_UNREACHABLE, CANNOT_CONNECT, HOST_NAME_NOT_RESOLVED, CONNECTION_ABORTED, CONNECTION_RESET] {
            assert_eq!(kind(s), Some(Kind::Unreachable), "status {s}");
        }
        assert_eq!(kind(DISCONNECTED), Some(Kind::Offline));
        assert_eq!(kind(TIMEOUT), Some(Kind::Timeout));
        for s in [
            CERTIFICATE_COMMON_NAME_IS_INCORRECT,
            CERTIFICATE_EXPIRED,
            CLIENT_CERTIFICATE_CONTAINS_ERRORS,
            CERTIFICATE_REVOKED,
            CERTIFICATE_IS_INVALID,
        ] {
            assert_eq!(kind(s), Some(Kind::Certificate), "status {s}");
        }
        for s in [
            UNKNOWN,
            ERROR_HTTP_INVALID_SERVER_RESPONSE,
            REDIRECT_FAILED,
            UNEXPECTED_ERROR,
            VALID_AUTHENTICATION_CREDENTIALS_REQUIRED,
            VALID_PROXY_AUTHENTICATION_REQUIRED,
            19,
            -1,
        ] {
            assert_eq!(kind(s), Some(Kind::Other), "status {s}");
        }
        assert_eq!(kind(OPERATION_CANCELED), None, "a cancelled navigation explains nothing");
        assert_eq!(script(OPERATION_CANCELED, "https://example.com/"), None);
    }

    #[test]
    fn texts_are_the_turkish_ones() {
        assert_eq!(texts(Kind::Unreachable).0, "Bu siteye ulaşılamıyor");
        assert_eq!(texts(Kind::Offline).0, "İnternet bağlantınız yok");
        assert_eq!(texts(Kind::Timeout).0, "Bağlantı zaman aşımına uğradı");
        assert_eq!(texts(Kind::Certificate).0, "Bu sitenin güvenlik sertifikası geçersiz");
        assert_eq!(texts(Kind::Other).0, "Bu sayfa açılamadı");
        for k in [Kind::Unreachable, Kind::Offline, Kind::Timeout, Kind::Certificate, Kind::Other] {
            let (title, text) = texts(k);
            assert!(!title.is_empty() && !text.is_empty() && title != text, "{k:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn status_values_match_the_bindings() {
        use webview2_com::Microsoft::Web::WebView2::Win32::*;
        let pairs = [
            (COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN, status::UNKNOWN),
            (COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT, status::CERTIFICATE_COMMON_NAME_IS_INCORRECT),
            (COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED, status::CERTIFICATE_EXPIRED),
            (COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS, status::CLIENT_CERTIFICATE_CONTAINS_ERRORS),
            (COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED, status::CERTIFICATE_REVOKED),
            (COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID, status::CERTIFICATE_IS_INVALID),
            (COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE, status::SERVER_UNREACHABLE),
            (COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT, status::TIMEOUT),
            (COREWEBVIEW2_WEB_ERROR_STATUS_ERROR_HTTP_INVALID_SERVER_RESPONSE, status::ERROR_HTTP_INVALID_SERVER_RESPONSE),
            (COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED, status::CONNECTION_ABORTED),
            (COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET, status::CONNECTION_RESET),
            (COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED, status::DISCONNECTED),
            (COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT, status::CANNOT_CONNECT),
            (COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED, status::HOST_NAME_NOT_RESOLVED),
            (COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED, status::OPERATION_CANCELED),
            (COREWEBVIEW2_WEB_ERROR_STATUS_REDIRECT_FAILED, status::REDIRECT_FAILED),
            (COREWEBVIEW2_WEB_ERROR_STATUS_UNEXPECTED_ERROR, status::UNEXPECTED_ERROR),
            (COREWEBVIEW2_WEB_ERROR_STATUS_VALID_AUTHENTICATION_CREDENTIALS_REQUIRED, status::VALID_AUTHENTICATION_CREDENTIALS_REQUIRED),
            (COREWEBVIEW2_WEB_ERROR_STATUS_VALID_PROXY_AUTHENTICATION_REQUIRED, status::VALID_PROXY_AUTHENTICATION_REQUIRED),
        ];
        for (binding, ours) in pairs {
            assert_eq!(binding.0, ours);
        }
    }

    #[test]
    fn script_writes_only_into_an_error_document_and_only_text() {
        let url = "https://evil.example/</script><img src=x onerror=alert(1)>\"';`${x}`\u{2028}";
        let js = script(status::HOST_NAME_NOT_RESOLVED, url).unwrap();
        assert!(js.starts_with("(() => { if (!document.URL.startsWith('chrome-error:')) return 'skip';"), "guard first");
        assert!(!js.contains("innerHTML") && !js.contains("insertAdjacentHTML") && !js.contains("document.write"), "text only");
        assert!(!js.contains('<'), "no markup can open: every '<' is escaped");
        assert!(!js.contains('\u{2028}'), "no raw line separator");
        assert!(js.contains("\\u003c/script>\\u003cimg src=x onerror=alert(1)>\\\"';`${x}`\\u2028"), "{js}");
        assert!(js.contains("\"Bu siteye ulaşılamıyor\"") && js.contains("\"Yeniden dene\"") && js.contains("location.reload()"));
    }

    #[test]
    fn the_https_only_page_offers_http_through_the_sentinel_only() {
        let js = https_only_script("http://evil.example/<b>'\"", "https://xb-http.invalid/");
        assert!(js.starts_with("(() => { if (!document.URL.startsWith('chrome-error:')) return 'skip';"), "guard first");
        assert!(!js.contains("innerHTML") && !js.contains("insertAdjacentHTML") && !js.contains("document.write"), "text only");
        assert!(!js.contains('<'), "no markup can open");
        assert!(js.contains("\"Bu site güvenli bağlantı sunmuyor\"") && js.contains("\"Yine de http ile aç\""));
        assert!(js.contains("location.href = \"https://xb-http.invalid/\""), "{js}");
        assert!(!js.contains("location.href = \"http://evil"), "the http address is only shown, never gone to from here");
        assert!(js.contains("\"Yeniden dene\"") && js.contains("location.reload()"));
        let plain = script(status::TIMEOUT, "https://example.com/").unwrap();
        assert!(!plain.contains("xb-error-open") && !plain.contains("location.href"), "other pages have no such button");
    }

    #[test]
    fn a_long_address_is_cut_for_the_page() {
        let url = format!("https://example.com/{}", "a".repeat(5000));
        let js = script(status::TIMEOUT, &url).unwrap();
        assert!(js.contains(&format!("{}…\"", "a".repeat(MAX_SHOWN_URL - "https://example.com/".len()))));
        assert!(!js.contains(&"a".repeat(MAX_SHOWN_URL)));
    }
}
