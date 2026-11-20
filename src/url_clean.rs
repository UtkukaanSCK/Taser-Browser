//! Address rewrites made before a page loads (see `privacy`): dropping
//! known tracking parameters from the query, and trying `https://` for an
//! `http://` address. Plain functions of the text, nothing else.

use crate::url::{host_of, is_web_url, parse_ipv4, parse_ipv6};

/// Query parameters that only say where a click came from. Compared with the
/// name lowercased; `utm_*` is matched as a prefix (`TRACKING_PREFIXES`).
/// Chosen after the lists browsers keep (Brave's URL sanitizer, Firefox's
/// query stripping): parameters no page needs in order to show itself.
const TRACKING_PARAMS: &[&str] = &[
    // Google Ads and Analytics.
    "gclid",
    "gclsrc",
    "dclid",
    "gbraid",
    "wbraid",
    "srsltid",
    // Facebook, Instagram.
    "fbclid",
    "igshid",
    // Microsoft Ads.
    "msclkid",
    // Mailchimp.
    "mc_eid",
    // Yandex.
    "yclid",
    "_openstat",
    // HubSpot.
    "_hsenc",
    "_hsmsclk",
    "__hssc",
    "__hstc",
    "__hsfp",
    "hsctatracking",
    // X (Twitter), TikTok, LinkedIn, Pinterest.
    "twclid",
    "ttclid",
    "li_fat_id",
    "epik",
    // Marketo, Adobe, Olytics, Vero, Wicked Reports, Rakuten, Klaviyo.
    "mkt_tok",
    "s_cid",
    "oly_anon_id",
    "oly_enc_id",
    "vero_id",
    "wickedid",
    "rb_clickid",
    "_kx",
];

/// Name prefixes that are tracking whatever follows (`utm_source`, ...).
const TRACKING_PREFIXES: &[&str] = &["utm_"];

/// Whether a query parameter's name (as written, before `=`) is tracking.
fn is_tracking(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    TRACKING_PARAMS.contains(&name.as_str()) || TRACKING_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// `url` without its tracking parameters, or None if it has none (or is not
/// an http(s) address). The other parameters keep their order and spelling,
/// the `#fragment` stays, and a query left empty goes with its `?`.
pub fn strip_tracking(url: &str) -> Option<String> {
    if !is_web_url(url) {
        return None;
    }
    let (rest, fragment) = match url.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (url, None),
    };
    let (base, query) = rest.split_once('?')?;
    let kept: Vec<&str> = query.split('&').filter(|p| !is_tracking(p.split('=').next().unwrap_or(""))).collect();
    if kept.len() == query.split('&').count() {
        return None;
    }
    let mut out = String::with_capacity(url.len());
    out.push_str(base);
    let kept = kept.join("&");
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept);
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    Some(out)
}

/// The `https://` form of an `http://` address worth upgrading, or None:
/// not for local or private hosts (they rarely have a certificate a browser
/// trusts) nor for an explicit port (`:8080` with TLS is a different server).
pub fn https_upgrade(url: &str) -> Option<String> {
    let rest = url.get(..7).filter(|s| s.eq_ignore_ascii_case("http://")).map(|_| &url[7..])?;
    let host = host_of(url)?;
    if is_private_host(&host) {
        return None;
    }
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let port = match host_port.rfind(']') {
        Some(end) => &host_port[end + 1..],
        None => host_port.split_once(':').map_or("", |(_, p)| p),
    };
    // (`http://a:` is the default port; WebView2 writes `:80` away.)
    port.trim_start_matches(':').is_empty().then(|| format!("https://{rest}"))
}

/// A host on this machine or a private network (as `url::host_of` gives it:
/// lowercased, IPv6 in brackets): never upgraded to https. Single-label
/// names (`router`, `nas`) and the suffixes reserved for local use count.
pub fn is_private_host(host: &str) -> bool {
    const LOCAL_SUFFIXES: &[&str] =
        &[".localhost", ".local", ".internal", ".lan", ".intranet", ".home.arpa", ".test", ".invalid", ".example"];
    if let Some(inner) = host.strip_prefix('[') {
        let Some(v6) = inner.strip_suffix(']').and_then(parse_ipv6) else { return true };
        return match v6 {
            [0, 0, 0, 0, 0, 0, 0, 0 | 1] => true,
            // IPv4-mapped (::ffff:a.b.c.d).
            [0, 0, 0, 0, 0, 0xffff, hi, lo] => {
                let [a, b] = hi.to_be_bytes();
                let [c, d] = lo.to_be_bytes();
                private_v4([a, b, c, d])
            }
            [first, ..] => first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80,
        };
    }
    if let Some(v4) = parse_ipv4(host) {
        return private_v4(v4);
    }
    host.is_empty() || !host.contains('.') || host == "localhost" || LOCAL_SUFFIXES.iter().any(|s| host.ends_with(s))
}

fn private_v4([a, b, ..]: [u8; 4]) -> bool {
    matches!((a, b), (0 | 10 | 127, _) | (169, 254) | (192, 168)) || (a == 172 && b & 0xf0 == 16) || (a == 100 && b & 0xc0 == 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_parameters_are_dropped_and_the_rest_kept_in_order() {
        let s = |u: &str| strip_tracking(u);
        assert_eq!(s("https://a.example/p?utm_source=x&id=7&fbclid=y&b=2").as_deref(), Some("https://a.example/p?id=7&b=2"));
        assert_eq!(s("https://a.example/p?utm_medium=e&utm_campaign=c").as_deref(), Some("https://a.example/p"));
        assert_eq!(s("http://a.example/?gclid=1#top").as_deref(), Some("http://a.example/#top"));
        assert_eq!(s("https://a.example/?q=1&GCLID=1&UTM_Term=z").as_deref(), Some("https://a.example/?q=1"), "names compare case aside");
        assert_eq!(s("https://a.example/?msclkid&x=").as_deref(), Some("https://a.example/?x="), "a name with no value");
        for p in ["dclid", "mc_eid", "igshid", "yclid", "_hsenc", "_hsmsclk", "twclid", "ttclid", "srsltid"] {
            assert_eq!(s(&format!("https://a.example/?{p}=v")).as_deref(), Some("https://a.example/"), "{p}");
        }
    }

    #[test]
    fn addresses_without_tracking_are_left_alone() {
        for u in [
            "https://a.example/",
            "https://a.example/p?id=7&b=2",
            "https://a.example/?utm=1&gclid_x=2&xfbclid=3",
            "https://a.example/#utm_source=x",
            "https://a.example/p#a?utm_source=x",
            "https://a.example/?",
            "file:///C:/x?utm_source=1",
            "about:blank?fbclid=1",
            "data:text/html,?fbclid=1",
        ] {
            assert_eq!(strip_tracking(u), None, "{u}");
        }
    }

    #[test]
    fn the_fragment_and_odd_separators_survive() {
        assert_eq!(strip_tracking("https://a.example/?a=1&&utm_x=2#f?g=1").as_deref(), Some("https://a.example/?a=1&#f?g=1"));
        assert_eq!(strip_tracking("https://a.example/?a=b=c&fbclid=1").as_deref(), Some("https://a.example/?a=b=c"));
        assert_eq!(strip_tracking("https://a.example/?a=%C3%A7&fbclid=1").as_deref(), Some("https://a.example/?a=%C3%A7"));
    }

    #[test]
    fn http_is_upgraded_for_public_hosts_only() {
        let u = |s: &str| https_upgrade(s);
        assert_eq!(u("http://example.com/").as_deref(), Some("https://example.com/"));
        assert_eq!(u("http://www.example.com/a?b=1#c").as_deref(), Some("https://www.example.com/a?b=1#c"));
        assert_eq!(u("HTTP://Example.com/").as_deref(), Some("https://Example.com/"));
        assert_eq!(u("http://user@example.com/").as_deref(), Some("https://user@example.com/"));
        assert_eq!(u("http://8.8.8.8/").as_deref(), Some("https://8.8.8.8/"), "a public address");
        assert_eq!(u("http://[2001:db8::1]/").as_deref(), Some("https://[2001:db8::1]/"));
        for s in [
            "https://example.com/",
            "http://example.com:8080/",
            "http://example.com:80/",
            "http://[2001:db8::1]:8080/",
            "http://localhost/",
            "http://localhost:3000/",
            "http://app.localhost/",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://172.16.5.4/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
            "http://169.254.1.1/",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            "http://[::ffff:192.168.0.1]/",
            "http://router/",
            "http://nas.local/",
            "http://printer.lan/",
            "http://x.home.arpa/",
            "http://site.test/",
            "ftp://example.com/",
            "file:///C:/",
            "http:example.com",
        ] {
            assert_eq!(u(s), None, "{s}");
        }
    }

    #[test]
    fn private_ranges_end_where_they_should() {
        for h in ["172.15.0.1", "172.32.0.1", "100.63.0.1", "100.128.0.1", "11.0.0.1", "192.169.0.1", "[2001:db8::1]", "[::ffff:8.8.8.8]", "example.com"] {
            assert!(!is_private_host(h), "{h}");
        }
        for h in ["", "[]", "[zz]", "[::]", "localhost", "intranet", "a.internal"] {
            assert!(is_private_host(h), "{h}");
        }
    }
}
