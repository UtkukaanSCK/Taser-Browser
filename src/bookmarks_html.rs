//! Bookmark import and export in the Netscape bookmark file format: the
//! `.html` file Chrome, Edge and Firefox write and read ("Export
//! bookmarks" / "Import bookmarks from HTML file").
//!
//! - `parse` reads a file the user picked, which is untrusted input: the
//!   caller caps its size (`MAX_IMPORT_BYTES`), the parser caps how many
//!   links it keeps (`MAX_ENTRIES`) and how deep folders may nest
//!   (`MAX_DEPTH`), keeps only http(s) addresses within `MAX_URL_LEN`, and
//!   trims and caps titles. It never fails: what it cannot read is skipped
//!   and counted.
//! - Taser has no folders, so an import is flattened (folder names are
//!   dropped) and an export is one flat list.
//! - `write` escapes every address and title, so a title such as
//!   `</A><script>` stays text in whichever browser reads the file.

use crate::bookmarks::{Bookmark, MAX_TITLE};
use crate::history;
use crate::store;
use std::io::Read;
use std::path::Path;

/// Larger files are not imported (Chrome's export of 10,000 bookmarks is
/// about 2 MB).
pub const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;
/// Most links one import keeps; the rest are counted as dropped.
pub const MAX_ENTRIES: usize = 50_000;
/// Deepest folder nesting read (`<DL>` levels); links below it are dropped.
pub const MAX_DEPTH: usize = 32;

/// What `parse` found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// Links fit to keep, in file order; repeats are left to the caller.
    pub items: Vec<Bookmark>,
    /// Links not kept: not a web address, too long, too deep, over
    /// `MAX_ENTRIES`.
    pub dropped: usize,
}

/// Reads the links of a Netscape bookmark file. Tags and attribute names
/// are matched in any case; attribute values may be double-, single- or
/// unquoted; a link without a closing `</A>` takes its title from the text
/// up to the next tag. Anything else (folders, separators, descriptions,
/// unknown tags) is skipped.
pub fn parse(text: &str) -> Parsed {
    let now = history::now_secs();
    let mut out = Parsed::default();
    let mut depth = 0usize;
    let mut pos = 0;
    while let Some(off) = text[pos..].find('<') {
        let start = pos + off;
        let Some(tag) = read_tag(text, start) else {
            // A `<` that does not start a tag ("a < b" in a title, or a cut
            // file): plain text.
            pos = start + 1;
            continue;
        };
        pos = tag.end;
        match (tag.closing, tag.name.to_ascii_lowercase().as_str()) {
            (false, "dl") => depth += 1,
            (true, "dl") => depth = depth.saturating_sub(1),
            (false, "a") => {
                let (title, end) = link_text(text, pos);
                pos = end;
                let kept = (depth <= MAX_DEPTH && out.items.len() < MAX_ENTRIES)
                    .then(|| link(&tag, &title, now))
                    .flatten();
                match kept {
                    Some(b) => out.items.push(b),
                    None => out.dropped += 1,
                }
            }
            _ => {}
        }
    }
    out
}

/// A bookmark from an `<A>` tag and its text, or None if its address may
/// not be kept.
fn link(tag: &Tag, title: &str, now: u64) -> Option<Bookmark> {
    let href = decode(tag.attr("href")?);
    let url = history::without_userinfo(href.trim());
    if !history::recordable(&url) {
        return None;
    }
    let added = tag.attr("add_date").and_then(|d| d.trim().parse::<u64>().ok()).unwrap_or(0).min(now);
    Some(Bookmark { url, title: clean_text(title), added })
}

/// Tags that end a link's text when its `</A>` is missing.
const BREAKS: [&str; 6] = ["a", "dt", "dd", "dl", "h3", "hr"];
/// Most raw title text read per link (bytes); the rest is skipped.
const MAX_RAW_TITLE: usize = 64 * 1024;

/// The raw text of a link: up to its `</A>`, or up to the next tag that
/// cannot be in a title (another link, the next item, a folder). Tags inside
/// the title (`<b>`, a stray `<br>`) are left out. Returns the text and
/// where reading goes on; every byte is looked at once, so a file of links
/// that never close stays linear.
fn link_text(text: &str, from: usize) -> (String, usize) {
    let mut raw = String::new();
    let mut pos = from;
    while let Some(off) = text[pos..].find('<') {
        let start = pos + off;
        push_capped(&mut raw, &text[pos..start]);
        match read_tag(text, start) {
            None => {
                push_capped(&mut raw, "<");
                pos = start + 1;
            }
            Some(tag) => {
                let name = tag.name.to_ascii_lowercase();
                if tag.closing && name == "a" {
                    return (raw, tag.end);
                }
                if BREAKS.contains(&name.as_str()) {
                    return (raw, start);
                }
                pos = tag.end;
            }
        }
    }
    push_capped(&mut raw, &text[pos..]);
    (raw, text.len())
}

fn push_capped(raw: &mut String, s: &str) {
    let mut end = MAX_RAW_TITLE.saturating_sub(raw.len()).min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    raw.push_str(&s[..end]);
}

/// A title as kept: entities decoded, runs of white space made one space,
/// control characters dropped, at most `MAX_TITLE` characters.
fn clean_text(raw: &str) -> String {
    let decoded = decode(raw);
    let spaced = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = history::clean_title(&spaced);
    debug_assert!(title.chars().count() <= MAX_TITLE);
    title
}

// ------------------------------------------------------------------ tags

/// One tag as read: its name, whether it closes, its attributes and the
/// byte offset just past its `>`.
struct Tag<'a> {
    name: &'a str,
    closing: bool,
    attrs: Vec<(&'a str, &'a str)>,
    end: usize,
}

impl<'a> Tag<'a> {
    /// The value of attribute `name` (in any case), as written.
    fn attr(&self, name: &str) -> Option<&'a str> {
        self.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|&(_, v)| v)
    }
}

/// Most attributes read per tag (Chrome writes up to four on a link).
const MAX_ATTRS: usize = 16;

/// The tag starting at `text[start]` (a `<`), or None when none starts
/// there or it is never closed. Comments (`<!-- -->`) and declarations
/// (`<!DOCTYPE>`) come back as a tag named `!`.
fn read_tag(text: &str, start: usize) -> Option<Tag<'_>> {
    let b = text.as_bytes();
    let mut i = start + 1;
    // A comment or declaration never closed runs to the end of the file
    // (as in a browser), so it is not looked for again from every `<`.
    if text[i..].starts_with("!--") {
        let end = text[i + 3..].find("-->").map_or(text.len(), |e| i + 3 + e + 3);
        return Some(Tag { name: "!", closing: false, attrs: Vec::new(), end });
    }
    if text[i..].starts_with(['!', '?']) {
        let end = text[i..].find('>').map_or(text.len(), |e| i + e + 1);
        return Some(Tag { name: "!", closing: false, attrs: Vec::new(), end });
    }
    let closing = b.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let name_start = i;
    while b.get(i).is_some_and(u8::is_ascii_alphanumeric) {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = &text[name_start..i];
    let mut attrs = Vec::new();
    loop {
        while b.get(i).is_some_and(|c| c.is_ascii_whitespace() || *c == b'/') {
            i += 1;
        }
        let item_start = i;
        match *b.get(i)? {
            b'>' => return Some(Tag { name, closing, attrs, end: i + 1 }),
            // A tag cut off by the next one: it ends here.
            b'<' => return Some(Tag { name, closing, attrs, end: i }),
            _ => {}
        }
        let key_start = i;
        while b.get(i).is_some_and(|c| !c.is_ascii_whitespace() && !matches!(c, b'=' | b'>' | b'<' | b'/')) {
            i += 1;
        }
        let key = &text[key_start..i];
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let mut value = "";
        if b.get(i) == Some(&b'=') {
            i += 1;
            while b.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            match *b.get(i)? {
                q @ (b'"' | b'\'') => {
                    let close = text[i + 1..].find(q as char)? + i + 1;
                    value = &text[i + 1..close];
                    i = close + 1;
                }
                _ => {
                    let v_start = i;
                    while b.get(i).is_some_and(|c| !c.is_ascii_whitespace() && !matches!(c, b'>' | b'<')) {
                        i += 1;
                    }
                    value = &text[v_start..i];
                }
            }
        }
        if !key.is_empty() && attrs.len() < MAX_ATTRS {
            attrs.push((key, value));
        }
        if i == item_start {
            // Nothing was read (cannot happen, but never loop forever).
            i += 1;
        }
    }
}

/// `s` with its character references decoded: the five named ones, `&nbsp;`
/// and numeric ones (`&#39;`, `&#x27;`). Others stay as written; a numeric
/// one that is no character becomes U+FFFD.
fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let semi = rest[1..].find(';').map(|i| i + 1).filter(|&i| i <= 12);
        let decoded = semi.and_then(|i| entity(&rest[1..i]).map(|c| (c, i + 1)));
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    let num = |digits: &str, radix| u32::from_str_radix(digits, radix).ok().map(|n| char::from_u32(n).unwrap_or('\u{FFFD}'));
    match name {
        "amp" | "AMP" => Some('&'),
        "lt" | "LT" => Some('<'),
        "gt" | "GT" => Some('>'),
        "quot" | "QUOT" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => match name.strip_prefix('#') {
            Some(hex) if hex.starts_with(['x', 'X']) => num(&hex[1..], 16),
            Some(dec) => num(dec, 10),
            None => None,
        },
    }
}

// ------------------------------------------------------------------ files

/// What an import or export came to. It is worked out on a thread of its
/// own (the dialog and the file) and handed back to the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transfer {
    /// The dialog was cancelled or could not be shown.
    Cancelled,
    /// The picked file, parsed.
    Read(Parsed),
    /// This many bookmarks were written.
    Written(usize),
    /// The picked file is over `MAX_IMPORT_BYTES`.
    TooBig,
    ReadFailed,
    WriteFailed,
}

/// Reads and parses the file at `path` (never more than
/// `MAX_IMPORT_BYTES`). Text that is not UTF-8 is read as well as it goes.
pub fn read_import(path: &Path) -> Transfer {
    let Ok(file) = std::fs::File::open(path) else { return Transfer::ReadFailed };
    if file.metadata().is_ok_and(|m| m.len() > MAX_IMPORT_BYTES) {
        return Transfer::TooBig;
    }
    let mut bytes = Vec::new();
    if file.take(MAX_IMPORT_BYTES + 1).read_to_end(&mut bytes).is_err() {
        return Transfer::ReadFailed;
    }
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Transfer::TooBig;
    }
    Transfer::Read(parse(&String::from_utf8_lossy(&bytes)))
}

/// Writes `items` to `path` as a whole file or not at all (a temporary
/// sibling renamed over it; removed again if that fails).
pub fn write_export(path: &Path, items: &[Bookmark]) -> Transfer {
    match store::write_atomic(path, write(items).as_bytes(), false) {
        Ok(()) => Transfer::Written(items.len()),
        Err(e) => {
            eprintln!("taser: cannot export bookmarks: {e}");
            let _ = std::fs::remove_file(store::sibling(path, ".tmp"));
            Transfer::WriteFailed
        }
    }
}

// ----------------------------------------------------------------- write

/// The bookmarks as a Netscape bookmark file (UTF-8), one flat list in the
/// order given, the way Chrome writes it.
pub fn write(items: &[Bookmark]) -> String {
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n\
         <!-- This is an automatically generated file.\n     \
         It will be read and overwritten.\n     \
         DO NOT EDIT! -->\n\
         <META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n\
         <TITLE>Bookmarks</TITLE>\n\
         <H1>Bookmarks</H1>\n\
         <DL><p>\n",
    );
    for b in items {
        out.push_str("    <DT><A HREF=\"");
        escape_into(&mut out, &b.url);
        out.push('"');
        if b.added > 0 {
            out.push_str(&format!(" ADD_DATE=\"{}\"", b.added));
        }
        out.push('>');
        escape_into(&mut out, &b.title);
        out.push_str("</A>\n");
    }
    out.push_str("</DL><p>\n");
    out
}

/// Appends `s` with `& < > " '` escaped: safe in text and in a quoted
/// attribute.
fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

// ----------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn bm(url: &str, title: &str, added: u64) -> Bookmark {
        Bookmark { url: url.into(), title: title.into(), added }
    }

    fn urls(p: &Parsed) -> Vec<&str> {
        p.items.iter().map(|b| b.url.as_str()).collect()
    }

    /// As Chrome (and Edge, which writes the same format) exports: folders
    /// as `<H3>` with their own `<DL>`, extra attributes, `&amp;` in links.
    const CHROME: &str = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<!-- This is an automatically generated file.
     It will be read and overwritten.
     DO NOT EDIT! -->
<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=UTF-8">
<TITLE>Bookmarks</TITLE>
<H1>Bookmarks</H1>
<DL><p>
    <DT><H3 ADD_DATE="1700000000" LAST_MODIFIED="1700000100" PERSONAL_TOOLBAR_FOLDER="true">Bookmarks bar</H3>
    <DL><p>
        <DT><A HREF="https://www.example.com/" ADD_DATE="1700000001" ICON="data:image/png;base64,iVBORw0KGgo=">Example</A>
        <DT><H3 ADD_DATE="1700000002" LAST_MODIFIED="0">News</H3>
        <DL><p>
            <DT><A HREF="https://news.example.org/a?x=1&amp;y=2" ADD_DATE="1700000003">News &amp; more</A>
        </DL><p>
    </DL><p>
    <DT><A HREF="http://plain.example.net/" ADD_DATE="1700000004">Plain</A>
</DL><p>
"#;

    #[test]
    fn reads_a_chrome_or_edge_export_flattened() {
        let p = parse(CHROME);
        assert_eq!(urls(&p), ["https://www.example.com/", "https://news.example.org/a?x=1&y=2", "http://plain.example.net/"]);
        assert_eq!(p.items[1].title, "News & more");
        assert_eq!(p.items[0].added, 1_700_000_001);
        assert_eq!(p.dropped, 0);
    }

    /// As Firefox exports: lower-case `<p>`, `LAST_MODIFIED`, `TAGS`,
    /// `<DD>` descriptions, `<HR>` separators, a `place:` query and
    /// numeric references.
    const FIREFOX: &str = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<!-- This is an automatically generated file.
     It will be read and overwritten.
     DO NOT EDIT! -->
<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=UTF-8">
<meta http-equiv="Content-Security-Policy"
      content="default-src 'self'; script-src 'none'; img-src data: *; object-src 'none'"></meta>
<TITLE>Bookmarks</TITLE>
<H1>Bookmarks Menu</H1>

<DL><p>
    <DT><H3 ADD_DATE="1690000000" LAST_MODIFIED="1690000500">Mozilla Firefox</H3>
    <DL><p>
        <DT><A HREF="https://support.mozilla.org/products/firefox" ADD_DATE="1690000001" LAST_MODIFIED="1690000002" ICON_URI="https://support.mozilla.org/favicon.ico" TAGS="help,docs">Get Help</A>
        <DD>Firefox help &#8211; articles
        <HR>
        <DT><A HREF="place:sort=8&amp;maxResults=10" ADD_DATE="1690000003">Recent Tags</A>
    </DL><p>
    <DT><A HREF="https://t&#252;rk&#xE7;e.example/" ADD_DATE="1690000004">T&#252;rk&#xE7;e &#39;sayfa&#39;</A>
</DL>
"#;

    #[test]
    fn reads_a_firefox_export() {
        let p = parse(FIREFOX);
        assert_eq!(urls(&p), ["https://support.mozilla.org/products/firefox", "https://türkçe.example/"]);
        assert_eq!(p.items[0].title, "Get Help");
        assert_eq!(p.items[1].title, "Türkçe 'sayfa'");
        // `place:` is Firefox's own query, not a web address.
        assert_eq!(p.dropped, 1);
    }

    #[test]
    fn keeps_only_web_addresses() {
        let text = r#"<DL>
            <DT><A HREF="javascript:alert(1)">js</A>
            <DT><A HREF=" JavaScript:alert(1)">js2</A>
            <DT><A HREF="data:text/html,<b>x</b>">data</A>
            <DT><A HREF="file:///C:/Windows/win.ini">file</A>
            <DT><A HREF="xb://ui/index.html">ui</A>
            <DT><A HREF="ftp://example.com/">ftp</A>
            <DT><A HREF="chrome://settings">chrome</A>
            <DT><A HREF="">empty</A>
            <DT><A>no href</A>
            <DT><A HREF="https://ok.example/">ok</A>
            <DT><A HREF="  HTTPS://Upper.example/  ">upper</A>
            <DT><A HREF="https://user:pw@secret.example/">userinfo</A>
            <DT><A HREF="https://ctl.example/&#10;x">control</A>
        </DL>"#;
        let p = parse(text);
        assert_eq!(urls(&p), ["https://ok.example/", "HTTPS://Upper.example/", "https://secret.example/"]);
        assert_eq!(p.dropped, 10);
    }

    #[test]
    fn caps_titles_and_addresses() {
        let long_title = "ş".repeat(MAX_TITLE + 50);
        let long_url = format!("https://a.example/{}", "x".repeat(crate::url::MAX_URL_LEN));
        let text = format!(
            "<DL><DT><A HREF=\"https://t.example/\">  {long_title}  </A><DT><A HREF=\"{long_url}\">long</A><DT><A HREF=\"https://w.example/\">\n  a \t\n b\u{7}c  </A></DL>"
        );
        let p = parse(&text);
        assert_eq!(urls(&p), ["https://t.example/", "https://w.example/"]);
        assert_eq!(p.items[0].title.chars().count(), MAX_TITLE);
        assert_eq!(p.items[1].title, "a bc");
        assert_eq!(p.dropped, 1);
    }

    #[test]
    fn caps_the_link_count() {
        let mut text = String::from("<DL>");
        for i in 0..MAX_ENTRIES + 7 {
            text.push_str(&format!("<DT><A HREF=\"https://e.example/{i}\">{i}</A>\n"));
        }
        let p = parse(&text);
        assert_eq!(p.items.len(), MAX_ENTRIES);
        assert_eq!(p.dropped, 7);
        assert_eq!(p.items.last().map(|b| b.url.as_str()), Some(format!("https://e.example/{}", MAX_ENTRIES - 1).as_str()));
    }

    #[test]
    fn caps_the_nesting_depth() {
        let open = "<DL><p>".repeat(MAX_DEPTH + 3);
        let text = format!(
            "<DL><DT><A HREF=\"https://top.example/\">top</A>{open}<DT><A HREF=\"https://deep.example/\">deep</A>{}<DT><A HREF=\"https://back.example/\">back</A></DL>",
            "</DL><p>".repeat(MAX_DEPTH + 3)
        );
        let p = parse(&text);
        assert_eq!(urls(&p), ["https://top.example/", "https://back.example/"]);
        assert_eq!(p.dropped, 1);
        // Exactly MAX_DEPTH levels are still read.
        let edge = format!("{}<DT><A HREF=\"https://edge.example/\">edge</A>", "<DL>".repeat(MAX_DEPTH));
        assert_eq!(urls(&parse(&edge)), ["https://edge.example/"]);
    }

    #[test]
    fn survives_malformed_input() {
        let cases = [
            "",
            "<",
            "<<<<>>>>",
            "<A",
            "<A HREF=\"https://cut.example/",
            "<A HREF='https://x.example/'",
            "</DL></DL></DL><A HREF=https://u.example/>unquoted</A>",
            "<!-- never closed <A HREF=\"https://c.example/\">c</A>",
            "<A HREF=\"https://n.example/\">no close<DT><A HREF=\"https://m.example/\">m</a >",
            "<A = = HREF = \"https://s.example/\" >spaced</A>",
            "<A HREF=\"https://b.example/\"><b>bold</b> &amp &bogus; &#xZZ; &#99999999; 1 < 2</A>",
            "&&&;;;<<//>><A/>",
            "\u{0}\u{FEFF}<A HREF=\"https://z.example/\">z</A>",
        ];
        for text in cases {
            let p = parse(text);
            assert!(p.items.iter().all(|b| history::recordable(&b.url)), "{text}");
        }
        assert_eq!(urls(&parse(cases[6])), ["https://u.example/"]);
        assert_eq!(parse(cases[6]).items[0].title, "unquoted");
        assert!(parse(cases[7]).items.is_empty());
        let p = parse(cases[8]);
        assert_eq!(urls(&p), ["https://n.example/", "https://m.example/"]);
        assert_eq!(p.items[0].title, "no close");
        assert_eq!(p.items[1].title, "m");
        assert_eq!(parse(cases[9]).items[0].title, "spaced");
        assert_eq!(parse(cases[10]).items[0].title, "bold &amp &bogus; &#xZZ; \u{FFFD} 1 < 2");
        assert_eq!(urls(&parse(cases[12])), ["https://z.example/"]);
    }

    /// Links that never close, comments and declarations that never close:
    /// each byte is read a bounded number of times (quadratic scanning of
    /// these 1 MiB files would not finish in time for the test run).
    #[test]
    fn stays_linear_on_hostile_input() {
        let open_links = "<A HREF=\"https://e.example/\">t".repeat(1 << 15);
        let p = parse(&open_links);
        assert_eq!(p.items.len() + p.dropped, 1 << 15);
        assert!(parse(&"<!".repeat(1 << 19)).items.is_empty());
        assert!(parse(&"<!--".repeat(1 << 18)).items.is_empty());
        assert!(parse(&"<A x='".repeat(1 << 17)).items.is_empty());
        assert!(parse(&"<b<".repeat(1 << 18)).items.is_empty());
    }

    #[test]
    fn writes_escaped_text_and_attributes() {
        let out = write(&[
            bm("https://e.example/?a=1&b=\"2\"&c='3'", "</A><script>alert(1)</script> & \"q\" 'a'", 1_700_000_000),
            bm("https://plain.example/", "", 0),
        ]);
        assert!(out.starts_with("<!DOCTYPE NETSCAPE-Bookmark-file-1>\n"));
        assert!(out.contains("<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">"));
        assert!(out.contains(
            "<DT><A HREF=\"https://e.example/?a=1&amp;b=&quot;2&quot;&amp;c=&#39;3&#39;\" ADD_DATE=\"1700000000\">&lt;/A&gt;&lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;q&quot; &#39;a&#39;</A>\n"
        ));
        assert!(out.contains("<DT><A HREF=\"https://plain.example/\"></A>\n"));
        assert!(!out.contains("<script"));
        assert!(out.trim_end().ends_with("</DL><p>"));
    }

    #[test]
    fn round_trips() {
        let items = vec![
            bm("https://a.example/", "A", 1_600_000_000),
            bm("https://b.example/x?y=1&z=<2>", "B & \"b\" <i>'", 1_600_000_001),
            bm("http://c.example/#frag", "Çığ öşü – 日本", 0),
            bm("https://d.example/", "", 1_600_000_002),
        ];
        let back = parse(&write(&items));
        assert_eq!(back.items, items);
        assert_eq!(back.dropped, 0);
        assert_eq!(parse(&write(&[])), Parsed::default());
    }

    #[test]
    fn keeps_repeats_for_the_caller_and_clamps_dates() {
        let text = "<DL><DT><A HREF=\"https://r.example/\" ADD_DATE=\"99999999999999\">1</A><DT><A HREF=\"https://r.example/\" ADD_DATE=\"x\">2</A></DL>";
        let p = parse(text);
        assert_eq!(urls(&p), ["https://r.example/", "https://r.example/"]);
        assert!(p.items[0].added <= history::now_secs());
        assert_eq!(p.items[1].added, 0);
    }

    #[test]
    fn reads_and_writes_files() {
        let dir = crate::store::tests::temp_dir("bm-html-files");
        let path = dir.join("out.html");
        let items = vec![bm("https://a.example/", "A <b>", 1_600_000_000)];
        assert_eq!(write_export(&path, &items), Transfer::Written(1));
        assert!(!crate::store::sibling(&path, ".tmp").exists());
        assert_eq!(read_import(&path), Transfer::Read(Parsed { items, dropped: 0 }));
        // Writing below a file (not a folder) fails and leaves nothing.
        let bad = path.join("x.html");
        assert_eq!(write_export(&bad, &[]), Transfer::WriteFailed);
        assert_eq!(read_import(&dir.join("missing.html")), Transfer::ReadFailed);
        // One byte over the cap is not read at all.
        let big = dir.join("big.html");
        std::fs::write(&big, vec![b' '; MAX_IMPORT_BYTES as usize + 1]).unwrap();
        assert_eq!(read_import(&big), Transfer::TooBig);
        // Not UTF-8: the links are still found.
        let latin = dir.join("latin.html");
        std::fs::write(&latin, b"<DT><A HREF=\"https://l.example/\">caf\xe9</A>").unwrap();
        let Transfer::Read(p) = read_import(&latin) else { panic!("not read") };
        assert_eq!(urls(&p), ["https://l.example/"]);
        assert_eq!(p.items[0].title, "caf\u{fffd}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
