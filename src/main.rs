//! voicechat — headless dictation daemon. No window.
//!
//!   voicechat              run the daemon (foreground; use the systemd --user unit normally)
//!   voicechat toggle       toggle listening on the running daemon (taskbar hex / Meta+Escape)
//!   voicechat start|stop   aliases for toggle (the daemon flips its own state)
//!
//! Flow: toggle -> capture mic (publish generic JSON status) -> toggle again ->
//! whispermodel -> clipboard + smart paste -> idle.

mod bus;
mod capture;
mod clean;
mod keywatch;
mod paste;
mod rules;
mod state;
mod stt;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};

fn pid_file() -> PathBuf {
    let run = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    PathBuf::from(run).join("voicechat.pid")
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "toggle" | "start" | "stop" => send_toggle(),
        "" | "run" | "daemon" => run_daemon(),
        other => {
            eprintln!("voicechat: unknown command '{other}'. Use: toggle | run");
            std::process::exit(2);
        }
    }
}

/// CLI: signal the running daemon to toggle listening.
fn send_toggle() {
    let pid = std::fs::read_to_string(pid_file())
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok());
    match pid {
        Some(pid) => {
            // SAFETY: kill(2) with a parsed pid and a fixed signal.
            let rc = unsafe { libc::kill(pid, SIGUSR1) };
            if rc != 0 {
                eprintln!("voicechat: daemon (pid {pid}) not reachable — is it running?");
                std::process::exit(1);
            }
        }
        None => {
            eprintln!("voicechat: no running daemon (missing {:?}). Start the service first.", pid_file());
            std::process::exit(1);
        }
    }
}

/// What the daemon's main loop reacts to.
enum Ev {
    /// Push-to-talk hotkey (SIGUSR1): start if idle, stop+process if listening.
    Toggle,
    /// Any key was pressed while listening (the any-key-stop watcher): stop+process.
    KeyFinish,
    /// SIGTERM/SIGINT: shut down.
    Shutdown,
}

struct Session {
    cap: capture::Capture,
    writer_running: Arc<AtomicBool>,
    writer: Option<thread::JoinHandle<()>>,
    keywatch: Option<keywatch::KeyWatch>,
}

fn run_daemon() {
    // Single-instance pid file.
    let _ = std::fs::write(pid_file(), std::process::id().to_string());
    state::write("idle", 0.0);
    eprintln!("voicechat: daemon up (pid {}), endpoint {}", std::process::id(), stt::endpoint());

    // Transcript broadcast socket: every transcript is pushed here for any app to consume.
    let bus = bus::Bus::start();

    let (tx, rx) = mpsc::channel::<Ev>();
    let sig_tx = tx.clone();
    let mut signals = signal_hook::iterator::Signals::new([SIGUSR1, SIGTERM, SIGINT])
        .expect("register signals");
    thread::spawn(move || {
        for sig in signals.forever() {
            let ev = if sig == SIGUSR1 { Ev::Toggle } else { Ev::Shutdown };
            let _ = sig_tx.send(ev);
        }
    });

    let mut session: Option<Session> = None;
    // The push-to-talk hotkey is seen by both the global shortcut (-> Toggle) and the
    // any-key watcher (-> KeyFinish). When a KeyFinish stops a recording, swallow the
    // Toggle echo that lands right after so it doesn't immediately start a new one.
    let mut last_keystop: Option<Instant> = None;
    const ECHO_GUARD: Duration = Duration::from_millis(600);

    for ev in rx {
        match ev {
            Ev::Toggle => {
                if let Some(s) = session.take() {
                    // DIAG: a SIGUSR1 toggle is the *only* non-keyboard way a recording stops.
                    // If a cutoff shows this with no preceding any-key-stop line, a phantom
                    // toggle (taskbar hex, gesture daemon, stray `voicechat toggle`) ended it.
                    eprintln!("voicechat: stop reason = SIGUSR1 toggle (hotkey / taskbar / CLI)");
                    stop_and_process(s, &bus); // normal hotkey stop — no guard set
                } else if last_keystop.map_or(true, |t| t.elapsed() >= ECHO_GUARD) {
                    // No-focus routing: if nothing is focused (the desktop), this hotkey is an
                    // OS voice-command trigger, not dictation. Hand off to `voiceagent command`
                    // (which owns its own mic -> STT -> classify -> action) and do NOT start a
                    // recording here — otherwise we'd double-record. When a window IS focused,
                    // fall through to normal dictation below.
                    if no_window_focused() {
                        eprintln!("voicechat: no focus -> handing off to `voiceagent command`");
                        spawn_voiceagent_command();
                        continue;
                    }
                    match start_listening(tx.clone()) {
                        Ok(s) => {
                            session = Some(s);
                            eprintln!("voicechat: listening");
                        }
                        Err(e) => {
                            eprintln!("voicechat: capture failed: {e}");
                            flash_error();
                        }
                    }
                }
            }
            Ev::KeyFinish => {
                if let Some(s) = session.take() {
                    // DIAG: paired with the `any-key-stop fired on <Key>` line from keywatch,
                    // this confirms a keyboard key (not a signal) ended the recording.
                    eprintln!("voicechat: stop reason = any-key-stop (keyboard)");
                    stop_and_process(s, &bus);
                    last_keystop = Some(Instant::now());
                }
            }
            Ev::Shutdown => {
                eprintln!("voicechat: shutting down");
                if let Some(mut s) = session.take() {
                    if let Some(kw) = s.keywatch.take() {
                        kw.stop();
                    }
                    s.writer_running.store(false, Ordering::Relaxed);
                    if let Some(h) = s.writer {
                        let _ = h.join();
                    }
                }
                let _ = std::fs::remove_file(pid_file());
                let _ = std::fs::remove_file(bus::socket_path());
                state::write("idle", 0.0);
                break;
            }
        }
    }
}

fn start_listening(tx: mpsc::Sender<Ev>) -> Result<Session, String> {
    let cap = capture::start()?;
    state::write("listening", 0.0);
    play_sound("VOICECHAT_SOUND_START");

    // Publish the mic level to the status file ~30 Hz for any external visualizer.
    let running = Arc::new(AtomicBool::new(true));
    let level = cap.level.clone();
    let run_flag = running.clone();
    let writer = thread::spawn(move || {
        while run_flag.load(Ordering::Relaxed) {
            state::write("listening", capture::level_of(&level));
            thread::sleep(Duration::from_millis(33));
        }
    });

    // Any key (not just the hotkey) ends the recording, unless disabled.
    let keywatch = if keywatch::enabled() {
        keywatch::spawn(move || {
            let _ = tx.send(Ev::KeyFinish);
        })
    } else {
        None
    };

    Ok(Session {
        cap,
        writer_running: running,
        writer: Some(writer),
        keywatch,
    })
}

fn stop_and_process(mut s: Session, bus: &bus::Bus) {
    // Stop watching the keyboard first so stray keys during processing/paste are ignored.
    if let Some(kw) = s.keywatch.take() {
        kw.stop();
    }
    play_sound("VOICECHAT_SOUND_STOP");
    // Stop the level writer first so it can't overwrite the processing state.
    s.writer_running.store(false, Ordering::Relaxed);
    if let Some(h) = s.writer.take() {
        let _ = h.join();
    }

    let sample_rate = s.cap.sample_rate;
    let samples = s.cap.finish(); // stops parec and returns the recording
    // DIAG: how much audio we actually captured. If this is far shorter than how long you
    // spoke, capture stopped early (see capture.rs parec EOF logs); if it matches your speech
    // but still feels cut, the recording was stopped early by a signal/key (see stop reason).
    eprintln!(
        "voicechat: captured {:.1}s ({} samples)",
        samples.len() as f32 / sample_rate as f32,
        samples.len()
    );

    if samples.len() < sample_rate as usize / 5 {
        // < ~0.2 s captured — treat as a no-op tap.
        eprintln!("voicechat: too little audio, ignoring");
        state::write("idle", 0.0);
        return;
    }

    state::write("processing", 0.0);
    match stt::transcribe(&samples, sample_rate) {
        Ok(raw) => {
            // Strip spoken filler words ("um", "uh", …) before delivery — the raw transcript
            // stays verbatim on the bus/STT side; this only cleans what we paste. A pure-filler
            // utterance cleans to empty and is treated the same as an empty transcript.
            let text = clean::strip_fillers(&raw);
            if !text.is_empty() {
                eprintln!("voicechat: \"{text}\"");
                if let Err(e) = paste::deliver(&text, bus) {
                    eprintln!("voicechat: delivery failed: {e}");
                }
                state::write("done", 0.0);
                thread::sleep(Duration::from_millis(250));
                state::write("idle", 0.0);
            } else {
                eprintln!("voicechat: empty transcript");
                flash_error();
            }
        }
        Err(e) => {
            eprintln!("voicechat: transcribe failed: {e}");
            flash_error();
        }
    }
}

/// True when no real application window is focused (the desktop is "focused"): the focus hint
/// file is missing/empty, or names the shell (plasmashell). Mirrors the desktop/System detection
/// in `rules.rs` so the no-focus hotkey routing agrees with the paste guard. Unset/missing focus
/// file = treat as desktop (no focus), so the OS-command path is reachable without the optional
/// focus listener installed; install it to get true dictation-vs-command switching.
fn no_window_focused() -> bool {
    let file = std::env::var("VOICECHAT_ACTIVE_WINDOW_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| state::cache_dir().join("active-window"));
    let focus = std::fs::read_to_string(file)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    focus.is_empty() || focus.contains("plasmashell")
}

/// Launch the OS voice-command agent (`voiceagent command`) detached. Overridable via
/// `VOICECHAT_OSCOMMAND_CMD` (whitespace-split argv) for forks / testing.
fn spawn_voiceagent_command() {
    let cmd = std::env::var("VOICECHAT_OSCOMMAND_CMD").unwrap_or_default();
    let argv: Vec<String> = if cmd.trim().is_empty() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
        vec![format!("{home}/.local/bin/voiceagent"), "command".into()]
    } else {
        cmd.split_whitespace().map(str::to_string).collect()
    };
    let (prog, args) = argv.split_first().expect("non-empty argv");
    match std::process::Command::new(prog).args(args).spawn() {
        Ok(child) => {
            // Reap it so it doesn't linger as a zombie.
            thread::spawn(move || {
                let mut c = child;
                let _ = c.wait();
            });
        }
        Err(e) => eprintln!("voicechat: failed to launch `{prog}`: {e}"),
    }
}

/// Play a notification sound (non-blocking) if `env_var` points at an existing audio file.
/// voicechat.service / config.env.example ship these vars enabled by default, pointing at the
/// start/stop sounds in `sounds/`; users replace those files, point elsewhere, or comment the
/// lines out to go silent. Unset or missing-file = no sound (so a bad path is never fatal).
fn play_sound(env_var: &str) {
    let Ok(path) = std::env::var(env_var) else {
        return;
    };
    if path.is_empty() || !std::path::Path::new(&path).exists() {
        return;
    }
    if let Ok(child) = std::process::Command::new("pw-play").arg(&path).spawn() {
        thread::spawn(move || {
            let mut c = child;
            let _ = c.wait();
        });
    }
}

/// Briefly publish an error state, then settle back to idle.
fn flash_error() {
    state::write("error", 0.0);
    thread::sleep(Duration::from_millis(1100));
    state::write("idle", 0.0);
}
