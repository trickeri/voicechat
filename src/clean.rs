//! Light transcript cleanup applied to every dictation before delivery.
//!
//! Strips spoken filler words ("um", "uh", …) the way Windows' Win+H voice typing does, then
//! tidies the spacing and leading capitalization the removal leaves behind. This runs only on
//! what voicechat hands off to the focused app — the raw STT transcript is untouched, so other
//! backends and the mic-track captioning path still see verbatim text.
//!
//! The filler list is overridable with `VOICECHAT_FILLERS` (comma/whitespace-separated). Set it
//! to empty (`VOICECHAT_FILLERS=`) to disable stripping entirely.

/// Default filler words removed from dictation. Deliberately conservative: only sounds that are
/// almost never meaningful content. Matched whole-word, case-insensitively, ignoring any
/// surrounding punctuation (so "Um,", "uh.", "UH" all match).
const DEFAULT_FILLERS: &[&str] = &["um", "umm", "uh", "uhm", "erm", "er", "hmm"];

/// The active filler set (lowercased). From `VOICECHAT_FILLERS` if set, else the defaults.
fn fillers() -> Vec<String> {
    match std::env::var("VOICECHAT_FILLERS") {
        Ok(v) => v
            .split([',', ' ', '\t'])
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
        Err(_) => DEFAULT_FILLERS.iter().map(|s| s.to_string()).collect(),
    }
}

/// True if `tok` is a filler word: its alphanumeric core (surrounding punctuation trimmed),
/// lowercased, matches the set. Pure-punctuation tokens have an empty core and never match.
fn is_filler(tok: &str, set: &[String]) -> bool {
    let core = tok
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();
    !core.is_empty() && set.iter().any(|f| *f == core)
}

/// Uppercase the first alphabetic character (a leading filler like "Uh checking" leaves the new
/// first word lowercased; sentence-internal capitals are already correct, so only the first
/// matters). Everything else is left exactly as-is.
fn capitalize_first(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut done = false;
    for ch in s.chars() {
        if !done && ch.is_alphabetic() {
            out.extend(ch.to_uppercase());
            done = true;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Strip filler words from a transcript and tidy the spacing/capitalization the removal leaves
/// behind. May return an empty string if the whole utterance was filler (the caller treats an
/// empty result the same as an empty transcript).
pub fn strip_fillers(text: &str) -> String {
    let set = fillers();
    if set.is_empty() {
        return text.trim().to_string();
    }
    let joined = text
        .split_whitespace()
        .filter(|tok| !is_filler(tok, &set))
        .collect::<Vec<_>>()
        .join(" ");
    capitalize_first(joined.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        // Ensure tests use the defaults, not an inherited env override.
        std::env::remove_var("VOICECHAT_FILLERS");
        strip_fillers(s)
    }

    #[test]
    fn removes_leading_filler_and_recapitalizes() {
        assert_eq!(strip("Uh checking, making sure this works."),
                   "Checking, making sure this works.");
    }

    #[test]
    fn removes_midsentence_filler_with_attached_comma() {
        assert_eq!(strip("Hey, um a Claude crashed"), "Hey, a Claude crashed");
        assert_eq!(strip("so um, yeah"), "So yeah");
    }

    #[test]
    fn keeps_real_words_that_contain_filler_letters() {
        // "summer", "another", "her", "ermine" must survive — only whole-word fillers go.
        assert_eq!(strip("another summer her umbrella"), "Another summer her umbrella");
    }

    #[test]
    fn all_filler_collapses_to_empty() {
        assert_eq!(strip("um uh, um"), "");
    }

    #[test]
    fn disabled_when_set_empty() {
        std::env::set_var("VOICECHAT_FILLERS", "");
        assert_eq!(strip_fillers("um hello"), "um hello");
        std::env::remove_var("VOICECHAT_FILLERS");
    }
}
