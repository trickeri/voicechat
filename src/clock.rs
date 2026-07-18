//! Wall-clock timestamps for the dictation diagnostics.
//!
//! journald already prefixes each log line with a receive time, but that's when the line
//! was *written*, not when the event happened, and it isn't visible if you read the logs
//! any other way. These stamps are self-contained local "HH:MM:SS.mmm" times you can line
//! up against the exact moment you *felt* the hotkey fire — which is the whole point of
//! chasing a "it cut off the end" cutoff. Dep-free: libc (already a dependency) via
//! localtime_r, so no chrono/time crate needed.

use std::time::{SystemTime, UNIX_EPOCH};

/// Local wall-clock as "HH:MM:SS.mmm". Falls back to raw "epoch.mmm" if the system clock
/// is somehow before the epoch or localtime_r fails (never fatal — this is only logging).
pub fn stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as libc::time_t;
    let millis = now.subsec_millis();
    // SAFETY: localtime_r is the thread-safe variant; we pass a valid time_t and a zeroed tm
    // it fills in. It returns a pointer into `tm` (or null on failure), which we only null-check.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if ok {
        format!(
            "{:02}:{:02}:{:02}.{:03}",
            tm.tm_hour, tm.tm_min, tm.tm_sec, millis
        )
    } else {
        format!("{}.{:03}", now.as_secs(), millis)
    }
}
