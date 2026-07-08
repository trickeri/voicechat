//! Optional "any key finishes dictation" watcher.
//!
//! While listening, voicechat watches the keyboard evdev devices directly; on the first
//! key press it fires a callback so the user can end a recording with *any* key, not just
//! the push-to-talk hotkey (the hotkey still works — it's seen here too, which the daemon
//! de-dupes). Reading `/dev/input/event*` needs membership in the `input` group; if no
//! device can be opened the watcher is a silent no-op and only the hotkey ends recording.
//!
//! Controlled by `VOICECHAT_ANYKEY_STOP` (default on; set `0`/`false`/`off` to disable).

use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use evdev::{Device, InputEventKind, Key};

/// Whether any-key-stop is enabled. Defaults to on; any of `0`/`false`/`no`/`off` disables.
pub fn enabled() -> bool {
    match std::env::var("VOICECHAT_ANYKEY_STOP") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// Handle to the running watchers. Drop-stop via `stop()` to join the threads.
pub struct KeyWatch {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl KeyWatch {
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        for h in self.handles {
            let _ = h.join();
        }
    }
}

/// Spawn a watcher thread per keyboard device. `on_key` is invoked exactly once, from a
/// watcher thread, on the first key press after a short grace period (so the hotkey that
/// *started* recording, if still settling, doesn't immediately stop it). Returns `None`
/// when no readable keyboard device is found (e.g. not in the `input` group).
pub fn spawn<F>(on_key: F) -> Option<KeyWatch>
where
    F: Fn() + Send + Clone + 'static,
{
    // evdev::enumerate() silently skips devices it can't open, so a missing `input` group
    // membership just yields an empty list here.
    let devices: Vec<Device> = evdev::enumerate()
        .map(|(_, d)| d)
        .filter(is_keyboard)
        .collect();
    if devices.is_empty() {
        eprintln!(
            "voicechat: any-key-stop off (no readable keyboard in /dev/input — add yourself \
             to the 'input' group, or set VOICECHAT_ANYKEY_STOP=0 to silence this)"
        );
        return None;
    }

    // Shared so the first key press on any device wins; each thread gets its own clone of the
    // callback (avoids requiring the callback be Sync — it captures an mpsc::Sender).
    let stop = Arc::new(AtomicBool::new(false));
    let fired = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();

    for mut dev in devices {
        let stop = stop.clone();
        let fired = fired.clone();
        let on_key = on_key.clone();
        handles.push(thread::spawn(move || {
            let fd = dev.as_raw_fd();
            let started = Instant::now();
            let grace = Duration::from_millis(250);
            while !stop.load(Ordering::Relaxed) {
                // poll with a short timeout so we re-check `stop` between reads.
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let rc = unsafe { libc::poll(&mut pfd, 1, 150) };
                if rc <= 0 {
                    continue; // timeout (0) or interrupted (<0): loop and re-check stop
                }
                let events = match dev.fetch_events() {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for ev in events {
                    // value 1 == key press (0 release, 2 autorepeat).
                    if let InputEventKind::Key(key) = ev.kind() {
                        if ev.value() == 1 && started.elapsed() >= grace {
                            if !fired.swap(true, Ordering::SeqCst) {
                                // DIAG: name the exact key that ended the recording so a
                                // mysterious "it cut off and I didn't press anything" can be
                                // traced to a real keypress (or ruled out entirely).
                                eprintln!("voicechat: any-key-stop fired on {key:?}");
                                on_key();
                            }
                            return;
                        }
                    }
                }
            }
        }));
    }

    Some(KeyWatch { stop, handles })
}

/// Heuristic: a real keyboard reports the Enter or Space key. Filters out mice, the power
/// button, and other EV_KEY-only devices so a mouse click doesn't end dictation.
fn is_keyboard(d: &Device) -> bool {
    d.supported_keys().map_or(false, |keys| {
        keys.contains(Key::KEY_ENTER) || keys.contains(Key::KEY_SPACE)
    })
}
