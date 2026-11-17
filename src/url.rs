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
        if scheme == "about" {
            // Only the blank page. Other about: URLs (about:flags,
            // about:settings...) map to the WebView's internal pages.
            return if rest.eq_ignore_ascii_case("blank") {
                Resolved::Url(BLANK.to_string())
            } else {
                Resolved::Url(search_url(input))
            };
        }
        if scheme == "http" || scheme == "https" {
            let url = if rest.starts_with("//") {
                input.to_string()
            } else {
                format!("{scheme}://{}", rest.trim_start_matches('/'))
            };
            // "http://", "http://:80", "https://[1]": nothing to connect to.
            let after = &url[scheme.len() + 3..];
            let authority = after.split(['/', '?', '#', '\\']).next().unwrap_or("");
            return if valid_authority(authority) {
                Resolved::Url(url)
            } else {
                Resolved::Url(search_url(input))
            };
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
    let Some((host, _port)) = split_host_port(authority) else { return HostKind::NotAHost };
    if let Some(inner) = host.strip_prefix('[') {
        let inner = inner.strip_suffix(']').unwrap_or("");
        return match parse_ipv6(inner) {
            Some(LOOPBACK_V6) => HostKind::Loopback,
            Some(_) => HostKind::Public,
            None => HostKind::NotAHost,
        };
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

/// Splits `host[:port]` (host may be a bracketed IPv6 literal, kept with its
/// brackets). `None` if the host is empty, a bracketed literal is not a valid
/// IPv6 address, or the port is not 1-5 digits up to 65535.
fn split_host_port(authority: &str) -> Option<(&str, Option<u16>)> {
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']')? + 1;
        let (host, after) = authority.split_at(end);
        parse_ipv6(&host[1..end - 1])?;
        match after {
            "" => (host, None),
            _ => (host, Some(after.strip_prefix(':')?)),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(p) if !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()) => Some(p.parse().ok()?),
        Some(_) => return None,
        None => None,
    };
    Some((host, port))
}

/// Authority of a URL typed with an explicit http(s) scheme: optional
/// `userinfo@`, then a non-empty host and a valid port. An empty port
/// (`http://example.com:`) means the default one, as in WHATWG URLs.
fn valid_authority(authority: &str) -> bool {
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host_port = host_port.strip_suffix(':').unwrap_or(host_port);
    split_host_port(host_port).is_some()
}

const LOOPBACK_V6: [u16; 8] = [0, 0, 0, 0, 0, 0, 0, 1];

/// Parses the text form of an IPv6 address (RFC 4291 section 2.2): up to
/// eight groups of 1-4 hex digits, at most one `::`, optionally ending in a
/// dotted IPv4 address. Zone ids (`%...`) are not valid in URLs.
fn parse_ipv6(s: &str) -> Option<[u16; 8]> {
    fn groups(part: &str, out: &mut Vec<u16>) -> Option<()> {
        if part.is_empty() {
            return Some(());
        }
        let fields: Vec<&str> = part.split(':').collect();
        for (i, f) in fields.iter().enumerate() {
            if i == fields.len() - 1 && f.contains('.') {
                let v4 = parse_ipv4(f)?;
                out.push(u16::from_be_bytes([v4[0], v4[1]]));
                out.push(u16::from_be_bytes([v4[2], v4[3]]));
            } else if !f.is_empty() && f.len() <= 4 && f.bytes().all(|b| b.is_ascii_hexdigit()) {
                out.push(u16::from_str_radix(f, 16).ok()?);
            } else {
                return None;
            }
        }
        Some(())
    }
    let (mut head, mut tail) = (Vec::new(), Vec::new());
    let compressed = match s.split_once("::") {
        Some((h, t)) => {
            if t.contains("::") || h.contains('.') {
                return None;
            }
            groups(h, &mut head)?;
            groups(t, &mut tail)?;
            true
        }
        None => {
            groups(s, &mut head)?;
            false
        }
    };
    let n = head.len() + tail.len();
    if (compressed && n > 7) || (!compressed && n != 8) {
        return None;
    }
    let mut out = [0u16; 8];
    out[..head.len()].copy_from_slice(&head);
    out[8 - tail.len()..].copy_from_slice(&tail);
    Some(out)
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

/// Search URL for free text, on the engine the settings chose.
pub fn search_url(query: &str) -> String {
    search_url_on(crate::settings::engine().search_url(), query)
}

/// `query` appended, encoded, to a search address (up to its `q=`).
fn search_url_on(base: &str, query: &str) -> String {
    let mut out = String::with_capacity(base.len() + query.len() * 3);
    out.push_str(base);
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

/// Longest URL (in bytes) a tab is opened with or remembered for reopening.
pub const MAX_URL_LEN: usize = 8 * 1024;

/// A web URL within `MAX_URL_LEN`: what a page may open as a new tab.
/// Longer ones are refused, never cut (a cut URL is a different address).
pub fn is_tab_url(url: &str) -> bool {
    url.len() <= MAX_URL_LEN && is_web_url(url)
}

/// A page's URL as kept for display. One over `MAX_URL_LEN` is cut, but to
/// just over it (at a character boundary), so it still fails `is_tab_url`
/// and a cut URL is never reopened as if it were the page's.
pub fn cap_url(url: &str) -> String {
    if url.len() <= MAX_URL_LEN {
        return url.to_string();
    }
    let end = url.char_indices().map(|(i, c)| i + c.len_utf8()).find(|&end| end > MAX_URL_LEN).unwrap_or(url.len());
    url[..end].to_string()
}

/// Lowercased host of an absolute `scheme://` URL, without userinfo, port or
/// trailing dots (IPv6 literals keep their brackets). `None` if there is no
/// `//` authority.
pub fn host_of(url: &str) -> Option<String> {
    let (_, rest) = split_scheme(url)?;
    let authority = rest.strip_prefix("//")?.split(['/', '?', '#', '\\']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.find(']') {
        Some(end) if host_port.starts_with('[') => &host_port[..=end],
        _ => host_port.split(':').next()?,
    };
    Some(host.trim_end_matches('.').to_ascii_lowercase())
}

/// Whether going from `from` to `to` stays in the same document: `to` has a
/// `#fragment` and nothing before it differs (a jump within the page, which
/// loads nothing). The very same address is a reload (F5 on `doc#x`, a form
/// posting to it), which does load.
pub fn same_document(from: &str, to: &str) -> bool {
    to != from && to.split_once('#').is_some_and(|(base, _)| from.split('#').next() == Some(base))
}

/// What the address bar shows for a URL (blank pages show nothing).
pub fn display_url(url: &str) -> std::borrow::Cow<'_, str> {
    if url == BLANK { "".into() } else { crate::policy::display(url) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_text_goes_to_the_chosen_engine_encoded() {
        for e in crate::settings::Engine::ALL {
            let url = search_url_on(e.search_url(), "a b&c/ş");
            assert_eq!(url, format!("{}a+b%26c%2F%C5%9F", e.search_url()));
        }
    }

    #[test]
    fn tab_urls_are_web_and_capped() {
        let at_cap = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN - 20));
        assert_eq!(at_cap.len(), MAX_URL_LEN);
        assert!(is_tab_url(&at_cap));
        assert!(!is_tab_url(&format!("{at_cap}a")), "one byte over is refused");
        assert!(!is_tab_url("javascript:alert(1)"));
        assert!(!is_tab_url("about:blank"));
        // Multi-byte characters count as bytes.
        let wide = format!("https://example.com/{}", "ş".repeat(MAX_URL_LEN / 2));
        assert!(wide.chars().count() < MAX_URL_LEN && !is_tab_url(&wide));
    }

    #[test]
    fn capped_urls_stay_over_the_cap() {
        let short = "https://example.com/";
        assert_eq!(cap_url(short), short);
        let at_cap = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN - 20));
        assert_eq!(cap_url(&at_cap), at_cap, "at the cap: unchanged");
        let long = format!("{at_cap}{}", "b".repeat(100_000));
        let cut = cap_url(&long);
        assert_eq!(cut.len(), MAX_URL_LEN + 1);
        assert!(!is_tab_url(&cut), "a cut URL is never a tab URL");
        let wide = format!("https://example.com/{}", "ş".repeat(MAX_URL_LEN));
        let cut = cap_url(&wide);
        assert!(cut.len() > MAX_URL_LEN && cut.len() <= MAX_URL_LEN + 4 && wide.starts_with(&cut));
        assert!(!is_tab_url(&cut));
    }

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
    fn invalid_ipv6_literals_are_searched() {
        assert_eq!(resolve_input("[1]"), url("https://duckduckgo.com/?q=%5B1%5D"));
        assert_eq!(resolve_input("[abc]"), url("https://duckduckgo.com/?q=%5Babc%5D"));
        assert_eq!(resolve_input("[::1]:abc"), url("https://duckduckgo.com/?q=%5B%3A%3A1%5D%3Aabc"));
        assert_eq!(resolve_input("[::1]x"), url("https://duckduckgo.com/?q=%5B%3A%3A1%5Dx"));
        assert_eq!(resolve_input("[::1]:"), url("https://duckduckgo.com/?q=%5B%3A%3A1%5D%3A"));
        assert_eq!(resolve_input("[::1]:65536"), url("https://duckduckgo.com/?q=%5B%3A%3A1%5D%3A65536"));
        assert_eq!(resolve_input("[1::2::3]"), url("https://duckduckgo.com/?q=%5B1%3A%3A2%3A%3A3%5D"));
        assert_eq!(resolve_input("[::1"), url("https://duckduckgo.com/?q=%5B%3A%3A1"));
        assert_eq!(resolve_input("[]"), url("https://duckduckgo.com/?q=%5B%5D"));
    }

    #[test]
    fn valid_ipv6_literals_pass() {
        assert_eq!(resolve_input("[::1]"), url("http://[::1]"));
        assert_eq!(resolve_input("[0:0:0:0:0:0:0:1]:8080/x"), url("http://[0:0:0:0:0:0:0:1]:8080/x"));
        assert_eq!(resolve_input("[2001:db8::1]:443"), url("https://[2001:db8::1]:443"));
        assert_eq!(resolve_input("[::ffff:192.0.2.1]"), url("https://[::ffff:192.0.2.1]"));
        assert_eq!(resolve_input("[1:2:3:4:5:6:7:8]"), url("https://[1:2:3:4:5:6:7:8]"));
    }

    #[test]
    fn ipv6_parser() {
        assert_eq!(parse_ipv6("::1"), Some(LOOPBACK_V6));
        assert_eq!(parse_ipv6("::"), Some([0; 8]));
        assert_eq!(parse_ipv6("1::"), Some([1, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(parse_ipv6("::ffff:1.2.3.4"), Some([0, 0, 0, 0, 0, 0xffff, 0x0102, 0x0304]));
        assert_eq!(parse_ipv6("1:2:3:4:5:6:7::"), Some([1, 2, 3, 4, 5, 6, 7, 0]));
        for bad in [
            "", "1", ":", ":::", "1:2:3:4:5:6:7", "1:2:3:4:5:6:7:8:9", "1:2:3:4::5:6:7:8", "12345::", "g::",
            ":1::", "1::2:", "1.2.3.4::", "::1.2.3", "::1.2.3.4:5", "fe80::1%25eth0", " ::1",
        ] {
            assert_eq!(parse_ipv6(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn ports_are_validated() {
        assert_eq!(resolve_input("localhost:65535"), url("http://localhost:65535"));
        assert_eq!(resolve_input("localhost:65536"), url("https://duckduckgo.com/?q=localhost%3A65536"));
        assert_eq!(resolve_input("localhost:80abc"), url("https://duckduckgo.com/?q=localhost%3A80abc"));
        assert_eq!(resolve_input("localhost:000080"), url("https://duckduckgo.com/?q=localhost%3A000080"));
        assert_eq!(resolve_input("http://example.com:80abc/"), url("https://duckduckgo.com/?q=http%3A%2F%2Fexample.com%3A80abc%2F"));
        assert_eq!(resolve_input("http://example.com:99999"), url("https://duckduckgo.com/?q=http%3A%2F%2Fexample.com%3A99999"));
        assert_eq!(resolve_input("http://[::1]:abc"), url("https://duckduckgo.com/?q=http%3A%2F%2F%5B%3A%3A1%5D%3Aabc"));
        assert_eq!(resolve_input("http://[1]/"), url("https://duckduckgo.com/?q=http%3A%2F%2F%5B1%5D%2F"));
        assert_eq!(resolve_input("http://[::1]:8080/a"), url("http://[::1]:8080/a"));
    }

    #[test]
    fn empty_port_after_scheme_and_host_is_default_port() {
        assert_eq!(resolve_input("http://example.com:"), url("http://example.com:"));
        assert_eq!(resolve_input("https://example.com:/a?b#c"), url("https://example.com:/a?b#c"));
        assert_eq!(resolve_input("https:example.com:"), url("https://example.com:"));
        assert_eq!(resolve_input("http://[::1]:/"), url("http://[::1]:/"));
        assert_eq!(resolve_input("http://user@example.com:"), url("http://user@example.com:"));
        // Still needs a host, and only one colon may be empty.
        assert_eq!(resolve_input("http://:"), url("https://duckduckgo.com/?q=http%3A%2F%2F%3A"));
        assert_eq!(resolve_input("http://user@:/"), url("https://duckduckgo.com/?q=http%3A%2F%2Fuser%40%3A%2F"));
        assert_eq!(resolve_input("http://example.com::"), url("https://duckduckgo.com/?q=http%3A%2F%2Fexample.com%3A%3A"));
        // Without a scheme, "example.com:" reads as an unknown scheme, not a
        // host with a port (a port needs digits): a search, as before.
        assert_eq!(resolve_input("example.com:"), url("https://duckduckgo.com/?q=example.com%3A"));
        assert_eq!(resolve_input("localhost:"), url("https://duckduckgo.com/?q=localhost%3A"));
    }

    #[test]
    fn empty_hosts_are_rejected() {
        assert_eq!(resolve_input("http://"), url("https://duckduckgo.com/?q=http%3A%2F%2F"));
        assert_eq!(resolve_input("https://"), url("https://duckduckgo.com/?q=https%3A%2F%2F"));
        assert_eq!(resolve_input("http:"), url("https://duckduckgo.com/?q=http%3A"));
        assert_eq!(resolve_input("http:///path"), url("https://duckduckgo.com/?q=http%3A%2F%2F%2Fpath"));
        assert_eq!(resolve_input("http://:80/"), url("https://duckduckgo.com/?q=http%3A%2F%2F%3A80%2F"));
        assert_eq!(resolve_input("https://user@"), url("https://duckduckgo.com/?q=https%3A%2F%2Fuser%40"));
        assert_eq!(resolve_input(":80"), url("https://duckduckgo.com/?q=%3A80"));
    }

    #[test]
    fn userinfo_behavior_is_pinned() {
        // Typed without a scheme: looks like an email / note, so it is searched.
        assert_eq!(resolve_input("user:pass@example.com"), url("https://duckduckgo.com/?q=user%3Apass%40example.com"));
        assert_eq!(resolve_input("user@example.com:8080"), url("https://duckduckgo.com/?q=user%40example.com%3A8080"));
        // With an explicit scheme it is a URL; the host after '@' is validated.
        assert_eq!(resolve_input("https://user:pass@example.com/"), url("https://user:pass@example.com/"));
        assert_eq!(resolve_input("http://user:pass@:80/"), url("https://duckduckgo.com/?q=http%3A%2F%2Fuser%3Apass%40%3A80%2F"));
    }

    #[test]
    fn only_about_blank_is_allowed() {
        assert_eq!(resolve_input("about:blank"), url(BLANK));
        assert_eq!(resolve_input("ABOUT:Blank"), url(BLANK));
        assert_eq!(resolve_input("about:flags"), url("https://duckduckgo.com/?q=about%3Aflags"));
        assert_eq!(resolve_input("about:settings"), url("https://duckduckgo.com/?q=about%3Asettings"));
        assert_eq!(resolve_input("about:"), url("https://duckduckgo.com/?q=about%3A"));
        assert_eq!(resolve_input("about:blank#x"), url("https://duckduckgo.com/?q=about%3Ablank%23x"));
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
    fn host_extraction() {
        let h = |s: &str| host_of(s);
        assert_eq!(h("http://xb.ui/index.html").as_deref(), Some("xb.ui"));
        assert_eq!(h("HTTPS://XB.UI:443/x").as_deref(), Some("xb.ui"));
        assert_eq!(h("http://user:pw@xb.ui./").as_deref(), Some("xb.ui"));
        assert_eq!(h("http://xb.ui?x").as_deref(), Some("xb.ui"));
        assert_eq!(h("ws://xb.ui#x").as_deref(), Some("xb.ui"));
        assert_eq!(h("http://[::1]:80/").as_deref(), Some("[::1]"));
        assert_eq!(h("https://example.com/?u=http://xb.ui/").as_deref(), Some("example.com"));
        assert_eq!(h("http://xb.ui.evil.com/").as_deref(), Some("xb.ui.evil.com"));
        assert_eq!(h("about:blank"), None);
        assert_eq!(h("data:text/html,http://xb.ui/"), None);
        assert_eq!(h(""), None);
    }

    #[test]
    fn display_hides_blank() {
        assert_eq!(display_url("about:blank"), "");
        assert_eq!(display_url("https://a.com/"), "https://a.com/");
    }

    #[test]
    fn only_a_fragment_change_stays_in_the_document() {
        assert!(same_document("https://a.com/doc", "https://a.com/doc#two"));
        assert!(same_document("https://a.com/doc#one", "https://a.com/doc#two"));
        assert!(same_document("https://a.com/doc#one", "https://a.com/doc#"));
        assert!(!same_document("https://a.com/doc#one", "https://a.com/doc"), "dropping the fragment loads the page");
        assert!(!same_document("https://a.com/doc", "https://a.com/doc"), "a reload loads");
        assert!(!same_document("https://a.com/app#/inbox", "https://a.com/app#/inbox"), "a reload with a fragment loads");
        assert!(!same_document("https://a.com/doc", "https://a.com/other#two"));
        assert!(!same_document("https://a.com/doc?q=1", "https://a.com/doc?q=2#x"));
    }
}
