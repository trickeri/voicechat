//! Transcript delivery: clipboard + smart paste + socket broadcast, routed per focused app.
//!
//! `deliver` is the entry point. It reads the focused app id (from a user-provided
//! active-window file), resolves a per-app `Mode` (see `rules.rs`), ALWAYS broadcasts the
//! transcript on the bus (see `bus.rs`) so any app can tap in, then performs the local action:
//!   - `Paste`     — copy with persistent wl-copy, then synthesize the app's paste combo.
//!   - `Clipboard` — copy only (e.g. the desktop shell), leave it for a manual paste.
//!   - `Emit`      — copy as a fallback but do NOT synthesize a keystroke; the forked app
//!                   consumes the broadcast instead.
//!
//! Copies use PERSISTENT wl-copy (never `-o`/one-shot, which clears the selection after the
//! first paste). The paste keystroke is synthesized with ydotool.

use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::bus::Bus;
use crate::rules::{self, Mode};
use crate::state;

/// Map a key/modifier name to its evdev keycode (as a string, for ydotool). Covers the
/// modifiers, a–z, and Insert — enough for paste shortcuts. Returns None for unknown names.
fn keycode(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "leftctrl" => "29",
        "shift" | "leftshift" => "42",
        "alt" | "leftalt" => "56",
        "super" | "meta" | "win" | "leftmeta" => "125",
        "a" => "30", "b" => "48", "c" => "46", "d" => "32", "e" => "18",
        "f" => "33", "g" => "34", "h" => "35", "i" => "23", "j" => "36",
        "k" => "37", "l" => "38", "m" => "50", "n" => "49", "o" => "24",
        "p" => "25", "q" => "16", "r" => "19", "s" => "31", "t" => "20",
        "u" => "22", "v" => "47", "w" => "17", "x" => "45", "y" => "21",
        "z" => "44",
        "insert" | "ins" => "110",
        _ => return None,
    })
}

/// Turn a combo like `ctrl+shift+v` into a ydotool key sequence: modifiers down in order,
/// then key down, key up, modifiers up in reverse. None if any token is unknown.
fn build_seq(combo: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).filter(|s| !s.is_empty()).collect();
    let (key, mods) = parts.split_last()?;
    let key_code = keycode(key)?;
    let mod_codes: Option<Vec<&str>> = mods.iter().map(|m| keycode(m)).collect();
    let mod_codes = mod_codes?;

    let mut seq = Vec::with_capacity(mod_codes.len() * 2 + 2);
    for c in &mod_codes {
        seq.push(format!("{c}:1"));
    }
    seq.push(format!("{key_code}:1"));
    seq.push(format!("{key_code}:0"));
    for c in mod_codes.iter().rev() {
        seq.push(format!("{c}:0"));
    }
    Some(seq)
}

fn active_window_file() -> PathBuf {
    std::env::var("VOICECHAT_ACTIVE_WINDOW_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| state::cache_dir().join("active-window"))
}

/// The focused app id (lowercased), from the user-provided active-window file. Empty when no
/// file / no app is focused.
fn read_focus() -> String {
    std::fs::read_to_string(active_window_file())
        .unwrap_or_default()
        .trim()
        .to_lowercase()
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

/// Copy `text` to the clipboard with a PERSISTENT wl-copy and wait until the compositor reports
/// our text is the selection (so a following paste doesn't race a previous owner). Returns
/// Ok once copied (whether or not ownership was confirmed within the timeout).
fn copy(text: &str) -> Result<(), String> {
    // Persistent wl-copy: it daemonizes and keeps serving the clipboard, so a later paste does
    // NOT clear it; a future copy just overrides it. Reap it when it's eventually displaced
    // (otherwise it lingers as a zombie).
    let child = Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .spawn()
        .map_err(|e| format!("wl-copy: {e}"))?;
    thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });

    // Wait until wl-copy has ACTUALLY taken ownership and the clipboard reports our text.
    // Otherwise we race the previous owner (e.g. an image) and the app pastes the stale
    // content instead. Poll up to ~1.5 s.
    let want = text.trim();
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(30));
        if let Ok(out) = Command::new("wl-paste").arg("--no-newline").output() {
            if String::from_utf8_lossy(&out.stdout).trim() == want {
                return Ok(());
            }
        }
    }
    eprintln!("voicechat: clipboard didn't settle to our text within timeout");
    Ok(())
}

/// Synthesize the paste keystroke for `combo`, falling back to `ctrl+v` on an unknown combo.
fn synthesize(combo: &str) -> Result<(), String> {
    let seq = build_seq(combo).unwrap_or_else(|| {
        eprintln!("voicechat: unrecognized paste combo '{combo}', using ctrl+v");
        build_seq("ctrl+v").unwrap()
    });
    let refs: Vec<&str> = seq.iter().map(|s| s.as_str()).collect();
    ydotool_key(&refs)
}

/// Route a finished transcript: resolve the focused app's mode, broadcast it on the bus
/// (always), then perform the local action for that mode.
pub fn deliver(text: &str, bus: &Bus) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let focus = read_focus();
    let mode = rules::resolve(&focus);
    let app = if focus.is_empty() { "none" } else { focus.as_str() };

    // Always broadcast so any app (forks, loggers, observers) can tap in, regardless of mode.
    bus.broadcast(text, app, mode.label());

    // Safety/testing escape hatch: never synthesize keystrokes, just copy.
    let dry = std::env::var("VOICECHAT_DRY_PASTE").is_ok();

    match mode {
        Mode::Paste { combo } => {
            copy(text)?;
            if dry {
                eprintln!("voicechat: dry paste (clipboard set, no keystroke)");
                return Ok(());
            }
            eprintln!("voicechat: paste -> {app} ({combo})");
            synthesize(&combo)
        }
        Mode::Clipboard => {
            copy(text)?;
            eprintln!("voicechat: clipboard only ({app}) — left for manual paste");
            Ok(())
        }
        Mode::Emit => {
            // Copy as a fallback so nothing is lost if no socket client is listening, but do
            // not synthesize a keystroke — the forked app consumes the broadcast.
            copy(text)?;
            eprintln!("voicechat: emit ({app}) — sent on socket, not pasting");
            Ok(())
        }
    }
}
