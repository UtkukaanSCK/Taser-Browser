//! The two command vocabularies, both closed sets:
//! - `UiCommand`: what the trusted UI webview may ask for over IPC.
//! - `Shortcut`: what a key press may trigger. It is computed in Rust from a
//!   native key event, so content pages cannot inject one and none carries
//!   data (no URL, path or text). The one path IPC carries is `st-folder`
//!   (settings page), checked in Rust: absolute, no UNC.

use crate::bookmarks::{BookmarkCmd, BookmarkKey};
use crate::ext_ui::ExtCmd;
use crate::menu::MenuCmd;
use crate::overlay::OverlayCmd;
use crate::page_tools::{PageCmd, PageKey};
use crate::policy::LaunchCmd;
use crate::settings::SettingsCmd;
use crate::strip::{StripCmd, TabKey};
use crate::tabs::TabId;

/// Longest address bar input accepted over IPC.
pub const MAX_NAV_INPUT: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Ready,
    NewTab,
    CloseTab(TabId),
    Activate(TabId),
    Navigate(String),
    Back,
    Forward,
    Reload,
    Stop,
    /// The strip's home button.
    Home,
    Minimize,
    ToggleMaximize,
    CloseWindow,
    DragWindow,
    ResizeTop(TopEdge),
    /// The address bar lost focus; return focus to the page (Esc).
    FocusContent,
    /// Reorder and mute (see `strip`).
    Strip(StripCmd),
    /// Zoom reset (see `page_tools`).
    Page(PageCmd),
    /// Suggestions and the history list (see `overlay`).
    Overlay(OverlayCmd),
    /// The star, the editor and the list (see `bookmarks`).
    Bookmark(BookmarkCmd),
    /// The main menu (see `menu`).
    Menu(MenuCmd),
    /// The settings page (see `settings`).
    Settings(SettingsCmd),
    /// The answer to "open another application?" (see `policy`).
    Launch(LaunchCmd),
    /// The Web Store button and question (see `webstore`).
    Store(crate::webstore::StoreCmd),
    /// The extensions page and the strip's extensions button (see `ext_ui`).
    Ext(ExtCmd),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopEdge {
    Left,
    Middle,
    Right,
}

impl UiCommand {
    /// Parses `name` or `name:arg`. Anything unknown or malformed is rejected.
    pub fn parse(msg: &str) -> Option<Self> {
        let (name, arg) = match msg.split_once(':') {
            Some((n, a)) => (n, Some(a)),
            None => (msg, None),
        };
        let id = || arg?.parse::<TabId>().ok();
        let cmd = match (name, arg) {
            ("ready", None) => Self::Ready,
            ("new", None) => Self::NewTab,
            ("close", Some(_)) => Self::CloseTab(id()?),
            ("activate", Some(_)) => Self::Activate(id()?),
            ("navigate", Some(text)) if text.len() <= MAX_NAV_INPUT => Self::Navigate(text.to_string()),
            ("back", None) => Self::Back,
            ("forward", None) => Self::Forward,
            ("reload", None) => Self::Reload,
            ("stop", None) => Self::Stop,
            ("home", None) => Self::Home,
            ("min", None) => Self::Minimize,
            ("max", None) => Self::ToggleMaximize,
            ("quit", None) => Self::CloseWindow,
            ("drag", None) => Self::DragWindow,
            ("resize", Some("n")) => Self::ResizeTop(TopEdge::Middle),
            ("resize", Some("nw")) => Self::ResizeTop(TopEdge::Left),
            ("resize", Some("ne")) => Self::ResizeTop(TopEdge::Right),
            ("focus-content", None) => Self::FocusContent,
            ("move" | "mute", Some(a)) => Self::Strip(StripCmd::parse(name, a)?),
            ("zoom-reset", Some(a)) => Self::Page(PageCmd::parse(name, a)?),
            (n, a) if OverlayCmd::NAMES.contains(&n) => Self::Overlay(OverlayCmd::parse(n, a)?),
            (n, a) if BookmarkCmd::NAMES.contains(&n) => Self::Bookmark(BookmarkCmd::parse(n, a)?),
            (n, a) if MenuCmd::NAMES.contains(&n) => Self::Menu(MenuCmd::parse(n, a)?),
            (n, a) if SettingsCmd::NAMES.contains(&n) => Self::Settings(SettingsCmd::parse(n, a)?),
            ("launch-open" | "launch-cancel" | "launch-press", None) => Self::Launch(LaunchCmd::parse(name)?),
            (n, None) if crate::webstore::StoreCmd::NAMES.contains(&n) => Self::Store(crate::webstore::StoreCmd::parse(n)?),
            (n, a) if ExtCmd::NAMES.contains(&n) => Self::Ext(ExtCmd::parse(n, a)?),
            _ => return None,
        };
        Some(cmd)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    NewTab,
    CloseTab,
    FocusAddress,
    Reload,
    NextTab,
    PrevTab,
    Back,
    Forward,
    /// Reopen, Ctrl+1..9 and keyboard reorder (see `strip`).
    Tab(TabKey),
    /// Zoom, F11 and Esc out of fullscreen (see `page_tools`).
    Page(PageKey),
    /// Ctrl+H: the history list (see `overlay`).
    History,
    /// Ctrl+D and Ctrl+Shift+O (see `bookmarks`).
    Bookmark(BookmarkKey),
    /// Alt+F / Alt+E: the main menu (see `menu`).
    Menu,
    /// Ctrl+J: WebView2's download flyout (see `downloads`).
    Downloads,
}

/// Windows virtual-key codes used below (identical values on every keyboard layout).
pub mod vk {
    pub const TAB: u32 = 0x09;
    pub const PRIOR: u32 = 0x21; // Page Up
    pub const NEXT: u32 = 0x22; // Page Down
    pub const LEFT: u32 = 0x25;
    pub const RIGHT: u32 = 0x27;
    pub const E: u32 = 0x45;
    pub const F: u32 = 0x46;
    pub const H: u32 = 0x48;
    pub const J: u32 = 0x4A;
    pub const L: u32 = 0x4C;
    pub const R: u32 = 0x52;
    pub const T: u32 = 0x54;
    pub const W: u32 = 0x57;
    pub const F5: u32 = 0x74;
    pub const BROWSER_BACK: u32 = 0xA6;
    pub const BROWSER_FORWARD: u32 = 0xA7;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Shortcut {
    /// `from_key_in` outside fullscreen, where Esc is always the page's.
    #[cfg(any(test, feature = "selftest"))]
    pub fn from_key(vk: u32, m: Mods) -> Option<Self> {
        Self::from_key_in(vk, m, false)
    }

    /// Maps a key-down to a shortcut. Ctrl+Alt (AltGr on e.g. Turkish layouts)
    /// never matches, so typing characters like `@` or `€` is unaffected.
    /// `esc_exits`: the window is in F11 fullscreen, where Esc leaves it
    /// (`page_tools::esc_exits_fullscreen`).
    pub fn from_key_in(vk: u32, m: Mods, esc_exits: bool) -> Option<Self> {
        use vk::*;
        let ctrl_only = m.ctrl && !m.alt;
        let alt_only = m.alt && !m.ctrl;
        let none = !m.ctrl && !m.alt;
        Some(match vk {
            T if ctrl_only && !m.shift => Self::NewTab,
            W if ctrl_only && !m.shift => Self::CloseTab,
            L if ctrl_only && !m.shift => Self::FocusAddress,
            H if ctrl_only && !m.shift => Self::History,
            J if ctrl_only && !m.shift => Self::Downloads,
            F | E if alt_only && !m.shift => Self::Menu,
            R if ctrl_only => Self::Reload,
            F5 if none || ctrl_only => Self::Reload,
            TAB if ctrl_only && m.shift => Self::PrevTab,
            TAB if ctrl_only => Self::NextTab,
            NEXT if ctrl_only && !m.shift => Self::NextTab,
            PRIOR if ctrl_only && !m.shift => Self::PrevTab,
            LEFT if alt_only && !m.shift => Self::Back,
            RIGHT if alt_only && !m.shift => Self::Forward,
            BROWSER_BACK if none => Self::Back,
            BROWSER_FORWARD if none => Self::Forward,
            _ => {
                return TabKey::from_key(vk, m)
                    .map(Self::Tab)
                    .or_else(|| PageKey::from_key(vk, m, esc_exits).map(Self::Page))
                    .or_else(|| BookmarkKey::from_key(vk, m).map(Self::Bookmark));
            }
        })
    }

    /// Whether holding the key down should repeat the action.
    pub fn repeats(self) -> bool {
        match self {
            Self::NextTab | Self::PrevTab => true,
            Self::Tab(k) => k.repeats(),
            Self::Page(k) => k.repeats(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_commands() {
        assert_eq!(UiCommand::parse("ready"), Some(UiCommand::Ready));
        assert_eq!(UiCommand::parse("new"), Some(UiCommand::NewTab));
        assert_eq!(UiCommand::parse("close:7"), Some(UiCommand::CloseTab(7)));
        assert_eq!(UiCommand::parse("activate:2"), Some(UiCommand::Activate(2)));
        assert_eq!(
            UiCommand::parse("navigate:a:b c"),
            Some(UiCommand::Navigate("a:b c".into())),
            "only the first colon splits"
        );
        assert_eq!(UiCommand::parse("navigate:"), Some(UiCommand::Navigate(String::new())));
        assert_eq!(UiCommand::parse("resize:nw"), Some(UiCommand::ResizeTop(TopEdge::Left)));
        assert_eq!(UiCommand::parse("quit"), Some(UiCommand::CloseWindow));
        assert_eq!(UiCommand::parse("home"), Some(UiCommand::Home));
        assert_eq!(UiCommand::parse("move:4:2"), Some(UiCommand::Strip(StripCmd::Move { id: 4, to: 2 })));
        assert_eq!(UiCommand::parse("mute:4"), Some(UiCommand::Strip(StripCmd::ToggleMute(4))));
        assert_eq!(UiCommand::parse("zoom-reset:4"), Some(UiCommand::Page(PageCmd::ZoomReset(4))));
        assert_eq!(
            UiCommand::parse("suggest:12:300:exa:mple"),
            Some(UiCommand::Overlay(OverlayCmd::Suggest { x: 12, w: 300, text: "exa:mple".into() }))
        );
        assert_eq!(UiCommand::parse("ov-delete-all"), Some(UiCommand::Overlay(OverlayCmd::DeleteAll)));
        assert_eq!(UiCommand::parse("menu-toggle"), Some(UiCommand::Menu(MenuCmd::Toggle)));
        assert_eq!(
            UiCommand::parse("menu-pick:downloads"),
            Some(UiCommand::Menu(MenuCmd::Pick(crate::menu::Item::Downloads)))
        );
        assert_eq!(
            UiCommand::parse("st-autofill:0"),
            Some(UiCommand::Settings(SettingsCmd::Autofill(false)))
        );
        assert_eq!(UiCommand::parse("ext-actions:900"), Some(UiCommand::Ext(ExtCmd::Actions(900))));
        assert_eq!(UiCommand::parse("ext-check-updates"), Some(UiCommand::Ext(ExtCmd::CheckUpdates)));
        assert_eq!(UiCommand::parse("ext-check-updates:x"), None);
        assert_eq!(
            UiCommand::parse("ext-enable:mmbkafeccnpmgicaldnppdljpaogpjdm:0"),
            Some(UiCommand::Ext(ExtCmd::Enable { id: "mmbkafeccnpmgicaldnppdljpaogpjdm".into(), on: false }))
        );
    }

    #[test]
    fn rejects_malformed_or_unknown() {
        for bad in [
            "", "nope", "close", "close:", "close:x", "close:-1", "activate:1.5", "new:1", "ready:x",
            "resize:s", "resize", "navigate", "Back", " back", "move", "move:4", "move:4:x", "mute", "mute:",
            "zoom-reset", "zoom-reset:", "zoom-reset:x", "zoom-reset:1:2", "zoom", "zoom:1", "fullscreen",
            "suggest", "suggest:1", "suggest:x:1:a", "suggest-key:left", "ov-open:1", "ov-close:1", "ov-cleared:1",
            "ov-data-cleared", "menu-toggle:1", "menu-pick", "menu-pick:", "menu-pick:Find", "menu-pick:exit",
            "st-engine", "st-engine:yahoo", "st-startup:x", "st-autofill:2", "st-folder", "st-theme:dark",
            "home:1", "Home", "st-home",
            "ext-actions", "ext-actions:x", "ext-remove", "ext-remove:abc", "ext-popup:", "ext-timeout:1",
        ] {
            assert_eq!(UiCommand::parse(bad), None, "{bad:?}");
        }
        let huge = format!("navigate:{}", "a".repeat(MAX_NAV_INPUT + 1));
        assert_eq!(UiCommand::parse(&huge), None);
        let home = "a".repeat(crate::settings::MAX_HOME);
        assert_eq!(
            UiCommand::parse(&format!("st-home:{home}")),
            Some(UiCommand::Settings(SettingsCmd::Home(home.clone())))
        );
        assert_eq!(UiCommand::parse(&format!("st-home:{home}a")), None);
        assert_eq!(UiCommand::parse("st-home:"), Some(UiCommand::Settings(SettingsCmd::Home(String::new()))));
    }

    const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false };
    const ALT: Mods = Mods { ctrl: false, shift: false, alt: true };
    const ALTGR: Mods = Mods { ctrl: true, shift: false, alt: true };
    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false };

    #[test]
    fn maps_required_shortcuts() {
        use vk::*;
        assert_eq!(Shortcut::from_key(T, CTRL), Some(Shortcut::NewTab));
        assert_eq!(Shortcut::from_key(W, CTRL), Some(Shortcut::CloseTab));
        assert_eq!(Shortcut::from_key(L, CTRL), Some(Shortcut::FocusAddress));
        assert_eq!(Shortcut::from_key(H, CTRL), Some(Shortcut::History));
        assert_eq!(Shortcut::from_key(H, CTRL_SHIFT), None);
        assert_eq!(Shortcut::from_key(J, CTRL), Some(Shortcut::Downloads));
        assert_eq!(Shortcut::from_key(J, CTRL_SHIFT), None);
        assert_eq!(Shortcut::from_key(J, NONE), None, "plain J types a letter");
        assert_eq!(Shortcut::from_key(F, ALT), Some(Shortcut::Menu));
        assert_eq!(Shortcut::from_key(E, ALT), Some(Shortcut::Menu));
        assert_eq!(Shortcut::from_key(F, Mods { shift: true, ..ALT }), None);
        assert_eq!(Shortcut::from_key(F, NONE), None);
        assert_eq!(Shortcut::from_key(F, ALTGR), None, "AltGr+F is a character");
        assert_eq!(Shortcut::from_key(H, NONE), None, "plain H types a letter");
        assert_eq!(Shortcut::from_key(R, CTRL), Some(Shortcut::Reload));
        assert_eq!(Shortcut::from_key(TAB, CTRL), Some(Shortcut::NextTab));
        assert_eq!(Shortcut::from_key(TAB, CTRL_SHIFT), Some(Shortcut::PrevTab));
        assert_eq!(Shortcut::from_key(LEFT, ALT), Some(Shortcut::Back));
        assert_eq!(Shortcut::from_key(RIGHT, ALT), Some(Shortcut::Forward));
    }

    #[test]
    fn ignores_unrelated_and_altgr() {
        use vk::*;
        assert_eq!(Shortcut::from_key(T, NONE), None, "plain T types a letter");
        assert_eq!(Shortcut::from_key(T, ALTGR), None, "AltGr+T is a character");
        assert_eq!(Shortcut::from_key(L, ALTGR), None);
        assert_eq!(Shortcut::from_key(LEFT, NONE), None, "plain arrows move the caret");
        assert_eq!(Shortcut::from_key(LEFT, CTRL), None, "Ctrl+Left jumps words");
        assert_eq!(Shortcut::from_key(TAB, NONE), None, "plain Tab moves focus");
        assert_eq!(Shortcut::from_key(T, CTRL_SHIFT), Some(Shortcut::Tab(TabKey::Reopen)), "Ctrl+Shift+T reopens");
        assert_eq!(Shortcut::from_key(0x43, CTRL), None, "Ctrl+C stays copy");
    }

    #[test]
    fn altgr_never_matches_any_key() {
        // AltGr arrives as Ctrl+Alt; no virtual key may turn into a shortcut
        // with it, with or without Shift, or typing @ / € / \ would break.
        for key in 0..=0xFF_u32 {
            for shift in [false, true] {
                let m = Mods { shift, ..ALTGR };
                assert_eq!(Shortcut::from_key(key, m), None, "vk {key:#x} shift={shift}");
                assert_eq!(Shortcut::from_key_in(key, m, true), None, "vk {key:#x} shift={shift} in fullscreen");
            }
        }
    }

    #[test]
    fn maps_alternative_keys() {
        use vk::*;
        assert_eq!(Shortcut::from_key(F5, NONE), Some(Shortcut::Reload));
        assert_eq!(Shortcut::from_key(F5, CTRL), Some(Shortcut::Reload));
        assert_eq!(Shortcut::from_key(F5, ALT), None, "Alt+F5 is not reload");
        assert_eq!(Shortcut::from_key(NEXT, CTRL), Some(Shortcut::NextTab), "Ctrl+PageDown");
        assert_eq!(Shortcut::from_key(PRIOR, CTRL), Some(Shortcut::PrevTab), "Ctrl+PageUp");
        assert_eq!(Shortcut::from_key(NEXT, NONE), None, "plain PageDown scrolls");
        assert_eq!(Shortcut::from_key(BROWSER_BACK, NONE), Some(Shortcut::Back));
        assert_eq!(Shortcut::from_key(BROWSER_FORWARD, NONE), Some(Shortcut::Forward));
    }

    #[test]
    fn extra_modifiers_do_not_trigger_other_shortcuts() {
        use vk::*;
        let alt_shift = Mods { alt: true, shift: true, ctrl: false };
        let shift = Mods { shift: true, ..NONE };
        assert_eq!(Shortcut::from_key(LEFT, alt_shift), None, "Alt+Shift+Left");
        assert_eq!(Shortcut::from_key(RIGHT, alt_shift), None, "Alt+Shift+Right");
        assert_eq!(Shortcut::from_key(T, ALT), None, "Alt+T");
        assert_eq!(Shortcut::from_key(T, shift), None, "Shift+T types a capital");
        assert_eq!(Shortcut::from_key(W, CTRL_SHIFT), None, "Ctrl+Shift+W");
        assert_eq!(Shortcut::from_key(L, CTRL_SHIFT), None, "Ctrl+Shift+L");
        assert_eq!(Shortcut::from_key(TAB, ALT), None, "Alt+Tab belongs to the OS");
        assert_eq!(Shortcut::from_key(TAB, shift), None, "Shift+Tab moves focus back");
    }

    #[test]
    fn only_tab_cycling_repeats() {
        assert!(Shortcut::NextTab.repeats());
        assert!(!Shortcut::CloseTab.repeats());
        assert!(!Shortcut::NewTab.repeats());
        assert!(Shortcut::Tab(TabKey::MoveLeft).repeats());
        assert!(!Shortcut::Tab(TabKey::Reopen).repeats(), "a held Ctrl+Shift+T reopens one tab");
    }

    #[test]
    fn strip_keys_reach_the_shortcut_set() {
        use vk::*;
        assert_eq!(Shortcut::from_key(0x31, CTRL), Some(Shortcut::Tab(TabKey::Select(1))));
        assert_eq!(Shortcut::from_key(0x39, CTRL), Some(Shortcut::Tab(TabKey::Last)));
        assert_eq!(Shortcut::from_key(NEXT, CTRL_SHIFT), Some(Shortcut::Tab(TabKey::MoveRight)));
        assert_eq!(Shortcut::from_key(NEXT, CTRL), Some(Shortcut::NextTab), "Ctrl+PgDn still cycles");
        assert_eq!(Shortcut::from_key(0x31, ALTGR), None);
    }

    #[test]
    fn page_keys_reach_the_shortcut_set() {
        use crate::page_tools::keys::*;
        assert_eq!(Shortcut::from_key(OEM_PLUS, CTRL), Some(Shortcut::Page(PageKey::ZoomIn)));
        assert_eq!(Shortcut::from_key(OEM_MINUS, CTRL), Some(Shortcut::Page(PageKey::ZoomOut)));
        assert_eq!(Shortcut::from_key(ZERO, CTRL), Some(Shortcut::Page(PageKey::ZoomReset)), "Ctrl+0 is not a tab");
        assert_eq!(Shortcut::from_key(F11, NONE), Some(Shortcut::Page(PageKey::ToggleFullscreen)));
        assert_eq!(Shortcut::from_key(ESCAPE, NONE), None, "Esc stays the page's");
        assert_eq!(Shortcut::from_key_in(ESCAPE, NONE, true), Some(Shortcut::Page(PageKey::ExitFullscreen)));
        assert!(Shortcut::Page(PageKey::ZoomIn).repeats());
        assert!(!Shortcut::Page(PageKey::ToggleFullscreen).repeats(), "a held F11 toggles once");
    }

    #[test]
    fn find_print_and_devtools_keys_reach_webview2() {
        use crate::page_tools::keys::*;
        for esc in [false, true] {
            assert_eq!(Shortcut::from_key_in(F, CTRL, esc), None, "Ctrl+F opens WebView2's find bar");
            assert_eq!(Shortcut::from_key_in(P, CTRL, esc), None, "Ctrl+P opens WebView2's print preview");
            assert_eq!(Shortcut::from_key_in(F12, NONE, esc), None, "F12 is not ours (DevTools stay off)");
        }
    }

    #[test]
    fn only_esc_depends_on_fullscreen() {
        // Outside Esc, being in fullscreen changes no mapping.
        for key in (0..=0xFF_u32).filter(|&k| k != crate::page_tools::keys::ESCAPE) {
            for m in [NONE, CTRL, CTRL_SHIFT, ALT, Mods { shift: true, ..NONE }] {
                assert_eq!(Shortcut::from_key_in(key, m, true), Shortcut::from_key(key, m), "vk {key:#x} {m:?}");
            }
        }
    }
}
