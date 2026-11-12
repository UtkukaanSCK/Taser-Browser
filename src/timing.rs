//! Startup measurement. With `XB_TIMING=<file>` set, appends one line per
//! milestone: `<label> <ms since the OS created the process>`. Costs nothing
//! when the variable is unset.

/// With `XB_TIMING_QUIT` also set, the app closes normally once the UI is
/// ready, so startup can be measured over many runs without killing it.
pub fn quit_when_ready() -> bool {
    std::env::var_os("XB_TIMING").is_some() && std::env::var_os("XB_TIMING_QUIT").is_some()
}

pub fn mark(label: &str) {
    let Some(path) = std::env::var_os("XB_TIMING") else { return };
    let Some(up) = crate::native::process_uptime() else { return };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{label} {:.1}", up.as_secs_f64() * 1000.0);
    }
}
