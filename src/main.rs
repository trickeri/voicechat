//! voicechat — headless dictation daemon. No window.
//!
//!   voicechat              run the daemon (foreground; use the systemd --user unit normally)
//!   voicechat toggle       toggle listening on the running daemon (taskbar hex / Meta+Escape)
//!   voicechat start|stop   aliases for toggle (the daemon flips its own state)
//!
//! Flow: toggle -> capture mic (publish generic JSON status) -> toggle again ->
//! whispervulkan -> clipboard + smart paste -> idle.

mod capture;
mod keywatch;
mod paste;
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
                    stop_and_process(s); // normal hotkey stop — no guard set
                } else if last_keystop.map_or(true, |t| t.elapsed() >= ECHO_GUARD) {
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
                    stop_and_process(s);
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

fn stop_and_process(mut s: Session) {
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

    if samples.len() < sample_rate as usize / 5 {
        // < ~0.2 s captured — treat as a no-op tap.
        eprintln!("voicechat: too little audio, ignoring");
        state::write("idle", 0.0);
        return;
    }

    state::write("processing", 0.0);
    match stt::transcribe(&samples, sample_rate) {
        Ok(text) if !text.is_empty() => {
            eprintln!("voicechat: \"{text}\"");
            if let Err(e) = paste::copy_and_paste(&text) {
                eprintln!("voicechat: paste failed: {e}");
            }
            state::write("done", 0.0);
            thread::sleep(Duration::from_millis(250));
            state::write("idle", 0.0);
        }
        Ok(_) => {
            eprintln!("voicechat: empty transcript");
            flash_error();
        }
        Err(e) => {
            eprintln!("voicechat: transcribe failed: {e}");
            flash_error();
        }
    }
}

/// Play a notification sound (non-blocking) if `env_var` points at an existing audio file.
/// Disabled unless the env var is set — voicechat.service / config.env.example carry
/// commented lines that point at the placeholder sounds shipped in `sounds/`; users
/// replace those files (or point elsewhere) and uncomment to enable.
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
