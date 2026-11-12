//! Address bar input → navigable URL. Pure logic, no platform code.

pub const SEARCH_URL: &str = "https://duckduckgo.com/?q=";
pub const BLANK: &str = "about:blank";

#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    /// Navigate to this URL.
    Url(String),
    /// Input is empty; do nothing.
    Empty,
    /// Scheme is not allowed from the address bar (`javascript:`, `file:`, drive paths...).
    Blocked,
}

/// Schemes the address bar refuses to navigate to.
const BLOCKED_SCHEMES: &[&str] = &["javascript", "file", "vbscript"];
/// Schemes passed through unchanged.
const ALLOWED_SCHEMES: &[&str] = &["http", "https", "about"];

/// Turns what the user typed into a URL, a search, or a refusal.
pub fn resolve_input(raw: &str) -> Resolved {
    // Like the WHATWG URL parser: drop tabs/newlines anywhere, trim C0 controls
    // and spaces at both ends. Otherwise "java\tscript:" would slip through.
    let cleaned: String = raw.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let input = cleaned.trim_matches(|c: char| c <= ' ');
    if input.is_empty() {
        return Resolved::Empty;
    }

    if let Some((scheme, rest)) = split_scheme(input) {
        let scheme = scheme.to_ascii_lowercase();
        if BLOCKED_SCHEMES.contains(&scheme.as_str()) {
            return Resolved::Blocked;
        }
        // "C:\Users" or "c:/x": a Windows drive path, i.e. a file URL in disguise.
        if scheme.len() == 1 && (rest.starts_with('\\') || rest.starts_with('/')) {
            return Resolved::Blocked;
        }
        if ALLOWED_SCHEMES.contains(&scheme.as_str()) {
            if (scheme == "http" || scheme == "https") && !rest.starts_with("//") {
                return Resolved::Url(format!("{scheme}://{}", rest.trim_start_matches('/')));
            }
            return Resolved::Url(input.to_string());
        }
        // "localhost:8080/x" parses as scheme "localhost"; fall through to host checks.
        let port_like = rest.split(['/', '?', '#']).next().is_some_and(|p| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())
        });
        if !port_like {
            return Resolved::Url(search_url(input));
        }
    }
    // UNC paths "\\server\share" would also be file access.
    if input.starts_with("\\\\") {
        return Resolved::Blocked;
    }

    if input.contains(char::is_whitespace) {
        return Resolved::Url(search_url(input));
    }
    match host_kind(input) {
        HostKind::Loopback => Resolved::Url(format!("http://{input}")),
        HostKind::Public => Resolved::Url(format!("https://{input}")),
        HostKind::NotAHost => Resolved::Url(search_url(input)),
    }
}

/// Splits "scheme:rest" if the prefix is a syntactically valid URL scheme.
fn split_scheme(s: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = s.split_once(':')?;
    let mut bytes = scheme.bytes();
    let first_ok = bytes.next().is_some_and(|b| b.is_ascii_alphabetic());
    let rest_ok = bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    (first_ok && rest_ok).then_some((scheme, rest))
}

enum HostKind {
    Loopback,
    Public,
    NotAHost,
}

fn host_kind(input: &str) -> HostKind {
    let authority = input.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        // "user@host" style input is far more likely an email/search than a URL.
        return HostKind::NotAHost;
    }
    if authority.starts_with('[') {
        return if authority.contains(']') { ipv6_kind(authority) } else { HostKind::NotAHost };
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if let Some(p) = port
        && (p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.parse::<u16>().is_err())
    {
        return HostKind::NotAHost;
    }
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return HostKind::Loopback;
    }
    if let Some(octets) = parse_ipv4(&host) {
        return if octets[0] == 127 { HostKind::Loopback } else { HostKind::Public };
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| !valid_label(l)) {
        return HostKind::NotAHost;
    }
    let tld = labels[labels.len() - 1];
    // A TLD is alphabetic (or punycode "xn--..."), at least two characters.
    let tld_ok = tld.len() >= 2
        && (tld.bytes().all(|b| b.is_ascii_alphabetic()) || tld.starts_with("xn--"));
    if tld_ok { HostKind::Public } else { HostKind::NotAHost }
}

fn ipv6_kind(authority: &str) -> HostKind {
    let inner = &authority[1..authority.find(']').unwrap_or(1)];
    if inner.is_empty() || !inner.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.') {
        return HostKind::NotAHost;
    }
    if inner == "::1" { HostKind::Loopback } else { HostKind::Public }
}

fn valid_label(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 63
        && !l.starts_with('-')
        && !l.ends_with('-')
        && l.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

fn parse_ipv4(host: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = host.split('.');
    for slot in &mut out {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = p.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// DuckDuckGo search URL for free text.
pub fn search_url(query: &str) -> String {
    let mut out = String::with_capacity(SEARCH_URL.len() + query.len() * 3);
    out.push_str(SEARCH_URL);
    for b in query.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 15) as usize] as char);
            }
        }
    }
    out
}

/// Whether an untrusted page may open this URL in a new tab: http(s) only.
pub fn is_web_url(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// What the address bar shows for a URL (blank pages show nothing).
pub fn display_url(url: &str) -> &str {
    if url == BLANK { "" } else { url }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Resolved {
        Resolved::Url(s.to_string())
    }

    #[test]
    fn full_urls_pass_through() {
        assert_eq!(resolve_input("https://example.com/a?b=c"), url("https://example.com/a?b=c"));
        assert_eq!(resolve_input("http://example.com"), url("http://example.com"));
        assert_eq!(resolve_input("HTTPS://Example.com"), url("HTTPS://Example.com"));
        assert_eq!(resolve_input("about:blank"), url("about:blank"));
    }

    #[test]
    fn http_without_slashes_is_repaired() {
        assert_eq!(resolve_input("https:example.com"), url("https://example.com"));
    }

    #[test]
    fn schemeless_hosts_get_https() {
        assert_eq!(resolve_input("example.com"), url("https://example.com"));
        assert_eq!(resolve_input("  example.com/path?q=1#x  "), url("https://example.com/path?q=1#x"));
        assert_eq!(resolve_input("sub.domain.co.uk"), url("https://sub.domain.co.uk"));
        assert_eq!(resolve_input("example.com:8443"), url("https://example.com:8443"));
        assert_eq!(resolve_input("8.8.8.8"), url("https://8.8.8.8"));
        assert_eq!(resolve_input("[2001:db8::1]/x"), url("https://[2001:db8::1]/x"));
        assert_eq!(resolve_input("xn--bcher-kva.example"), url("https://xn--bcher-kva.example"));
        assert_eq!(resolve_input("türkçe.com"), url("https://türkçe.com"));
    }

    #[test]
    fn loopback_hosts_get_http() {
        assert_eq!(resolve_input("localhost"), url("http://localhost"));
        assert_eq!(resolve_input("localhost:3000/app"), url("http://localhost:3000/app"));
        assert_eq!(resolve_input("127.0.0.1:8080"), url("http://127.0.0.1:8080"));
        assert_eq!(resolve_input("[::1]:80"), url("http://[::1]:80"));
    }

    #[test]
    fn free_text_is_searched() {
        assert_eq!(resolve_input("rust lang"), url("https://duckduckgo.com/?q=rust+lang"));
        assert_eq!(resolve_input("rust"), url("https://duckduckgo.com/?q=rust"));
        assert_eq!(resolve_input("c++"), url("https://duckduckgo.com/?q=c%2B%2B"));
        assert_eq!(resolve_input("what is 1.5"), url("https://duckduckgo.com/?q=what+is+1.5"));
        assert_eq!(resolve_input("1.5"), url("https://duckduckgo.com/?q=1.5"));
        assert_eq!(resolve_input("me@example.com"), url("https://duckduckgo.com/?q=me%40example.com"));
        assert_eq!(resolve_input("note: buy milk"), url("https://duckduckgo.com/?q=note%3A+buy+milk"));
        assert_eq!(resolve_input("ğüş"), url("https://duckduckgo.com/?q=%C4%9F%C3%BC%C5%9F"));
        assert_eq!(resolve_input("foo.123"), url("https://duckduckgo.com/?q=foo.123"));
        assert_eq!(resolve_input("example.com:99999"), url("https://duckduckgo.com/?q=example.com%3A99999"));
    }

    #[test]
    fn dangerous_schemes_are_blocked() {
        assert_eq!(resolve_input("javascript:alert(1)"), Resolved::Blocked);
        assert_eq!(resolve_input("  JavaScript:alert(1)"), Resolved::Blocked);
        assert_eq!(resolve_input("java\tscript:alert(1)"), Resolved::Blocked);
        assert_eq!(resolve_input("java\nscript:alert(1)"), Resolved::Blocked);
        assert_eq!(resolve_input("\u{1}javascript:alert(1)"), Resolved::Blocked);
        assert_eq!(resolve_input("file:///C:/Windows/win.ini"), Resolved::Blocked);
        assert_eq!(resolve_input("FILE://server/share"), Resolved::Blocked);
        assert_eq!(resolve_input("vbscript:msgbox"), Resolved::Blocked);
        assert_eq!(resolve_input(r"C:\Windows\win.ini"), Resolved::Blocked);
        assert_eq!(resolve_input("c:/windows"), Resolved::Blocked);
        assert_eq!(resolve_input(r"\\server\share"), Resolved::Blocked);
    }

    #[test]
    fn empty_input_does_nothing() {
        assert_eq!(resolve_input(""), Resolved::Empty);
        assert_eq!(resolve_input("   \t "), Resolved::Empty);
    }

    #[test]
    fn only_web_urls_may_open_tabs() {
        assert!(is_web_url("https://example.com/x"));
        assert!(is_web_url("HTTP://example.com"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("file:///C:/x"));
        assert!(!is_web_url("about:blank"));
        assert!(!is_web_url("data:text/html,hi"));
        assert!(!is_web_url("http"));
        assert!(!is_web_url(""));
        assert!(!is_web_url("ğğğğğ"), "non-ASCII must not panic on byte slicing");
    }

    #[test]
    fn display_hides_blank() {
        assert_eq!(display_url("about:blank"), "");
        assert_eq!(display_url("https://a.com/"), "https://a.com/");
    }
}
