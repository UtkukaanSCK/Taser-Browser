//! Taser's new tab page: a search box, the most visited sites and the
//! first bookmarks, shown in every blank tab (Ctrl+T, the strip's "+", the
//! home button with no home address set).
//!
//! The tab stays at `about:blank`, so everything a blank tab does (the
//! address bar empty and focused, the "Yeni sekme" label, a session that
//! keeps it) is unchanged; once its load has completed, `fill` writes the
//! page into the blank document. Only a document the app itself opened is
//! filled: the script checks it is a top-level `about:blank` with an opaque
//! origin, which a blank page a site opened or navigated to never has (it
//! takes the site's origin), so no site can read the lists; and only while
//! it is still empty, so a page written into a blank document (HTML the app
//! loads, `document.write`) is left alone. Every address
//! and title goes in as a JSON string, and into the page as text only.
//!
//! The app also writes it only into a blank tab it opened itself and that
//! has not navigated since (`Tab::new_tab_page`): running the script gives
//! the page a user gesture, which no site's blank page may get from us.
//!
//! The tiles are letters, not site icons: drawing the page loads nothing.

use crate::app::App;
use crate::tabs::{TabId, push_json_str};

/// Tiles per list at most.
pub const TILES: usize = 8;

/// Characters of a tile's label at most.
const MAX_LABEL: usize = 80;

/// The search box's placeholder and label.
pub const SEARCH_LABEL: &str = "Arayın ya da bir adres yazın";
pub const SITES_HEADING: &str = "Sık ziyaret edilenler";
pub const MARKS_HEADING: &str = "Yer imleri";

/// One tile: where it goes, what it says, and the site its letter and
/// color come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub url: String,
    pub label: String,
    pub site: String,
}

impl Tile {
    /// A tile for a web address (`None` for anything else), labelled with
    /// `title` or, without one, its site.
    pub fn new(url: &str, title: &str) -> Option<Tile> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return None;
        }
        let site = crate::history::site_of(url).to_string();
        let title = title.trim();
        let label = if title.is_empty() { site.clone() } else { title.chars().take(MAX_LABEL).collect() };
        Some(Tile { url: url.to_string(), label, site })
    }
}

/// The page's look: the colors and type of `ui/input.css`, light and dark.
/// The page has the toolbar's color, so the active tab, the toolbar and the
/// page above the fold read as one surface; the search box is a field set
/// into it, like the address field.
const CSS: &str = ":root { color-scheme: light dark; --page: #f6f7f8; --field: #e9ebee; --surface: #ffffff; \
  --ink: #1b1f24; --muted: #575f69; --line: #d6d9de; --hover: rgb(24 32 44 / 0.07); --accent: #3d4a5c; } \
  @media (prefers-color-scheme: dark) { :root { --page: #212428; --field: #191b1f; --surface: #2a2d32; \
  --ink: #e8eaed; --muted: #a4abb4; --line: #3a3e45; --hover: rgb(255 255 255 / 0.08); --accent: #a7b6c8; } } \
  html, body { margin: 0; min-height: 100%; } \
  body { background: var(--page); color: var(--ink); \
  font: 14px/1.5 \"Segoe UI Variable Text\", \"Segoe UI\", system-ui, sans-serif; } \
  main { box-sizing: border-box; max-width: 720px; margin: 0 auto; padding: 20vh 24px 48px; } \
  form { margin: 0 0 44px; } \
  input { box-sizing: border-box; width: 100%; height: 48px; font: inherit; font-size: 15px; color: inherit; \
  background: var(--field); border: 1px solid transparent; border-radius: 12px; padding: 0 18px; outline: none; \
  transition: background-color 100ms, border-color 100ms, box-shadow 100ms; } \
  input::placeholder { color: var(--muted); } \
  input:focus-visible { background: var(--surface); border-color: var(--accent); \
  box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 20%, transparent); } \
  section { margin: 0 0 28px; } \
  h2 { font-size: 13px; font-weight: 600; color: var(--muted); margin: 0 0 10px 10px; } \
  ul { list-style: none; margin: 0; padding: 0; display: grid; \
  grid-template-columns: repeat(auto-fill, minmax(104px, 1fr)); gap: 6px; } \
  .tile { display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 14px 8px 12px; \
  border-radius: 12px; color: inherit; text-decoration: none; transition: background-color 100ms; } \
  .tile:hover { background: var(--hover); } \
  .tile:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; } \
  .icon { width: 48px; height: 48px; border-radius: 14px; display: grid; place-items: center; color: #fff; \
  font-weight: 600; font-size: 18px; box-shadow: inset 0 0 0 1px rgb(0 0 0 / 0.06); } \
  .label { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }";

fn lit(s: &str) -> String {
    let mut out = String::new();
    push_json_str(&mut out, s);
    out
}

fn push_tiles(out: &mut String, tiles: &[Tile]) {
    out.push('[');
    for (i, t) in tiles.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        push_json_str(out, &t.url);
        out.push(',');
        push_json_str(out, &t.label);
        out.push(',');
        push_json_str(out, &t.site);
        out.push(']');
    }
    out.push(']');
}

/// The script that writes the page into a blank tab, or brings its lists
/// up to date when it is there already. `search` is the search engine's
/// address that the typed text is appended to. Run in any other document
/// it does nothing and returns `"skip"` (`"shown"` when it filled it).
///
/// What the box sends: an `http(s)://` address as it is; a word with a dot
/// and no spaces ("example.com/x") as an `https://` address; anything else
/// as a search.
pub fn script(search: &str, sites: &[Tile], marks: &[Tile]) -> String {
    let mut data = String::from("{\"search\":");
    push_json_str(&mut data, search);
    data.push_str(",\"sites\":");
    push_tiles(&mut data, sites);
    data.push_str(",\"marks\":");
    push_tiles(&mut data, marks);
    data.push('}');
    format!(
        "(() => {{ if (location.href !== 'about:blank' || window.top !== window || self.origin !== 'null') return 'skip'; \
         const data = {data}; const d = document; \
         const make = (tag, props) => Object.assign(d.createElement(tag), props); \
         let main = d.getElementById('xb-newtab'); \
         if (!main && (d.title || (d.head && d.head.childElementCount) || (d.body && d.body.childElementCount))) return 'skip'; \
         if (!main) {{ \
           const sheet = new CSSStyleSheet(); sheet.replaceSync({css}); d.adoptedStyleSheets = [sheet]; \
           d.documentElement.lang = 'tr'; \
           main = make('main', {{ id: 'xb-newtab' }}); \
           const form = make('form', {{ id: 'xb-search' }}); form.setAttribute('role', 'search'); \
           const input = make('input', {{ id: 'xb-q', type: 'search', placeholder: {label}, autocomplete: 'off', spellcheck: false }}); \
           input.setAttribute('aria-label', {label}); \
           form.append(input); \
           form.addEventListener('submit', (e) => {{ e.preventDefault(); const t = input.value.trim(); if (!t) return; \
             location.href = /^https?:\\/\\/\\S+$/i.test(t) ? t \
               : /^[^\\s\\/?#@]+\\.[a-z][a-z0-9-]*(:\\d+)?([\\/?#]\\S*)?$/i.test(t) ? 'https://' + t \
               : form.dataset.search + encodeURIComponent(t); }}); \
           main.append(form, make('section', {{ id: 'xb-sites' }}), make('section', {{ id: 'xb-marks' }})); \
           if (!d.body) d.documentElement.append(d.createElement('body')); \
           d.body.replaceChildren(main); \
         }} \
         d.getElementById('xb-search').dataset.search = data.search; \
         const list = (id, heading, tiles) => {{ const sec = d.getElementById(id); sec.hidden = !tiles.length; \
           const ul = make('ul', {{}}); \
           for (const [url, label, site] of tiles) {{ \
             const a = make('a', {{ href: url, title: label === site ? url : label + '\\n' + url, className: 'tile' }}); \
             let hue = 0; for (const c of site) hue = (hue * 31 + c.codePointAt(0)) % 80; \
             const icon = make('span', {{ className: 'icon', textContent: (site[0] || '?').toUpperCase() }}); \
             icon.style.background = 'oklch(0.58 0.14 ' + (205 + hue) + ')'; icon.setAttribute('aria-hidden', 'true'); \
             a.append(icon, make('span', {{ className: 'label', textContent: label }})); \
             const li = make('li', {{}}); li.append(a); ul.append(li); }} \
           sec.replaceChildren(...(tiles.length ? [make('h2', {{ textContent: heading }}), ul] : [])); }}; \
         list('xb-sites', {sites_h}, data.sites); list('xb-marks', {marks_h}, data.marks); \
         return 'shown'; }})()",
        css = lit(CSS),
        label = lit(SEARCH_LABEL),
        sites_h = lit(SITES_HEADING),
        marks_h = lit(MARKS_HEADING),
    )
}

/// The page's script with the app's current lists and search engine.
fn app_script(app: &App) -> String {
    let sites: Vec<Tile> =
        app.history.top_sites(TILES).iter().filter_map(|s| Tile::new(&s.u, &s.t)).collect();
    let marks: Vec<Tile> =
        app.bookmarks.items().iter().filter_map(|b| Tile::new(&b.url, &b.title)).take(TILES).collect();
    script(crate::settings::engine().search_url(), &sites, &marks)
}

fn is_blank(app: &App, id: TabId) -> bool {
    app.tabs.get(id).is_some_and(|t| t.new_tab_page && t.url == crate::url::BLANK)
}

/// Fills tab `id` with the page if it is blank (its load just completed).
pub fn fill(app: &App, id: TabId) {
    if !is_blank(app, id) {
        return;
    }
    if let Some(view) = app.view(id) {
        let _ = view.evaluate_script(&app_script(app));
    }
}

/// Brings every blank tab's page up to date (the history or bookmarks
/// finished loading, the search engine changed).
pub fn refresh(app: &App) {
    let mut js = None;
    for id in app.tabs.ids() {
        if !is_blank(app, id) {
            continue;
        }
        if let Some(view) = app.view(id) {
            let _ = view.evaluate_script(js.get_or_insert_with(|| app_script(app)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_addresses_make_tiles() {
        assert_eq!(
            Tile::new("https://www.example.com/a", "  Example  "),
            Some(Tile { url: "https://www.example.com/a".into(), label: "Example".into(), site: "example.com".into() })
        );
        assert_eq!(Tile::new("http://h:8080/", "").map(|t| t.label), Some("h:8080".into()));
        assert_eq!(Tile::new("https://x.com/", &"a".repeat(500)).map(|t| t.label.chars().count()), Some(MAX_LABEL));
        for bad in ["about:blank", "file:///C:/x", "chrome-extension://abc/p.html", "javascript:alert(1)", "xb://ui/"] {
            assert_eq!(Tile::new(bad, "t"), None, "{bad}");
        }
    }

    #[test]
    fn the_script_fills_only_the_apps_own_blank_page_and_escapes_its_data() {
        let evil = Tile { url: "https://e.com/</script>\"".into(), label: "<img onerror=x>\u{2028}".into(), site: "e.com".into() };
        let js = script("https://duckduckgo.com/?q=", &[evil], &[]);
        assert!(js.starts_with(
            "(() => { if (location.href !== 'about:blank' || window.top !== window || self.origin !== 'null') return 'skip';"
        ));
        // A blank document that already holds a page is not filled.
        assert!(js.contains("if (!main && (d.title || (d.head && d.head.childElementCount) || (d.body && d.body.childElementCount))) return 'skip';"));
        assert!(!js.contains("</script>") && !js.contains("<img") && !js.contains('\u{2028}'));
        assert!(js.contains(r#""search":"https://duckduckgo.com/?q=","sites":[["https://e.com/\u003c/script>\"","\u003cimg onerror=x>\u2028","e.com"]],"marks":[]"#));
        // Text goes in as text, never as markup.
        assert!(!js.contains("innerHTML") && !js.contains("insertAdjacentHTML"));
    }
}
