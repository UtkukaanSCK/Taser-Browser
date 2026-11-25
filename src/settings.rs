//! Settings (`settings.json`) and their page in the overlay. A small flat
//! file, `{"v":1, ...}`, read back as untrusted (`sanitize`):
//!
//! - `engine`: which search engine the address bar uses for free text.
//! - `startup`: restore the last session, open one blank tab, or open the
//!   home page.
//! - `home`: the address the strip's home button (and a home page start)
//!   opens; empty is the new tab page (`newtab`).
//! - `autofill`: WebView2's general autofill in pages (default on). Saving
//!   passwords is never offered, whatever this says.
//! - `download_dir`: where downloads go; empty is WebView2's own folder.
//! - `tracking`: WebView2's tracking prevention level for the content
//!   profile (off, basic, balanced or strict; default balanced, WebView2's
//!   own default).
//! - `https_only`, `strip_params`, `gpc`: HTTPS-only, dropping tracking
//!   parameters from addresses, and Global Privacy Control (`privacy`; all
//!   on by default).
//! - `exit_clear`: clear browsing data when Taser quits (default off), and
//!   which kinds: `exit_clear_history`, `exit_clear_cookies`,
//!   `exit_clear_cache` (all on by default; see `exit_clear`).
//!
//! The page is drawn by the overlay (`ui/js/settings.js`), never in a content
//! tab, and talks over the same origin-checked IPC as the rest of the UI.
//! "Clear browsing data" is the history list's own command (`overlay`).

use crate::app::App;
use crate::overlay::{self, Mode};
use crate::store::{self, Store, Stored};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

/// Longest download folder path kept or accepted (characters).
pub const MAX_DIR: usize = 1024;

/// Longest home address kept or accepted (characters).
pub const MAX_HOME: usize = 2048;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    DuckDuckGo,
    Google,
    Bing,
}

impl Engine {
    pub const ALL: [Engine; 3] = [Engine::DuckDuckGo, Engine::Google, Engine::Bing];

    pub fn id(self) -> &'static str {
        match self {
            Engine::DuckDuckGo => "duckduckgo",
            Engine::Google => "google",
            Engine::Bing => "bing",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.id() == id)
    }

    /// The search address up to the query.
    pub fn search_url(self) -> &'static str {
        match self {
            Engine::DuckDuckGo => crate::url::SEARCH_URL,
            Engine::Google => "https://www.google.com/search?q=",
            Engine::Bing => "https://www.bing.com/search?q=",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Startup {
    /// The tabs of the last session.
    #[default]
    Last,
    /// One blank tab.
    Blank,
    /// One tab with the home page (`Settings::home`).
    Home,
}

impl Startup {
    pub fn id(self) -> &'static str {
        match self {
            Startup::Last => "last",
            Startup::Blank => "blank",
            Startup::Home => "home",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [Startup::Last, Startup::Blank, Startup::Home].into_iter().find(|s| s.id() == id)
    }
}

/// How hard WebView2 blocks trackers. Off leaves blocking to the extensions
/// (uBlock Origin Lite): the way out when the two together break a site.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tracking {
    /// WebView2 blocks nothing itself.
    Off,
    /// Harmful trackers only.
    Basic,
    /// Also trackers from sites the user has not visited.
    #[default]
    Balanced,
    /// Most trackers everywhere; some sites may break.
    Strict,
}

impl Tracking {
    pub const ALL: [Tracking; 4] = [Tracking::Off, Tracking::Basic, Tracking::Balanced, Tracking::Strict];

    pub fn id(self) -> &'static str {
        match self {
            Tracking::Off => "off",
            Tracking::Basic => "basic",
            Tracking::Balanced => "balanced",
            Tracking::Strict => "strict",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.id() == id)
    }

    /// `COREWEBVIEW2_TRACKING_PREVENTION_LEVEL`.
    pub fn level(self) -> i32 {
        match self {
            Tracking::Off => 0,
            Tracking::Basic => 1,
            Tracking::Balanced => 2,
            Tracking::Strict => 3,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// A value this version does not know reads as the default, and costs
    /// only this field, not the whole file.
    #[serde(deserialize_with = "lenient_default")]
    pub engine: Engine,
    #[serde(deserialize_with = "lenient_default")]
    pub startup: Startup,
    #[serde(deserialize_with = "lenient_on")]
    pub autofill: bool,
    #[serde(deserialize_with = "lenient_default")]
    pub download_dir: String,
    #[serde(deserialize_with = "lenient_default")]
    pub tracking: Tracking,
    /// A web address (`home_address`), or empty for the new tab page.
    #[serde(deserialize_with = "lenient_default")]
    pub home: String,
    #[serde(deserialize_with = "lenient_on")]
    pub https_only: bool,
    #[serde(deserialize_with = "lenient_on")]
    pub strip_params: bool,
    #[serde(deserialize_with = "lenient_on")]
    pub gpc: bool,
    #[serde(deserialize_with = "lenient_off")]
    pub exit_clear: bool,
    #[serde(deserialize_with = "lenient_on")]
    pub exit_clear_history: bool,
    #[serde(deserialize_with = "lenient_on")]
    pub exit_clear_cookies: bool,
    #[serde(deserialize_with = "lenient_on")]
    pub exit_clear_cache: bool,
}

fn lenient_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

/// Anything but a boolean reads as on (the default of every switch here).
fn lenient_on<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_bool().unwrap_or(true))
}

/// Anything but a boolean reads as off (for a switch that defaults off).
pub(crate) fn lenient_off<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_bool().unwrap_or(false))
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: Engine::default(),
            startup: Startup::default(),
            autofill: true,
            download_dir: String::new(),
            tracking: Tracking::default(),
            home: String::new(),
            https_only: true,
            strip_params: true,
            gpc: true,
            exit_clear: false,
            exit_clear_history: true,
            exit_clear_cookies: true,
            exit_clear_cache: true,
        }
    }
}

/// The web address `text` names, as the address bar would go to it, or
/// `None` when the address bar would search for it, refuse it, or go
/// somewhere that is not a web page.
pub fn home_address(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > MAX_HOME {
        return None;
    }
    match crate::url::resolve_input(text) {
        crate::url::Resolved::Url(u)
            if (u.starts_with("https://") || u.starts_with("http://"))
                && u.chars().count() <= MAX_HOME
                && u != crate::url::search_url(text) =>
        {
            Some(u)
        }
        _ => None,
    }
}

/// What a home setting read back may be: empty, or an address as
/// `home_address` gives it.
fn valid_home(home: &str) -> bool {
    home.is_empty() || home_address(home).as_deref() == Some(home)
}

/// A download folder worth passing to WebView2: absolute, plain text, and not
/// a folder whose files Windows runs at sign-in.
fn valid_dir(dir: &str) -> bool {
    // No UNC or device paths: `is_dir()` on an unreachable host stalls the UI
    // thread and sends NTLM credentials.
    !dir.is_empty()
        && dir.chars().count() <= MAX_DIR
        && !dir.chars().any(char::is_control)
        && !dir.starts_with(['/', '\\'])
        && Path::new(dir).is_absolute()
        && !startup_folders().iter().any(|s| inside(dir, s))
}

/// `dir` is `root` or a folder below it, compared the way Windows compares
/// paths: case aside, either separator, trailing separators and `.`/`..`
/// steps resolved by text. (A short 8.3 name or a junction still gets past;
/// this is a guard against a slip, not a sandbox.)
fn inside(dir: &str, root: &str) -> bool {
    fn steps(p: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for part in p.split(['\\', '/']).filter(|s| !s.is_empty() && *s != ".") {
            if part == ".." {
                if out.len() > 1 {
                    out.pop();
                }
            } else {
                // Windows drops trailing dots and spaces: `Startup.` is Startup.
                let part = part.trim_end_matches(['.', ' ']);
                if !part.is_empty() {
                    out.push(part.to_lowercase());
                }
            }
        }
        out
    }
    let (dir, root) = (steps(dir), steps(root));
    !root.is_empty() && dir.starts_with(&root)
}

/// The per-user and all-users Startup folders, asked of Windows once.
fn startup_folders() -> &'static [String] {
    static FOLDERS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FOLDERS.get_or_init(|| {
        #[cfg(windows)]
        {
            use windows::Win32::UI::Shell::{FOLDERID_CommonStartup, FOLDERID_Startup};
            [FOLDERID_Startup, FOLDERID_CommonStartup].iter().filter_map(known_folder).collect()
        }
        #[cfg(not(windows))]
        Vec::new()
    })
}

#[cfg(windows)]
fn known_folder(id: &windows::core::GUID) -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// The OS Downloads folder (not WebView2's profile value, which keeps whatever
/// was chosen last).
#[cfg(windows)]
fn os_downloads() -> Option<String> {
    known_folder(&windows::Win32::UI::Shell::FOLDERID_Downloads).filter(|d| valid_dir(d))
}

/// Elsewhere there is no known-folder API; WebView2's own default stands.
#[cfg(not(windows))]
fn os_downloads() -> Option<String> {
    None
}

impl Stored for Settings {
    const NAME: &'static str = "settings.json";
    const MAX_BYTES: u64 = 64 * 1024;

    fn sanitize(&mut self) {
        if !valid_dir(&self.download_dir) {
            self.download_dir.clear();
        }
        if !valid_home(&self.home) {
            self.home.clear();
        }
    }
}

/// The engine the address bar searches with right now (read by `url`).
static ENGINE: AtomicU8 = AtomicU8::new(0);

pub fn engine() -> Engine {
    Engine::ALL.get(usize::from(ENGINE.load(Ordering::Relaxed))).copied().unwrap_or_default()
}

fn set_engine(e: Engine) {
    let i = Engine::ALL.iter().position(|&x| x == e).unwrap_or(0);
    ENGINE.store(i as u8, Ordering::Relaxed);
}

fn set_privacy(s: &Settings) {
    crate::privacy::set(s.https_only, s.strip_params, s.gpc);
}

/// What `App` keeps: the values, and where they are saved.
#[derive(Default)]
pub struct State {
    pub value: Settings,
    store: Option<Store<Settings>>,
    /// The OS Downloads folder, what an empty setting goes back to.
    system_dir: Option<String>,
    /// The download folder is still to be set: there was no tab to set it
    /// through yet (see `on_view_created`).
    folder_pending: bool,
    /// The same for the tracking prevention level.
    tracking_pending: bool,
    /// What a quit that is not clean leaves to clear (`exit_clear`).
    pub exit_marker: crate::exit_clear::Marker,
}

impl State {
    /// Reads `settings.json` from `dir` (none: defaults, nothing saved).
    pub fn open(dir: Option<&Path>) -> Self {
        let Some(dir) = dir else { return Self::default() };
        let mut store = Store::<Settings>::new(dir);
        let loaded = store.load();
        if loaded.source == store::Source::Backup {
            eprintln!("taser: {} restored from its backup", Settings::NAME);
        }
        set_engine(loaded.value.engine);
        set_privacy(&loaded.value);
        Self {
            value: loaded.value,
            store: Some(store),
            system_dir: None,
            folder_pending: false,
            tracking_pending: false,
            exit_marker: crate::exit_clear::Marker::open(Some(dir)),
        }
    }

    fn save(&mut self) {
        if let Some(s) = &mut self.store {
            s.save(&self.value);
        }
    }

    /// Where the home button goes: the home address, or the new tab page.
    pub fn home_url(&self) -> &str {
        if self.value.home.is_empty() { crate::url::BLANK } else { &self.value.home }
    }

    /// The first tab's address when no session is brought back.
    pub fn start_url(&self) -> &str {
        if self.value.startup == Startup::Home { self.home_url() } else { crate::url::BLANK }
    }
}

/// Puts the settings to work: the download folder, tracking prevention and
/// autofill, on every webview there is (new tabs get autofill when they are
/// made; the other two belong to the shared profile).
pub fn apply(app: &mut App) {
    apply_folder(app);
    apply_tracking(app);
    apply_autofill(app);
}

/// The level belongs to the content tabs' profile (where the extensions
/// are too), never the UI's own, so it is set through a tab, like the
/// download folder. WebView2 keeps the level in the profile as well; this
/// file stays the source. False if WebView2 refused (a runtime without
/// `ICoreWebView2Profile3`); with no tab yet it waits and counts as done.
fn apply_tracking(app: &mut App) -> bool {
    let level = app.settings.value.tracking.level();
    let Some(v) = app.content_view() else {
        app.settings.tracking_pending = true;
        return true;
    };
    let ok = crate::native::set_tracking(v, level);
    app.settings.tracking_pending = false;
    if !ok {
        eprintln!("taser: tracking prevention level not set");
    }
    ok
}

fn apply_autofill(app: &App) {
    for (_, v) in &app.views {
        let _ = crate::native::apply_content_settings(v, app.settings.value.autofill);
    }
}

/// The privacy switches, for every tab's next navigation; GPC's header and
/// script into or out of every tab (new tabs get them when made).
fn apply_privacy(app: &App) {
    set_privacy(&app.settings.value);
    for (id, v) in &app.views {
        let _ = crate::native::apply_gpc(v, *id, app.settings.value.gpc);
    }
}

/// The download folder belongs to the content tabs' profile (the UI has its
/// own, `ui::UI_PROFILE`), so it is set through a tab; any tab will do. With
/// none yet (the restored tab's webview could not be made), the first one
/// made later does it (`on_view_created`).
fn apply_folder(app: &mut App) {
    let s = &mut app.settings;
    if s.system_dir.is_none() {
        s.system_dir = os_downloads();
    }
    let chosen = Some(s.value.download_dir.as_str()).filter(|d| valid_dir(d) && Path::new(d).is_dir());
    let Some(dir) = chosen.or(s.system_dir.as_deref()).map(str::to_owned) else { return };
    match app.content_view() {
        Some(v) => {
            crate::native::set_download_folder(v, &dir);
            app.settings.folder_pending = false;
        }
        None => app.settings.folder_pending = true,
    }
}

/// A content webview was made: sets the download folder and the tracking
/// prevention level if they waited for one.
pub fn on_view_created(app: &mut App) {
    if app.settings.folder_pending {
        apply_folder(app);
    }
    if app.settings.tracking_pending {
        apply_tracking(app);
    }
}

/// What the settings page is told.
#[derive(Serialize)]
struct View<'a> {
    engine: &'a str,
    startup: &'a str,
    autofill: bool,
    folder: &'a str,
    tracking: &'a str,
    home: &'a str,
    https_only: bool,
    strip_params: bool,
    gpc: bool,
    exit_clear: bool,
    exit_history: bool,
    exit_cookies: bool,
    exit_cache: bool,
    /// WebView2's own folder, shown when `folder` is empty.
    default_folder: &'a str,
    /// A short code for the page to word (never text from here).
    status: &'a str,
}

fn render(app: &mut App, keep: bool, status: &str) {
    let s = &app.settings;
    let view = View {
        engine: s.value.engine.id(),
        startup: s.value.startup.id(),
        autofill: s.value.autofill,
        folder: &s.value.download_dir,
        tracking: s.value.tracking.id(),
        home: &s.value.home,
        https_only: s.value.https_only,
        strip_params: s.value.strip_params,
        gpc: s.value.gpc,
        exit_clear: s.value.exit_clear,
        exit_history: s.value.exit_clear_history,
        exit_cookies: s.value.exit_clear_cookies,
        exit_cache: s.value.exit_clear_cache,
        default_folder: s.system_dir.as_deref().unwrap_or(""),
        status,
    };
    let Ok(json) = serde_json::to_string(&view) else { return };
    let bounds = overlay::panel_rect(overlay::win_size(app));
    overlay::show(app, Mode::Settings, format!("xbs.show({json},{keep})"), bounds);
}

/// The menu's "Ayarlar": opens the settings page.
pub fn open(app: &mut App) {
    // (Hands over, never hides: the menu already did, and hide-then-show blurs the page.)
    overlay::hand_over(app);
    if app.fullscreen.on() {
        crate::page_tools::leave_fullscreen(app);
    }
    render(app, false, "");
}

/// Messages from the settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsCmd {
    Engine(Engine),
    Startup(Startup),
    Autofill(bool),
    /// A folder to download to; empty goes back to WebView2's own.
    Folder(String),
    Tracking(Tracking),
    /// A home address as typed; empty goes back to the new tab page.
    Home(String),
    HttpsOnly(bool),
    StripParams(bool),
    Gpc(bool),
    /// "Kapanışta tarama verilerini temizle" and its three kinds.
    ExitClear(bool),
    ExitHistory(bool),
    ExitCookies(bool),
    ExitCache(bool),
}

impl SettingsCmd {
    pub const NAMES: &[&str] = &[
        "st-engine",
        "st-startup",
        "st-autofill",
        "st-folder",
        "st-tracking",
        "st-home",
        "st-https",
        "st-strip",
        "st-gpc",
        "st-exitclear",
        "st-exithistory",
        "st-exitcookies",
        "st-exitcache",
    ];

    pub fn parse(name: &str, arg: Option<&str>) -> Option<Self> {
        Some(match (name, arg?) {
            ("st-engine", a) => Self::Engine(Engine::from_id(a)?),
            ("st-startup", a) => Self::Startup(Startup::from_id(a)?),
            ("st-autofill", "1") => Self::Autofill(true),
            ("st-autofill", "0") => Self::Autofill(false),
            ("st-folder", a) if a.chars().count() <= MAX_DIR => Self::Folder(a.to_string()),
            ("st-tracking", a) => Self::Tracking(Tracking::from_id(a)?),
            ("st-home", a) if a.chars().count() <= MAX_HOME => Self::Home(a.to_string()),
            ("st-https", a) => Self::HttpsOnly(switch(a)?),
            ("st-strip", a) => Self::StripParams(switch(a)?),
            ("st-gpc", a) => Self::Gpc(switch(a)?),
            ("st-exitclear", a) => Self::ExitClear(switch(a)?),
            ("st-exithistory", a) => Self::ExitHistory(switch(a)?),
            ("st-exitcookies", a) => Self::ExitCookies(switch(a)?),
            ("st-exitcache", a) => Self::ExitCache(switch(a)?),
            _ => return None,
        })
    }
}

/// A switch's "1" or "0".
fn switch(a: &str) -> Option<bool> {
    match a {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

pub fn on_command(app: &mut App, cmd: SettingsCmd) {
    // Only while the settings page is what the overlay shows.
    if app.overlay.mode != Mode::Settings {
        return;
    }
    let mut status = "";
    let exit_changed = matches!(
        cmd,
        SettingsCmd::ExitClear(_) | SettingsCmd::ExitHistory(_) | SettingsCmd::ExitCookies(_) | SettingsCmd::ExitCache(_)
    );
    match cmd {
        SettingsCmd::Engine(e) => {
            app.settings.value.engine = e;
            set_engine(e);
            // The new tab pages' search box searches with it too.
            crate::newtab::refresh(app);
        }
        SettingsCmd::Startup(s) => app.settings.value.startup = s,
        SettingsCmd::Autofill(on) => {
            app.settings.value.autofill = on;
            apply_autofill(app);
        }
        SettingsCmd::Folder(dir) => {
            let dir = dir.trim();
            if dir.is_empty() {
                app.settings.value.download_dir.clear();
                status = "folder-reset";
            } else if valid_dir(dir) && Path::new(dir).is_dir() {
                app.settings.value.download_dir = dir.to_string();
                status = "folder-ok";
            } else {
                status = "folder-bad";
            }
            apply_folder(app);
        }
        SettingsCmd::Tracking(t) => {
            // The choice is kept (it is tried again at every start), but
            // the page is told it did not take.
            app.settings.value.tracking = t;
            if !apply_tracking(app) {
                status = "tracking-failed";
            }
        }
        SettingsCmd::Home(text) => {
            if text.trim().is_empty() {
                app.settings.value.home.clear();
                status = "home-reset";
            } else if let Some(home) = home_address(&text) {
                app.settings.value.home = home;
                status = "home-ok";
            } else {
                status = "home-bad";
            }
        }
        SettingsCmd::HttpsOnly(on) => {
            app.settings.value.https_only = on;
            apply_privacy(app);
        }
        SettingsCmd::StripParams(on) => {
            app.settings.value.strip_params = on;
            apply_privacy(app);
        }
        SettingsCmd::Gpc(on) => {
            app.settings.value.gpc = on;
            apply_privacy(app);
        }
        SettingsCmd::ExitClear(on) => app.settings.value.exit_clear = on,
        SettingsCmd::ExitHistory(on) => app.settings.value.exit_clear_history = on,
        SettingsCmd::ExitCookies(on) => app.settings.value.exit_clear_cookies = on,
        SettingsCmd::ExitCache(on) => app.settings.value.exit_clear_cache = on,
    }
    if exit_changed {
        crate::exit_clear::on_settings_changed(app);
    }
    app.settings.save();
    render(app, true, status);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(json: &str) -> Settings {
        let mut s: Settings = serde_json::from_str(json).unwrap();
        s.sanitize();
        s
    }

    #[test]
    fn a_home_address_is_a_web_address_the_address_bar_would_go_to() {
        assert_eq!(home_address("  example.com/x "), Some("https://example.com/x".into()));
        assert_eq!(home_address("http://intranet:8080/"), Some("http://intranet:8080/".into()));
        assert_eq!(home_address("localhost:3000"), Some("http://localhost:3000".into()));
        for bad in [
            "",
            "   ",
            "two words",
            "javascript:alert(1)",
            "file:///C:/x",
            r"C:\x",
            "about:blank",
            "about:flags",
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop/p.html",
            "nodot",
        ] {
            assert_eq!(home_address(bad), None, "{bad:?}");
        }
        assert_eq!(home_address(&format!("https://e.com/{}", "a".repeat(MAX_HOME))), None);
        // Read back: kept only as `home_address` would have written it.
        assert_eq!(read(r#"{"home":"https://e.com/"}"#).home, "https://e.com/");
        for bad in [r#"{"home":"e.com"}"#, r#"{"home":"javascript:x"}"#, r#"{"home":5}"#, r#"{"home":"a b"}"#] {
            assert_eq!(read(bad).home, "", "{bad}");
        }
        assert_eq!(read(r#"{"startup":"home"}"#).startup, Startup::Home);
    }

    #[test]
    fn unc_and_relative_download_folders_are_refused() {
        assert!(!valid_dir(r"\\host\share"));
        assert!(!valid_dir("//host/share"));
        assert!(!valid_dir(r"/\host\share"));
        assert!(!valid_dir(r"\/host\share"));
        assert!(!valid_dir("relative"));
        assert!(valid_dir(r"C:\Users\x\Downloads"));
    }

    #[test]
    fn a_folder_is_inside_another_by_windows_rules() {
        let root = r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup";
        for dir in [
            root,
            r"c:\users\X\appdata\roaming\microsoft\windows\start menu\programs\STARTUP\",
            r"C:/Users/x/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup//sub",
            r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\.\Startup\a\..\b",
            r"C:\Users\x\Downloads\..\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup",
            r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup.",
            r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup. \sub",
        ] {
            assert!(inside(dir, root), "{dir}");
        }
        for dir in [
            r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup2",
            r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs",
            r"C:\Users\x\Downloads",
            r"D:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup",
        ] {
            assert!(!inside(dir, root), "{dir}");
        }
        assert!(!inside(r"C:\x", ""), "no folder known, nothing refused");
    }

    #[cfg(windows)]
    #[test]
    fn the_startup_folders_are_refused_as_download_folders() {
        let folders = startup_folders();
        assert_eq!(folders.len(), 2, "{folders:?}");
        for f in folders {
            assert!(!valid_dir(f), "{f}");
            assert!(!valid_dir(&format!("{f}\\")), "{f}");
            assert!(!valid_dir(&format!("{}\\sub", f.to_uppercase())), "{f}");
            let mut s = Settings { download_dir: f.clone(), ..Settings::default() };
            s.sanitize();
            assert_eq!(s.download_dir, "", "a saved Startup folder is dropped on load");
        }
        assert!(valid_dir(&os_downloads().expect("known folder")));
    }

    #[cfg(windows)]
    #[test]
    fn the_os_downloads_folder_is_a_valid_absolute_path() {
        let d = os_downloads().expect("known folder");
        assert!(valid_dir(&d), "{d}");
    }

    #[test]
    fn defaults_are_duckduckgo_last_session_autofill_and_privacy_on() {
        let s = Settings::default();
        assert_eq!((s.engine, s.startup, s.autofill), (Engine::DuckDuckGo, Startup::Last, true));
        assert_eq!((s.https_only, s.strip_params, s.gpc), (true, true, true));
        assert_eq!(s.download_dir, "");
        assert_eq!(s.tracking, Tracking::Balanced);
        // Clearing at quit is off, with every kind chosen for when it is on.
        assert!(!s.exit_clear);
        assert_eq!((s.exit_clear_history, s.exit_clear_cookies, s.exit_clear_cache), (true, true, true));
        assert_eq!(read("{}"), s);
        assert_eq!(read(r#"{"v":1}"#), s);
    }

    #[test]
    fn reads_what_was_saved() {
        let s = read(
            r#"{"v":1,"engine":"bing","startup":"blank","autofill":false,"download_dir":"C:\\Users\\x\\Downloads","tracking":"strict","https_only":false,"strip_params":false,"gpc":false,"exit_clear":true,"exit_clear_history":false,"exit_clear_cookies":true,"exit_clear_cache":false}"#,
        );
        assert_eq!((s.engine, s.startup, s.autofill), (Engine::Bing, Startup::Blank, false));
        assert_eq!((s.https_only, s.strip_params, s.gpc), (false, false, false));
        assert!(s.exit_clear);
        assert_eq!((s.exit_clear_history, s.exit_clear_cookies, s.exit_clear_cache), (false, true, false));
        assert_eq!(s.tracking, Tracking::Strict);
        assert_eq!(s.download_dir, "C:\\Users\\x\\Downloads");
        let again: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(again, s);
    }

    #[test]
    fn hostile_values_cost_only_their_field() {
        let s = read(
            r#"{"engine":"evil","startup":7,"autofill":"yes","download_dir":"..\\x","tracking":"none","https_only":0,"strip_params":null,"gpc":"off","exit_clear":"yes","exit_clear_history":0,"exit_clear_cookies":"off","exit_clear_cache":null}"#,
        );
        assert_eq!(s, Settings::default(), "unknown or mistyped values read as the defaults");
        let s = read(r#"{"engine":"google","startup":[],"autofill":false,"download_dir":{"a":1}}"#);
        assert_eq!((s.engine, s.startup, s.autofill), (Engine::Google, Startup::Last, false));
        assert_eq!(s.download_dir, "");
        // A file from before the setting reads as off.
        let s = read(r#"{"v":1,"engine":"bing","gpc":false}"#);
        assert!(!s.exit_clear);
        let s = read(r#"{"exit_clear":1}"#);
        assert!(!s.exit_clear, "only a boolean turns clearing on");
    }

    #[test]
    fn download_folder_must_be_a_plain_absolute_path() {
        let abs = if cfg!(windows) { "C:\\Data" } else { "/data" };
        assert!(valid_dir(abs));
        for bad in ["", "relative\\dir", "Downloads", "\u{0}", "C:\\a\u{7}b", &"C:\\".repeat(MAX_DIR)] {
            assert!(!valid_dir(bad), "{bad:?}");
        }
        let mut s = Settings { download_dir: "x\ny".into(), ..Settings::default() };
        s.sanitize();
        assert_eq!(s.download_dir, "");
    }

    #[test]
    fn engines_search_over_https() {
        for e in Engine::ALL {
            assert!(e.search_url().starts_with("https://"), "{e:?}");
            assert!(e.search_url().ends_with("?q="), "{e:?}");
            assert_eq!(Engine::from_id(e.id()), Some(e));
        }
        assert_eq!(Engine::default().search_url(), crate::url::SEARCH_URL);
        assert_eq!(Engine::from_id("Google"), None);
        assert_eq!(Startup::from_id("blank"), Some(Startup::Blank));
        assert_eq!(Startup::from_id("none"), None);
    }

    #[test]
    fn tracking_levels_are_webview2_values() {
        assert_eq!(Tracking::ALL.map(Tracking::level), [0, 1, 2, 3]);
        assert_eq!(Tracking::from_id("off"), Some(Tracking::Off));
        for t in Tracking::ALL {
            assert_eq!(Tracking::from_id(t.id()), Some(t));
        }
        assert_eq!(Tracking::from_id("none"), None);
        assert_eq!(Tracking::from_id("Strict"), None);
    }

    #[test]
    fn commands_are_a_closed_set() {
        let p = SettingsCmd::parse;
        assert_eq!(p("st-engine", Some("google")), Some(SettingsCmd::Engine(Engine::Google)));
        assert_eq!(p("st-startup", Some("blank")), Some(SettingsCmd::Startup(Startup::Blank)));
        assert_eq!(p("st-autofill", Some("0")), Some(SettingsCmd::Autofill(false)));
        assert_eq!(p("st-autofill", Some("1")), Some(SettingsCmd::Autofill(true)));
        assert_eq!(p("st-folder", Some("")), Some(SettingsCmd::Folder(String::new())));
        assert_eq!(p("st-folder", Some("C:\\a:b")), Some(SettingsCmd::Folder("C:\\a:b".into())));
        assert_eq!(p("st-tracking", Some("strict")), Some(SettingsCmd::Tracking(Tracking::Strict)));
        assert_eq!(p("st-tracking", Some("off")), Some(SettingsCmd::Tracking(Tracking::Off)));
        assert_eq!(p("st-https", Some("0")), Some(SettingsCmd::HttpsOnly(false)));
        assert_eq!(p("st-strip", Some("1")), Some(SettingsCmd::StripParams(true)));
        assert_eq!(p("st-gpc", Some("0")), Some(SettingsCmd::Gpc(false)));
        assert_eq!(p("st-exitclear", Some("1")), Some(SettingsCmd::ExitClear(true)));
        assert_eq!(p("st-exithistory", Some("0")), Some(SettingsCmd::ExitHistory(false)));
        assert_eq!(p("st-exitcookies", Some("1")), Some(SettingsCmd::ExitCookies(true)));
        assert_eq!(p("st-exitcache", Some("0")), Some(SettingsCmd::ExitCache(false)));
        for (n, a) in [
            ("st-engine", None),
            ("st-engine", Some("")),
            ("st-engine", Some("yahoo")),
            ("st-startup", Some("both")),
            ("st-autofill", Some("2")),
            ("st-autofill", Some("true")),
            ("st-folder", None),
            ("st-tracking", Some("none")),
            ("st-tracking", Some("")),
            ("st-https", None),
            ("st-https", Some("true")),
            ("st-strip", Some("")),
            ("st-gpc", Some("on")),
            ("st-exitclear", None),
            ("st-exitclear", Some("true")),
            ("st-exithistory", Some("")),
            ("st-exitcookies", Some("2")),
            ("st-exitcache", Some("01")),
            ("st-theme", Some("dark")),
        ] {
            assert_eq!(p(n, a), None, "{n} {a:?}");
        }
        assert_eq!(p("st-folder", Some(&"a".repeat(MAX_DIR + 1))), None);
        for n in SettingsCmd::NAMES {
            assert!(p(n, Some("x")).is_none() || *n == "st-folder" || *n == "st-home", "{n}");
        }
    }
}
