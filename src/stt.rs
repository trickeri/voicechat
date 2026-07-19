//! NulSpeech2Text HTTP client (the STT daemon on :48450, currently the Parakeet TDT 0.6B
//! backend). Writes the captured mono f32 samples to a 16-bit WAV at 16 kHz — the engine's
//! native rate, so no server-side resample is needed — and POSTs it as multipart/form-data.

use std::io::Cursor;
use std::time::Duration;

/// Hard client-side watchdog for one whole transcribe turn, used by the caller (main.rs) which
/// runs `transcribe` on a worker thread and abandons it if it exceeds this. This is the
/// backstop of last resort: a server that accepts the POST and then stalls mid-response has
/// wedged this daemon *indefinitely* in the past (ureq's body read outlived even the request
/// timeout, and a killed server didn't unblock it), which left dictation dead until a manual
/// restart. Kept a little above the server's own hard decode deadline (PK_DECODE_TIMEOUT, 45s)
/// so the server's 504 normally lands first and we log a real error instead of a generic trip.
/// Override with VOICECHAT_STT_WATCHDOG_S.
pub fn watchdog() -> Duration {
    let secs = std::env::var("VOICECHAT_STT_WATCHDOG_S")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(55);
    Duration::from_secs(secs)
}

/// Overall HTTP deadline (connect + send + response read) for the STT call, so the worker
/// thread itself unwinds rather than lingering forever behind the watchdog. Override with
/// VOICECHAT_STT_HTTP_TIMEOUT_S.
fn http_timeout() -> Duration {
    let secs = std::env::var("VOICECHAT_STT_HTTP_TIMEOUT_S")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(50);
    Duration::from_secs(secs)
}

pub fn endpoint() -> String {
    // WHISPER_HTTP_URL is the long-standing shared env name for this endpoint across the stack
    // (nulcaption, the NulSpeech2Text reference clients). Kept for compatibility even though the
    // engine is now Parakeet, not Whisper — the name is just an address, not the model.
    std::env::var("WHISPER_HTTP_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:48450/inference".to_string())
}

fn wav_bytes(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::<u8>::new());
    {
        let mut w = hound::WavWriter::new(&mut cursor, spec).map_err(|e| e.to_string())?;
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            w.write_sample(v).map_err(|e| e.to_string())?;
        }
        w.finalize().map_err(|e| e.to_string())?;
    }
    Ok(cursor.into_inner())
}

/// Trailing silence (ms) appended before transcription. When you hit the end-key the instant
/// you stop talking, the audio ends abruptly on your last word with no trailing silence, and
/// the STT decoder can fail to finalize + drop the final segment (the "…and then it cut off the
/// last few words" bug). A short pad of zero samples gives the decoder room to close the last
/// segment. Tunable via VOICECHAT_TAIL_PAD_MS; 0 disables. Default 800ms.
fn tail_pad_ms() -> u32 {
    std::env::var("VOICECHAT_TAIL_PAD_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(800)
}

/// Transcribe samples via NulSpeech2Text, returning the transcript as a single
/// line (internal whitespace/newlines collapsed to single spaces).
pub fn transcribe(samples: &[f32], sample_rate: u32) -> Result<String, String> {
    // Pad trailing silence so the decoder doesn't drop the last word on an abrupt cutoff.
    let pad = (sample_rate as u64 * tail_pad_ms() as u64 / 1000) as usize;
    let wav = if pad > 0 {
        let mut padded = Vec::with_capacity(samples.len() + pad);
        padded.extend_from_slice(samples);
        padded.resize(samples.len() + pad, 0.0);
        wav_bytes(&padded, sample_rate)?
    } else {
        wav_bytes(samples, sample_rate)?
    };
    let boundary = "----voicechatBoundary7e3f";
    let mut body: Vec<u8> = Vec::with_capacity(wav.len() + 256);
    let field = |name: &str, value: &str, body: &mut Vec<u8>| {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    };
    field("response_format", "text", &mut body);
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(&wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    // Build an agent with an overall deadline that actually covers the *response read*, not just
    // the send — a bare `Request::timeout()` let a stalled body read hang forever. `.timeout()`
    // on the agent applies its deadline to every read of the response too, so `into_string()`
    // below can't block past `http_timeout()`.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(http_timeout())
        .build();
    let resp = agent
        .post(&endpoint())
        .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send_bytes(&body)
        .map_err(|e| format!("POST {}: {e}", endpoint()))?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    // The STT `text` format emits one line per segment, so a multi-sentence
    // utterance comes back with embedded newlines. Collapse all whitespace runs
    // (newlines included) into single spaces so dictation pastes as one clean
    // line — otherwise terminals/Claude show a "[Pasted +N lines]" chip instead
    // of the text.
    Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
}
