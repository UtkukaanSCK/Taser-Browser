//! Chrome Web Store glue (v0.3, design B.3–B.4 and B.6): detail-page ids, the
//! download and update URLs, the store-button hook and its sentinel URL, the
//! update-check response, and the Turkish permission text for the install dialog.
//!
//! Everything here is pure: no I/O, no WebView2.


use crate::crx::is_extension_id;
use crate::net::MAX_URL;

const DETAIL_PREFIX: &str = "https://chromewebstore.google.com/detail/";
/// Everything under this path is X-Browser's and never reaches the store.
const SENTINEL_ROOT: &str = "https://chromewebstore.google.com/_xb";
const SENTINEL_PREFIX: &str = "https://chromewebstore.google.com/_xb/";
const UPDATE_SERVICE: &str = "https://clients2.google.com/service/update2/crx";

// --- detail pages ---------------------------------------------------------------

/// The extension id of a store detail page:
/// `^https://chromewebstore\.google\.com/detail/(?:[^/?#]+/)?([a-p]{32})(?:[/?#]|$)`.
/// This is design B.4's regex with the slug tightened from `[^/]+` to `[^/?#]+`.
///
/// The slug may not hold `?` or `#`, so the id always comes from the path, never
/// from a query or fragment; the hook script reads `location.pathname` by the
/// same rule. As in the regex, a slug is tried first, then the bare id.
pub fn detail_id(url: &str) -> Option<String> {
    let rest = url.strip_prefix(DETAIL_PREFIX)?;
    let path = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    let mut segs = path.split('/');
    let first = segs.next()?;
    if let Some(second) = segs.next()
        && !first.is_empty()
        && is_extension_id(second)
    {
        return Some(second.to_string());
    }
    is_extension_id(first).then(|| first.to_string())
}

/// A Chrome-style version: 1–4 dot-separated integers, each 0–65535, no
/// leading zeros. Used for extension versions and for `prodversion`.
pub fn is_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (1..=4).contains(&parts.len())
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 5
                && p.bytes().all(|c| c.is_ascii_digit())
                && !(p.len() > 1 && p.starts_with('0'))
                && p.parse::<u32>().is_ok_and(|n| n <= 65535)
        })
}

/// Whether `candidate` is a later version than `installed`, comparing the
/// numbers part by part with missing parts as 0 (so `1.0` equals `1.0.0`).
/// False if either is not a valid version.
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    matches!((version_parts(candidate), version_parts(installed)), (Some(c), Some(i)) if c > i)
}

/// Whether `a` and `b` are the same version, compared as numbers (`1.0`
/// equals `1.0.0`). False if either is not a valid version.
pub fn same_version(a: &str, b: &str) -> bool {
    matches!((version_parts(a), version_parts(b)), (Some(x), Some(y)) if x == y)
}

/// A valid version's four numbers, missing parts as 0.
fn version_parts(v: &str) -> Option<[u32; 4]> {
    if !is_version(v) {
        return None;
    }
    let mut out = [0u32; 4];
    for (slot, p) in out.iter_mut().zip(v.split('.')) {
        *slot = p.parse().ok()?;
    }
    Some(out)
}

/// The update service URL that redirects to the current CRX of `id`.
/// `None` if `id` or `prodversion` is malformed.
pub fn crx_url(id: &str, prodversion: &str) -> Option<String> {
    (is_extension_id(id) && is_version(prodversion)).then(|| {
        format!("{UPDATE_SERVICE}?response=redirect&prodversion={prodversion}&acceptformat=crx3&x=id%3D{id}%26uc")
    })
}

// --- the store button -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Remove,
}

#[cfg(any(test, feature = "selftest"))]
impl Action {
    fn word(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Remove => "remove",
        }
    }
}

/// The sentinel address the store page's hook navigates to. X-Browser
/// itself only reads them (`parse_sentinel`); the tests and the self-test
/// write them.
#[cfg(any(test, feature = "selftest"))]
pub fn sentinel_url(action: Action, id: &str) -> String {
    format!("{SENTINEL_PREFIX}{}/{id}", action.word())
}

/// Reads `https://chromewebstore.google.com/_xb/{install|remove}/<id>`, exactly:
/// no query, fragment, trailing slash or other variation.
pub fn parse_sentinel(url: &str) -> Option<(Action, String)> {
    let rest = url.strip_prefix(SENTINEL_PREFIX)?;
    let (word, id) = rest.split_once('/')?;
    let action = match word {
        "install" => Action::Install,
        "remove" => Action::Remove,
        _ => return None,
    };
    is_extension_id(id).then(|| (action, id.to_string()))
}

/// What a content tab's `NavigationStarting` handler does with a main-frame
/// navigation (design B.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SentinelDecision {
    /// Not under `/_xb`: the ordinary navigation policy applies.
    Pass,
    /// Under `/_xb` but not a request to honour: cancel it and do nothing else.
    Cancel,
    /// Cancel it, then ask the user in X-Browser's own dialog. Both actions ask:
    /// the store page can `pushState` to any detail page, so the URL alone
    /// proves nothing about what the user saw.
    Ask(Action, String),
}

/// Whether `url` is under the sentinel root: a navigation the policy handler
/// always cancels, so it never loads and starts no spinner.
pub fn is_sentinel(url: &str) -> bool {
    url.get(..SENTINEL_ROOT.len()).is_some_and(|p| p.eq_ignore_ascii_case(SENTINEL_ROOT))
}

/// Decides a navigation to `url` from a tab whose committed `Source` is
/// `current_source`. Pass `is_redirected` from the event args: a redirect hop
/// is never honoured, because `Source` still shows the old page while any
/// server answers with a `Location` pointing at the sentinel.
pub fn sentinel_decision(current_source: &str, url: &str, is_redirected: bool) -> SentinelDecision {
    if !is_sentinel(url) {
        return SentinelDecision::Pass;
    }
    if is_redirected {
        return SentinelDecision::Cancel;
    }
    match (detail_id(current_source), parse_sentinel(url)) {
        (Some(page), Some((action, id))) if page == id => SentinelDecision::Ask(action, id),
        _ => SentinelDecision::Cancel,
    }
}

/// Document-created script for all content tabs (design B.4).
///
/// On the store origin only, it wraps `chrome.webstorePrivate.beginInstallWithManifest3`
/// and `chrome.management.uninstall`. In the top frame a call navigates the tab
/// to the sentinel for the id in `location.pathname` (never an id the page passes
/// in). In a subframe, such as an `about:blank` frame the store page made, it only
/// refuses, so the native Edge dialog never opens from there either. Either way
/// it answers `'user_cancelled'`, by callback or promise. It captures every
/// built-in it uses before the page's scripts run, and pins its wrappers in place.
pub const HOOK_SCRIPT: &str = r#"(() => {
  'use strict';
  try {
    if (location.origin !== 'https://chromewebstore.google.com') return;
    const isTop = window === window.top;
    const loc = location, apply = Reflect.apply, exec = RegExp.prototype.exec,
      assign = loc.assign, later = setTimeout, P = Promise, resolve = Promise.resolve,
      define = Object.defineProperty;
    const re = /^\/detail\/(?:[^/]+\/)?([a-p]{32})(?:\/|$)/;
    const go = (action) => {
      const m = apply(exec, re, [loc.pathname]);
      if (m !== null) apply(assign, loc, ['https://chromewebstore.google.com/_xb/' + action + '/' + m[1]]);
    };
    const blocked = (action) => function () {
      if (isTop) go(action);
      const cb = arguments.length > 0 ? arguments[arguments.length - 1] : undefined;
      if (typeof cb === 'function') {
        later(() => apply(cb, undefined, ['user_cancelled']), 0);
        return undefined;
      }
      return apply(resolve, P, ['user_cancelled']);
    };
    const pin = (obj, name, fn) => {
      if (!obj || typeof obj[name] !== 'function') return;
      try { define(obj, name, { value: fn, writable: false, configurable: false }); }
      catch (e) { try { obj[name] = fn; } catch (e2) {} }
    };
    const c = globalThis.chrome;
    if (!c) return;
    pin(c.webstorePrivate, 'beginInstallWithManifest3', blocked('install'));
    pin(c.management, 'uninstall', blocked('remove'));
  } catch (e) {}
})();"#;

// --- update checks ----------------------------------------------------------------

/// One batched update-check URL for `(id, installed version)` pairs. `None` if
/// the list is empty, any id or version is malformed, or the URL would pass
/// `net::get`'s length limit (split the list then).
pub fn update_check_url(exts: &[(&str, &str)], prodversion: &str) -> Option<String> {
    if exts.is_empty() || !is_version(prodversion) {
        return None;
    }
    let mut url = format!("{UPDATE_SERVICE}?response=updatecheck&prodversion={prodversion}&acceptformat=crx3");
    for (id, v) in exts {
        if !is_extension_id(id) || !is_version(v) {
            return None;
        }
        url.push_str(&format!("&x=id%3D{id}%26v%3D{v}%26uc"));
    }
    (url.len() <= MAX_URL).then_some(url)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub id: String,
    /// `ok` (an update is offered), `noupdate`, or the service's error word.
    pub status: String,
    /// Set, and checked, when `status` is `ok`.
    pub version: Option<String>,
    /// An `https://` URL, set when `status` is `ok`.
    pub codebase: Option<String>,
    /// The CRX's SHA-256, set when `status` is `ok`. The caller checks the
    /// download against it, and checks that `version` is newer than the
    /// installed one and equals the CRX manifest's.
    pub hash_sha256: Option<[u8; 32]>,
    /// The CRX's size in bytes as the service states it, when `status` is
    /// `ok` and the attribute is a plain positive number; only a download
    /// cap, never trusted beyond that.
    pub size: Option<usize>,
}

/// Biggest update-check response read; anything longer yields nothing. Pass it
/// as `net::Limits::max_body` for the update request, so an oversized answer
/// fails as `TooLarge` instead of reading as "no updates".
pub const MAX_UPDATE_RESPONSE: usize = 256 * 1024;
const MAX_APPS: usize = 1000;

/// Reads a gupdate (protocol 2.0) response: one [`Update`] per `<app>`. A tiny,
/// tolerant reader: it looks only at `<app>` and its first `<updatecheck>`, and
/// drops any entry that is malformed (bad id, status, version, URL, or a
/// missing or bad hash) or repeats an id.
pub fn parse_update_response(xml: &[u8]) -> Vec<Update> {
    let mut out: Vec<Update> = Vec::new();
    if xml.len() > MAX_UPDATE_RESPONSE {
        return out;
    }
    let Ok(text) = std::str::from_utf8(xml) else { return out };
    let mut seen = std::collections::HashSet::new();
    let mut app: Option<OpenApp<'_>> = None;
    for tag in tags(text) {
        match tag.name {
            "app" => {
                finish(app.take(), &mut out, &mut seen);
                let Some(attrs) = tag.attrs else { continue };
                let open = OpenApp { attrs, check: None };
                if tag.self_closing {
                    finish(Some(open), &mut out, &mut seen);
                } else {
                    app = Some(open);
                }
            }
            "/app" => finish(app.take(), &mut out, &mut seen),
            "updatecheck" => {
                if let Some(a) = app.as_mut()
                    && a.check.is_none()
                    && let Some(attrs) = tag.attrs
                {
                    a.check = Some(attrs);
                }
            }
            _ => {}
        }
    }
    finish(app.take(), &mut out, &mut seen);
    out
}

type Attrs<'a> = Vec<(&'a str, String)>;

/// An `<app>` being read, with its first `<updatecheck>`.
struct OpenApp<'a> {
    attrs: Attrs<'a>,
    check: Option<Attrs<'a>>,
}

/// Keeps a well-formed entry whose id is new, up to [`MAX_APPS`].
fn finish(app: Option<OpenApp<'_>>, out: &mut Vec<Update>, seen: &mut std::collections::HashSet<String>) {
    if let Some(a) = app
        && out.len() < MAX_APPS
        && let Some(u) = update_from(&a.attrs, a.check.as_deref())
        && seen.insert(u.id.clone())
    {
        out.push(u);
    }
}

fn attr<'a>(attrs: &'a [(&str, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
}

fn is_status_word(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    // from_str_radix alone would take a leading '+'.
    if s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn update_from(app: &[(&str, String)], uc: Option<&[(&str, String)]>) -> Option<Update> {
    let id = attr(app, "appid")?;
    if !is_extension_id(id) {
        return None;
    }
    let app_status = attr(app, "status")?;
    if !is_status_word(app_status) {
        return None;
    }
    let none = |status: &str| {
        Some(Update { id: id.into(), status: status.into(), version: None, codebase: None, hash_sha256: None, size: None })
    };
    if app_status != "ok" {
        return none(app_status);
    }
    let uc = uc?;
    let status = attr(uc, "status")?;
    if !is_status_word(status) {
        return None;
    }
    if status != "ok" {
        return none(status);
    }
    let version = attr(uc, "version").filter(|v| is_version(v))?;
    let codebase = attr(uc, "codebase").filter(|c| crate::net::parse_https(c).is_ok())?;
    let hash = hex32(attr(uc, "hash_sha256")?)?;
    Some(Update {
        id: id.into(),
        status: status.into(),
        version: Some(version.into()),
        codebase: Some(codebase.into()),
        hash_sha256: Some(hash),
        size: attr(uc, "size").and_then(byte_count),
    })
}

/// A plain decimal byte count above zero; no sign, no spaces, at most 12 digits.
fn byte_count(s: &str) -> Option<usize> {
    if s.is_empty() || s.len() > 12 || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse::<usize>().ok().filter(|&n| n > 0)
}

struct Tag<'a> {
    /// The element name without a namespace prefix; `/name` for an end tag.
    name: &'a str,
    /// `None` if the attributes are malformed.
    attrs: Option<Vec<(&'a str, String)>>,
    self_closing: bool,
}

/// The tags of `text` in order. Skips `<?…?>`, `<!…>`, comments and CDATA
/// sections; stops at the first unterminated tag.
fn tags(text: &str) -> impl Iterator<Item = Tag<'_>> {
    let mut rest = text;
    std::iter::from_fn(move || {
        loop {
            let start = rest.find('<')?;
            rest = &rest[start..];
            if let Some(after) = rest.strip_prefix("<!--") {
                rest = &after[after.find("-->")? + 3..];
                continue;
            }
            if let Some(after) = rest.strip_prefix("<![CDATA[") {
                rest = &after[after.find("]]>")? + 3..];
                continue;
            }
            // The closing '>' outside quotes.
            let mut quote = None;
            let end = rest.char_indices().skip(1).find(|&(_, c)| match quote {
                Some(q) if c == q => {
                    quote = None;
                    false
                }
                Some(_) => false,
                None if c == '"' || c == '\'' => {
                    quote = Some(c);
                    false
                }
                None => c == '>',
            })?;
            let inner = &rest[1..end.0];
            rest = &rest[end.0 + 1..];
            if inner.starts_with('?') || inner.starts_with('!') {
                continue;
            }
            let (inner, self_closing) = match inner.strip_suffix('/') {
                Some(i) => (i, true),
                None => (inner, false),
            };
            let name_end = inner.find(|c: char| c.is_ascii_whitespace()).unwrap_or(inner.len());
            let (raw_name, attrs) = inner.split_at(name_end);
            let (slash, bare) = match raw_name.strip_prefix('/') {
                Some(n) => ("/", n),
                None => ("", raw_name),
            };
            let local = bare.rsplit(':').next().unwrap_or(bare);
            let name = match (slash, local) {
                ("/", "app") => "/app",
                ("", n) => n,
                _ => "",
            };
            return Some(Tag { name, attrs: parse_attrs(attrs), self_closing });
        }
    })
}

/// `name="value"` pairs (either quote), entities decoded. `None` if malformed.
fn parse_attrs(mut s: &str) -> Option<Vec<(&str, String)>> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start();
        if s.is_empty() {
            return Some(out);
        }
        let eq = s.find('=')?;
        let name = s[..eq].trim_end();
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b':' | b'.')) {
            return None;
        }
        let v = s[eq + 1..].trim_start();
        let q = v.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let close = v[1..].find(q)? + 1;
        out.push((name, unescape(&v[1..close])?));
        s = &v[close + 1..];
        if !(s.is_empty() || s.starts_with(|c: char| c.is_ascii_whitespace())) {
            return None;
        }
    }
}

/// The five predefined XML entities; anything else after `&` is malformed.
fn unescape(s: &str) -> Option<String> {
    if s.contains('<') {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let semi = after.find(';')?;
        out.push(match &after[..semi] {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => return None,
        });
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    Some(out)
}

// --- permissions ------------------------------------------------------------------

/// What a manifest asks for, as raw strings with a kind prefix:
/// - `p:` `permissions`, `h:` `host_permissions` and content-script
///   `matches` (what the install question lists);
/// - `x:` who may message it (`externally_connectable` `matches` and `ids`);
/// - `f:<flag>:<match>` a content script that also runs in frames
///   (`all_frames`), in `about:`/`blob:`/`data:` frames
///   (`match_about_blank`, `match_origin_as_fallback`);
/// - `o:` a browser page it replaces (`chrome_url_overrides`);
/// - `m:` its `manifest_version`.
///
/// A signed update that only widens one of the later kinds still asks
/// (`widens_permissions`). `None` if the manifest is not a JSON object.
fn requested(manifest_json: &[u8]) -> Option<std::collections::BTreeSet<String>> {
    let bytes = manifest_json.strip_prefix(b"\xef\xbb\xbf").unwrap_or(manifest_json);
    let Ok(serde_json::Value::Object(m)) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return None;
    };
    let mut out = std::collections::BTreeSet::new();
    let mut add = |prefix: &str, v: Option<&serde_json::Value>| {
        for x in v.and_then(|v| v.as_array()).into_iter().flatten() {
            if let Some(s) = x.as_str() {
                out.insert(format!("{prefix}{s}"));
            }
        }
    };
    add("p:", m.get("permissions"));
    add("h:", m.get("host_permissions"));
    if let Some(ec) = m.get("externally_connectable") {
        add("x:", ec.get("matches"));
        add("x:id:", ec.get("ids"));
    }
    for cs in m.get("content_scripts").and_then(|v| v.as_array()).into_iter().flatten() {
        add("h:", cs.get("matches"));
        for flag in ["all_frames", "match_about_blank", "match_origin_as_fallback"] {
            if cs.get(flag).and_then(|v| v.as_bool()) == Some(true) {
                add(&format!("f:{flag}:"), cs.get("matches"));
            }
        }
    }
    if let Some(o) = m.get("chrome_url_overrides").and_then(|v| v.as_object()) {
        out.extend(o.keys().map(|k| format!("o:{k}")));
    }
    if let Some(v) = m.get("manifest_version") {
        out.insert(format!("m:{v}"));
    }
    Some(out)
}

/// Whether an update from `old` to `new` asks for anything `old` did not, so
/// the user must be asked again. True when either manifest is unreadable.
pub fn widens_permissions(old_manifest: &[u8], new_manifest: &[u8]) -> bool {
    match (requested(old_manifest), requested(new_manifest)) {
        (Some(old), Some(new)) => !new.is_subset(&old),
        _ => true,
    }
}

/// The question's lines for what an update from `old` to `new` asks for and
/// `old` did not (`describe_permissions` of those alone). Every line of
/// `new` when `old` is unreadable. Can be empty though `widens_permissions`
/// holds (a pattern Chrome ignores): the question then says so without a
/// list.
pub fn added_permissions(old_manifest: &[u8], new_manifest: &[u8]) -> Vec<String> {
    let (Some(old), Some(new)) = (requested(old_manifest), requested(new_manifest)) else {
        return describe_permissions(new_manifest);
    };
    let (mut perms, mut hosts, mut other) = (Vec::new(), Vec::new(), Vec::new());
    for item in new.difference(&old) {
        if let Some(p) = item.strip_prefix("p:") {
            perms.push(p);
        } else if let Some(h) = item.strip_prefix("h:") {
            hosts.push(h);
        } else {
            // Fixed text, one line per kind: nothing from the manifest.
            let line = match &item[..2] {
                "x:" => "Web siteleri ya da başka uzantılar onunla doğrudan konuşabilir",
                "f:" => "Sitelere eklediği betikler sayfaların çerçevelerinde de çalışır",
                "o:" => "Bir tarayıcı sayfasının (ör. yeni sekme) yerine geçer",
                _ => "Yeni bir manifest sürümüne geçiyor",
            };
            if !other.contains(&line) {
                other.push(line);
            }
        }
    }
    let only_new = serde_json::json!({ "permissions": perms, "host_permissions": hosts });
    let mut lines = describe_permissions(only_new.to_string().as_bytes());
    lines.extend(other.into_iter().map(str::to_string));
    lines
}

/// Turkish lines for the install confirmation (design B.3.5): host access
/// first, then one line per known permission, then the unknown ones, at most
/// [`MAX_LINES`] in all.
///
/// Nothing from the manifest reaches the dialog as free text, so a developer
/// cannot write a reassuring sentence into it:
/// - An unknown permission shows as "Bilinmeyen izin: <name>", and only when the
///   name is an identifier (`[A-Za-z0-9._-]`, at most 64 characters).
/// - A host shows only when it is a valid host name, with an optional leading
///   `*.`.
///
/// The dialog must still insert every line as text, never as HTML.
///
/// Reads `permissions`, `host_permissions` and `content_scripts[].matches`. The
/// result is empty if the manifest asks for nothing or is not a JSON object.
pub fn describe_permissions(manifest_json: &[u8]) -> Vec<String> {
    let bytes = manifest_json.strip_prefix(b"\xef\xbb\xbf").unwrap_or(manifest_json);
    let Ok(serde_json::Value::Object(m)) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Vec::new();
    };
    let strings = |v: Option<&serde_json::Value>| -> Vec<String> {
        v.and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let mut hosts: Vec<String> = strings(m.get("host_permissions"));
    let mut perms: Vec<String> = Vec::new();
    for p in strings(m.get("permissions")) {
        if p == "<all_urls>" || p.contains("://") {
            hosts.push(p);
        } else {
            perms.push(p);
        }
    }
    if let Some(cs) = m.get("content_scripts").and_then(|v| v.as_array()) {
        for s in cs {
            hosts.extend(strings(s.get("matches")));
        }
    }

    // Known permissions come before unknown ones, so a manifest cannot push a
    // risky permission past the line cap with a list of made-up names.
    let mut seen = std::collections::HashSet::new();
    let mut known: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for p in &perms {
        let (line, list) = match permission_text(p) {
            Some(t) => (t.to_string(), &mut known),
            None if is_identifier(p) => (format!("Bilinmeyen izin: {p}"), &mut unknown),
            None => (UNKNOWN_PERMISSION.to_string(), &mut unknown),
        };
        if seen.insert(line.clone()) {
            list.push(line);
        }
    }
    let mut out: Vec<String> = describe_hosts(&hosts).into_iter().collect();
    out.append(&mut known);
    out.append(&mut unknown);
    if out.len() > MAX_LINES {
        let rest = out.len() - (MAX_LINES - 1);
        out.truncate(MAX_LINES - 1);
        out.push(format!("ve {rest} izin daha"));
    }
    out
}

const UNKNOWN_PERMISSION: &str = "Bilinmeyen bir izin";
/// Lines in the dialog, the "ve N izin daha" line included.
const MAX_LINES: usize = 30;

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// The host of a match pattern as shown in the dialog: a lower-case DNS name,
/// optionally behind `*.`, without its port. `None` if it is anything else.
fn shown_host(host: &str) -> Option<String> {
    let host = match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && (port == "*" || port.bytes().all(|c| c.is_ascii_digit())) => h,
        Some(_) => return None,
        None => host,
    };
    let name = host.strip_prefix("*.").unwrap_or(host);
    let ok = !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|l| !l.is_empty() && l.len() <= 63 && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'));
    if !ok {
        return None;
    }
    let host = host.to_ascii_lowercase();
    // Keep the end of a long name: it holds the registrable domain.
    Some(match host.len() {
        n if n > 64 => format!("…{}", &host[n - 63..]),
        _ => host,
    })
}

const ALL_SITES: &str = "Tüm sitelerdeki verilerinizi okuyup değiştirebilir";
const MAX_HOSTS_SHOWN: usize = 8;
const OTHER_SITES: &str = "Adı gösterilemeyen adreslerdeki verilerinizi okuyup değiştirebilir";

fn describe_hosts(patterns: &[String]) -> Option<String> {
    let mut hosts: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // A pattern whose host cannot be shown still grants access, so it is
    // reported without its name.
    let mut hidden = false;
    for p in patterns {
        if p == "<all_urls>" {
            return Some(ALL_SITES.into());
        }
        // Not a match pattern: Chrome ignores it, so it grants nothing.
        let Some((scheme, rest)) = p.split_once("://") else { continue };
        let raw = rest.split('/').next().unwrap_or("");
        // `*` or `*:443` is every site.
        if raw.split(':').next() == Some("*") {
            return Some(ALL_SITES.into());
        }
        let host = if scheme == "file" { Some("bilgisayarınızdaki dosyalar".to_string()) } else { shown_host(raw) };
        match host {
            Some(host) => {
                if seen.insert(host.clone()) {
                    hosts.push(host);
                }
            }
            None => hidden = true,
        }
    }
    if hosts.is_empty() {
        return hidden.then(|| OTHER_SITES.to_string());
    }
    let shown = hosts.len().min(MAX_HOSTS_SHOWN);
    let mut list = hosts[..shown].join(", ");
    match (hosts.len() - shown, hidden) {
        (0, false) => {}
        (0, true) => list.push_str(" ve başka adresler"),
        (n, false) => list.push_str(&format!(" ve {n} site daha")),
        (n, true) => list.push_str(&format!(" ve {n} site ile başka adresler")),
    }
    Some(format!("Şu sitelerdeki verilerinizi okuyup değiştirebilir: {list}"))
}

fn permission_text(p: &str) -> Option<&'static str> {
    Some(match p {
        "activeTab" => "Uzantı simgesine tıkladığınızda etkin sekmeye erişebilir",
        "alarms" => "Zamanlanmış işler çalıştırabilir",
        "bookmarks" => "Yer imlerinizi okuyup değiştirebilir",
        "browsingData" => "Tarama verilerinizi silebilir",
        "clipboardRead" => "Panodaki verileri okuyabilir",
        "clipboardWrite" => "Panoya veri yazabilir",
        "contentSettings" => "Sitelerin çerez, JavaScript ve eklenti izinlerini değiştirebilir",
        "contextMenus" => "Sağ tık menüsüne öğe ekleyebilir",
        "cookies" => "Çerezlerinizi okuyup değiştirebilir",
        "debugger" => "Sayfaları hata ayıklayıcıyla denetleyip içlerindeki her şeye erişebilir",
        "declarativeContent" => "Sayfa içeriğine göre uzantı simgesini gösterebilir",
        "declarativeNetRequest" => "Sayfalardaki içeriği engelleyebilir",
        "declarativeNetRequestWithHostAccess" => "İzin verilen sitelerde ağ isteklerini engelleyebilir veya yönlendirebilir",
        "declarativeNetRequestFeedback" => "Göz atma geçmişinizi okuyabilir",
        "desktopCapture" => "Ekranınızın görüntüsünü alabilir",
        "downloads" => "İndirmelerinizi yönetebilir",
        "downloads.open" => "İndirilen dosyaları açabilir",
        "downloads.ui" => "İndirme çubuğunu değiştirebilir",
        "favicon" => "Sitelerin simgelerini okuyabilir",
        "fontSettings" => "Yazı tipi ayarlarınızı değiştirebilir",
        "gcm" => "Google üzerinden anlık mesaj alabilir",
        "geolocation" => "Konumunuzu öğrenebilir",
        "history" => "Göz atma geçmişinizi okuyup değiştirebilir",
        "identity" | "identity.email" => "Hesap kimliğinizi ve e-posta adresinizi isteyebilir",
        "idle" => "Bilgisayarı kullanıp kullanmadığınızı görebilir",
        "management" => "Diğer uzantılarınızı yönetebilir",
        "nativeMessaging" => "Bilgisayarınızdaki programlarla iletişim kurabilir",
        "notifications" => "Bildirim gösterebilir",
        "offscreen" => "Arka planda görünmez sayfa açabilir",
        "pageCapture" => "Sayfaları kaydedebilir",
        "power" => "Bilgisayarın uykuya geçmesini engelleyebilir",
        "printing" => "Yazıcılarınıza belge gönderebilir",
        "privacy" => "Gizlilik ayarlarınızı değiştirebilir",
        "proxy" => "Tarayıcının proxy (vekil sunucu) ayarlarını değiştirebilir",
        "readingList" => "Okuma listenizi okuyup değiştirebilir",
        "scripting" => "İzin verilen sayfalara kod ekleyebilir",
        "search" => "Varsayılan arama motorunuzla arama yapabilir",
        "sessions" => "Son kapatılan sekmelerinizi görebilir",
        "sidePanel" => "Yan panelde sayfa gösterebilir",
        "storage" => "Tarayıcıda veri saklayabilir",
        "system.cpu" | "system.memory" | "system.display" | "system.storage" => "Sistem bilgilerini okuyabilir",
        "tabCapture" => "Sekmenin görüntüsünü ve sesini kaydedebilir",
        "tabGroups" => "Sekme gruplarınızı görebilir ve düzenleyebilir",
        "tabs" => "Sekmelerinizi ve açık sayfaların adreslerini görebilir",
        "topSites" => "En çok ziyaret ettiğiniz siteleri görebilir",
        "tts" => "Metinleri sesli okuyabilir",
        "unlimitedStorage" => "Sınırsız veri saklayabilir",
        "userScripts" => "Kullanıcı betikleri çalıştırabilir",
        "webNavigation" => "Ziyaret ettiğiniz sayfaları görebilir",
        "webRequest" => "Ağ isteklerinizi görebilir",
        "webRequestBlocking" => "Ağ isteklerinizi engelleyebilir veya değiştirebilir",
        "webRequestAuthProvider" => "Sitelerin kimlik doğrulama isteklerine yanıt verebilir",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "ddkjiahejlhfcafbddmgiahcphecmpfh";
    const OTHER: &str = "eimadpbcbfnmbkopoojfekhnkhdbieeh";

    fn detail(rest: &str) -> String {
        format!("https://chromewebstore.google.com/detail/{rest}")
    }

    #[test]
    fn detail_id_reads_store_pages() {
        for rest in [
            ID.to_string(),
            format!("ublock-origin-lite/{ID}"),
            format!("{ID}/"),
            format!("{ID}?hl=tr"),
            format!("{ID}#reviews"),
            format!("slug/{ID}?hl=tr&utm=x"),
            format!("slug/{ID}#x"),
            format!("slug/{ID}/reviews"),
            format!("{ID}/support"),
            format!("%C3%BCber/{ID}"),
        ] {
            assert_eq!(detail_id(&detail(&rest)).as_deref(), Some(ID), "{rest}");
        }
        // As in the regex, a slug wins over a bare id when both fit.
        assert_eq!(detail_id(&detail(&format!("{OTHER}/{ID}"))).as_deref(), Some(ID));
    }

    #[test]
    fn detail_id_refuses_everything_else() {
        let upper = ID.to_ascii_uppercase();
        for url in [
            detail(&upper),
            detail(&format!("slug/{upper}")),
            detail(&format!("{ID}x")),
            detail(&format!("slug/{ID}x")),
            detail(&ID[..31]),
            detail(&format!("//{ID}")),
            detail(&format!("a/b/{ID}")),
            detail(&format!("?{ID}")),
            detail(&format!("slug?x=/{ID}")),
            detail(&format!("slug#/{ID}")),
            detail(""),
            format!("https://chromewebstore.google.com/{ID}"),
            format!("https://chromewebstore.google.com/details/{ID}"),
            format!("https://chromewebstore.google.com/Detail/{ID}"),
            format!("http://chromewebstore.google.com/detail/{ID}"),
            format!("https://chromewebstore.google.com:443/detail/{ID}"),
            format!("https://user@chromewebstore.google.com/detail/{ID}"),
            format!("https://chromewebstore.google.com.evil.example/detail/{ID}"),
            format!("https://evil.example/chromewebstore.google.com/detail/{ID}"),
            format!("https://evil.example/?u=https://chromewebstore.google.com/detail/{ID}"),
            format!("https://CHROMEWEBSTORE.google.com/detail/{ID}"),
            format!("https://chrome.google.com/webstore/detail/{ID}"),
            format!(" https://chromewebstore.google.com/detail/{ID}"),
        ] {
            assert_eq!(detail_id(&url), None, "{url}");
        }
    }

    #[test]
    fn versions() {
        for v in ["0", "1", "1.2", "1.2.3.4", "65535.0", "2026.1116.1650", "157.0.4341.46"] {
            assert!(is_version(v), "{v}");
        }
        for v in ["", ".", "1.", ".1", "1..2", "1.2.3.4.5", "01", "1.02", "65536", "-1", "1a", "1 ", "+1", "١"] {
            assert!(!is_version(v), "{v:?}");
        }
    }

    #[test]
    fn crx_url_is_the_update_redirect() {
        assert_eq!(
            crx_url(ID, "157.0.4341.46").unwrap(),
            "https://clients2.google.com/service/update2/crx?response=redirect&prodversion=157.0.4341.46&acceptformat=crx3&x=id%3Dddkjiahejlhfcafbddmgiahcphecmpfh%26uc"
        );
        assert_eq!(crx_url(&ID.to_ascii_uppercase(), "1"), None);
        assert_eq!(crx_url(ID, "1&x=id%3Devil"), None);
        assert_eq!(crx_url(ID, ""), None);
        assert!(crate::net::parse_https(&crx_url(ID, "1").unwrap()).is_ok());
    }

    #[test]
    fn sentinel_round_trip() {
        for a in [Action::Install, Action::Remove] {
            let s = sentinel_url(a, ID);
            assert_eq!(parse_sentinel(&s), Some((a, ID.to_string())));
        }
        assert_eq!(sentinel_url(Action::Install, ID), format!("https://chromewebstore.google.com/_xb/install/{ID}"));
        assert_eq!(sentinel_url(Action::Remove, ID), format!("https://chromewebstore.google.com/_xb/remove/{ID}"));
    }

    #[test]
    fn parse_sentinel_is_exact() {
        let base = "https://chromewebstore.google.com/_xb";
        for url in [
            format!("{base}/install/{ID}/"),
            format!("{base}/install/{ID}?x"),
            format!("{base}/install/{ID}#x"),
            format!("{base}/install/{ID}x"),
            format!("{base}/install//{ID}"),
            format!("{base}/install/{}", ID.to_ascii_uppercase()),
            format!("{base}/INSTALL/{ID}"),
            format!("{base}/update/{ID}"),
            format!("{base}/install"),
            format!("{base}//install/{ID}"),
            format!("{base}/install/{ID}/remove/{OTHER}"),
            format!("{base}/install/%64dkjiahejlhfcafbddmgiahcphecmpfh"),
            format!("https://chromewebstore.google.com/_XB/install/{ID}"),
            format!("http://chromewebstore.google.com/_xb/install/{ID}"),
            format!("https://chromewebstore.google.com:443/_xb/install/{ID}"),
            format!("https://chromewebstore.google.com.evil.example/_xb/install/{ID}"),
            format!("https://evil.example/_xb/install/{ID}"),
            format!("https://u@chromewebstore.google.com/_xb/install/{ID}"),
            format!("https://chromewebstore.google.com/detail/_xb/install/{ID}"),
        ] {
            assert_eq!(parse_sentinel(&url), None, "{url}");
        }
    }

    #[test]
    fn sentinel_needs_the_matching_detail_page() {
        use SentinelDecision::*;
        let install = sentinel_url(Action::Install, ID);
        let remove = sentinel_url(Action::Remove, ID);
        assert_eq!(sentinel_decision(&detail(ID), &install, false), Ask(Action::Install, ID.into()));
        assert_eq!(sentinel_decision(&detail(&format!("slug/{ID}?hl=tr")), &remove, false), Ask(Action::Remove, ID.into()));
        for source in [
            // Another extension's page asking for this id.
            detail(OTHER),
            detail(&format!("slug/{OTHER}")),
            // The store, but not a detail page.
            "https://chromewebstore.google.com/".to_string(),
            format!("https://chromewebstore.google.com/search/{ID}"),
            format!("https://chromewebstore.google.com/?q={ID}"),
            format!("https://chromewebstore.google.com/_xb/install/{ID}"),
            // The id only in a query or fragment.
            detail(&format!("slug?id=/{ID}")),
            detail(&format!("{OTHER}#/{ID}")),
            // Any other site, however it is named.
            format!("https://evil.example/detail/{ID}"),
            format!("https://evil.example/?https://chromewebstore.google.com/detail/{ID}"),
            format!("https://chromewebstore.google.com.evil.example/detail/{ID}"),
            format!("http://chromewebstore.google.com/detail/{ID}"),
            "about:blank".to_string(),
            String::new(),
        ] {
            assert_eq!(sentinel_decision(&source, &install, false), Cancel, "{source}");
        }
        // From the right page, but a redirect hop: any server can answer with
        // a Location at the sentinel while Source still shows the detail page.
        assert_eq!(sentinel_decision(&detail(ID), &install, true), Cancel);
        assert_eq!(sentinel_decision(&detail(ID), &remove, true), Cancel);
        // Anything under /_xb that is not an exact sentinel is cancelled, never let through.
        for url in [
            format!("{install}?"),
            format!("{install}/"),
            format!("https://chromewebstore.google.com/_xb/install/{OTHER}"),
            format!("https://chromewebstore.google.com/_xb/update/{ID}"),
            "https://chromewebstore.google.com/_xb".to_string(),
            "https://chromewebstore.google.com/_xb?x".to_string(),
            format!("HTTPS://CHROMEWEBSTORE.GOOGLE.COM/_xb/install/{ID}"),
        ] {
            assert_eq!(sentinel_decision(&detail(ID), &url, false), Cancel, "{url}");
        }
        // Everything else is ordinary navigation.
        for url in [
            detail(ID),
            "https://chromewebstore.google.com/".to_string(),
            format!("https://evil.example/_xb/install/{ID}"),
            format!("https://chromewebstore.google.com.evil.example/_xb/install/{ID}"),
            "https://chromewebstore.google.com/detail/_xb".to_string(),
        ] {
            assert_eq!(sentinel_decision(&detail(ID), &url, false), Pass, "{url}");
        }
    }

    #[test]
    fn hook_script_shape() {
        let s = HOOK_SCRIPT;
        assert!(s.contains("if (location.origin !== 'https://chromewebstore.google.com') return;"));
        // Subframes are hooked too, but only the top frame navigates.
        assert!(s.contains("const isTop = window === window.top;"));
        assert!(s.contains("if (isTop) go(action);"));
        assert!(s.contains("'https://chromewebstore.google.com/_xb/' + action + '/'"));
        assert!(s.contains(r"/^\/detail\/(?:[^/]+\/)?([a-p]{32})(?:\/|$)/"));
        assert!(s.contains("blocked('install')") && s.contains("blocked('remove')"));
        assert!(s.contains("'beginInstallWithManifest3'") && s.contains("'uninstall'"));
        assert!(s.contains("'user_cancelled'"));
        // The id never comes from the call's arguments.
        assert!(!s.contains("arguments[0]"));
        assert!(s.is_ascii());
    }

    // --- update checks --------------------------------------------------------

    /// Captured from the update service on 2026-11-17 for uBOL at 0.0.0.0,
    /// Dark Reader at 99999.0 and an unknown id at 1.0.
    const REAL_RESPONSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?><gupdate xmlns="http://www.google.com/update2/response" protocol="2.0" server="prod"><daystart elapsed_days="7260" elapsed_seconds="39381"/><app appid="ddkjiahejlhfcafbddmgiahcphecmpfh" cohort="1::" cohortname="" status="ok"><updatecheck _esbAllowlist="true" codebase="https://clients2.googleusercontent.com/crx/blobs/ARGaeZLeALIRsuijSwhFBl-Z0ltArvPUOJ-CKdMbUL3RI27qxXQehxFLDTs-E5uh-kq2wKXL36kpjPwSzzPf1A9UBtpDmtC3CWFFVL7CTNsJQj4pTUnzlsthgtG2tNiagPuQAMZSmuXNQ96tXtyqK_JDDWtYTsok8YbagA/DDKJIAHEJLHFCAFBDDMGIAHCPHECMPFH_2026_1116_1650_0.crx" fp="1.0908f4b6d19a38f2681bd746e4597d7818f083bb3e2e3cc5534b407d896b07fd" hash_sha256="0908f4b6d19a38f2681bd746e4597d7818f083bb3e2e3cc5534b407d896b07fd" protected="0" size="9097388" status="ok" version="2026.1116.1650"/></app><app appid="eimadpbcbfnmbkopoojfekhnkhdbieeh" cohort="1::" cohortname="" status="ok"><updatecheck _esbAllowlist="true" status="noupdate"/></app><app appid="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" status="error-unknownApplication"/></gupdate>"#;

    #[test]
    fn update_check_url_batches() {
        let url = update_check_url(&[(ID, "0.0.0.0"), (OTHER, "4.9.110")], "157.0.4341.46").unwrap();
        assert_eq!(
            url,
            "https://clients2.google.com/service/update2/crx?response=updatecheck&prodversion=157.0.4341.46&acceptformat=crx3\
             &x=id%3Dddkjiahejlhfcafbddmgiahcphecmpfh%26v%3D0.0.0.0%26uc\
             &x=id%3Deimadpbcbfnmbkopoojfekhnkhdbieeh%26v%3D4.9.110%26uc"
        );
        assert!(crate::net::parse_https(&url).is_ok());
        assert_eq!(update_check_url(&[], "1"), None);
        assert_eq!(update_check_url(&[(ID, "1")], "x"), None);
        assert_eq!(update_check_url(&[(ID, "1"), ("nope", "1")], "1"), None);
        assert_eq!(update_check_url(&[(ID, "1&x=id%3Devil")], "1"), None);
        let many: Vec<(&str, &str)> = std::iter::repeat_n((ID, "65535.65535.65535.65535"), 200).collect();
        assert_eq!(update_check_url(&many, "1"), None);
        assert!(update_check_url(&many[..100], "1").is_some());
    }

    #[test]
    fn parses_the_real_response() {
        let ups = parse_update_response(REAL_RESPONSE.as_bytes());
        assert_eq!(ups.len(), 3);
        let hash = hex32("0908f4b6d19a38f2681bd746e4597d7818f083bb3e2e3cc5534b407d896b07fd").unwrap();
        assert_eq!(
            ups[0],
            Update {
                id: ID.into(),
                status: "ok".into(),
                version: Some("2026.1116.1650".into()),
                codebase: Some("https://clients2.googleusercontent.com/crx/blobs/ARGaeZLeALIRsuijSwhFBl-Z0ltArvPUOJ-CKdMbUL3RI27qxXQehxFLDTs-E5uh-kq2wKXL36kpjPwSzzPf1A9UBtpDmtC3CWFFVL7CTNsJQj4pTUnzlsthgtG2tNiagPuQAMZSmuXNQ96tXtyqK_JDDWtYTsok8YbagA/DDKJIAHEJLHFCAFBDDMGIAHCPHECMPFH_2026_1116_1650_0.crx".into()),
                hash_sha256: Some(hash),
                size: Some(9_097_388),
            }
        );
        assert_eq!(hash[..4], [0x09, 0x08, 0xf4, 0xb6]);
        assert_eq!((ups[1].id.as_str(), ups[1].status.as_str(), &ups[1].version), (OTHER, "noupdate", &None));
        assert_eq!((ups[2].id.as_str(), ups[2].status.as_str()), ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "error-unknownApplication"));
    }

    const NONE: Vec<Update> = Vec::new();

    fn one(xml: &str) -> Vec<Update> {
        parse_update_response(xml.as_bytes())
    }

    fn ok_app(uc_attrs: &str) -> String {
        format!(r#"<gupdate><app appid="{ID}" status="ok"><updatecheck {uc_attrs}/></app></gupdate>"#)
    }

    const HASH: &str = "0908f4b6d19a38f2681bd746e4597d7818f083bb3e2e3cc5534b407d896b07fd";

    #[test]
    fn compares_versions() {
        assert!(is_newer("1.10", "1.9"));
        assert!(!is_newer("1.9", "1.10"));
        assert!(!is_newer("1.0", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0"));
        assert!(is_newer("1.0.0.1", "1.0"));
        assert!(is_newer("2", "1.65535.65535.65535"));
        assert!(!is_newer("1.2", "1.2"));
        for bad in ["", "1.", "01.2", "1.2.3.4.5", "1.65536", "a.b", "+1", " 1"] {
            assert!(!is_newer(bad, "1.0"), "{bad:?}");
            assert!(!is_newer("9.0", bad), "{bad:?}");
        }
    }

    #[test]
    fn compares_versions_as_numbers() {
        assert!(same_version("1.0", "1.0.0"));
        assert!(same_version("1.0.0.0", "1"));
        assert!(same_version("1.10", "1.10.0"));
        assert!(!same_version("1.1", "1.10"));
        assert!(!same_version("1.0.0.1", "1.0"));
        for bad in ["", "1.", "01.0", "1.0.0.0.0", "x"] {
            assert!(!same_version(bad, "1.0"), "{bad:?}");
            assert!(!same_version(bad, bad), "{bad:?}");
        }
    }

    #[test]
    fn lists_only_the_added_permissions() {
        let old = br#"{"permissions":["storage"],"host_permissions":["https://a.example/*"]}"#;
        let new = br#"{"permissions":["storage","history"],"host_permissions":["https://a.example/*","https://b.example/*"]}"#;
        let added = added_permissions(old, new);
        assert_eq!(added.len(), 2, "{added:?}");
        assert!(added[0].contains("b.example") && !added[0].contains("a.example"), "{added:?}");
        assert!(!added.iter().any(|l| l == &describe_permissions(br#"{"permissions":["storage"]}"#)[0]), "{added:?}");
        assert_eq!(added[1], describe_permissions(br#"{"permissions":["history"]}"#)[0]);
        // Nothing new: nothing listed.
        assert!(added_permissions(new, old).is_empty());
        // Widened by a pattern Chrome ignores: no line to show.
        let odd = br#"{"permissions":["storage"],"host_permissions":["https://a.example/*","not a pattern"]}"#;
        assert!(widens_permissions(old, odd));
        assert!(added_permissions(old, odd).is_empty());
        // An unreadable old manifest: everything the new one asks for.
        assert_eq!(added_permissions(b"not json", new), describe_permissions(new));
    }

    #[test]
    fn reads_the_stated_size() {
        let at = |size: &str| one(&ok_app(&format!(r#"status="ok" version="1" codebase="https://e.example/x" hash_sha256="{HASH}" {size}"#)))[0].size;
        assert_eq!(at(r#"size="1234""#), Some(1234));
        assert_eq!(at(""), None);
        for bad in ["", "0", "-1", "+5", " 5", "5 ", "1e3", "0x10", "9999999999999"] {
            assert_eq!(at(&format!(r#"size="{bad}""#)), None, "{bad:?}");
        }
    }

    #[test]
    fn spots_widened_permissions() {
        let old = br#"{"permissions":["storage","tabs"],"host_permissions":["https://a.example/*"],"content_scripts":[{"matches":["https://b.example/*"]}]}"#;
        // The same, a subset, or reordered: no question.
        assert!(!widens_permissions(old, old));
        assert!(!widens_permissions(old, br#"{"permissions":["tabs"]}"#));
        assert!(!widens_permissions(old, br#"{"host_permissions":["https://b.example/*"],"permissions":["tabs","storage"]}"#));
        assert!(!widens_permissions(b"\xef\xbb\xbf{}", br#"{"optional_permissions":["history"]}"#));
        // Anything new asks, as does an unreadable manifest.
        assert!(widens_permissions(old, br#"{"permissions":["storage","tabs","history"]}"#));
        assert!(widens_permissions(old, br#"{"host_permissions":["<all_urls>"]}"#));
        assert!(widens_permissions(old, br#"{"content_scripts":[{"matches":["https://c.example/*"]}]}"#));
        // An API name moved to hosts is new, not the same string.
        assert!(widens_permissions(br#"{"permissions":["https://a.example/*"]}"#, br#"{"host_permissions":["https://a.example/*"]}"#));
        assert!(widens_permissions(b"not json", br#"{}"#));
        assert!(widens_permissions(br#"{}"#, b"[]"));
    }

    #[test]
    fn spots_widened_powers_beyond_permissions() {
        let old = br#"{"manifest_version":3,"permissions":["storage"],"content_scripts":[{"matches":["https://b.example/*"]}],"externally_connectable":{"matches":["https://a.example/*"]}}"#;
        let with = |extra: &str| format!(r#"{{"manifest_version":3,"permissions":["storage"],{extra}}}"#);
        let cs = r#""externally_connectable":{"matches":["https://a.example/*"]},"content_scripts":[{"matches":["https://b.example/*"]"#;
        // Unchanged, or the same thing turned off: no question.
        assert!(!widens_permissions(old, with(&format!("{cs}}}]")).as_bytes()));
        assert!(!widens_permissions(old, with(&format!(r#"{cs},"all_frames":false}}]"#)).as_bytes()));
        // Each widening asks, with a fixed line for it.
        for (extra, word) in [
            (format!(r#"{cs},"all_frames":true}}]"#), "çerçeve"),
            (format!(r#"{cs},"match_about_blank":true}}]"#), "çerçeve"),
            (format!(r#"{cs},"match_origin_as_fallback":true}}]"#), "çerçeve"),
            (r#""externally_connectable":{"matches":["https://a.example/*","https://c.example/*"]},"content_scripts":[{"matches":["https://b.example/*"]}]"#.to_string(), "konuşabilir"),
            (r#""externally_connectable":{"matches":["https://a.example/*"],"ids":["*"]},"content_scripts":[{"matches":["https://b.example/*"]}]"#.to_string(), "konuşabilir"),
            (format!(r#"{cs}}}],"chrome_url_overrides":{{"newtab":"n.html"}}"#), "yeni sekme"),
        ] {
            let new = with(&extra);
            assert!(widens_permissions(old, new.as_bytes()), "{new}");
            let lines = added_permissions(old, new.as_bytes());
            assert!(lines.len() == 1 && lines[0].contains(word), "{new}: {lines:?}");
        }
        let v2 = String::from_utf8(old.to_vec()).unwrap().replace(r#""manifest_version":3"#, r#""manifest_version":2"#);
        assert!(widens_permissions(v2.as_bytes(), old));
        // Manifest text never reaches the lines.
        let evil = with(&format!(r#"{cs}}}],"chrome_url_overrides":{{"Güvenli, onaylayın":"n.html"}}"#));
        assert!(!added_permissions(old, evil.as_bytes()).iter().any(|l| l.contains("onaylayın")));
    }

    #[test]
    fn update_parser_tolerates_harmless_variation() {
        let cb = "https://example.com/a.crx?x=1&amp;y=2";
        for xml in [
            // Single quotes, extra whitespace, a namespace prefix, comments.
            format!("<g:gupdate xmlns:g='x'><!-- <app appid='{OTHER}' status='ok'/> --><g:app  appid='{ID}'\n status='ok' ><g:updatecheck status='ok' version='1.2' codebase='{cb}' hash_sha256='{HASH}' /></g:app></g:gupdate>"),
            // An unknown child first, a '>' inside a quoted value.
            format!(r#"<gupdate><app appid="{ID}" status="ok" note="a>b"><ping status="ok"/><updatecheck status="ok" version="1.2" codebase="{cb}" hash_sha256="{HASH}"/></app></gupdate>"#),
            // Missing </app> and </gupdate>.
            format!(r#"<gupdate><app appid="{ID}" status="ok"><updatecheck status="ok" version="1.2" codebase="{cb}" hash_sha256="{HASH}"/>"#),
            // A CDATA section holding markup is skipped whole.
            format!(r#"<gupdate><![CDATA[ <app appid="{OTHER}" status="ok"> ]]><app appid="{ID}" status="ok"><updatecheck status="ok" version="1.2" codebase="{cb}" hash_sha256="{HASH}"/></app></gupdate>"#),
        ] {
            let ups = one(&xml);
            assert_eq!(ups.len(), 1, "{xml}");
            assert_eq!(ups[0].id, ID);
            assert_eq!(ups[0].codebase.as_deref(), Some("https://example.com/a.crx?x=1&y=2"));
            assert_eq!(ups[0].version.as_deref(), Some("1.2"));
            assert_eq!(ups[0].hash_sha256, hex32(HASH));
        }
        // Upper-case hex is fine.
        let h = "AB".repeat(32);
        let ups = one(&ok_app(&format!(r#"status="ok" version="1" codebase="https://e.example/x" hash_sha256="{h}""#)));
        assert_eq!(ups[0].hash_sha256, Some([0xab; 32]));
        // An unterminated CDATA section ends the read.
        assert_eq!(one(&format!("<gupdate><![CDATA[ {}", ok_app(&format!(r#"status="ok" version="1" codebase="https://e.example/x" hash_sha256="{HASH}""#)))), NONE);
    }

    #[test]
    fn update_parser_drops_malformed_entries() {
        let good = &format!(r#"status="ok" version="1.2" codebase="https://e.example/x.crx" hash_sha256="{HASH}""#);
        assert_eq!(one(&ok_app(good)).len(), 1);
        // An offered update must carry its hash.
        assert_eq!(one(&ok_app(r#"status="ok" version="1.2" codebase="https://e.example/x.crx""#)), NONE);
        for uc in [
            r#"status="ok" version="1.2" codebase="http://e.example/x.crx""#,
            r#"status="ok" version="1.2" codebase="file:///C:/x.crx""#,
            r#"status="ok" version="1.2" codebase="https://u@e.example/x.crx""#,
            r#"status="ok" version="1.2" codebase="//e.example/x.crx""#,
            r#"status="ok" version="1.2" codebase="https://e.example/x&#10;.crx""#,
            r#"status="ok" version="1.2" codebase="https://e.example/x&bogus;""#,
            r#"status="ok" version="1.2""#,
            r#"status="ok" codebase="https://e.example/x.crx""#,
            r#"status="ok" version="1.2.3.4.5" codebase="https://e.example/x.crx""#,
            r#"status="ok" version="1.02" codebase="https://e.example/x.crx""#,
            r#"status="ok" version="1.2" codebase="https://e.example/x.crx" hash_sha256="00""#,
            r#"status="ok" version="1.2" codebase="https://e.example/x.crx" hash_sha256="zz08f4b6d19a38f2681bd746e4597d7818f083bb3e2e3cc5534b407d896b07fd""#,
            r#"version="1.2" codebase="https://e.example/x.crx""#,
            r#"status="o k" version="1.2" codebase="https://e.example/x.crx""#,
            r#"status="ok" version="1.2" codebase="https://e.example/x.crx" status"#,
            r#"status="ok" version="1.2" codebase="https://e.example/x.crx"bad="1""#,
            r#"status="ok" version="1.2" codebase=https://e.example/x.crx"#,
            r#"status="ok" version="1.2" codebase="https://e.example/x.crx"#,
        ] {
            assert_eq!(one(&ok_app(uc)), NONE, "{uc}");
        }
        // A bad app element.
        for app in [
            format!(r#"<app appid="{}" status="ok">"#, ID.to_ascii_uppercase()),
            r#"<app appid="ddkj" status="ok">"#.to_string(),
            format!(r#"<app status="ok" id="{ID}">"#),
            format!(r#"<app appid="{ID}">"#),
            format!(r#"<app appid="{ID}" status="">"#),
            format!(r#"<app appid="{ID}" status="ok"<x>"#),
        ] {
            let xml = format!("<gupdate>{app}<updatecheck {good}/></app></gupdate>");
            assert_eq!(one(&xml), NONE, "{app}");
        }
        // An updatecheck outside any app is ignored; only the first one counts.
        assert_eq!(one(&format!("<gupdate><updatecheck {good}/></gupdate>")), NONE);
        let two = format!(r#"<gupdate><app appid="{ID}" status="ok"><updatecheck status="noupdate"/><updatecheck {good}/></app></gupdate>"#);
        assert_eq!(one(&two)[0].status, "noupdate");
    }

    #[test]
    fn update_parser_survives_hostile_input() {
        assert_eq!(one(""), NONE);
        assert_eq!(one("not xml at all"), NONE);
        assert_eq!(one("<"), NONE);
        assert_eq!(one("<app appid=\""), NONE);
        assert_eq!(one("<!--"), NONE);
        assert_eq!(one(&"<app ".repeat(10_000)), NONE);
        assert_eq!(parse_update_response(&[0xff, 0xfe, b'<', b'a']), NONE);
        // Over the size cap: nothing, even if well formed.
        let mut big = REAL_RESPONSE.to_string();
        big.push_str(&" ".repeat(MAX_UPDATE_RESPONSE));
        assert_eq!(parse_update_response(big.as_bytes()), NONE);
        // A repeated id keeps only its first entry.
        let twice = format!(r#"<gupdate><app appid="{ID}" status="error-a"/><app appid="{ID}" status="ok"><updatecheck status="noupdate"/></app></gupdate>"#);
        let ups = one(&twice);
        assert_eq!((ups.len(), ups[0].status.as_str()), (1, "error-a"));
        // Many apps are capped.
        let many: String = (0..MAX_APPS + 50)
            .map(|i| {
                let id: String = format!("{i:032}").bytes().map(|d| (b'a' + d - b'0') as char).collect();
                format!(r#"<app appid="{id}" status="error-x"/>"#)
            })
            .collect();
        assert_eq!(one(&many).len(), MAX_APPS);
    }

    // --- permissions ----------------------------------------------------------

    fn perms(json: &str) -> Vec<String> {
        describe_permissions(json.as_bytes())
    }

    #[test]
    fn describes_common_permissions() {
        let lines = perms(
            r#"{"manifest_version":3,"permissions":["tabs","storage","webRequest","declarativeNetRequest","cookies","history","downloads","scripting"],"host_permissions":["<all_urls>"]}"#,
        );
        assert_eq!(
            lines,
            [
                ALL_SITES,
                "Sekmelerinizi ve açık sayfaların adreslerini görebilir",
                "Tarayıcıda veri saklayabilir",
                "Ağ isteklerinizi görebilir",
                "Sayfalardaki içeriği engelleyebilir",
                "Çerezlerinizi okuyup değiştirebilir",
                "Göz atma geçmişinizi okuyup değiştirebilir",
                "İndirmelerinizi yönetebilir",
                "İzin verilen sayfalara kod ekleyebilir",
            ]
        );
    }

    #[test]
    fn describes_host_access() {
        assert_eq!(perms(r#"{"host_permissions":["*://*/*"]}"#), [ALL_SITES]);
        assert_eq!(perms(r#"{"permissions":["https://*/*"]}"#), [ALL_SITES]);
        assert_eq!(perms(r#"{"content_scripts":[{"matches":["<all_urls>"],"js":["a.js"]}]}"#), [ALL_SITES]);
        assert_eq!(
            perms(r#"{"host_permissions":["https://*.example.com/*","https://a.example/x/*"],"content_scripts":[{"matches":["https://a.example/*","*://b.example/*"]}]}"#),
            ["Şu sitelerdeki verilerinizi okuyup değiştirebilir: *.example.com, a.example, b.example"]
        );
        assert_eq!(perms(r#"{"permissions":["file:///*"]}"#), ["Şu sitelerdeki verilerinizi okuyup değiştirebilir: bilgisayarınızdaki dosyalar"]);
        let many: Vec<String> = (0..12).map(|i| format!("\"https://h{i}.example/*\"")).collect();
        let line = &perms(&format!(r#"{{"host_permissions":[{}]}}"#, many.join(",")))[0];
        assert!(line.ends_with("h7.example ve 4 site daha"), "{line}");
        // A port does not hide "every site", and is not shown.
        assert_eq!(perms(r#"{"host_permissions":["https://*:443/*"]}"#), [ALL_SITES]);
        assert_eq!(
            perms(r#"{"host_permissions":["https://A.Example:8443/*","https://*.b.example:*/*"]}"#),
            ["Şu sitelerdeki verilerinizi okuyup değiştirebilir: a.example, *.b.example"]
        );
    }

    #[test]
    fn host_names_cannot_spoof_the_dialog() {
        let hosts = |list: &str| perms(&format!(r#"{{"host_permissions":[{list}]}}"#));
        // Not a host name: its text is dropped, whatever it says, but the access
        // is still reported. RLO is built here so the source stays plain.
        let rlo = char::from_u32(0x202E).unwrap();
        for bad in [
            r#""https://Bu uzantı güvenlidir/*""#.to_string(),
            format!("\"https://a.example{rlo}.evil/*\""),
            r#""https://<img src=x onerror=alert(1)>/*""#.to_string(),
            r#""https://a..example/*""#.to_string(),
            r#""https://a.example:x/*""#.to_string(),
            r#""https://[::1]/*""#.to_string(),
        ] {
            assert_eq!(hosts(&bad), [OTHER_SITES], "{bad:?}");
        }
        assert_eq!(hosts(r#""https://a.example/*","https://[::1]/*""#), ["Şu sitelerdeki verilerinizi okuyup değiştirebilir: a.example ve başka adresler"]);
        // Not a match pattern at all: Chrome ignores it, so nothing is shown.
        assert_eq!(hosts(r#""not a pattern""#), Vec::<String>::new());
        // A long name keeps its end, which holds the registrable domain.
        let long = format!("accounts.google.com.{}.evil.example", "a".repeat(60));
        let line = &hosts(&format!(r#""https://{long}/*""#))[0];
        assert!(line.ends_with("aaaa.evil.example"), "{line}");
        assert!(line.contains('…') && !line.contains("accounts.google.com"), "{line}");
    }

    #[test]
    fn permissions_edge_cases() {
        // Unknown identifiers are labelled as such; anything else is not quoted; duplicates once.
        assert_eq!(
            perms(&format!(
                r#"{{"permissions":["storage","fooBar","storage","bad{bel}{nl}name","Bu uzantı hiçbir veri toplamaz","x{rlo}y"]}}"#,
                bel = r"\u0007",
                nl = r"\n",
                rlo = char::from_u32(0x202E).unwrap()
            )),
            ["Tarayıcıda veri saklayabilir", "Bilinmeyen izin: fooBar", UNKNOWN_PERMISSION]
        );
        assert_eq!(perms(&format!(r#"{{"permissions":["{}"]}}"#, "x".repeat(500))), [UNKNOWN_PERMISSION]);
        // Many lines are capped, the last one counting the rest.
        let names: Vec<String> = (0..100).map(|i| format!("\"p{i}\"")).collect();
        let lines = perms(&format!(r#"{{"host_permissions":["<all_urls>"],"permissions":[{}]}}"#, names.join(",")));
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[MAX_LINES - 1], "ve 72 izin daha");
        // Made-up names cannot push a known permission past the cap.
        let mut names: Vec<String> = (0..40).map(|i| format!("\"p{i}\"")).collect();
        names.push("\"debugger\"".into());
        let lines = perms(&format!(r#"{{"permissions":[{}]}}"#, names.join(",")));
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[0], "Sayfaları hata ayıklayıcıyla denetleyip içlerindeki her şeye erişebilir");
        // Nothing asked, or not a manifest.
        for json in ["{}", r#"{"permissions":[]}"#, r#"{"permissions":"tabs"}"#, r#"{"permissions":[1,null,{}]}"#, "[]", "null", "", "{", "\u{feff}"] {
            assert_eq!(perms(json), Vec::<String>::new(), "{json:?}");
        }
        // A UTF-8 BOM is fine.
        assert_eq!(describe_permissions(b"\xef\xbb\xbf{\"permissions\":[\"tabs\"]}").len(), 1);
        // Optional permissions are asked later, at run time, so they are not listed.
        assert_eq!(perms(r#"{"optional_permissions":["history"],"optional_host_permissions":["<all_urls>"]}"#), Vec::<String>::new());
    }

    #[test]
    fn every_known_permission_has_text() {
        for p in ["activeTab", "alarms", "bookmarks", "contextMenus", "nativeMessaging", "notifications", "unlimitedStorage", "webNavigation", "webRequestBlocking", "declarativeNetRequestWithHostAccess", "declarativeNetRequestFeedback", "offscreen", "sidePanel", "management", "debugger", "proxy", "privacy", "gcm", "printing"] {
            assert!(permission_text(p).is_some_and(|t| t.ends_with("bilir")), "{p}");
        }
    }
}
