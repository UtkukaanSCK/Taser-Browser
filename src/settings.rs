//! Settings (`settings.json`) and their page in the overlay. A small flat
//! file, `{"v":1, ...}`, read back as untrusted (`sanitize`):
//!
//! - `engine`: which search engine the address bar uses for free text.
//! - `startup`: restore the last session, or open one blank tab.
//! - `autofill`: WebView2's general autofill in pages (default on). Saving
//!   passwords is never offered, whatever this says.
//! - `download_dir`: where downloads go; empty is WebView2's own folder.
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
}

impl Startup {
    pub fn id(self) -> &'static str {
        match self {
            Startup::Last => "last",
            Startup::Blank => "blank",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [Startup::Last, Startup::Blank].into_iter().find(|s| s.id() == id)
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
    #[serde(deserialize_with = "lenient_autofill")]
    pub autofill: bool,
    #[serde(deserialize_with = "lenient_default")]
    pub download_dir: String,
}

fn lenient_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

/// Anything but a boolean reads as on (the default).
fn lenient_autofill<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_bool().unwrap_or(true))
}

impl Default for Settings {
    fn default() -> Self {
        Self { engine: Engine::default(), startup: Startup::default(), autofill: true, download_dir: String::new() }
    }
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
}

impl State {
    /// Reads `settings.json` from `dir` (none: defaults, nothing saved).
    pub fn open(dir: Option<&Path>) -> Self {
        let Some(dir) = dir else { return Self::default() };
        let mut store = Store::<Settings>::new(dir);
        let loaded = store.load();
        if loaded.source == store::Source::Backup {
            eprintln!("x-browser: {} restored from its backup", Settings::NAME);
        }
        set_engine(loaded.value.engine);
        Self { value: loaded.value, store: Some(store), system_dir: None, folder_pending: false }
    }

    fn save(&mut self) {
        if let Some(s) = &mut self.store {
            s.save(&self.value);
        }
    }
}

/// Puts the settings to work: the download folder and autofill, on every
/// webview there is (new tabs get autofill when they are made).
pub fn apply(app: &mut App) {
    apply_folder(app);
    apply_autofill(app);
}

fn apply_autofill(app: &App) {
    for (_, v) in &app.views {
        let _ = crate::native::apply_content_settings(v, app.settings.value.autofill);
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

/// A content webview was made: sets the download folder if it waited for one.
pub fn on_view_created(app: &mut App) {
    if app.settings.folder_pending {
        apply_folder(app);
    }
}

/// What the settings page is told.
#[derive(Serialize)]
struct View<'a> {
    engine: &'a str,
    startup: &'a str,
    autofill: bool,
    folder: &'a str,
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
}

impl SettingsCmd {
    pub const NAMES: &[&str] = &["st-engine", "st-startup", "st-autofill", "st-folder"];

    pub fn parse(name: &str, arg: Option<&str>) -> Option<Self> {
        Some(match (name, arg?) {
            ("st-engine", a) => Self::Engine(Engine::from_id(a)?),
            ("st-startup", a) => Self::Startup(Startup::from_id(a)?),
            ("st-autofill", "1") => Self::Autofill(true),
            ("st-autofill", "0") => Self::Autofill(false),
            ("st-folder", a) if a.chars().count() <= MAX_DIR => Self::Folder(a.to_string()),
            _ => return None,
        })
    }
}

pub fn on_command(app: &mut App, cmd: SettingsCmd) {
    // Only while the settings page is what the overlay shows.
    if app.overlay.mode != Mode::Settings {
        return;
    }
    let mut status = "";
    match cmd {
        SettingsCmd::Engine(e) => {
            app.settings.value.engine = e;
            set_engine(e);
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
    fn defaults_are_duckduckgo_last_session_autofill_on() {
        let s = Settings::default();
        assert_eq!((s.engine, s.startup, s.autofill), (Engine::DuckDuckGo, Startup::Last, true));
        assert_eq!(s.download_dir, "");
        assert_eq!(read("{}"), s);
        assert_eq!(read(r#"{"v":1}"#), s);
    }

    #[test]
    fn reads_what_was_saved() {
        let s = read(r#"{"v":1,"engine":"bing","startup":"blank","autofill":false,"download_dir":"C:\\Users\\x\\Downloads"}"#);
        assert_eq!((s.engine, s.startup, s.autofill), (Engine::Bing, Startup::Blank, false));
        assert_eq!(s.download_dir, "C:\\Users\\x\\Downloads");
        let again: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(again, s);
    }

    #[test]
    fn hostile_values_cost_only_their_field() {
        let s = read(r#"{"engine":"evil","startup":7,"autofill":"yes","download_dir":"..\\x"}"#);
        assert_eq!(s, Settings::default(), "unknown or mistyped values read as the defaults");
        let s = read(r#"{"engine":"google","startup":[],"autofill":false,"download_dir":{"a":1}}"#);
        assert_eq!((s.engine, s.startup, s.autofill), (Engine::Google, Startup::Last, false));
        assert_eq!(s.download_dir, "");
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
    fn commands_are_a_closed_set() {
        let p = SettingsCmd::parse;
        assert_eq!(p("st-engine", Some("google")), Some(SettingsCmd::Engine(Engine::Google)));
        assert_eq!(p("st-startup", Some("blank")), Some(SettingsCmd::Startup(Startup::Blank)));
        assert_eq!(p("st-autofill", Some("0")), Some(SettingsCmd::Autofill(false)));
        assert_eq!(p("st-autofill", Some("1")), Some(SettingsCmd::Autofill(true)));
        assert_eq!(p("st-folder", Some("")), Some(SettingsCmd::Folder(String::new())));
        assert_eq!(p("st-folder", Some("C:\\a:b")), Some(SettingsCmd::Folder("C:\\a:b".into())));
        for (n, a) in [
            ("st-engine", None),
            ("st-engine", Some("")),
            ("st-engine", Some("yahoo")),
            ("st-startup", Some("both")),
            ("st-autofill", Some("2")),
            ("st-autofill", Some("true")),
            ("st-folder", None),
            ("st-theme", Some("dark")),
        ] {
            assert_eq!(p(n, a), None, "{n} {a:?}");
        }
        assert_eq!(p("st-folder", Some(&"a".repeat(MAX_DIR + 1))), None);
        for n in SettingsCmd::NAMES {
            assert!(p(n, Some("x")).is_none() || *n == "st-folder", "{n}");
        }
    }
}
