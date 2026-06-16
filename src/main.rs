//! voicechat — headless dictation daemon. No window.
//!
//!   voicechat              run the daemon (foreground; use the systemd --user unit normally)
//!   voicechat toggle       toggle listening on the running daemon (taskbar hex / Meta+Escape)
//!   voicechat start|stop   aliases for toggle (the daemon flips its own state)
//!
//! Flow: toggle -> capture mic (stream `level` to the taskbar) -> toggle again ->
//! whispervulkan -> clipboard + smart paste -> idle.

mod capture;
mod paste;
mod state;
mod stt;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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

struct Session {
    cap: capture::Capture,
    writer_running: Arc<AtomicBool>,
    writer: Option<thread::JoinHandle<()>>,
}

fn run_daemon() {
    // Single-instance pid file.
    let _ = std::fs::write(pid_file(), std::process::id().to_string());
    state::write("idle", 0.0);
    eprintln!("voicechat: daemon up (pid {}), endpoint {}", std::process::id(), stt::endpoint());

    let (tx, rx) = mpsc::channel::<i32>();
    let mut signals = signal_hook::iterator::Signals::new([SIGUSR1, SIGTERM, SIGINT])
        .expect("register signals");
    thread::spawn(move || {
        for sig in signals.forever() {
            let _ = tx.send(sig);
        }
    });

    let mut session: Option<Session> = None;

    for sig in rx {
        match sig {
            SIGUSR1 => {
                if session.is_none() {
                    match start_listening() {
                        Ok(s) => {
                            session = Some(s);
                            eprintln!("voicechat: listening");
                        }
                        Err(e) => {
                            eprintln!("voicechat: capture failed: {e}");
                            flash_error();
                        }
                    }
                } else {
                    let s = session.take().unwrap();
                    stop_and_process(s);
                }
            }
            SIGTERM | SIGINT => {
                eprintln!("voicechat: shutting down");
                if let Some(s) = session.take() {
                    s.writer_running.store(false, Ordering::Relaxed);
                    if let Some(h) = s.writer {
                        let _ = h.join();
                    }
                }
                let _ = std::fs::remove_file(pid_file());
                state::write("idle", 0.0);
                break;
            }
            _ => {}
        }
    }
}

fn start_listening() -> Result<Session, String> {
    let cap = capture::start()?;
    state::write("listening", 0.0);
    play_sound("VOICECHAT_SOUND_START");

    // Stream the mic level to the state file ~30 Hz for the taskbar's reactive bars.
    let running = Arc::new(AtomicBool::new(true));
    let level = cap.level.clone();
    let run_flag = running.clone();
    let writer = thread::spawn(move || {
        while run_flag.load(Ordering::Relaxed) {
            state::write("listening", capture::level_of(&level));
            thread::sleep(Duration::from_millis(33));
        }
    });

    Ok(Session {
        cap,
        writer_running: running,
        writer: Some(writer),
    })
}

fn stop_and_process(mut s: Session) {
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

/// Play a notification sound (non-blocking) if the given env var points at an audio file.
/// e.g. VOICECHAT_SOUND_START / VOICECHAT_SOUND_STOP.
fn play_sound(env_var: &str) {
    if let Ok(path) = std::env::var(env_var) {
        if !path.is_empty() && std::path::Path::new(&path).exists() {
            if let Ok(child) = std::process::Command::new("pw-play").arg(&path).spawn() {
                thread::spawn(move || {
                    let mut c = child;
                    let _ = c.wait();
                });
            }
        }
    }
}

/// Briefly show an error on the taskbar hex, then settle back to idle.
fn flash_error() {
    state::write("error", 0.0);
    thread::sleep(Duration::from_millis(1100));
    state::write("idle", 0.0);
}
