//! Per-application routing rules — what voicechat does with a transcript based on the
//! focused app. Read from a plain-text rules file so users (and downstream forks) can add
//! their own app behaviors without recompiling, and edits take effect on the next dictation
//! (the file is re-read per transcript).
//!
//! Rules file (default `~/.config/voicechat/rules.conf`, override `VOICECHAT_RULES_FILE`):
//! one rule per line, whitespace-separated columns, `#` for comments, first match wins:
//!
//! ```text
//! # pattern      mode        option
//! ghostty        paste       ctrl+shift+v
//! kdenlive       emit
//! krita          emit
//! obs            emit
//! plasmashell    clipboard
//! *              paste                       # catch-all default
//! ```
//!
//! `pattern` is a case-insensitive substring matched against the focused app id (`*` matches
//! anything). `mode` is one of:
//!   - `paste`     — copy to the clipboard AND synthesize a paste keystroke. The optional
//!                   third column is the key combo (default `VOICECHAT_PASTE_KEY`, else `ctrl+v`).
//!   - `clipboard` — copy to the clipboard only; no keystroke (good for the desktop shell or
//!                   apps where a synthesized paste would misfire).
//!   - `emit`      — don't paste; the transcript is delivered over the broadcast socket only
//!                   (it's still copied to the clipboard as a fallback so nothing is lost).
//!                   This is the mode for forked apps that consume voicechat output directly.
//!
//! Regardless of mode, every transcript is broadcast on the socket (see `bus.rs`), so any app
//! can tap in. The mode only governs voicechat's *local* action.

use std::path::PathBuf;

/// What to do locally with a transcript for the focused app.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Copy + synthesize the paste keystroke with this combo.
    Paste { combo: String },
    /// Copy to the clipboard only; no keystroke.
    Clipboard,
    /// Don't paste; deliver over the broadcast socket only (still copied as a fallback).
    Emit,
    /// No window focused (the desktop): a system-wide voice command. No paste, no clipboard
    /// copy — only the broadcast, for a system command service to consume. Distinct from `emit`
    /// so per-app consumers (which act on `emit`) never grab a desktop command.
    System,
}

impl Mode {
    /// Short label for the socket payload / logs.
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Paste { .. } => "paste",
            Mode::Clipboard => "clipboard",
            Mode::Emit => "emit",
            Mode::System => "system",
        }
    }
}

fn default_combo() -> String {
    std::env::var("VOICECHAT_PASTE_KEY").unwrap_or_else(|_| "ctrl+v".to_string())
}

fn rules_file() -> PathBuf {
    if let Ok(p) = std::env::var("VOICECHAT_RULES_FILE") {
        return PathBuf::from(p);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    base.join("voicechat").join("rules.conf")
}

/// Parse a single `mode [option]` into a `Mode`. Unknown modes fall back to paste so a typo
/// never silently swallows dictation.
fn parse_mode(mode: &str, option: Option<&str>) -> Mode {
    match mode.to_ascii_lowercase().as_str() {
        "clipboard" | "copy" => Mode::Clipboard,
        "emit" | "send" => Mode::Emit,
        "system" => Mode::System,
        "paste" => Mode::Paste {
            combo: option.map(str::to_string).unwrap_or_else(default_combo),
        },
        other => {
            eprintln!("voicechat: unknown rule mode '{other}', treating as paste");
            Mode::Paste {
                combo: option.map(str::to_string).unwrap_or_else(default_combo),
            }
        }
    }
}

/// Resolve the mode for a focused app id from the rules file, if one exists. `focus` is
/// expected lowercased. Returns None when there is no rules file (caller uses built-in
/// defaults), or when the file has no matching rule.
fn from_file(focus: &str) -> Option<Mode> {
    let text = std::fs::read_to_string(rules_file()).ok()?;
    let mut catch_all: Option<Mode> = None;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split_whitespace();
        let Some(pat) = cols.next() else { continue };
        let mode = cols.next().unwrap_or("paste");
        let option = cols.next();
        let parsed = parse_mode(mode, option);
        if pat == "*" {
            catch_all.get_or_insert(parsed);
            continue;
        }
        if focus.contains(&pat.to_ascii_lowercase()) {
            return Some(parsed);
        }
    }
    catch_all
}

/// Built-in defaults used when there is no rules file, preserving voicechat's historical
/// behavior: the desktop shell / empty focus copies only, terminals (and any legacy
/// `VOICECHAT_PASTE_RULES` entries) paste with their combo, everything else uses the default
/// paste combo.
fn builtin(focus: &str) -> Mode {
    if focus.is_empty() || focus.contains("plasmashell") {
        return Mode::Clipboard;
    }
    // Legacy env rules: `;`-separated app-substring=combo (defaults to ghostty=ctrl+shift+v).
    let rules = std::env::var("VOICECHAT_PASTE_RULES")
        .unwrap_or_else(|_| "ghostty=ctrl+shift+v".to_string());
    for rule in rules.split(';') {
        if let Some((pat, combo)) = rule.split_once('=') {
            let pat = pat.trim().to_ascii_lowercase();
            if !pat.is_empty() && focus.contains(&pat) {
                return Mode::Paste { combo: combo.trim().to_string() };
            }
        }
    }
    Mode::Paste { combo: default_combo() }
}

/// Resolve the mode for the focused app. `focus` is expected lowercased. An empty focus (no
/// window focused — the desktop) maps to `System`: voicechat never pastes into nothing, and the
/// transcript is delivered only over the broadcast socket for a system-wide command service to
/// consume.
pub fn resolve(focus: &str) -> Mode {
    if focus.is_empty() {
        return Mode::System;
    }
    from_file(focus).unwrap_or_else(|| builtin(focus))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Point the resolver at a throwaway rules file and exercise matching/precedence. Env is
    // process-global, so keep this to a single test (cargo runs test fns in parallel threads).
    #[test]
    fn rules_file_routing() {
        let dir = std::env::temp_dir().join(format!("vc-rules-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rules.conf");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            "# a comment\n\
             Ghostty   paste   ctrl+shift+v\n\
             kdenlive  emit\n\
             plasmashell clipboard\n\
             kde       emit            # earlier-but-after kdenlive; first match wins\n\
             *         paste   ctrl+v   # catch-all\n"
        )
        .unwrap();
        std::env::set_var("VOICECHAT_RULES_FILE", &path);

        // Case-insensitive substring + per-app combo.
        assert_eq!(
            resolve("com.mitchellh.ghostty"),
            Mode::Paste { combo: "ctrl+shift+v".into() }
        );
        // emit mode, no option.
        assert_eq!(resolve("org.kde.kdenlive"), Mode::Emit);
        // clipboard mode.
        assert_eq!(resolve("plasmashell"), Mode::Clipboard);
        // First match wins: "org.kde.kdenlive" hits the kdenlive rule, not the later "kde" one
        // (already covered above); a bare kde app hits the kde emit rule before catch-all.
        assert_eq!(resolve("org.kde.dolphin"), Mode::Emit);
        // Catch-all for anything unmatched.
        assert_eq!(
            resolve("com.example.editor"),
            Mode::Paste { combo: "ctrl+v".into() }
        );
        // Empty focus (the desktop) is a system command, never a paste.
        assert_eq!(resolve(""), Mode::System);

        std::env::remove_var("VOICECHAT_RULES_FILE");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
