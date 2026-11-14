//! Tab list state. Rust is the only owner; the UI just renders `to_json()`.

use crate::url::display_url;

pub type TabId = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub id: TabId,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
    /// The tab whose page opened this one (for placing later popups).
    pub opener: Option<TabId>,
    /// The page's favicon as a validated `data:image/png;base64,` URL.
    pub favicon: Option<String>,
    /// `favicon` changed since the UI last got it (see `render_json`).
    icon_dirty: bool,
    /// The page is playing audio (shown even while muted).
    pub audible: bool,
    pub muted: bool,
    /// Page zoom in whole percent (100 = none), as WebView2 last reported it.
    pub zoom: u16,
}

#[derive(Debug, Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: TabId,
}

/// Where a new tab goes in the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Ctrl+T / "+" button: at the end.
    End,
    /// Opened from a page: after its opener and the tabs it already opened,
    /// so popups A then B line up as opener, A, B (as in Chrome).
    After(TabId),
    /// At this index (clamped to the end): a reopened tab goes back where it was.
    At(usize),
}

/// Window caption for the active tab: `<title> - X-Browser`, or just the app
/// name for a blank tab or one without a title yet.
pub fn window_title(tab: Option<&Tab>) -> String {
    const APP: &str = "X-Browser";
    match tab {
        Some(t) if t.url != crate::url::BLANK && !t.title.trim().is_empty() => format!("{} - {APP}", t.title.trim()),
        _ => APP.to_string(),
    }
}

impl Tabs {
    pub fn new() -> Self {
        Self { tabs: Vec::new(), active: None, next_id: 1 }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active(&self) -> Option<TabId> {
        self.active
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    #[cfg(any(test, feature = "selftest"))]
    pub fn ids(&self) -> impl Iterator<Item = TabId> + '_ {
        self.tabs.iter().map(|t| t.id)
    }

    /// Position of tab `id` in the strip.
    pub fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// The tab at strip position `i`.
    pub fn nth(&self, i: usize) -> Option<TabId> {
        self.tabs.get(i).map(|t| t.id)
    }

    /// Adds a tab and returns its id. Does not activate it.
    pub fn add(&mut self, url: &str, placement: Placement) -> TabId {
        let id = self.next_id;
        self.next_id += 1;
        let tab = Tab {
            id,
            url: url.to_string(),
            title: String::new(),
            loading: false,
            can_back: false,
            can_forward: false,
            opener: match placement {
                Placement::After(opener) => Some(opener),
                Placement::End | Placement::At(_) => None,
            },
            favicon: None,
            icon_dirty: false,
            audible: false,
            muted: false,
            zoom: 100,
        };
        let at = match placement {
            Placement::End => self.tabs.len(),
            Placement::At(i) => i.min(self.tabs.len()),
            Placement::After(opener) => match self.index_of(opener) {
                // After the last tab this opener already opened, else right after it.
                Some(i) => {
                    let last_sibling = self.tabs[i + 1..].iter().rposition(|t| t.opener == Some(opener));
                    i + 1 + last_sibling.map_or(0, |j| j + 1)
                }
                None => self.tabs.len(),
            },
        };
        self.tabs.insert(at, tab);
        if self.active.is_none() {
            self.active = Some(id);
        }
        id
    }

    /// Makes `id` the active tab. Returns false if no such tab.
    pub fn activate(&mut self, id: TabId) -> bool {
        if self.index_of(id).is_some() {
            self.active = Some(id);
            true
        } else {
            false
        }
    }

    /// Removes a tab. If it was active, the tab to its right (or else left)
    /// becomes active. Returns false if no such tab.
    pub fn close(&mut self, id: TabId) -> bool {
        let Some(i) = self.index_of(id) else { return false };
        self.tabs.remove(i);
        if self.active == Some(id) {
            self.active = self.tabs.get(i).or_else(|| self.tabs.last()).map(|t| t.id);
        }
        true
    }

    /// Moves tab `id` to strip position `to` (the others close up around it).
    /// False, and nothing changes, for an unknown tab, a position past the
    /// end, or the position it already has. A moved tab forgets its opener:
    /// the user placed it, so later popups no longer line up after it.
    pub fn move_to(&mut self, id: TabId, to: usize) -> bool {
        let Some(from) = self.index_of(id) else { return false };
        if to >= self.tabs.len() || to == from {
            return false;
        }
        let mut tab = self.tabs.remove(from);
        tab.opener = None;
        self.tabs.insert(to, tab);
        true
    }

    /// Sets a tab's favicon. True if it changed (the UI then gets it once).
    pub fn set_favicon(&mut self, id: TabId, icon: Option<String>) -> bool {
        let Some(t) = self.get_mut(id) else { return false };
        if t.favicon == icon {
            return false;
        }
        t.favicon = icon;
        t.icon_dirty = true;
        true
    }

    /// The UI lost its icons (it was reloaded): send every one again.
    pub fn resend_icons(&mut self) {
        for t in &mut self.tabs {
            t.icon_dirty = true;
        }
    }

    /// The UI has the icons from the last `render_json`.
    pub fn icons_sent(&mut self) {
        for t in &mut self.tabs {
            t.icon_dirty = false;
        }
    }

    /// The tab `step` positions away from the active one, wrapping around.
    pub fn cycle(&self, step: isize) -> Option<TabId> {
        let i = self.index_of(self.active?)? as isize;
        let n = self.tabs.len() as isize;
        Some(self.tabs[(i + step).rem_euclid(n) as usize].id)
    }

    /// Serializes the state the UI needs, without favicons (logs, tests).
    #[cfg(any(test, feature = "selftest"))]
    pub fn to_json(&self) -> String {
        self.json(false)
    }

    /// `to_json` plus `"icon"` (a data URL or null) for each tab whose favicon
    /// changed since `icons_sent`. Icons are up to ~43 KB each, so they are
    /// not resent on every render; the UI keeps the last one per tab.
    pub fn render_json(&self) -> String {
        self.json(true)
    }

    fn json(&self, icons: bool) -> String {
        let mut s = String::with_capacity(64 + self.tabs.len() * 180);
        s.push_str("{\"active\":");
        s.push_str(&self.active.unwrap_or(0).to_string());
        s.push_str(",\"tabs\":[");
        for (i, t) in self.tabs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str("{\"id\":");
            s.push_str(&t.id.to_string());
            s.push_str(",\"url\":");
            push_json_str(&mut s, display_url(&t.url));
            s.push_str(",\"title\":");
            push_json_str(&mut s, &t.title);
            s.push_str(",\"loading\":");
            s.push_str(bool_str(t.loading));
            s.push_str(",\"back\":");
            s.push_str(bool_str(t.can_back));
            s.push_str(",\"fwd\":");
            s.push_str(bool_str(t.can_forward));
            s.push_str(",\"audio\":");
            s.push_str(bool_str(t.audible));
            s.push_str(",\"muted\":");
            s.push_str(bool_str(t.muted));
            s.push_str(",\"zoom\":");
            s.push_str(&t.zoom.to_string());
            if icons && t.icon_dirty {
                s.push_str(",\"icon\":");
                match &t.favicon {
                    Some(icon) => push_json_str(&mut s, icon),
                    None => s.push_str("null"),
                }
            }
            s.push('}');
        }
        s.push_str("]}");
        s
    }
}

fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// Appends `v` as a JSON string literal. Escapes everything that could break
/// out of a `<script>`-less `evaluate_script` call too (U+2028/2029, `<`).
pub fn push_json_str(out: &mut String, v: &str) {
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(t: &Tabs) -> Vec<TabId> {
        t.ids().collect()
    }

    #[test]
    fn first_tab_becomes_active() {
        let mut t = Tabs::new();
        assert_eq!(t.active(), None);
        let a = t.add("about:blank", Placement::End);
        assert_eq!(t.active(), Some(a));
        let b = t.add("about:blank", Placement::End);
        assert_eq!(t.active(), Some(a), "adding does not steal focus");
        assert!(t.activate(b));
        assert_eq!(t.active(), Some(b));
        assert!(!t.activate(999));
    }

    #[test]
    fn ids_are_unique_even_after_close() {
        let mut t = Tabs::new();
        let a = t.add("x", Placement::End);
        t.close(a);
        let b = t.add("x", Placement::End);
        assert_ne!(a, b);
    }

    #[test]
    fn placement_after_opener() {
        let mut t = Tabs::new();
        let a = t.add("a", Placement::End);
        let b = t.add("b", Placement::End);
        let c = t.add("c", Placement::After(a));
        assert_eq!(order(&t), vec![a, c, b]);
        let d = t.add("d", Placement::After(999));
        assert_eq!(order(&t), vec![a, c, b, d], "unknown opener appends");
    }

    #[test]
    fn popups_from_one_opener_keep_their_order() {
        let mut t = Tabs::new();
        let o = t.add("o", Placement::End);
        let x = t.add("x", Placement::End);
        let a = t.add("a", Placement::After(o));
        let b = t.add("b", Placement::After(o));
        assert_eq!(order(&t), vec![o, a, b, x], "A then B, not B then A");
        let c = t.add("c", Placement::After(o));
        assert_eq!(order(&t), vec![o, a, b, c, x]);
        // A popup of a popup goes right after its own opener.
        let a1 = t.add("a1", Placement::After(a));
        assert_eq!(order(&t), vec![o, a, a1, b, c, x]);
        // The opener's next popup still goes after its last one.
        let d = t.add("d", Placement::After(o));
        assert_eq!(order(&t), vec![o, a, a1, b, c, d, x]);
        // Closing the opener's popups: placement falls back to right after it.
        for id in [a, a1, b, c, d] {
            t.close(id);
        }
        let e = t.add("e", Placement::After(o));
        assert_eq!(order(&t), vec![o, e, x]);
        assert_eq!(t.get(e).unwrap().opener, Some(o));
        assert_eq!(t.get(x).unwrap().opener, None);
    }

    #[test]
    fn window_title_follows_active_tab() {
        let mut t = Tabs::new();
        let a = t.add(crate::url::BLANK, Placement::End);
        assert_eq!(window_title(None), "X-Browser");
        assert_eq!(window_title(t.get(a)), "X-Browser", "blank tab");
        t.get_mut(a).unwrap().title = "about:blank".into();
        assert_eq!(window_title(t.get(a)), "X-Browser", "blank tab with WebView2's URL title");
        t.get_mut(a).unwrap().url = "https://example.com/".into();
        t.get_mut(a).unwrap().title = "  ".into();
        assert_eq!(window_title(t.get(a)), "X-Browser", "no title yet");
        t.get_mut(a).unwrap().title = " Example Domain ".into();
        assert_eq!(window_title(t.get(a)), "Example Domain - X-Browser");
    }

    #[test]
    fn closing_active_selects_right_then_left() {
        let mut t = Tabs::new();
        let a = t.add("a", Placement::End);
        let b = t.add("b", Placement::End);
        let c = t.add("c", Placement::End);
        t.activate(b);
        assert!(t.close(b));
        assert_eq!(t.active(), Some(c), "right neighbour");
        assert!(t.close(c));
        assert_eq!(t.active(), Some(a), "left neighbour when closing last");
        assert!(t.close(a));
        assert_eq!(t.active(), None);
        assert!(t.is_empty());
        assert!(!t.close(a), "closing twice is a no-op");
    }

    #[test]
    fn closing_inactive_keeps_active() {
        let mut t = Tabs::new();
        let a = t.add("a", Placement::End);
        let b = t.add("b", Placement::End);
        t.close(b);
        assert_eq!(t.active(), Some(a));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn cycle_wraps_both_ways() {
        let mut t = Tabs::new();
        assert_eq!(t.cycle(1), None);
        let a = t.add("a", Placement::End);
        assert_eq!(t.cycle(1), Some(a));
        let b = t.add("b", Placement::End);
        let c = t.add("c", Placement::End);
        assert_eq!(t.cycle(1), Some(b));
        assert_eq!(t.cycle(-1), Some(c));
        t.activate(c);
        assert_eq!(t.cycle(1), Some(a));
    }

    #[test]
    fn json_shape_and_escaping() {
        let mut t = Tabs::new();
        let a = t.add("about:blank", Placement::End);
        {
            let tab = t.get_mut(a).unwrap();
            tab.title = "He said \"hi\" \\ </script>\u{2028}\u{1}".into();
            tab.loading = true;
            tab.can_back = true;
        }
        // `<`, U+2028 and control chars must come out as backslash-u escapes
        // (spelled with format! so no editor ever turns them into raw chars).
        let u = |hex: &str| format!("{}u{hex}", '\\');
        let title = format!(r#"He said \"hi\" \\ {}/script>{}{}"#, u("003c"), u("2028"), u("0001"));
        let expected = format!(
            r#"{{"active":1,"tabs":[{{"id":1,"url":"","title":"{title}","loading":true,"back":true,"fwd":false,"audio":false,"muted":false,"zoom":100}}]}}"#
        );
        let json = t.to_json();
        assert_eq!(json, expected);
        assert!(!json.contains('<') && !json.contains('\u{2028}'));
        assert_eq!(Tabs::new().to_json(), r#"{"active":0,"tabs":[]}"#);
    }

    #[test]
    fn move_to_reorders_and_rejects_bad_targets() {
        let mut t = Tabs::new();
        let a = t.add("a", Placement::End);
        let b = t.add("b", Placement::End);
        let c = t.add("c", Placement::After(a));
        assert_eq!(order(&t), vec![a, c, b]);
        assert!(t.move_to(a, 2));
        assert_eq!(order(&t), vec![c, b, a], "to the end");
        assert!(t.move_to(a, 0));
        assert_eq!(order(&t), vec![a, c, b], "back to the front");
        assert!(t.move_to(c, 2));
        assert_eq!(order(&t), vec![a, b, c]);
        assert_eq!(t.get(c).unwrap().opener, None, "a moved tab forgets its opener");
        assert!(!t.move_to(b, 1), "already there");
        assert!(!t.move_to(b, 3), "past the end");
        assert!(!t.move_to(b, usize::MAX));
        assert!(!t.move_to(999, 0), "unknown tab");
        assert_eq!(order(&t), vec![a, b, c], "failed moves change nothing");
        assert_eq!(t.active(), Some(a), "moving never changes the active tab");
        assert_eq!((t.index_of(c), t.nth(2), t.nth(3)), (Some(2), Some(c), None));
    }

    #[test]
    fn placement_at_clamps_to_the_end() {
        let mut t = Tabs::new();
        let a = t.add("a", Placement::End);
        let b = t.add("b", Placement::At(0));
        let c = t.add("c", Placement::At(1));
        let d = t.add("d", Placement::At(99));
        assert_eq!(order(&t), vec![b, c, a, d]);
        assert_eq!(t.get(c).unwrap().opener, None);
    }

    #[test]
    fn icons_are_sent_once_and_again_after_a_ui_reload() {
        let mut t = Tabs::new();
        let a = t.add("a", Placement::End);
        let b = t.add("b", Placement::End);
        let icon = "data:image/png;base64,AAAA".to_string();
        assert!(!t.render_json().contains("icon"), "no icon yet, none sent");
        assert!(t.set_favicon(a, Some(icon.clone())));
        assert!(!t.set_favicon(a, Some(icon.clone())), "same icon is no change");
        assert!(!t.set_favicon(999, None));
        let j = t.render_json();
        assert!(j.contains(r#""icon":"data:image/png;base64,AAAA""#), "{j}");
        assert!(!t.to_json().contains("icon"), "to_json never carries icons");
        t.icons_sent();
        assert!(!t.render_json().contains("icon"), "sent once");
        assert!(t.set_favicon(a, None));
        assert!(t.render_json().contains(r#""id":1,"#) && t.render_json().contains(r#""icon":null"#), "cleared icon is sent as null");
        t.icons_sent();
        t.set_favicon(b, Some(icon.clone()));
        t.icons_sent();
        t.resend_icons();
        let j = t.render_json();
        assert_eq!(j.matches(r#""icon":"#).count(), 2, "UI reload resends every tab's icon: {j}");
    }
}
