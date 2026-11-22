//! The main menu (the toolbar's menu button, Alt+F, Alt+E): a list in the overlay.
//! It only chooses; every item calls what its own shortcut already calls
//! (`page_tools`, `overlay`, `downloads`, `native::{find, print}`), so there
//! is no second copy of any of them.
//!
//! It holds the keyboard (↑/↓/Home/End/Enter in `ui/js/menu.js`, Esc through
//! the overlay's own Esc) and gives it back to the page when it closes.

use crate::app::{App, UI_HEIGHT};
use crate::commands::Shortcut;
use crate::native;
use crate::overlay::{self, Mode};
use crate::page_tools::{self, PageKey};
use wry::Rect;

/// Size of the list (logical px); must match overlay.html.
const MENU_W: f64 = 264.0;
const MENU_H: f64 = 324.0;
/// Gap to the window's edges and the strip.
const EDGE: f64 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    NewTab,
    Find,
    ZoomOut,
    ZoomIn,
    ZoomReset,
    Print,
    History,
    Bookmarks,
    Downloads,
    Settings,
    Extensions,
}

impl Item {
    const ALL: [(&'static str, Item); 11] = [
        ("new-tab", Item::NewTab),
        ("find", Item::Find),
        ("zoom-out", Item::ZoomOut),
        ("zoom-in", Item::ZoomIn),
        ("zoom-reset", Item::ZoomReset),
        ("print", Item::Print),
        ("history", Item::History),
        ("bookmarks", Item::Bookmarks),
        ("downloads", Item::Downloads),
        ("settings", Item::Settings),
        ("extensions", Item::Extensions),
    ];

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|&(_, i)| i)
    }
}

/// Messages from the strip and from the menu page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCmd {
    /// The ⋮ button.
    Toggle,
    /// An item was chosen (also: the strip's downloads button, and Ctrl+F or
    /// Ctrl+P typed in the strip, which WebView2 would answer in the strip).
    Pick(Item),
}

impl MenuCmd {
    pub const NAMES: &[&str] = &["menu-toggle", "menu-pick"];

    pub fn parse(name: &str, arg: Option<&str>) -> Option<Self> {
        match (name, arg) {
            ("menu-toggle", None) => Some(Self::Toggle),
            ("menu-pick", Some(a)) => Item::from_name(a).map(Self::Pick),
            _ => None,
        }
    }
}

pub fn on_command(app: &mut App, cmd: MenuCmd) {
    match cmd {
        // (Only the strip's ⋮ button sends it: the keyboard is in the strip.)
        MenuCmd::Toggle => toggle_from(app, true),
        MenuCmd::Pick(item) => pick(app, item),
    }
}

/// Where the list goes: at the right, under the strip, inside the window.
fn menu_rect(size: tao::dpi::LogicalSize<f64>) -> Rect {
    let w = MENU_W.min((size.width - 2.0 * EDGE).max(1.0));
    let y = UI_HEIGHT + 2.0;
    let h = MENU_H.min((size.height - y - EDGE).max(1.0));
    overlay::rect((size.width - w - EDGE).max(0.0), y, w, h)
}

/// Alt+F / Alt+E: opens the menu, or closes it.
pub fn toggle(app: &mut App) {
    toggle_from(app, false);
}

/// `strip`: the ⋮ button asked, so the keyboard is in the strip and the menu
/// must take it, even from a launch question still up (it does not close on
/// blur; showing the menu answers it "no").
fn toggle_from(app: &mut App, strip: bool) {
    if app.overlay.mode == Mode::Menu {
        overlay::close(app);
        app.focus_after_switch();
        return;
    }
    // (Another overlay page may be up: keep its webview, show the menu in it.)
    if strip {
        overlay::hand_over_from_strip(app);
    } else {
        overlay::hand_over(app);
    }
    // The menu hangs under the strip, so fullscreen gives way (as for Ctrl+H).
    if app.fullscreen.on() {
        page_tools::leave_fullscreen(app);
    }
    let bounds = menu_rect(overlay::win_size(app));
    overlay::show(app, Mode::Menu, format!("xbm.show({})", active_zoom(app)), bounds);
}

/// The active tab's zoom in whole percent.
fn active_zoom(app: &App) -> u16 {
    app.tabs.active().and_then(|id| app.tabs.get(id)).map_or(100, |t| t.zoom)
}

/// Runs a menu item. All but the zoom items close the menu first; the zoom
/// ones leave it open to be pressed again, showing the new percentage.
pub fn pick(app: &mut App, item: Item) {
    let zoom = match item {
        Item::ZoomOut => Some(PageKey::ZoomOut),
        Item::ZoomIn => Some(PageKey::ZoomIn),
        Item::ZoomReset => Some(PageKey::ZoomReset),
        _ => None,
    };
    if let Some(key) = zoom {
        page_tools::on_key(app, key);
        let js = format!("xbm.zoom({})", active_zoom(app));
        overlay::run(app, Mode::Menu, &js);
        return;
    }
    // These draw in the overlay's own webview: it stays up between pages.
    if matches!(item, Item::History | Item::Bookmarks | Item::Settings | Item::Extensions) {
        overlay::hand_over(app);
    } else {
        overlay::close(app);
    }
    match item {
        Item::NewTab => app.on_shortcut(Shortcut::NewTab),
        Item::Find => {
            // The find bar is the page's: it needs the keyboard there.
            if let Some(v) = app.active_view() {
                let _ = wry::WebView::focus(v);
                native::find(v);
            }
        }
        Item::Print => {
            app.focus_after_switch();
            if let Some(v) = app.active_view() {
                native::print(v);
            }
        }
        Item::History => overlay::toggle_history(app),
        Item::Bookmarks => crate::bookmarks::open_list(app),
        Item::Downloads => {
            app.focus_after_switch();
            crate::downloads::toggle(app);
        }
        Item::Settings => crate::settings::open(app),
        Item::Extensions => crate::ext_ui::open(app),
        Item::ZoomOut | Item::ZoomIn | Item::ZoomReset => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tao::dpi::LogicalSize;

    fn parts(r: Rect) -> (f64, f64, f64, f64) {
        let (p, s) = (r.position.to_logical::<f64>(1.0), r.size.to_logical::<f64>(1.0));
        (p.x, p.y, s.width, s.height)
    }

    #[test]
    fn parses_menu_messages() {
        assert_eq!(MenuCmd::parse("menu-toggle", None), Some(MenuCmd::Toggle));
        assert_eq!(MenuCmd::parse("menu-pick", Some("find")), Some(MenuCmd::Pick(Item::Find)));
        for (name, item) in Item::ALL {
            assert_eq!(MenuCmd::parse("menu-pick", Some(name)), Some(MenuCmd::Pick(item)), "{name}");
        }
        for (n, a) in [
            ("menu-toggle", Some("1")),
            ("menu-pick", None),
            ("menu-pick", Some("")),
            ("menu-pick", Some("Find")),
            ("menu-pick", Some("quit")),
            ("menu-pick", Some("find:1")),
            ("menu-open", None),
        ] {
            assert_eq!(MenuCmd::parse(n, a), None, "{n} {a:?}");
        }
    }

    #[test]
    fn names_cover_every_command() {
        for n in MenuCmd::NAMES {
            assert!(MenuCmd::parse(n, Some("find")).is_some() || MenuCmd::parse(n, None).is_some(), "{n}");
        }
    }

    #[test]
    fn menu_sits_at_the_top_right_inside_the_window() {
        assert_eq!(parts(menu_rect(LogicalSize::new(1200.0, 800.0))), (928.0, 78.0, 264.0, 324.0));
        // A short window shortens it (it scrolls); a narrow one narrows it.
        assert_eq!(parts(menu_rect(LogicalSize::new(480.0, 240.0))), (208.0, 78.0, 264.0, 154.0));
        assert_eq!(parts(menu_rect(LogicalSize::new(200.0, 240.0))), (8.0, 78.0, 184.0, 154.0));
        // Windows at least as large as the app's minimum (480x240) hold it.
        for (w, h) in [(480.0, 240.0), (500.0, 300.0), (1920.0, 1080.0)] {
            let (x, y, mw, mh) = parts(menu_rect(LogicalSize::new(w, h)));
            assert!(x >= 0.0 && y >= 0.0 && x + mw <= w && y + mh <= h, "{w}x{h}");
        }
    }
}
