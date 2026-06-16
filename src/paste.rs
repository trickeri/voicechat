//! Clipboard + smart paste. Copies with PERSISTENT wl-copy (never `-o`/one-shot, which
//! clears the selection after the first paste), detects whether the focused window is
//! ghostty (via the active-window file the taskbar writes), and pastes with ydotool:
//! Ctrl+Shift+V in ghostty (terminal paste), Ctrl+V everywhere else.

use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::state;

// evdev keycodes for ydotool
const KEY_LEFTCTRL: &str = "29";
const KEY_LEFTSHIFT: &str = "42";
const KEY_V: &str = "47";

fn active_window_file() -> PathBuf {
    state::cache_dir().join("active-window")
}

/// True if the currently-focused window looks like ghostty (per the taskbar's hint file).
fn focused_is_ghostty() -> bool {
    std::fs::read_to_string(active_window_file())
        .map(|s| s.to_lowercase().contains("ghostty"))
        .unwrap_or(false)
}

fn ydotool_socket() -> String {
    std::env::var("YDOTOOL_SOCKET").unwrap_or_else(|_| {
        let run = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
        format!("{run}/.ydotool_socket")
    })
}

fn ydotool_key(seq: &[&str]) -> Result<(), String> {
    Command::new("ydotool")
        .arg("key")
        .args(seq)
        .env("YDOTOOL_SOCKET", ydotool_socket())
        .status()
        .map_err(|e| format!("ydotool: {e}"))
        .and_then(|s| if s.success() { Ok(()) } else { Err("ydotool exited non-zero".into()) })
}

/// Copy `text` to the clipboard (persistent) and paste it into the focused window.
pub fn copy_and_paste(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    // Persistent wl-copy: it daemonizes and keeps serving the clipboard, so the paste
    // below does NOT clear it; a later copy just overrides it. Reap it when it's
    // eventually displaced by a future copy (otherwise it lingers as a zombie).
    let child = Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .spawn()
        .map_err(|e| format!("wl-copy: {e}"))?;
    thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });

    // Wait until wl-copy has ACTUALLY taken ownership and the clipboard reports our text,
    // before synthesizing the paste. Otherwise we race the previous owner (e.g. an image)
    // and the app pastes the stale content instead. Poll up to ~1.5 s.
    let want = text.trim();
    let mut owned = false;
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(30));
        if let Ok(out) = Command::new("wl-paste").arg("--no-newline").output() {
            if String::from_utf8_lossy(&out.stdout).trim() == want {
                owned = true;
                break;
            }
        }
    }
    if !owned {
        eprintln!("voicechat: clipboard didn't settle to our text; pasting anyway");
    }

    // Safety/testing escape hatch: copy only, don't synthesize keystrokes.
    if std::env::var("VOICECHAT_DRY_PASTE").is_ok() {
        eprintln!("voicechat: dry paste (clipboard set, no keystroke)");
        return Ok(());
    }

    let ghostty = focused_is_ghostty();
    // key down in order, then key up in reverse: 29:1 [42:1] 47:1 47:0 [42:0] 29:0
    let seq: Vec<String> = if ghostty {
        vec![
            format!("{KEY_LEFTCTRL}:1"),
            format!("{KEY_LEFTSHIFT}:1"),
            format!("{KEY_V}:1"),
            format!("{KEY_V}:0"),
            format!("{KEY_LEFTSHIFT}:0"),
            format!("{KEY_LEFTCTRL}:0"),
        ]
    } else {
        vec![
            format!("{KEY_LEFTCTRL}:1"),
            format!("{KEY_V}:1"),
            format!("{KEY_V}:0"),
            format!("{KEY_LEFTCTRL}:0"),
        ]
    };
    let refs: Vec<&str> = seq.iter().map(|s| s.as_str()).collect();
    ydotool_key(&refs)
}
