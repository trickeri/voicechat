//! Transcript broadcast bus — a Unix domain socket that every final transcript is pushed to,
//! as one newline-delimited JSON object per transcript:
//!
//! ```json
//! {"text":"hello world","app":"kdenlive","mode":"emit","ts":1718755200.123}
//! ```
//!
//! Any application can `connect()` to the socket and read lines to "tap into" voicechat — most
//! relevantly the forked apps (Kdenlive/Krita/OBS) that run in `emit` mode and consume the
//! transcript directly instead of having it pasted. The broadcast is unconditional: transcripts
//! in *every* mode are sent, so observers/loggers see paste-mode dictation too. The per-app
//! mode (see `rules.rs`) only governs voicechat's local paste/clipboard action.
//!
//! Socket path: `VOICECHAT_SOCKET`, else `$XDG_RUNTIME_DIR/voicechat.sock`. The bus is
//! best-effort: if it can't bind, it degrades to a no-op so dictation still works.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

/// Broadcasts transcripts to all connected socket clients.
#[derive(Clone)]
pub struct Bus {
    clients: Arc<Mutex<Vec<UnixStream>>>,
}

pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("VOICECHAT_SOCKET") {
        return PathBuf::from(p);
    }
    let run = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    PathBuf::from(run).join("voicechat.sock")
}

/// Minimal JSON string escaping (quotes, backslashes, control chars) — enough for transcript
/// text and app ids without pulling in a JSON crate.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

impl Bus {
    /// Bind the socket and start accepting clients. Returns a (cloneable) handle. On failure
    /// (e.g. the socket is unavailable) it logs and returns a handle whose `broadcast` is a
    /// no-op, so the daemon keeps working.
    pub fn start() -> Bus {
        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
        let path = socket_path();
        // Clear a stale socket file from a previous run before binding.
        let _ = std::fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => {
                eprintln!("voicechat: transcript socket at {}", path.display());
                let accept_clients = clients.clone();
                thread::spawn(move || {
                    for stream in listener.incoming() {
                        match stream {
                            Ok(s) => {
                                if let Ok(mut list) = accept_clients.lock() {
                                    list.push(s);
                                }
                            }
                            Err(e) => eprintln!("voicechat: socket accept failed: {e}"),
                        }
                    }
                });
            }
            Err(e) => {
                eprintln!("voicechat: could not bind {} ({e}); transcript socket disabled", path.display());
            }
        }
        Bus { clients }
    }

    /// Broadcast one transcript to every connected client. Clients whose write fails (i.e. they
    /// disconnected) are dropped.
    pub fn broadcast(&self, text: &str, app: &str, mode: &str) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let line = format!(
            "{{\"text\":\"{}\",\"app\":\"{}\",\"mode\":\"{}\",\"ts\":{:.3}}}\n",
            json_escape(text),
            json_escape(app),
            json_escape(mode),
            ts
        );
        let Ok(mut clients) = self.clients.lock() else { return };
        clients.retain_mut(|c| c.write_all(line.as_bytes()).and_then(|_| c.flush()).is_ok());
    }
}
