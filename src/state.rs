//! Generic status/event writer for external voicechat listeners.
//!
//! The daemon does not own any UI. It publishes small JSON status messages that
//! taskbars, widgets, scripts, or visualizers can consume however they like.
//!
//! Defaults:
//! - current status: `~/.cache/voicechat/status.json`
//! - legacy/current-status alias: `~/.cache/voicechat/state.json`
//! - transition log: `~/.cache/voicechat/events.jsonl`
//!
//! Status shape: `{ "state": "idle|listening|processing|done|error", "level": 0.0, "ts": <epoch> }`

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LAST_STATE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

pub fn cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    base.join("voicechat")
}

fn configured_path(env_name: &str, default_name: &str) -> PathBuf {
    std::env::var(env_name)
        .map(PathBuf::from)
        .unwrap_or_else(|_| cache_dir().join(default_name))
}

pub fn status_path() -> PathBuf {
    configured_path("VOICECHAT_STATUS_FILE", "status.json")
}

fn legacy_state_path() -> PathBuf {
    cache_dir().join("state.json")
}

fn events_path() -> PathBuf {
    configured_path("VOICECHAT_EVENTS_FILE", "events.jsonl")
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn json_body(state: &str, level: f32, ts: f64) -> String {
    format!(
        "{{\"state\":\"{}\",\"level\":{:.4},\"ts\":{:.3}}}",
        state, level, ts
    )
}

fn write_atomic(path: PathBuf, body: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    if let Ok(mut f) = fs::File::create(&tmp) {
        if f.write_all(body.as_bytes()).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }
}

fn append_transition_event(state: &str, body: &str) {
    let lock = LAST_STATE.get_or_init(|| Mutex::new(None));
    let Ok(mut last) = lock.lock() else { return; };
    if last.as_deref() == Some(state) {
        return;
    }
    *last = Some(state.to_string());

    let path = events_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{body}");
    }
}

/// Publish the current daemon status. External visualizers can poll the status file;
/// event consumers can tail the JSONL transition log.
pub fn write(state: &str, level: f32) {
    let ts = now_secs();
    let body = json_body(state, level, ts);
    write_atomic(status_path(), &body);
    // Backward-compatible alias for existing local consumers. New consumers should read
    // VOICECHAT_STATUS_FILE or ~/.cache/voicechat/status.json.
    write_atomic(legacy_state_path(), &body);
    append_transition_event(state, &body);
}
