//! One running Taser per profile. The first instance owns a named mutex
//! and listens on a message-only window; a second start hands its address
//! over by WM_COPYDATA and exits, and the first one opens it in a new tab
//! (no address: just comes to the front).
//!
//! Both names carry a hash of the data folder, so a self-test run (own data
//! folder) never meets the user's browser.
//!
//! The message is untrusted (any program of the same user can send one):
//! `decode_payload` takes only `dwData == 1`, at most `MAX_PAYLOAD` bytes of
//! well-formed UTF-16 without NUL, and only what `url_from_arg` accepts.
//!
//! The receiving window is not trusted either: the names are predictable, so
//! another program could create a window of that class first. A second start
//! sends its address, and grants foreground rights, only to a window whose
//! process runs this same exe, as the same user, at no lower integrity
//! (`trusted_pid`). The mutex grants access to the current user only.

use crate::url::{self, Resolved};
use std::sync::atomic::{AtomicBool, Ordering};

/// `COPYDATASTRUCT::dwData` of an "open this" message.
pub const OPEN: usize = 1;
/// `dwData` of a probe: whether Windows lets our messages through to a
/// window at all (see `uipi_blocks`). Refused, silently and with no effect.
pub const PROBE: usize = 2;
/// Largest payload accepted, in bytes (8 KiB UTF-16 code units).
pub const MAX_PAYLOAD: usize = 16 * 1024;

/// Why a message was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Kind,
    TooLong,
    OddLength,
    Nul,
    BadUtf16,
    /// Well-formed, but not an address a tab may open (javascript:, file:...).
    NotWeb,
}

/// A command-line argument as an address to open: what the address bar would
/// make of it (a URL, or a search for free text), web addresses only.
pub fn url_from_arg(arg: &str) -> Option<String> {
    match url::resolve_input(arg) {
        Resolved::Url(u) if url::is_tab_url(&u) && !crate::ui::is_ui_url(&u) => Some(u),
        _ => None,
    }
}

static QUITTING: AtomicBool = AtomicBool::new(false);

/// The app has started to exit: refuse hand-overs from now on. The sender
/// keeps trying and, once this process is gone, starts as the first instance
/// itself, so its address is not lost.
pub fn set_quitting(on: bool) {
    QUITTING.store(on, Ordering::Relaxed);
}

/// The address in a received message: `Ok(None)` for an empty one (just come
/// to the front).
pub fn decode_payload(kind: usize, bytes: &[u8]) -> Result<Option<String>, Reject> {
    if kind != OPEN {
        return Err(Reject::Kind);
    }
    if bytes.len() > MAX_PAYLOAD {
        return Err(Reject::TooLong);
    }
    if !bytes.len().is_multiple_of(2) {
        return Err(Reject::OddLength);
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
    if units.contains(&0) {
        return Err(Reject::Nul);
    }
    let text = String::from_utf16(&units).map_err(|_| Reject::BadUtf16)?;
    url_from_arg(&text).map(Some).ok_or(Reject::NotWeb)
}

/// What to send for our own first argument: the argument itself if it is an
/// address the other instance will take, else nothing (it still comes to
/// the front).
pub fn payload(arg: Option<&str>) -> Vec<u8> {
    match arg {
        Some(a) if url_from_arg(a).is_some() && a.len() * 2 <= MAX_PAYLOAD => {
            a.encode_utf16().flat_map(u16::to_le_bytes).collect()
        }
        _ => Vec::new(),
    }
}

/// FNV-1a of the lowercased data folder path, as 16 hex digits.
pub fn profile_hash(dir: &std::path::Path) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in dir.to_string_lossy().to_lowercase().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// How long the same address handed over again counts as a repeat.
pub const REPEAT_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// Drops an address handed over again within `REPEAT_WINDOW`: a sender whose
/// message was answered late (after its timeout) retries, and the first
/// message may still arrive, so one start could open a URL twice.
#[derive(Default)]
pub struct Repeats {
    last: Option<(String, std::time::Instant)>,
}

impl Repeats {
    /// Whether `url`, arriving at `now`, should be opened (not a repeat).
    pub fn fresh(&mut self, url: &str, now: std::time::Instant) -> bool {
        if let Some((last, at)) = &self.last
            && last == url
            && now.saturating_duration_since(*at) < REPEAT_WINDOW
        {
            return false;
        }
        self.last = Some((url.to_string(), now));
        true
    }
}

/// How `start` went.
pub enum Start {
    /// This is the only instance; keep the guard for the life of the process.
    First(Guard),
    /// The running instance took the address; this one should exit.
    HandedOver,
    /// Another instance holds the profile but no genuine one answered in
    /// time (the user was told); this one should exit with an error.
    Unreachable,
}

#[cfg(windows)]
pub use imp::*;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::UserEvent;
    use crate::native::Proxy;
    use std::cell::RefCell;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HANDLE, HLOCAL, HWND,
        LPARAM, LRESULT, LocalFree, SetLastError, WPARAM,
    };
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
        SDDL_REVISION_1, SE_KERNEL_OBJECT,
    };
    use windows::Win32::Security::{
        ACE_HEADER, ACL, GetAce, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation,
        LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
        SYSTEM_MANDATORY_LABEL_ACE, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY, TOKEN_USER, TokenIntegrityLevel, TokenUser,
    };
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        CreateMutexW, GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, FindWindowExW, GetWindowThreadProcessId,
        HWND_MESSAGE, RegisterClassW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, SetForegroundWindow,
        WINDOW_EX_STYLE, WM_COPYDATA, WNDCLASSW, WS_OVERLAPPED,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR};

    /// How long a second start keeps trying to reach the first instance
    /// (which may still be starting up, or shutting down).
    const HAND_OVER_TIME: Duration = Duration::from_secs(6);
    /// One WM_COPYDATA attempt.
    const SEND_TIMEOUT_MS: u32 = 2000;

    /// Holds the mutex (closed by Windows when the process exits).
    pub struct Guard(#[allow(dead_code)] HANDLE);

    fn names(dir: &std::path::Path) -> (HSTRING, HSTRING) {
        let hash = profile_hash(dir);
        // Still the names from before the rename to Taser, on purpose: an old
        // X-Browser and a new Taser on the same profile must see each other.
        (HSTRING::from(format!("Local\\X-Browser-{hash}")), HSTRING::from(format!("XBrowserIpc-{hash}")))
    }

    /// A handle closed when dropped.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// Distinct processes refused per search (others may squat the class).
    const MAX_REFUSED: usize = 64;
    /// Windows of our class walked per search at most.
    const MAX_WINDOWS: usize = 4096;

    /// The process owning a window (0 if it is gone).
    fn window_pid(hwnd: HWND) -> u32 {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        pid
    }

    /// `find_by` with the real window-to-process lookup, checking processes
    /// only (the self-test's and tests' way in).
    #[cfg(any(test, feature = "selftest"))]
    fn find<V>(class: &HSTRING, verify: impl Fn(u32) -> Option<V>) -> Option<(HWND, u32, V)> {
        find_by(class, window_pid, |_, pid| verify(pid))
    }

    /// The first instance's IPC window, its process id and what `verify`
    /// made of that process: the first window of `class` whose process
    /// `verify` accepts. `owner` gives a window's process (tests fake it);
    /// `verify` gets each process with its first window. Each process is
    /// checked once; a refused one's other windows are skipped without
    /// counting.
    fn find_by<V>(
        class: &HSTRING,
        owner: impl Fn(HWND) -> u32,
        verify: impl Fn(HWND, u32) -> Option<V>,
    ) -> Option<(HWND, u32, V)> {
        let mut refused: Vec<u32> = Vec::new();
        let mut after = None;
        for _ in 0..MAX_WINDOWS {
            let hwnd = unsafe { FindWindowExW(Some(HWND_MESSAGE), after, class, PCWSTR::null()) }
                .ok()
                .filter(|h| !h.is_invalid())?;
            after = Some(hwnd);
            let pid = owner(hwnd);
            if refused.contains(&pid) {
                continue;
            }
            if let Some(v) = verify(hwnd, pid) {
                return Some((hwnd, pid, v));
            }
            eprintln!("taser: skipped the IPC windows of process {pid}: not this Taser");
            refused.push(pid);
            if refused.len() >= MAX_REFUSED {
                return None;
            }
        }
        None
    }

    /// What a window's process turned out to be.
    enum Examined {
        /// Taser itself: the same exe as this one, run by the same user,
        /// at an integrity level no lower than ours. Opened, so its id
        /// cannot be reused while the handle is held.
        Verified(Owned),
        /// Our exe, but its token is closed to us: an elevated Taser,
        /// seen from a normal start, or a lower process that shut its token
        /// (a Low one may do that to a copy of our exe it started). Never
        /// handed anything; evidence only with proof, see `counts_as_seen`.
        OursUnreadable,
        /// Anything else.
        Other,
    }

    /// `Examined` without the handle.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Kind {
        Verified,
        OursUnreadable,
        Other,
    }

    impl Examined {
        fn kind(&self) -> Kind {
            match self {
                Examined::Verified(_) => Kind::Verified,
                Examined::OursUnreadable => Kind::OursUnreadable,
                Examined::Other => Kind::Other,
            }
        }
    }

    fn examine(pid: u32) -> Examined {
        if pid == 0 {
            return Examined::Other;
        }
        let (Ok(exe), Some(me)) = (std::env::current_exe(), identity(unsafe { GetCurrentProcess() })) else {
            return Examined::Other;
        };
        let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
            return Examined::Other;
        };
        let process = Owned(process);
        if !image_path(process.0).is_some_and(|p| same_file(&p, &exe)) {
            return Examined::Other;
        }
        match identity(process.0) {
            Some(them) if them.user == me.user && them.integrity >= me.integrity => Examined::Verified(process),
            Some(_) => Examined::Other,
            None => Examined::OursUnreadable,
        }
    }

    /// Process `pid`, opened, if it is Taser itself (see `Examined`).
    #[cfg(any(test, feature = "selftest"))]
    fn verified(pid: u32) -> Option<Owned> {
        match examine(pid) {
            Examined::Verified(process) => Some(process),
            _ => None,
        }
    }

    /// Whether process `pid` is Taser itself (see `verified`).
    #[cfg(test)]
    fn trusted_pid(pid: u32) -> bool {
        verified(pid).is_some()
    }

    fn image_path(process: HANDLE) -> Option<std::path::PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        let mut buf = vec![0u16; 32 * 1024];
        let mut len = buf.len() as u32;
        unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
        Some(std::ffi::OsString::from_wide(buf.get(..len as usize)?).into())
    }

    /// Same file, compared as canonical paths, ignoring case.
    fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
        let canon = |p: &std::path::Path| {
            std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase()
        };
        canon(a) == canon(b)
    }

    /// Who a process runs as: its user SID (as text) and integrity level.
    struct Identity {
        user: String,
        integrity: u32,
    }

    fn identity(process: HANDLE) -> Option<Identity> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
        let token = Owned(token);
        let user_buf = token_info(token.0, TokenUser)?;
        // SAFETY: GetTokenInformation(TokenUser) filled the (8-aligned)
        // buffer with a TOKEN_USER whose SID points into the same buffer.
        let user = sid_string(unsafe { &*(user_buf.as_ptr() as *const TOKEN_USER) }.User.Sid)?;
        let label_buf = token_info(token.0, TokenIntegrityLevel)?;
        // SAFETY: as above, a TOKEN_MANDATORY_LABEL; the last subauthority
        // of its SID is the integrity level.
        let sid = unsafe { &*(label_buf.as_ptr() as *const TOKEN_MANDATORY_LABEL) }.Label.Sid;
        let count = unsafe { *GetSidSubAuthorityCount(sid) };
        if count == 0 {
            return None;
        }
        let integrity = unsafe { *GetSidSubAuthority(sid, u32::from(count) - 1) };
        Some(Identity { user, integrity })
    }

    /// A token information block, in a u64 buffer for alignment.
    fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Option<Vec<u64>> {
        let mut len = 0u32;
        let _ = unsafe { GetTokenInformation(token, class, None, 0, &mut len) };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        unsafe { GetTokenInformation(token, class, Some(buf.as_mut_ptr().cast()), len, &mut len) }.ok()?;
        Some(buf)
    }

    fn sid_string(sid: PSID) -> Option<String> {
        let mut text = PWSTR::null();
        unsafe { ConvertSidToStringSidW(sid, &mut text) }.ok()?;
        let out = unsafe { text.to_string() }.ok();
        unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
        out
    }

    /// A security descriptor owned by the current user, whose DACL grants
    /// that user, and no one else, full access, and whose mandatory label at
    /// our own integrity level keeps lower-level processes of the same user
    /// from opening it at all (no read, write or execute up, so not even
    /// SYNCHRONIZE: they cannot hold the name alive). Freed on drop.
    struct OwnerOnly(PSECURITY_DESCRIPTOR);

    impl OwnerOnly {
        fn new() -> Option<Self> {
            let me = identity(unsafe { GetCurrentProcess() })?;
            let sddl = HSTRING::from(format!(
                "O:{user}D:P(A;;GA;;;{user})S:(ML;;NWNRNX;;;S-1-16-{level})",
                user = me.user,
                level = me.integrity
            ));
            let mut sd = PSECURITY_DESCRIPTOR::default();
            unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None) }
                .ok()?;
            Some(Self(sd))
        }
    }

    impl Drop for OwnerOnly {
        fn drop(&mut self) {
            unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
        }
    }

    /// Creates (or opens) the named mutex; `true` when it already existed.
    fn create_mutex(name: &HSTRING) -> windows::core::Result<(HANDLE, bool)> {
        let sd = OwnerOnly::new();
        let sa = sd.as_ref().map(|sd| SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0.0,
            bInheritHandle: false.into(),
        });
        let handle = unsafe { CreateMutexW(sa.as_ref().map(|sa| sa as *const _), false, name) }?;
        Ok((handle, unsafe { GetLastError() } == ERROR_ALREADY_EXISTS))
    }

    /// Integrity level of an object with no explicit label (Medium).
    const IMPLICIT_LEVEL: u32 = 0x2000;
    /// SYSTEM_MANDATORY_LABEL_ACE_TYPE (in a feature this crate does not use).
    const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;

    /// Whether a mutex labelled at `label` cannot be a genuine Taser's,
    /// for an instance running at `ours`. The bar is our level, but never
    /// above Medium: a lower process can only make objects at its own level,
    /// so a Medium label means a normal Taser (inside the trust boundary),
    /// and an elevated start must not run beside it on the same profile.
    fn label_is_squat(label: u32, ours: u32) -> bool {
        label < ours.min(IMPLICIT_LEVEL)
    }

    /// Why an existing mutex of our name was not made by a Taser of this
    /// user (its owner is someone else, or its label is lower than one could
    /// be, see `label_is_squat`), or None if it was, or if that cannot be read.
    fn squatted(handle: HANDLE) -> Option<String> {
        let me = identity(unsafe { GetCurrentProcess() })?;
        let mut owner = PSID::default();
        let mut sacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let err = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
                Some(&mut owner),
                None,
                None,
                Some(&mut sacl),
                Some(&mut sd),
            )
        };
        if err != ERROR_SUCCESS {
            return None;
        }
        let owner = if owner.is_invalid() { None } else { sid_string(owner) };
        let level = label_level(sacl);
        unsafe { LocalFree(Some(HLOCAL(sd.0))) };
        if owner.as_deref() != Some(me.user.as_str()) {
            return Some(format!("owned by {}", owner.as_deref().unwrap_or("nobody")));
        }
        label_is_squat(level, me.integrity)
            .then(|| format!("integrity label {level:#x}, too low for ours ({:#x})", me.integrity))
    }

    /// The profile's mutex, as a second start found it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum MutexState {
        /// Opened, and made by a Taser of this user (or unreadable).
        Genuine,
        /// Opened, but someone else made it (see `squatted`).
        Squatted,
        /// It exists, but may not be opened: a higher-integrity Taser's,
        /// or a squatter's whose DACL shuts us out.
        Denied,
    }

    /// What a second start does when no Taser took its address in time.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum AtDeadline {
        /// Tell the user Taser is running but cannot be reached.
        Dialog,
        /// Run anyway, without the lock.
        Unguarded,
    }

    /// `seen`: a Taser window of ours (see `Examined`) turned up at any
    /// point. A squatter cannot fake one, so whenever there is one, it holds
    /// the profile and a second copy must not run beside it, even when the
    /// mutex itself is squatted (that Taser then runs unguarded too).
    fn at_deadline(mutex: MutexState, seen: bool) -> AtDeadline {
        match (mutex, seen) {
            (MutexState::Genuine, _) | (_, true) => AtDeadline::Dialog,
            (MutexState::Squatted | MutexState::Denied, false) => AtDeadline::Unguarded,
        }
    }

    /// Whether a window of process `kind` shows a Taser of ours holds
    /// the profile. Nothing unreadable is evidence by itself: a lower process
    /// can shut its token and its mutex to us. So an unreadable copy of our
    /// exe counts only if the mutex is shut to us too (as an elevated
    /// Taser's always is) and `probe_blocked` shows its window is above
    /// us: Windows (UIPI) refuses our message, which a lower process cannot
    /// arrange, as message filters only ever let more through. The probe
    /// runs only when it decides.
    fn counts_as_seen(kind: Kind, mutex: MutexState, probe_blocked: impl FnOnce() -> bool) -> bool {
        match kind {
            Kind::Verified => true,
            Kind::OursUnreadable => mutex == MutexState::Denied && probe_blocked(),
            Kind::Other => false,
        }
    }

    /// How long the UIPI probe waits for an answer.
    const PROBE_TIMEOUT_MS: u32 = 500;

    /// Whether Windows refuses our messages to `hwnd` for its higher
    /// integrity (UIPI): the send fails with access denied. A timeout, or
    /// any other failure, is not that. The `PROBE` kind is refused by an
    /// Taser, silently and with no effect.
    fn uipi_blocks(hwnd: HWND) -> bool {
        let data = COPYDATASTRUCT { dwData: PROBE, cbData: 0, lpData: std::ptr::null_mut() };
        let mut result = 0usize;
        unsafe { SetLastError(ERROR_SUCCESS) };
        let sent = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_COPYDATA,
                WPARAM(0),
                LPARAM(&data as *const COPYDATASTRUCT as isize),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                PROBE_TIMEOUT_MS,
                Some(&mut result),
            )
        };
        sent.0 == 0 && unsafe { GetLastError() } == ERROR_ACCESS_DENIED
    }

    /// The state of the profile's mutex as this round found it (None: it
    /// may not be opened), and why it is not ours, if it is not.
    fn mutex_state(existing: Option<&Owned>) -> (MutexState, String) {
        match existing {
            None => (MutexState::Denied, "it may not be opened".to_owned()),
            Some(existing) => match squatted(existing.0) {
                Some(why) => (MutexState::Squatted, why),
                None => (MutexState::Genuine, String::new()),
            },
        }
    }

    /// The integrity level in a SACL's mandatory label (implicit if none).
    fn label_level(sacl: *const ACL) -> u32 {
        if sacl.is_null() {
            return IMPLICIT_LEVEL;
        }
        for i in 0..u32::from(unsafe { (*sacl).AceCount }) {
            let mut ace = std::ptr::null_mut();
            if unsafe { GetAce(sacl, i, &mut ace) }.is_err() || ace.is_null() {
                continue;
            }
            // SAFETY: GetAce returned a pointer to an ACE inside the SACL;
            // every ACE starts with an ACE_HEADER, and a label ACE is a
            // SYSTEM_MANDATORY_LABEL_ACE whose SID starts at SidStart.
            if unsafe { (*(ace as *const ACE_HEADER)).AceType } == SYSTEM_MANDATORY_LABEL_ACE_TYPE {
                let ace = ace as *const SYSTEM_MANDATORY_LABEL_ACE;
                let sid = PSID(unsafe { std::ptr::addr_of!((*ace).SidStart) } as *mut _);
                let count = unsafe { *GetSidSubAuthorityCount(sid) };
                if count == 0 {
                    return 0;
                }
                return unsafe { *GetSidSubAuthority(sid, u32::from(count) - 1) };
            }
        }
        IMPLICIT_LEVEL
    }

    /// Sends one message; the receiver's answer (1 = taken), or None if it
    /// did not answer in time.
    pub fn send(hwnd: HWND, kind: usize, bytes: &[u8]) -> Option<usize> {
        let data = COPYDATASTRUCT {
            dwData: kind,
            cbData: bytes.len() as u32,
            lpData: if bytes.is_empty() { std::ptr::null_mut() } else { bytes.as_ptr() as *mut _ },
        };
        let mut result = 0usize;
        let ok = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_COPYDATA,
                WPARAM(0),
                LPARAM(&data as *const COPYDATASTRUCT as isize),
                SMTO_ABORTIFHUNG,
                SEND_TIMEOUT_MS,
                Some(&mut result),
            )
        };
        (ok.0 != 0).then_some(result)
    }

    /// Becomes the first instance, or hands `arg` to the running one.
    pub fn start(dir: &std::path::Path, arg: Option<&str>) -> Start {
        let (mutex, class) = names(dir);
        let payload = payload(arg);
        let deadline = Instant::now() + HAND_OVER_TIME;
        // Whether a Taser window of ours turned up, even one we may not
        // hand to (an elevated one): then the lock is in real use.
        let seen = std::cell::Cell::new(false);
        // Processes whose window was probed already (once each).
        let probed = RefCell::new(Vec::<u32>::new());
        loop {
            // Not ours: held only for this round, as keeping it open would
            // keep the name alive after the first instance exits. None: it
            // exists but may not be opened (a higher Taser's, or a
            // squatter's); the windows decide which, at the deadline.
            let existing = match create_mutex(&mutex) {
                Ok((handle, false)) => return Start::First(Guard(handle)),
                Ok((handle, true)) => Some(Owned(handle)),
                Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => None,
                // Any other failure: run unguarded rather than not at all.
                Err(_) => return Start::First(Guard(HANDLE::default())),
            };
            let (state, why) = mutex_state(existing.as_ref());
            let check = |hwnd: HWND, pid: u32| {
                let examined = examine(pid);
                let probe = || {
                    if seen.get() || probed.borrow().contains(&pid) {
                        return false;
                    }
                    probed.borrow_mut().push(pid);
                    uipi_blocks(hwnd)
                };
                if counts_as_seen(examined.kind(), state, probe) {
                    seen.set(true);
                }
                match examined {
                    Examined::Verified(process) => Some(process),
                    _ => None,
                }
            };
            // Only a verified Taser gets foreground rights or the address.
            // Its process stays open until the message is sent, so the id
            // cannot be reused meanwhile.
            if let Some((hwnd, pid, _process)) = find_by(&class, window_pid, check)
                && window_pid(hwnd) == pid
            {
                // Lets it come to the front: we are the foreground app.
                let _ = unsafe { AllowSetForegroundWindow(pid) };
                // 0 = refused (it is closing): try again, until it is gone
                // and we start as the first instance ourselves.
                if send(hwnd, OPEN, &payload) == Some(1) {
                    return Start::HandedOver;
                }
            }
            if Instant::now() >= deadline {
                // No genuine window answered. If the mutex is not one an
                // Taser of ours made, and no Taser window of ours is
                // around, someone holds the name to keep us out: run
                // unguarded rather than not at all.
                if at_deadline(state, seen.get()) == AtDeadline::Unguarded {
                    eprintln!(
                        "taser: the profile's mutex is not ours ({why}) and no Taser window was seen; \
                         starting unguarded"
                    );
                    return Start::First(Guard(HANDLE::default()));
                }
                if !crate::timing::quit_when_ready() {
                    crate::native::fatal(
                        "Taser zaten açık, ancak bu başlatma ona bağlanamadı \
                         (farklı yetki düzeyi ya da yanıt vermiyor).",
                    );
                }
                return Start::Unreachable;
            }
            drop(existing);
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    thread_local! {
        /// Where the IPC window delivers (it lives on the event-loop thread).
        static PROXY: RefCell<Option<Proxy>> = const { RefCell::new(None) };
    }

    /// Opens the IPC window for the other instances. Call on the event-loop
    /// thread; the window lives until the process exits.
    pub fn listen(dir: &std::path::Path, proxy: Proxy) -> windows::core::Result<()> {
        let (_, class) = names(dir);
        PROXY.with(|p| *p.borrow_mut() = Some(proxy));
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(ipc_proc),
                hInstance: instance.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                return Err(windows::core::Error::from_thread());
            }
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                &class,
                &class,
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance.into()),
                None,
            )?;
        }
        Ok(())
    }

    unsafe extern "system" fn ipc_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_COPYDATA {
            return LRESULT(on_copydata(lparam) as isize);
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    fn on_copydata(lparam: LPARAM) -> bool {
        let data = lparam.0 as *const COPYDATASTRUCT;
        // While quitting, refuse: the sender retries and then starts itself.
        if data.is_null() || QUITTING.load(Ordering::Relaxed) {
            return false;
        }
        // SAFETY: for WM_COPYDATA Windows hands us a valid COPYDATASTRUCT
        // whose buffer (copied into our process) is `cbData` bytes long.
        let data = unsafe { &*data };
        // Another start checking it can reach us at all: nothing to do.
        if data.dwData == PROBE {
            return false;
        }
        let len = data.cbData as usize;
        let decoded = if len > MAX_PAYLOAD {
            Err(Reject::TooLong)
        } else if len == 0 {
            decode_payload(data.dwData, &[])
        } else if data.lpData.is_null() {
            return false;
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, len) };
            decode_payload(data.dwData, bytes)
        };
        match decoded {
            Ok(url) => PROXY.with(|p| {
                p.borrow().as_ref().is_some_and(|p| p.send_event(UserEvent::OpenFromOtherInstance(url)).is_ok())
            }),
            Err(why) => {
                eprintln!("taser: ignored a message from another start ({why:?})");
                false
            }
        }
    }

    /// Restores and raises the window. SetForegroundWindow works because the
    /// sending instance allowed it; tao's `set_focus` is not used, as it may
    /// fake an Alt key press to force the switch.
    pub fn bring_forward(window: &tao::window::Window) {
        use tao::platform::windows::WindowExtWindows;
        if window.is_minimized() {
            window.set_minimized(false);
        }
        window.set_visible(true);
        let _ = unsafe { SetForegroundWindow(HWND(window.hwnd() as _)) };
    }

    /// Self-test: sends a raw message to this profile's IPC window (our own,
    /// found the way a second start finds it).
    #[cfg(feature = "selftest")]
    pub fn send_to_self(dir: &std::path::Path, kind: usize, bytes: &[u8]) -> Option<usize> {
        let (_, class) = names(dir);
        send(find(&class, verified)?.0, kind, bytes)
    }

    /// Self-test: the process id the search accepts for this profile's IPC
    /// window, and what it finds when the check refuses every process.
    #[cfg(feature = "selftest")]
    pub fn probe_find(dir: &std::path::Path) -> (Option<u32>, Option<u32>) {
        let (_, class) = names(dir);
        (find(&class, verified).map(|(_, pid, _)| pid), find(&class, |_| None::<()>).map(|(_, pid, _)| pid))
    }

    /// Self-test: whether the UIPI probe finds this profile's IPC window
    /// (our own, at our own level) out of reach. None: no window found.
    #[cfg(feature = "selftest")]
    pub fn probe_blocks_self(dir: &std::path::Path) -> Option<bool> {
        let (_, class) = names(dir);
        Some(uipi_blocks(find(&class, verified)?.0))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::System::Threading::GetCurrentProcessId;

        #[test]
        fn own_process_is_trusted_others_are_not() {
            assert!(trusted_pid(unsafe { GetCurrentProcessId() }));
            assert!(!trusted_pid(0), "idle process");
            assert!(!trusted_pid(4), "System: not our exe");
            assert!(!trusted_pid(u32::MAX), "no such process");
        }

        unsafe extern "system" fn plain_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        /// Registers `class` and creates `n` message-only windows of it on
        /// this thread (they go when the test thread ends).
        fn windows_of_class(class: &HSTRING, n: usize) -> Vec<HWND> {
            unsafe {
                let instance = GetModuleHandleW(None).unwrap();
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(plain_proc),
                    hInstance: instance.into(),
                    lpszClassName: PCWSTR(class.as_ptr()),
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
                (0..n)
                    .map(|_| {
                        CreateWindowExW(
                            WINDOW_EX_STYLE(0),
                            class,
                            class,
                            WS_OVERLAPPED,
                            0,
                            0,
                            0,
                            0,
                            Some(HWND_MESSAGE),
                            None,
                            Some(instance.into()),
                            None,
                        )
                        .unwrap()
                    })
                    .collect()
            }
        }

        fn test_class(tag: &str) -> HSTRING {
            HSTRING::from(format!("TaserIpcTest-{tag}-{}", unsafe { GetCurrentProcessId() }))
        }

        #[test]
        fn find_skips_windows_the_check_refuses() {
            let class = test_class("skip");
            windows_of_class(&class, 2);
            let me = unsafe { GetCurrentProcessId() };
            assert_eq!(find(&class, verified).map(|(_, pid, _)| pid), Some(me));
            assert_eq!(find(&class, |_| None::<()>).map(|(_, pid, _)| pid), None, "never an unverified one");
            // The first window's process refused, the next one's taken.
            let first = find(&class, |_| Some(())).unwrap().0;
            let found = find_by(&class, |h| if h == first { 1 } else { 2 }, |_, pid| (pid == 2).then_some(()));
            assert!(found.is_some_and(|(h, pid, _)| h != first && pid == 2));
        }

        #[test]
        fn a_flood_from_one_process_does_not_hide_the_real_one() {
            let class = test_class("flood");
            // New windows go to the top of the list, so the real one, made
            // first, is walked last, after every window of the flood.
            let real = windows_of_class(&class, 1)[0];
            let flood = windows_of_class(&class, 3 * MAX_REFUSED);
            assert_eq!(flood.len(), 3 * MAX_REFUSED);
            let walked = std::cell::Cell::new(0usize);
            let verified_calls = std::cell::Cell::new(0usize);
            let found = find_by(
                &class,
                |h| {
                    walked.set(walked.get() + 1);
                    if h == real { 2 } else { 1 }
                },
                |_, pid| {
                    verified_calls.set(verified_calls.get() + 1);
                    (pid == 2).then_some(())
                },
            );
            assert_eq!(found.map(|(h, _, _)| h), Some(real));
            assert!(walked.get() > MAX_REFUSED, "the real window came after the flood ({})", walked.get());
            assert_eq!(verified_calls.get(), 2, "each process is checked once");
        }

        #[test]
        fn refused_processes_are_capped() {
            let class = test_class("cap");
            let windows = windows_of_class(&class, MAX_REFUSED + 6);
            let verified_calls = std::cell::Cell::new(0usize);
            let found = find_by(
                &class,
                |h| windows.iter().position(|w| *w == h).unwrap() as u32 + 100,
                |_, _| {
                    verified_calls.set(verified_calls.get() + 1);
                    None::<()>
                },
            );
            assert!(found.is_none());
            assert_eq!(verified_calls.get(), MAX_REFUSED);
        }

        #[test]
        fn the_mutex_is_for_this_user_only() {
            let sd = OwnerOnly::new().expect("descriptor");
            assert!(!sd.0.0.is_null());
            let name = HSTRING::from(format!("Local\\Taser-test-{}", unsafe { GetCurrentProcessId() }));
            let (first, existed) = create_mutex(&name).unwrap();
            assert!(!existed);
            let (again, existed) = create_mutex(&name).unwrap();
            assert!(existed, "the same user opens it again");
            assert_eq!(squatted(again), None, "ours: our owner, our label");
            unsafe {
                let _ = CloseHandle(again);
                let _ = CloseHandle(first);
            }
        }

        /// Runs `f` on a new thread impersonating this user at Low integrity.
        fn as_low<R: Send>(f: impl FnOnce() -> R + Send) -> R {
            use windows::Win32::Security::Authorization::ConvertStringSidToSidW;
            use windows::Win32::Security::{
                DuplicateTokenEx, GetLengthSid, RevertToSelf, SID_AND_ATTRIBUTES,
                SecurityImpersonation, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_DUPLICATE, TOKEN_IMPERSONATE,
                TokenImpersonation,
            };
            use windows::Win32::System::Threading::SetThreadToken;

            /// Undoes the impersonation even if `f` panics.
            struct Revert;
            impl Drop for Revert {
                fn drop(&mut self) {
                    unsafe { RevertToSelf() }.expect("RevertToSelf");
                }
            }

            /// SE_GROUP_INTEGRITY (in a feature this crate does not use).
            const SE_GROUP_INTEGRITY: u32 = 0x20;

            std::thread::scope(|scope| {
                scope
                    .spawn(|| unsafe {
                        let mut process_token = HANDLE::default();
                        OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut process_token)
                            .unwrap();
                        let process_token = Owned(process_token);
                        let mut low = HANDLE::default();
                        DuplicateTokenEx(
                            process_token.0,
                            TOKEN_IMPERSONATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
                            None,
                            SecurityImpersonation,
                            TokenImpersonation,
                            &mut low,
                        )
                        .unwrap();
                        let low = Owned(low);
                        let mut sid = PSID::default();
                        ConvertStringSidToSidW(&HSTRING::from("S-1-16-4096"), &mut sid).unwrap();
                        let label = TOKEN_MANDATORY_LABEL {
                            Label: SID_AND_ATTRIBUTES { Sid: sid, Attributes: SE_GROUP_INTEGRITY },
                        };
                        let set = SetTokenInformation(
                            low.0,
                            TokenIntegrityLevel,
                            &label as *const TOKEN_MANDATORY_LABEL as *const _,
                            std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(sid),
                        );
                        LocalFree(Some(HLOCAL(sid.0)));
                        set.unwrap();
                        SetThreadToken(None, Some(low.0)).unwrap();
                        let _revert = Revert;
                        f()
                    })
                    .join()
                    .unwrap()
            })
        }

        /// A mutex made with `sddl` as its descriptor.
        fn mutex_with(sddl: &str, name: &HSTRING) -> Owned {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    &HSTRING::from(sddl),
                    SDDL_REVISION_1,
                    &mut sd,
                    None,
                )
                .unwrap();
            }
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            };
            let handle = unsafe { CreateMutexW(Some(&sa), false, name) };
            unsafe { LocalFree(Some(HLOCAL(sd.0))) };
            Owned(handle.unwrap())
        }

        #[test]
        fn a_lower_integrity_process_cannot_open_our_mutex() {
            use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
            let pid = unsafe { GetCurrentProcessId() };
            let ours = HSTRING::from(format!("Local\\Taser-test-label-{pid}"));
            let (handle, existed) = create_mutex(&ours).unwrap();
            let _ours_handle = Owned(handle);
            assert!(!existed);
            // The control: the same DACL without the label (an implicit
            // Medium, no-write-up one) lets a Low process synchronize.
            let me = identity(unsafe { GetCurrentProcess() }).unwrap();
            let plain = HSTRING::from(format!("Local\\Taser-test-nolabel-{pid}"));
            let _plain_handle = mutex_with(&format!("D:P(A;;GA;;;{})", me.user), &plain);

            let open = |name: &HSTRING| {
                unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, name) }.map(|h| drop(Owned(h)))
            };
            let (low_ours, low_plain) = as_low(|| (open(&ours), open(&plain)));
            assert!(low_plain.is_ok(), "control: unlabelled is open to Low ({low_plain:?})");
            assert_eq!(low_ours.map_err(|e| e.code()), Err(ERROR_ACCESS_DENIED.to_hresult()), "label blocks Low");
            assert!(open(&ours).is_ok(), "without impersonation it opens");
        }

        #[test]
        fn only_a_label_below_medium_or_ours_is_a_squat() {
            const LOW: u32 = 0x1000;
            const MEDIUM: u32 = 0x2000;
            const HIGH: u32 = 0x3000;
            assert!(label_is_squat(LOW, MEDIUM));
            assert!(!label_is_squat(MEDIUM, HIGH), "a normal Taser, met by an elevated start");
            assert!(label_is_squat(LOW, HIGH));
            assert!(!label_is_squat(MEDIUM, MEDIUM));
            assert!(!label_is_squat(HIGH, HIGH));
        }

        #[test]
        fn an_unreadable_copy_counts_only_when_windows_proves_it_is_above_us() {
            use MutexState::{Denied, Genuine, Squatted};
            let never = || -> bool { panic!("no probe needed") };
            // A verified Taser always counts; nothing else needs a probe.
            assert!(counts_as_seen(Kind::Verified, Genuine, never));
            assert!(counts_as_seen(Kind::Verified, Squatted, never));
            assert!(counts_as_seen(Kind::Verified, Denied, never));
            assert!(!counts_as_seen(Kind::Other, Denied, never));
            // An elevated Taser, met by a normal start: its lock is shut
            // to us and Windows refuses our message to its window.
            assert!(counts_as_seen(Kind::OursUnreadable, Denied, || true));
            // A Low copy that shut its token: our message gets through.
            assert!(!counts_as_seen(Kind::OursUnreadable, Denied, || false));
            // An elevated Taser's lock is never one we can open.
            assert!(!counts_as_seen(Kind::OursUnreadable, Squatted, never));
            assert!(!counts_as_seen(Kind::OursUnreadable, Genuine, never));
        }

        #[test]
        fn the_probe_reaches_a_window_at_our_own_level() {
            let class = test_class("probe");
            let window = windows_of_class(&class, 1)[0];
            assert!(!uipi_blocks(window));
        }

        #[test]
        fn at_the_deadline_only_an_unused_foreign_lock_is_ignored() {
            use AtDeadline::{Dialog, Unguarded};
            use MutexState::{Denied, Genuine, Squatted};
            // An elevated Taser's lock, seen from a normal start.
            assert_eq!(at_deadline(Denied, true), Dialog);
            // A lock shut to us, and no Taser of ours anywhere.
            assert_eq!(at_deadline(Denied, false), Unguarded);
            assert_eq!(at_deadline(Squatted, false), Unguarded);
            // Someone squats the name, but a Taser of ours (unguarded
            // itself) already runs on the profile: not a second one.
            assert_eq!(at_deadline(Squatted, true), Dialog);
            assert_eq!(at_deadline(Genuine, true), Dialog);
            assert_eq!(at_deadline(Genuine, false), Dialog);
        }

        #[test]
        fn a_mutex_we_may_not_open_is_access_denied() {
            // What a normal start meets in an elevated Taser's lock, or in
            // a squatter's that shuts us out: it exists, and opening it is
            // denied (not any other error, which would skip the search).
            let name = HSTRING::from(format!("Local\\Taser-test-denied-{}", unsafe { GetCurrentProcessId() }));
            let _held = mutex_with("D:P(A;;GA;;;SY)", &name);
            let opened = create_mutex(&name).map(|(h, existed)| (Owned(h), existed));
            assert_eq!(opened.err().map(|e| e.code()), Some(ERROR_ACCESS_DENIED.to_hresult()));
        }

        #[test]
        fn a_mutex_made_at_a_lower_level_counts_as_squatted() {
            let name = HSTRING::from(format!("Local\\Taser-test-squat-{}", unsafe { GetCurrentProcessId() }));
            let reason = as_low(|| {
                // Made while Low: Windows labels it Low.
                let squat = Owned(unsafe { CreateMutexW(None, false, &name) }.expect("Low creates a mutex"));
                let _ = unsafe { windows::Win32::Security::RevertToSelf() };
                let (handle, existed) = create_mutex(&name).unwrap();
                let handle = Owned(handle);
                drop(squat);
                assert!(existed);
                squatted(handle.0)
            });
            let reason = reason.expect("a Low-labelled mutex is not ours");
            assert!(reason.contains("integrity label 0x1000"), "{reason}");
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub struct Guard;
    pub fn start(_: &std::path::Path, _: Option<&str>) -> Start {
        Start::First(Guard)
    }
    pub fn listen(_: &std::path::Path, _: crate::native::Proxy) -> Result<(), std::io::Error> {
        Ok(())
    }
    pub fn bring_forward(window: &tao::window::Window) {
        window.set_minimized(false);
        window.set_visible(true);
    }
}
#[cfg(not(windows))]
pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn the_same_address_again_within_the_window_is_a_repeat() {
        let t0 = std::time::Instant::now();
        let mut r = Repeats::default();
        assert!(r.fresh("https://a.example/", t0));
        assert!(!r.fresh("https://a.example/", t0 + REPEAT_WINDOW / 2), "late copy of the same start");
        assert!(r.fresh("https://b.example/", t0 + REPEAT_WINDOW / 2), "another address");
        assert!(r.fresh("https://a.example/", t0 + REPEAT_WINDOW / 2), "after another one, not a repeat");
        assert!(r.fresh("https://a.example/", t0 + REPEAT_WINDOW * 2), "a later start of the same address");
    }

    #[test]
    fn valid_and_empty_payloads_are_taken() {
        assert_eq!(decode_payload(OPEN, &utf16("https://example.com/?a=1")), Ok(Some("https://example.com/?a=1".into())));
        assert_eq!(decode_payload(OPEN, &[]), Ok(None));
        // As in the address bar: a bare host gets https, free text a search.
        assert_eq!(decode_payload(OPEN, &utf16("example.com")), Ok(Some("https://example.com".into())));
        let search = decode_payload(OPEN, &utf16("kedi videoları")).unwrap().unwrap();
        assert!(search.starts_with(url::SEARCH_URL), "{search}");
    }

    #[test]
    fn malformed_payloads_are_refused() {
        let ok = utf16("https://example.com/");
        assert_eq!(decode_payload(2, &ok), Err(Reject::Kind));
        assert_eq!(decode_payload(0, &[]), Err(Reject::Kind));
        assert_eq!(decode_payload(OPEN, &ok[..ok.len() - 1]), Err(Reject::OddLength));
        assert_eq!(decode_payload(OPEN, &vec![b'a'; MAX_PAYLOAD + 2]), Err(Reject::TooLong));
        assert_eq!(decode_payload(OPEN, &utf16("https://exa\0mple.com/")), Err(Reject::Nul));
        assert_eq!(decode_payload(OPEN, &[0x00, 0xD8, b'a', 0]), Err(Reject::BadUtf16), "lone surrogate");
        assert_eq!(decode_payload(OPEN, &utf16("javascript:alert(1)")), Err(Reject::NotWeb));
        assert_eq!(decode_payload(OPEN, &utf16("file:///C:/Windows/win.ini")), Err(Reject::NotWeb));
        assert_eq!(decode_payload(OPEN, &utf16("C:\\Windows\\win.ini")), Err(Reject::NotWeb));
        assert_eq!(decode_payload(OPEN, &utf16("   ")), Err(Reject::NotWeb));
        let long = format!("https://example.com/{}", "a".repeat(url::MAX_URL_LEN));
        assert_eq!(decode_payload(OPEN, &utf16(&long)), Err(Reject::TooLong));
    }

    #[test]
    fn only_openable_arguments_are_sent() {
        assert_eq!(payload(Some("https://example.com/")), utf16("https://example.com/"));
        assert_eq!(payload(Some("javascript:alert(1)")), Vec::<u8>::new());
        assert_eq!(payload(Some("file:///C:/x")), Vec::<u8>::new());
        assert_eq!(payload(None), Vec::<u8>::new());
        // The browser's own UI page is never opened in a tab.
        assert_eq!(url_from_arg("http://xb.ui/index.html"), None);
        assert_eq!(payload(Some("http://xb.ui/index.html")), Vec::<u8>::new());
        // What is sent always passes the receiver's checks.
        for arg in ["https://example.com/", "example.com", "iki kelime"] {
            assert!(decode_payload(OPEN, &payload(Some(arg))).unwrap().is_some(), "{arg}");
        }
    }

    #[test]
    fn profile_hash_ignores_case_and_differs_per_folder() {
        let a = profile_hash(std::path::Path::new(r"C:\Users\x\AppData\Local\Taser"));
        assert_eq!(a.len(), 16);
        assert_eq!(a, profile_hash(std::path::Path::new(r"c:\users\X\appdata\local\taser")));
        assert_ne!(a, profile_hash(std::path::Path::new(r"C:\tmp\selftest\profile")));
    }
}
