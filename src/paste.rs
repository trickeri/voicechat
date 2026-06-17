//! Clipboard + smart paste. Copies with PERSISTENT wl-copy (never `-o`/one-shot, which
//! clears the selection after the first paste), optionally detects the focused app id
//! from a user-provided active-window file, and synthesizes the paste shortcut with
//! ydotool.
//!
//! The shortcut is configurable: `VOICECHAT_PASTE_KEY` is the default combo (default
//! `ctrl+v`). `VOICECHAT_PASTE_RULES` is a `;`-separated list of `app-substring=combo`
//! per-application overrides, matched (case-insensitively) against the focused app id;
//! it defaults to `ghostty=ctrl+shift+v` (terminals paste with Ctrl+Shift+V). Combos are
//! written like `ctrl+shift+v` / `shift+insert`.

use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

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

/// The paste combo for the focused app: the first matching `VOICECHAT_PASTE_RULES` entry,
/// else `VOICECHAT_PASTE_KEY`, else `ctrl+v`. `focus` is expected lowercased.
fn paste_combo_for(focus: &str) -> String {
    let rules = std::env::var("VOICECHAT_PASTE_RULES")
        .unwrap_or_else(|_| "ghostty=ctrl+shift+v".to_string());
    for rule in rules.split(';') {
        if let Some((pat, combo)) = rule.split_once('=') {
            let pat = pat.trim().to_lowercase();
            if !pat.is_empty() && focus.contains(&pat) {
                return combo.trim().to_string();
            }
        }
    }
    std::env::var("VOICECHAT_PASTE_KEY").unwrap_or_else(|_| "ctrl+v".to_string())
}

fn active_window_file() -> PathBuf {
    std::env::var("VOICECHAT_ACTIVE_WINDOW_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| state::cache_dir().join("active-window"))
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

    // If an external focus listener reports no real app focused (for example a desktop
    // shell), DON'T synthesize a paste. Leave it on the clipboard for manual paste.
    let focus = std::fs::read_to_string(active_window_file())
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if focus.is_empty() || focus.contains("plasmashell") {
        eprintln!("voicechat: no app focused (desktop) — left on clipboard, not pasting");
        return Ok(());
    }

    let combo = paste_combo_for(&focus);
    let seq = build_seq(&combo).unwrap_or_else(|| {
        eprintln!("voicechat: unrecognized paste combo '{combo}', using ctrl+v");
        build_seq("ctrl+v").unwrap()
    });
    let refs: Vec<&str> = seq.iter().map(|s| s.as_str()).collect();
    ydotool_key(&refs)
}
