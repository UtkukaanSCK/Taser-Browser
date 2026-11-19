//! Blocking HTTPS GET over WinHTTP, for extension downloads and update checks (v0.3, design A.5).
//!
//! Call it from a worker thread, never from the UI thread. HTTPS only, on every
//! redirect hop. Redirects are followed by hand so each `Location` is checked.
//! There is no cookie store, no automatic authentication and no default
//! credentials, and the User-Agent names the client and nothing else.

// The self-test build fetches from its fake store (`webstore::http_get`),
// so none of the network code runs there.
#![cfg_attr(feature = "selftest", allow(dead_code))]

use std::ffi::c_void;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Sent on every request. Names the client and nothing about the user.
pub const USER_AGENT: &str = "X-Browser";
/// Redirect hops followed after the first request.
pub const MAX_REDIRECTS: usize = 5;
/// Longest URL accepted, from the caller or from a `Location` header.
pub(crate) const MAX_URL: usize = 8 * 1024;

#[derive(Debug, Clone)]
pub struct Limits {
    /// Body size cap, checked against `Content-Length` and while reading.
    pub max_body: usize,
    /// Name resolution and TCP/TLS connect, per hop.
    pub connect_timeout: Duration,
    /// Each send and each read.
    pub receive_timeout: Duration,
    /// The whole call, all hops included.
    pub deadline: Duration,
    /// Set from another thread to stop the call. Checked before each hop,
    /// before connecting and before each read, so a stopped call ends within
    /// one read; a read already waiting ends at `receive_timeout` at worst.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Limits {
    fn check_cancel(&self) -> Result<(), NetError> {
        if self.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) { Err(NetError::Cancelled) } else { Ok(()) }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body: 64 << 20,
            connect_timeout: Duration::from_secs(15),
            receive_timeout: Duration::from_secs(30),
            deadline: Duration::from_secs(300),
            cancel: None,
        }
    }
}

#[derive(Debug)]
pub struct Response {
    /// The URL that answered, after redirects, without a fragment. Only
    /// the network tests read it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub final_url: String,
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NetError {
    BadUrl(&'static str),
    NotHttps,
    TooManyRedirects,
    BadRedirect,
    TooLarge,
    /// The body ended before its `Content-Length`.
    Truncated,
    Timeout,
    /// [`Limits::cancel`] was set.
    Cancelled,
    /// A WinHTTP or Win32 error code and the step that failed.
    Os { stage: &'static str, code: u32 },
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadUrl(why) => write!(f, "bad URL: {why}"),
            Self::NotHttps => write!(f, "only HTTPS is allowed"),
            Self::TooManyRedirects => write!(f, "too many redirects"),
            Self::BadRedirect => write!(f, "bad redirect"),
            Self::TooLarge => write!(f, "response is too large"),
            Self::Truncated => write!(f, "response was cut short"),
            Self::Timeout => write!(f, "timed out"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Os { stage, code } => write!(f, "{stage} failed ({code})"),
        }
    }
}

impl std::error::Error for NetError {}

// --- URLs (pure) ---------------------------------------------------------------

/// A checked `https://` URL, split for WinHTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Lower case, ASCII letters, digits, `-` and `.` only.
    pub host: String,
    pub port: u16,
    /// Starts with `/`; holds the query, never a fragment.
    pub path: String,
}

impl Target {
    pub fn url(&self) -> String {
        if self.port == 443 {
            format!("https://{}{}", self.host, self.path)
        } else {
            format!("https://{}:{}{}", self.host, self.port, self.path)
        }
    }
}

/// Printable ASCII without space and backslash: what a URL may hold here.
/// Anything else (controls, whitespace, non-ASCII) must arrive percent-encoded.
fn check_chars(s: &str) -> Result<(), NetError> {
    if s.len() > MAX_URL {
        return Err(NetError::BadUrl("too long"));
    }
    if !s.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'\\') {
        return Err(NetError::BadUrl("bad character"));
    }
    Ok(())
}

fn strip_fragment(s: &str) -> &str {
    s.split_once('#').map_or(s, |(a, _)| a)
}

/// `scheme:` per RFC 3986, if `s` starts with one.
fn scheme_of(s: &str) -> Option<&str> {
    let (scheme, _) = s.split_once(':')?;
    let mut b = scheme.bytes();
    let first_ok = b.next().is_some_and(|c| c.is_ascii_alphabetic());
    (first_ok && b.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))).then_some(scheme)
}

/// Parses an absolute `https://` URL. Refuses other schemes, userinfo, IP
/// addresses (bracketed, dotted, decimal or hex), single-label hosts, odd host
/// names, bad ports, and odd characters. Every host this module talks to is a
/// public DNS name.
pub fn parse_https(url: &str) -> Result<Target, NetError> {
    check_chars(url)?;
    let url = strip_fragment(url);
    let Some(scheme) = scheme_of(url) else {
        return Err(NetError::BadUrl("not absolute"));
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(NetError::NotHttps);
    }
    let Some(rest) = url[scheme.len() + 1..].strip_prefix("//") else {
        return Err(NetError::BadUrl("no host"));
    };
    let split = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(split);
    if authority.contains('@') {
        return Err(NetError::BadUrl("user info"));
    }
    if authority.starts_with('[') {
        return Err(NetError::BadUrl("bad host"));
    }
    let (host, port) = match authority.split_once(':') {
        None => (authority, 443),
        Some((h, p)) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|c| c.is_ascii_digit()) {
                return Err(NetError::BadUrl("bad port"));
            }
            match p.parse::<u16>() {
                Ok(n) if n > 0 => (h, n),
                _ => return Err(NetError::BadUrl("bad port")),
            }
        }
    };
    let host = host.to_ascii_lowercase();
    let labels_ok = host
        .split('.')
        .all(|l| !l.is_empty() && l.len() <= 63 && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'));
    if host.is_empty() || host.len() > 253 || !labels_ok {
        return Err(NetError::BadUrl("bad host"));
    }
    // A top-level label is never numeric, so this refuses `127.0.0.1`,
    // `2130706433` and `0x7f.1`; one label alone (`localhost`, an intranet
    // name) is refused too.
    let tld = host.rsplit('.').next().unwrap_or("");
    if !host.contains('.') || tld.bytes().all(|c| c.is_ascii_digit()) || tld.starts_with("0x") {
        return Err(NetError::BadUrl("bad host"));
    }
    let path = if tail.starts_with('/') { tail.to_string() } else { format!("/{tail}") };
    Ok(Target { host, port, path })
}

/// RFC 3986 5.2.4 on a path that starts with `/`.
fn remove_dot_segments(path: &str) -> String {
    let segs: Vec<&str> = path[1..].split('/').collect();
    let last = segs.len() - 1;
    let mut out: Vec<&str> = Vec::with_capacity(segs.len());
    for (i, s) in segs.iter().enumerate() {
        match *s {
            "." => {
                if i == last {
                    out.push("");
                }
            }
            ".." => {
                out.pop();
                if i == last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

/// `path?query` with dot segments removed from the path part.
fn normalize(path_and_query: &str) -> String {
    match path_and_query.split_once('?') {
        Some((p, q)) => format!("{}?{q}", remove_dot_segments(p)),
        None => remove_dot_segments(path_and_query),
    }
}

/// Resolves a `Location` header against the URL that sent it (RFC 3986 5.2).
/// The result must again be `https://`; anything else is refused.
pub fn resolve(base: &Target, location: &str) -> Result<Target, NetError> {
    check_chars(location)?;
    let loc = strip_fragment(location);
    if scheme_of(loc).is_some() {
        return parse_https(loc);
    }
    if loc.starts_with("//") {
        return parse_https(&format!("https:{loc}"));
    }
    let base_path = base.path.split_once('?').map_or(&*base.path, |(p, _)| p);
    let path = if loc.is_empty() {
        base.path.clone()
    } else if loc.starts_with('/') {
        normalize(loc)
    } else if loc.starts_with('?') {
        format!("{base_path}{loc}")
    } else {
        let dir = &base_path[..base_path.rfind('/').map_or(0, |i| i + 1)];
        normalize(&format!("{dir}{loc}"))
    };
    let t = Target { path, ..base.clone() };
    check_chars(&t.url())?;
    Ok(t)
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

// --- WinHTTP -----------------------------------------------------------------------

/// Fetches `url`, following up to [`MAX_REDIRECTS`] HTTPS redirects with GET.
/// Any status other than a redirect is returned as is, with its body.
pub fn get(url: &str, limits: &Limits) -> Result<Response, NetError> {
    let start = Instant::now();
    let mut target = parse_https(url)?;
    limits.check_cancel()?;
    let session = Session::open()?;
    for _ in 0..=MAX_REDIRECTS {
        limits.check_cancel()?;
        match session.fetch(&target, limits, start)? {
            Fetched::Redirect(location) => {
                target = resolve(&target, &location).map_err(|e| match e {
                    NetError::NotHttps => NetError::NotHttps,
                    _ => NetError::BadRedirect,
                })?;
            }
            Fetched::Done(status, body) => return Ok(Response { final_url: target.url(), status, body }),
        }
    }
    Err(NetError::TooManyRedirects)
}

enum Fetched {
    Redirect(String),
    Done(u16, Vec<u8>),
}

use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR};

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const READ_CHUNK: usize = 64 * 1024;

fn win32_code(e: &windows::core::Error) -> u32 {
    let hr = e.code().0 as u32;
    if hr & 0xffff_0000 == 0x8007_0000 { hr & 0xffff } else { hr }
}

fn os_err(stage: &'static str) -> impl Fn(windows::core::Error) -> NetError {
    move |e| match win32_code(&e) {
        ERROR_WINHTTP_TIMEOUT => NetError::Timeout,
        code => NetError::Os { stage, code },
    }
}

/// Closes a WinHTTP handle on drop.
struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void, stage: &'static str) -> Result<Self, NetError> {
        if h.is_null() { Err(os_err(stage)(windows::core::Error::from_thread())) } else { Ok(Self(h)) }
    }

    fn set_u32(&self, option: u32, value: u32, stage: &'static str) -> Result<(), NetError> {
        // SAFETY: a live handle and a 4-byte buffer, as these options expect.
        unsafe { WinHttpSetOption(Some(self.0 as *const c_void), option, Some(&value.to_ne_bytes())) }.map_err(os_err(stage))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from WinHTTP and is closed once.
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

struct Session(Handle);

/// Milliseconds left of `limit`, capped by what is left of the deadline.
/// Never 0, which WinHTTP reads as "no timeout".
fn ms(limit: Duration, left: Duration) -> i32 {
    limit.min(left).as_millis().clamp(1, i32::MAX as u128) as i32
}

impl Session {
    fn open() -> Result<Self, NetError> {
        // SAFETY: plain FFI with valid wide strings.
        let h = unsafe {
            WinHttpOpen(&HSTRING::from(USER_AGENT), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0)
        };
        let s = Self(Handle::new(h, "WinHttpOpen")?);
        // TLS 1.2 or later. Older Windows 10 builds refuse the 1.3 flag.
        if s.0.set_u32(WINHTTP_OPTION_SECURE_PROTOCOLS, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3, "tls").is_err() {
            s.0.set_u32(WINHTTP_OPTION_SECURE_PROTOCOLS, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2, "tls")?;
        }
        Ok(s)
    }

    fn fetch(&self, t: &Target, limits: &Limits, start: Instant) -> Result<Fetched, NetError> {
        let left = || limits.deadline.checked_sub(start.elapsed()).filter(|d| !d.is_zero()).ok_or(NetError::Timeout);
        let remaining = left()?;
        let (connect_ms, io_ms) = (ms(limits.connect_timeout, remaining), ms(limits.receive_timeout, remaining));
        // SAFETY: plain FFI on live handles; strings outlive the calls.
        unsafe {
            WinHttpSetTimeouts(self.0.0, connect_ms, connect_ms, io_ms, io_ms).map_err(os_err("timeouts"))?;
            limits.check_cancel()?;
            let connect = Handle::new(WinHttpConnect(self.0.0, &HSTRING::from(&*t.host), t.port, 0), "WinHttpConnect")?;
            let req = Handle::new(
                WinHttpOpenRequest(
                    connect.0,
                    &HSTRING::from("GET"),
                    &HSTRING::from(&*t.path),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    std::ptr::null(),
                    WINHTTP_FLAG_SECURE | WINHTTP_FLAG_REFRESH,
                ),
                "WinHttpOpenRequest",
            )?;
            req.set_u32(WINHTTP_OPTION_DISABLE_FEATURE, WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_REDIRECTS | WINHTTP_DISABLE_AUTHENTICATION, "options")?;
            req.set_u32(WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_NEVER, "options")?;
            req.set_u32(WINHTTP_OPTION_AUTOLOGON_POLICY, WINHTTP_AUTOLOGON_SECURITY_LEVEL_HIGH, "options")?;
            WinHttpSendRequest(req.0, None, None, 0, 0, 0).map_err(os_err("send"))?;
            WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(os_err("receive"))?;

            let mut status = 0u32;
            let mut len = 4u32;
            WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut c_void),
                &mut len,
                std::ptr::null_mut(),
            )
            .map_err(os_err("status"))?;
            let status = u16::try_from(status).map_err(|_| NetError::Os { stage: "status", code: status })?;
            if is_redirect(status) {
                return match header(&req, WINHTTP_QUERY_LOCATION, MAX_URL)? {
                    Some(loc) => Ok(Fetched::Redirect(loc)),
                    None => Err(NetError::BadRedirect),
                };
            }

            let declared = header(&req, WINHTTP_QUERY_CONTENT_LENGTH, 32)?.and_then(|v| v.trim().parse::<u64>().ok());
            if declared.is_some_and(|n| n > limits.max_body as u64) {
                return Err(NetError::TooLarge);
            }
            let mut body = Vec::with_capacity(declared.map_or(0, |n| n as usize));
            let mut buf = vec![0u8; READ_CHUNK];
            loop {
                limits.check_cancel()?;
                req.set_u32(WINHTTP_OPTION_RECEIVE_TIMEOUT, ms(limits.receive_timeout, left()?) as u32, "timeouts")?;
                // Wait for some data, then read only what has arrived, so one
                // read never waits to fill the whole buffer and the deadline is
                // checked again after every arrival.
                let mut avail = 0u32;
                WinHttpQueryDataAvailable(req.0, &mut avail).map_err(os_err("read"))?;
                if avail == 0 {
                    break;
                }
                let want = (avail as usize).min(buf.len()) as u32;
                let mut n = 0u32;
                WinHttpReadData(req.0, buf.as_mut_ptr() as *mut c_void, want, &mut n).map_err(os_err("read"))?;
                if n == 0 {
                    break;
                }
                if body.len() + n as usize > limits.max_body {
                    return Err(NetError::TooLarge);
                }
                body.extend_from_slice(&buf[..n as usize]);
            }
            // A dropped connection can look like a clean end of data.
            if declared.is_some_and(|n| n != body.len() as u64) {
                return Err(NetError::Truncated);
            }
            Ok(Fetched::Done(status, body))
        }
    }
}

/// A response header as text, or `None` if absent. `max` caps it in UTF-16 units.
fn header(req: &Handle, level: u32, max: usize) -> Result<Option<String>, NetError> {
    let mut bytes = 0u32;
    // SAFETY: the size query with no buffer, then a buffer of the reported size.
    unsafe {
        match WinHttpQueryHeaders(req.0, level, PCWSTR::null(), None, &mut bytes, std::ptr::null_mut()) {
            Ok(()) => return Ok(Some(String::new())),
            Err(e) => match win32_code(&e) {
                ERROR_WINHTTP_HEADER_NOT_FOUND => return Ok(None),
                ERROR_INSUFFICIENT_BUFFER => {}
                code => return Err(NetError::Os { stage: "headers", code }),
            },
        }
        let units = bytes as usize / 2;
        if units > max + 1 {
            return Err(if level == WINHTTP_QUERY_LOCATION { NetError::BadRedirect } else { NetError::TooLarge });
        }
        let mut buf = vec![0u16; units.max(1)];
        let mut bytes = (buf.len() * 2) as u32;
        WinHttpQueryHeaders(req.0, level, PCWSTR::null(), Some(buf.as_mut_ptr() as *mut c_void), &mut bytes, std::ptr::null_mut())
            .map_err(os_err("headers"))?;
        buf.truncate(bytes as usize / 2);
        String::from_utf16(&buf).map(Some).map_err(|_| NetError::BadRedirect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(url: &str) -> Target {
        parse_https(url).unwrap()
    }

    #[test]
    fn parses_https_urls() {
        assert_eq!(t("https://example.com"), Target { host: "example.com".into(), port: 443, path: "/".into() });
        assert_eq!(t("HTTPS://Example.COM/a/B?q=1#frag").url(), "https://example.com/a/B?q=1");
        assert_eq!(t("https://example.com?q").path, "/?q");
        assert_eq!(t("https://example.com:8443/x").url(), "https://example.com:8443/x");
        assert_eq!(t("https://example.com:443/x").url(), "https://example.com/x");
        assert_eq!(t("https://a-b.c0.example/%20x").path, "/%20x");
        let cws = "https://clients2.google.com/service/update2/crx?response=redirect&prodversion=1.2&acceptformat=crx3&x=id%3Dabc%26uc";
        assert_eq!(t(cws).url(), cws);
    }

    #[test]
    fn refuses_bad_urls() {
        use NetError::*;
        for (url, want) in [
            ("http://example.com/", NotHttps),
            ("ftp://example.com/", NotHttps),
            ("file:///C:/x", NotHttps),
            ("javascript:alert(1)", NotHttps),
            ("data:text/plain,x", NotHttps),
            ("https:example.com", BadUrl("no host")),
            ("https:/example.com", BadUrl("no host")),
            ("//example.com/", BadUrl("not absolute")),
            ("example.com", BadUrl("not absolute")),
            ("", BadUrl("not absolute")),
            ("https://", BadUrl("bad host")),
            ("https:///x", BadUrl("bad host")),
            ("https://user@example.com/", BadUrl("user info")),
            ("https://user:pw@example.com/", BadUrl("user info")),
            ("https://example.com@evil.com/", BadUrl("user info")),
            ("https://example.com:/", BadUrl("bad port")),
            ("https://example.com:0/", BadUrl("bad port")),
            ("https://example.com:65536/", BadUrl("bad port")),
            ("https://example.com:+443/", BadUrl("bad port")),
            ("https://example.com:443:1/", BadUrl("bad port")),
            ("https://[::1]/", BadUrl("bad host")),
            ("https://exa_mple.com/", BadUrl("bad host")),
            ("https://example..com/", BadUrl("bad host")),
            ("https://example.com./", BadUrl("bad host")),
            ("https://.example.com/", BadUrl("bad host")),
            ("https://example.com%2f.evil/", BadUrl("bad host")),
            ("https://example.com/a b", BadUrl("bad character")),
            ("https://example.com/a\tb", BadUrl("bad character")),
            ("https://example.com/a\r\nX: y", BadUrl("bad character")),
            ("https://example.com/\\evil", BadUrl("bad character")),
            ("https://example.com\\@evil.com/", BadUrl("bad character")),
            ("https://exämple.com/", BadUrl("bad character")),
            ("https://example.com/\u{0}", BadUrl("bad character")),
            // IP addresses in every spelling WinHTTP might resolve, and single labels.
            ("https://127.0.0.1/", BadUrl("bad host")),
            ("https://2130706433/", BadUrl("bad host")),
            ("https://0x7f.1/", BadUrl("bad host")),
            ("https://0x7f000001/", BadUrl("bad host")),
            ("https://10.0.0.0x1/", BadUrl("bad host")),
            ("https://localhost/", BadUrl("bad host")),
            ("https://intranet:8443/", BadUrl("bad host")),
        ] {
            assert_eq!(parse_https(url), Err(want), "{url:?}");
        }
        assert_eq!(parse_https(&format!("https://example.com/{}", "a".repeat(MAX_URL))), Err(BadUrl("too long")));
        // A numeric label is fine anywhere but last.
        assert_eq!(t("https://1.2.example/").host, "1.2.example");
        assert_eq!(t("https://xn--mnchen-3ya.example/").host, "xn--mnchen-3ya.example");
    }

    #[test]
    fn removes_dot_segments() {
        for (path, want) in [
            ("/", "/"),
            ("/a/b/c/./../../g", "/a/g"),
            ("/a/b/..", "/a/"),
            ("/a/b/.", "/a/b/"),
            ("/../../x", "/x"),
            ("/..", "/"),
            ("/a//b/../c", "/a//c"),
            ("/a/.../b", "/a/.../b"),
            ("/a/%2e%2e/b", "/a/%2e%2e/b"),
        ] {
            assert_eq!(remove_dot_segments(path), want, "{path}");
        }
    }

    #[test]
    fn resolves_redirects() {
        // RFC 3986 5.4.1, on an https base.
        let base = t("https://a.example/b/c/d;p?q");
        for (loc, want) in [
            ("g", "https://a.example/b/c/g"),
            ("./g", "https://a.example/b/c/g"),
            ("g/", "https://a.example/b/c/g/"),
            ("/g", "https://a.example/g"),
            ("//g.example", "https://g.example/"),
            ("?y", "https://a.example/b/c/d;p?y"),
            ("g?y", "https://a.example/b/c/g?y"),
            ("#s", "https://a.example/b/c/d;p?q"),
            ("g#s", "https://a.example/b/c/g"),
            (";x", "https://a.example/b/c/;x"),
            ("", "https://a.example/b/c/d;p?q"),
            (".", "https://a.example/b/c/"),
            ("..", "https://a.example/b/"),
            ("../g", "https://a.example/b/g"),
            ("../..", "https://a.example/"),
            ("../../../g", "https://a.example/g"),
            ("/./g", "https://a.example/g"),
            ("g?y/./x", "https://a.example/b/c/g?y/./x"),
            ("HTTPS://Other.example:444/z", "https://other.example:444/z"),
        ] {
            assert_eq!(resolve(&base, loc).map(|t| t.url()), Ok(want.to_string()), "{loc:?}");
        }
        // A port carries over to relative hops.
        assert_eq!(resolve(&t("https://a.example:8443/x/y"), "z").unwrap().url(), "https://a.example:8443/x/z");
    }

    #[test]
    fn refuses_bad_redirects() {
        let base = t("https://a.example/b");
        for (loc, want) in [
            ("http://a.example/", NetError::NotHttps),
            ("HTTP://a.example/", NetError::NotHttps),
            ("javascript:alert(1)", NetError::NotHttps),
            ("file:///etc/passwd", NetError::NotHttps),
            ("chrome-extension://abc/", NetError::NotHttps),
            ("//user@evil.example/", NetError::BadUrl("user info")),
            ("//evil.example:0/", NetError::BadUrl("bad port")),
            ("/a b", NetError::BadUrl("bad character")),
            ("\\\\evil.example\\x", NetError::BadUrl("bad character")),
            ("/x\r\nSet-Cookie: a=b", NetError::BadUrl("bad character")),
            ("https://ä.example/", NetError::BadUrl("bad character")),
        ] {
            assert_eq!(resolve(&base, loc), Err(want), "{loc:?}");
        }
        assert_eq!(resolve(&base, &"a".repeat(MAX_URL + 1)), Err(NetError::BadUrl("too long")));
        // Not a scheme: a path segment with a colon after a slash.
        assert_eq!(resolve(&base, "./x:y").unwrap().url(), "https://a.example/x:y");
    }

    #[test]
    fn redirect_statuses() {
        for s in [301, 302, 303, 307, 308] {
            assert!(is_redirect(s));
        }
        for s in [200, 204, 300, 304, 305, 306, 309, 400, 404, 500] {
            assert!(!is_redirect(s));
        }
    }

    #[test]
    fn timeouts_never_become_infinite() {
        let s = Duration::from_secs;
        assert_eq!(ms(s(30), s(300)), 30_000);
        assert_eq!(ms(s(30), Duration::from_millis(1500)), 1500);
        assert_eq!(ms(s(30), Duration::ZERO), 1);
        assert_eq!(ms(Duration::MAX, Duration::MAX), i32::MAX);
    }

    #[test]
    fn get_refuses_before_any_connection() {
        assert_eq!(get("http://example.com/", &Limits::default()).unwrap_err(), NetError::NotHttps);
        assert_eq!(get("https://u@example.com/", &Limits::default()).unwrap_err(), NetError::BadUrl("user info"));
        // A spent deadline stops before connecting (no network needed).
        let spent = Limits { deadline: Duration::ZERO, ..Limits::default() };
        assert_eq!(get("https://example.invalid/", &spent).unwrap_err(), NetError::Timeout);
        // So does a call already cancelled; an unset flag changes nothing.
        let flag = Arc::new(AtomicBool::new(true));
        let stopped = Limits { cancel: Some(flag.clone()), ..Limits::default() };
        assert_eq!(get("https://example.invalid/", &stopped).unwrap_err(), NetError::Cancelled);
        assert_eq!(NetError::Cancelled.to_string(), "cancelled");
        flag.store(false, Ordering::Relaxed);
        assert_eq!(stopped.check_cancel(), Ok(()));
        let spent_too = Limits { deadline: Duration::ZERO, ..stopped };
        assert_eq!(get("https://example.invalid/", &spent_too).unwrap_err(), NetError::Timeout);
    }

    // --- the real store ---------------------------------------------------------

    const UBOL: &str = "ddkjiahejlhfcafbddmgiahcphecmpfh";
    const PRODVERSION: &str = "157.0.4341.46";

    /// `cargo test net::tests::real_crx_download -- --ignored --nocapture` (network)
    #[test]
    #[ignore = "downloads uBlock Origin Lite from the Chrome Web Store"]
    fn real_crx_download() {
        use crate::crx::{Policy, verify_crx3};
        let url = crate::cws::crx_url(UBOL, PRODVERSION).unwrap();
        let t0 = Instant::now();
        let r = get(&url, &Limits::default()).unwrap();
        println!("{} {} bytes in {:?} from {}", r.status, r.body.len(), t0.elapsed(), r.final_url);
        assert_eq!(r.status, 200);
        assert!(r.final_url.starts_with("https://clients2.googleusercontent.com/"), "{}", r.final_url);
        let p = verify_crx3(&r.body, UBOL, Policy::WebStore).unwrap();
        println!("verified {}: {} proof keys", p.id(), p.key_hashes().len());
        // The cap holds on the same download.
        let small = Limits { max_body: 1 << 20, ..Limits::default() };
        assert_eq!(get(&url, &small).unwrap_err(), NetError::TooLarge);
    }

    /// `cargo test net::tests::real_update_check -- --ignored --nocapture` (network)
    #[test]
    #[ignore = "asks the Chrome Web Store update service"]
    fn real_update_check() {
        use crate::cws::{parse_update_response, update_check_url};
        let url = update_check_url(&[(UBOL, "0.0.0.1"), ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "1.0")], PRODVERSION).unwrap();
        let r = get(&url, &Limits::default()).unwrap();
        assert_eq!(r.status, 200);
        let ups = parse_update_response(&r.body);
        println!("{ups:#?}");
        let u = ups.iter().find(|u| u.id == UBOL).unwrap();
        assert_eq!(u.status, "ok");
        assert!(u.codebase.as_deref().unwrap().starts_with("https://"));
        assert_eq!(u.hash_sha256.map(|h| h.len()), Some(32));
    }
}
