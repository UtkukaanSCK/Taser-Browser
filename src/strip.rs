//! Tab strip features beyond open/close/switch: reorder (drag and keys),
//! reopen closed tabs, jump to a tab by position, mute.
//!
//! Both vocabularies here are closed sets nested into the ones in
//! `commands`: `StripCmd` comes from the trusted UI (bounds-checked numbers
//! only), `TabKey` from a native key event (no data at all).

use crate::app::App;
use crate::commands::{Mods, vk};
use crate::native;
use crate::tabs::{Placement, TabId, Tabs};
use crate::url::is_tab_url;
use std::collections::VecDeque;

/// How many closed tabs Ctrl+Shift+T can bring back.
pub const MAX_CLOSED: usize = 25;

/// UI commands of the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripCmd {
    /// `move:<id>:<index>`: the user dropped tab `id` at strip position `to`.
    Move { id: TabId, to: usize },
    /// `mute:<id>`: the speaker button of a tab.
    ToggleMute(TabId),
}

impl StripCmd {
    /// `name` and `arg` of a `name:arg` message. Numbers only: the index is
    /// at most a `u16` here and `Tabs::move_to` refuses one past the end.
    pub fn parse(name: &str, arg: &str) -> Option<Self> {
        match name {
            "move" => {
                let (id, to) = arg.split_once(':')?;
                Some(Self::Move { id: id.parse().ok()?, to: to.parse::<u16>().ok()?.into() })
            }
            "mute" => Some(Self::ToggleMute(arg.parse().ok()?)),
            _ => None,
        }
    }
}

/// Keyboard shortcuts of the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKey {
    /// Ctrl+Shift+T.
    Reopen,
    /// Ctrl+1..8: the tab at that position (1-based), if there is one.
    Select(u8),
    /// Ctrl+9: the last tab, however many there are.
    Last,
    /// Ctrl+Shift+PgUp / PgDn: move the active tab one place.
    MoveLeft,
    MoveRight,
}

/// What a `TabKey` does to the current strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Activate(TabId),
    Move { id: TabId, to: usize },
    Reopen,
}

impl TabKey {
    /// Ctrl without Alt only, so AltGr (Ctrl+Alt) never matches: typing `{`
    /// or `~` with AltGr+digit on many layouts stays typing.
    pub fn from_key(key: u32, m: Mods) -> Option<Self> {
        if !m.ctrl || m.alt {
            return None;
        }
        Some(match key {
            vk::T if m.shift => Self::Reopen,
            vk::PRIOR if m.shift => Self::MoveLeft,
            vk::NEXT if m.shift => Self::MoveRight,
            _ if m.shift => return None,
            // Top-row digits and the numpad (NumLock on), as in Chrome.
            0x31..=0x38 => Self::Select((key - 0x30) as u8),
            0x61..=0x68 => Self::Select((key - 0x60) as u8),
            0x39 | 0x69 => Self::Last,
            _ => return None,
        })
    }

    /// Moving with a held key repeats; jumping and reopening do not (a held
    /// Ctrl+Shift+T would reopen the whole stack).
    pub fn repeats(self) -> bool {
        matches!(self, Self::MoveLeft | Self::MoveRight)
    }

    /// The action on this strip, or None when there is nothing to do
    /// (Ctrl+5 with three tabs, moving the first tab left, no tabs).
    pub fn action(self, tabs: &Tabs) -> Option<KeyAction> {
        let active = tabs.active()?;
        let at = tabs.index_of(active)?;
        Some(match self {
            Self::Reopen => KeyAction::Reopen,
            Self::Select(n) => KeyAction::Activate(tabs.nth(usize::from(n).checked_sub(1)?)?),
            Self::Last => KeyAction::Activate(tabs.nth(tabs.len().checked_sub(1)?)?),
            Self::MoveLeft => KeyAction::Move { id: active, to: at.checked_sub(1)? },
            Self::MoveRight => {
                tabs.nth(at + 1)?;
                KeyAction::Move { id: active, to: at + 1 }
            }
        })
    }
}

/// Recently closed tabs, newest last: their address and strip position.
#[derive(Debug, Default)]
pub struct ClosedTabs(VecDeque<(String, usize)>);

impl ClosedTabs {
    /// Remembers a closed tab. Only http(s) pages: a blank tab has nothing to
    /// bring back, and nothing else is ever loaded in a content tab. Never
    /// the UI's own origin, and never a URL over `MAX_URL_LEN` (refused, not
    /// cut: a cut URL would reopen some other address).
    pub fn push(&mut self, url: &str, index: usize) {
        if !is_tab_url(url) || crate::ui::is_ui_url(url) {
            return;
        }
        if self.0.len() == MAX_CLOSED {
            self.0.pop_front();
        }
        self.0.push_back((url.to_string(), index));
    }

    /// Remembers tab `id` of `tabs`, which is about to close.
    pub fn remember(&mut self, tabs: &Tabs, id: TabId) {
        if let (Some(t), Some(i)) = (tabs.get(id), tabs.index_of(id)) {
            self.push(&t.url, i);
        }
    }

    pub fn pop(&mut self) -> Option<(String, usize)> {
        self.0.pop_back()
    }

    /// Forgets every closed tab (history deleted or browsing data cleared).
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Forgets the closed tabs at `url` (compared as history stores it:
    /// without `#fragment` or credentials), as that address left history.
    pub fn forget(&mut self, url: &str) {
        let url = crate::history::normalize(url);
        self.0.retain(|(u, _)| crate::history::normalize(u) != url);
    }

    #[cfg(any(test, feature = "selftest"))]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The address Ctrl+Shift+T would reopen next.
    #[cfg(feature = "selftest")]
    pub fn newest(&self) -> Option<&str> {
        self.0.back().map(|(url, _)| url.as_str())
    }
}

/// `UiCommand::Strip`.
pub fn on_command(app: &mut App, cmd: StripCmd) {
    match cmd {
        StripCmd::Move { id, to } => {
            app.tabs.move_to(id, to);
            // Always answered with a render, refused moves too: after a drop
            // the UI keeps the dragged order until this state arrives.
            app.mark_dirty();
        }
        StripCmd::ToggleMute(id) => {
            let Some(muted) = app.tabs.get(id).map(|t| !t.muted) else { return };
            if app.view(id).is_some_and(|v| native::set_muted(v, muted)) {
                // IsMutedChanged confirms it too; showing it now saves a frame.
                if let Some(t) = app.tabs.get_mut(id) {
                    t.muted = muted;
                }
                app.mark_dirty();
            }
        }
    }
}

/// `Shortcut::Tab`.
pub fn on_key(app: &mut App, key: TabKey) {
    match key.action(&app.tabs) {
        Some(KeyAction::Activate(id)) => app.activate(id),
        Some(KeyAction::Move { id, to }) => on_command(app, StripCmd::Move { id, to }),
        Some(KeyAction::Reopen) => {
            let Some((url, at)) = app.closed.pop() else { return };
            match app.open_tab(&url, Placement::At(at), true) {
                Ok(id) => {
                    if let Some(v) = app.view(id) {
                        let _ = v.focus();
                    }
                }
                // Keep it for the next try rather than losing it.
                Err(_) => app.closed.push(&url, at),
            }
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false };
    const ALTGR: Mods = Mods { ctrl: true, shift: false, alt: true };
    const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };

    #[test]
    fn parses_strip_commands() {
        assert_eq!(StripCmd::parse("move", "3:0"), Some(StripCmd::Move { id: 3, to: 0 }));
        assert_eq!(StripCmd::parse("move", "3:65535"), Some(StripCmd::Move { id: 3, to: 65535 }));
        assert_eq!(StripCmd::parse("mute", "12"), Some(StripCmd::ToggleMute(12)));
        for (name, arg) in [
            ("move", ""), ("move", "3"), ("move", "3:"), ("move", ":1"), ("move", "3:-1"),
            ("move", "3:65536"), ("move", "3:1:2"), ("move", "x:1"), ("move", "3:1.0"),
            ("mute", ""), ("mute", "-1"), ("mute", "1:2"), ("mute", "a"), ("other", "1"),
        ] {
            assert_eq!(StripCmd::parse(name, arg), None, "{name}:{arg}");
        }
    }

    #[test]
    fn maps_ctrl_digits_and_strip_keys() {
        for d in 1..=8u8 {
            assert_eq!(TabKey::from_key(0x30 + u32::from(d), CTRL), Some(TabKey::Select(d)));
            assert_eq!(TabKey::from_key(0x60 + u32::from(d), CTRL), Some(TabKey::Select(d)), "numpad {d}");
        }
        assert_eq!(TabKey::from_key(0x39, CTRL), Some(TabKey::Last));
        assert_eq!(TabKey::from_key(0x69, CTRL), Some(TabKey::Last));
        assert_eq!(TabKey::from_key(0x30, CTRL), None, "Ctrl+0 is zoom reset");
        assert_eq!(TabKey::from_key(vk::T, CTRL_SHIFT), Some(TabKey::Reopen));
        assert_eq!(TabKey::from_key(vk::PRIOR, CTRL_SHIFT), Some(TabKey::MoveLeft));
        assert_eq!(TabKey::from_key(vk::NEXT, CTRL_SHIFT), Some(TabKey::MoveRight));
        assert_eq!(TabKey::from_key(vk::T, CTRL), None, "Ctrl+T is the new-tab shortcut");
        assert_eq!(TabKey::from_key(0x31, CTRL_SHIFT), None, "Ctrl+Shift+1");
        assert_eq!(TabKey::from_key(0x31, SHIFT), None, "Shift+1 types !");
        assert_eq!(TabKey::from_key(0x31, Mods::default()), None);
        assert_eq!(TabKey::from_key(vk::PRIOR, CTRL), None, "Ctrl+PgUp is tab cycling");
    }

    #[test]
    fn altgr_never_matches() {
        for key in 0..=0xFF_u32 {
            for shift in [false, true] {
                assert_eq!(TabKey::from_key(key, Mods { shift, ..ALTGR }), None, "vk {key:#x}");
            }
        }
    }

    #[test]
    fn only_moves_repeat() {
        assert!(TabKey::MoveLeft.repeats() && TabKey::MoveRight.repeats());
        assert!(!TabKey::Reopen.repeats() && !TabKey::Select(1).repeats() && !TabKey::Last.repeats());
    }

    #[test]
    fn key_actions_follow_positions() {
        let mut t = Tabs::new();
        assert_eq!(TabKey::Select(1).action(&t), None, "no tabs");
        let ids: Vec<TabId> = (0..10).map(|i| t.add(&i.to_string(), Placement::End)).collect();
        assert_eq!(TabKey::Select(1).action(&t), Some(KeyAction::Activate(ids[0])));
        assert_eq!(TabKey::Select(8).action(&t), Some(KeyAction::Activate(ids[7])));
        assert_eq!(TabKey::Last.action(&t), Some(KeyAction::Activate(ids[9])), "Ctrl+9 is the 10th of 10");
        assert_eq!(TabKey::Select(0).action(&t), None);
        assert_eq!(TabKey::MoveLeft.action(&t), None, "first tab cannot move left");
        assert_eq!(TabKey::MoveRight.action(&t), Some(KeyAction::Move { id: ids[0], to: 1 }));
        t.activate(ids[9]);
        assert_eq!(TabKey::MoveRight.action(&t), None, "last tab cannot move right");
        assert_eq!(TabKey::MoveLeft.action(&t), Some(KeyAction::Move { id: ids[9], to: 8 }));
        assert_eq!(TabKey::Reopen.action(&t), Some(KeyAction::Reopen));
        for id in &ids[3..] {
            t.close(*id);
        }
        assert_eq!(TabKey::Select(5).action(&t), None, "Ctrl+5 with three tabs");
        assert_eq!(TabKey::Last.action(&t), Some(KeyAction::Activate(ids[2])));
    }

    #[test]
    fn closed_stack_is_lifo_capped_and_web_only() {
        let mut c = ClosedTabs::default();
        assert_eq!(c.pop(), None);
        c.push(crate::url::BLANK, 0);
        c.push("javascript:alert(1)", 0);
        c.push("file:///C:/x", 0);
        assert_eq!(c.len(), 0, "only http(s) pages are remembered");
        for i in 0..MAX_CLOSED + 5 {
            c.push(&format!("https://e.test/{i}"), i);
        }
        assert_eq!(c.len(), MAX_CLOSED);
        let last = MAX_CLOSED + 4;
        assert_eq!(c.pop(), Some((format!("https://e.test/{last}"), last)), "newest first");
        let mut n = 1;
        let mut oldest = None;
        while let Some(e) = c.pop() {
            n += 1;
            oldest = Some(e);
        }
        assert_eq!(n, MAX_CLOSED);
        assert_eq!(oldest, Some(("https://e.test/5".to_string(), 5)), "the five oldest fell off");
    }

    #[test]
    fn closed_stack_refuses_ui_and_over_long_urls() {
        use crate::url::MAX_URL_LEN;
        let mut c = ClosedTabs::default();
        c.push("https://xb.ui/", 0);
        c.push("https://xb.ui/js/core.js", 0);
        c.push("http://XB.UI./x", 0);
        assert_eq!(c.len(), 0, "never the UI origin");
        let at_cap = format!("https://e.test/{}", "a".repeat(MAX_URL_LEN - 15));
        assert_eq!(at_cap.len(), MAX_URL_LEN);
        c.push(&format!("{at_cap}a"), 1);
        assert_eq!(c.len(), 0, "one byte over the cap is refused");
        c.push(&crate::url::cap_url(&format!("{at_cap}{}", "b".repeat(50_000))), 1);
        assert_eq!(c.len(), 0, "a URL cut for display is refused too");
        c.push(&at_cap, 2);
        assert_eq!(c.pop(), Some((at_cap, 2)), "exactly at the cap, kept whole");
    }

    #[test]
    fn closed_stack_forgets_a_deleted_address() {
        let mut c = ClosedTabs::default();
        c.push("https://a.test/page#part", 0);
        c.push("https://b.test/", 1);
        c.push("https://a.test/page", 2);
        c.forget("https://a.test/page");
        assert_eq!(c.len(), 1, "every tab at that address, fragment or not");
        assert_eq!(c.pop(), Some(("https://b.test/".to_string(), 1)));
    }
}
