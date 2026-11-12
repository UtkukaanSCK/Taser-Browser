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
    /// Opened from a page: right after its opener (after earlier siblings is
    /// not tracked; keeping it simple).
    After(TabId),
}

impl Tabs {
    pub fn new() -> Self {
        Self { tabs: Vec::new(), active: None, next_id: 1 }
    }

    #[cfg(any(test, feature = "selftest"))]
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

    fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
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
        };
        let at = match placement {
            Placement::End => self.tabs.len(),
            Placement::After(opener) => self.index_of(opener).map_or(self.tabs.len(), |i| i + 1),
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

    /// The tab `step` positions away from the active one, wrapping around.
    pub fn cycle(&self, step: isize) -> Option<TabId> {
        let i = self.index_of(self.active?)? as isize;
        let n = self.tabs.len() as isize;
        Some(self.tabs[(i + step).rem_euclid(n) as usize].id)
    }

    /// Serializes the state the UI needs.
    pub fn to_json(&self) -> String {
        let mut s = String::with_capacity(64 + self.tabs.len() * 160);
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
            r#"{{"active":1,"tabs":[{{"id":1,"url":"","title":"{title}","loading":true,"back":true,"fwd":false}}]}}"#
        );
        let json = t.to_json();
        assert_eq!(json, expected);
        assert!(!json.contains('<') && !json.contains('\u{2028}'));
        assert_eq!(Tabs::new().to_json(), r#"{"active":0,"tabs":[]}"#);
    }
}
