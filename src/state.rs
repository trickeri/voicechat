//! Atomic writer for ~/.cache/voicechat/state.json — the contract the taskbar
//! VoiceMonitor.qml reads (see trikeri_taskbar/VOICE_INDICATOR_PLAN.md).
//! Shape: { "state": "idle|listening|processing|done", "level": 0.0, "ts": <epoch> }

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn cache_dir() -> PathBuf {
    let base = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(base).join(".cache/voicechat")
}

fn state_path() -> PathBuf {
    cache_dir().join("state.json")
}

/// Write the state file atomically (temp + rename) so the taskbar never reads a torn file.
pub fn write(state: &str, level: f32) {
    let dir = cache_dir();
    let _ = fs::create_dir_all(&dir);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let body = format!(
        "{{\"state\":\"{}\",\"level\":{:.4},\"ts\":{:.3}}}",
        state, level, ts
    );
    let tmp = dir.join("state.json.tmp");
    if let Ok(mut f) = fs::File::create(&tmp) {
        if f.write_all(body.as_bytes()).is_ok() {
            let _ = fs::rename(&tmp, state_path());
        }
    }
}
