//! Mic capture via `parec` (PulseAudio/PipeWire). Records mono 16-bit PCM at 16 kHz —
//! whisper's native rate, so no resampling needed. Exposes a smoothed RMS level (0..1)
//! for optional external listeners/visualizers.
//!
//! Why parec and not cpal/ALSA: this box is PipeWire, the default ALSA device fails
//! (dsnoop, pipewire-alsa not installed). parec talks to pipewire-pulse directly and
//! lets us pick the source by name. Override the source with VOICECHAT_SOURCE.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

pub const SAMPLE_RATE: u32 = 16_000;

pub struct Capture {
    child: Child,
    reader: Option<thread::JoinHandle<()>>,
    running: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<f32>>>,
    pub level: Arc<AtomicU32>,
    pub sample_rate: u32,
}

pub fn level_of(level: &AtomicU32) -> f32 {
    f32::from_bits(level.load(Ordering::Relaxed))
}

/// Start capturing from the default (or VOICECHAT_SOURCE) input. Runs until `finish()`.
pub fn start() -> Result<Capture, String> {
    let mut cmd = Command::new("parec");
    cmd.arg("--format=s16le")
        .arg(format!("--rate={SAMPLE_RATE}"))
        .arg("--channels=1")
        .arg("--latency-msec=30");
    if let Ok(src) = std::env::var("VOICECHAT_SOURCE") {
        if !src.is_empty() {
            cmd.arg(format!("--device={src}"));
        }
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        // DIAG: surface parec's own warnings/errors to the journal. If PipeWire suspends or
        // drops the mic source mid-dictation, parec logs it here — previously swallowed by null.
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn parec: {e}"))?;

    let mut stdout = child.stdout.take().ok_or("no parec stdout")?;
    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(SAMPLE_RATE as usize * 8)));
    let level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let running = Arc::new(AtomicBool::new(true));

    let buf = samples.clone();
    let lvl = level.clone();
    let run = running.clone();
    let reader = thread::spawn(move || {
        let mut raw = [0u8; 4096];
        let mut total: u64 = 0;
        let started = std::time::Instant::now();
        while run.load(Ordering::Relaxed) {
            match stdout.read(&mut raw) {
                Ok(0) => {
                    // EOF: parec closed stdout, i.e. the capture stream ended on its own
                    // (mic source removed/suspended, parec exited). The session is still
                    // "listening" but NO further audio will be captured from here on — a
                    // silent mid-dictation cutoff. This is the smoking gun if it appears.
                    eprintln!(
                        "voicechat: capture: parec EOF after {:.1}s ({} samples) — stream ended, \
                         capturing stops (session still listening!)",
                        started.elapsed().as_secs_f32(),
                        total
                    );
                    break;
                }
                Ok(n) => {
                    total += (n / 2) as u64;
                    let mut chunk = Vec::with_capacity(n / 2);
                    for pair in raw[..n].chunks_exact(2) {
                        let v = i16::from_le_bytes([pair[0], pair[1]]) as f32 / i16::MAX as f32;
                        chunk.push(v);
                    }
                    if !chunk.is_empty() {
                        let sum_sq: f32 = chunk.iter().map(|s| s * s).sum();
                        let rms = (sum_sq / chunk.len() as f32).sqrt();
                        let prev = f32::from_bits(lvl.load(Ordering::Relaxed));
                        let target = (rms * 4.0).min(1.0); // gain so quiet speech still moves the bars
                        let smoothed = prev * 0.6 + target * 0.4;
                        lvl.store(smoothed.to_bits(), Ordering::Relaxed);
                        if let Ok(mut b) = buf.lock() {
                            b.extend_from_slice(&chunk);
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "voicechat: capture: parec read error after {:.1}s ({} samples): {e} \
                         — capturing stops (session still listening!)",
                        started.elapsed().as_secs_f32(),
                        total
                    );
                    break;
                }
            }
        }
    });

    Ok(Capture {
        child,
        reader: Some(reader),
        running,
        samples,
        level,
        sample_rate: SAMPLE_RATE,
    })
}

impl Capture {
    /// Stop capture and return the recorded mono f32 samples.
    pub fn finish(mut self) -> Vec<f32> {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
        self.samples.lock().map(|b| b.clone()).unwrap_or_default()
    }
}
