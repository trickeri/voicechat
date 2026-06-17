//! whispervulkan HTTP client. Writes the captured mono f32 samples to a 16-bit WAV
//! (native rate) and POSTs it as multipart/form-data. whisper-server `--convert`
//! resamples to 16 kHz server-side via ffmpeg.

use std::io::Cursor;

pub fn endpoint() -> String {
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

/// Transcribe samples via whispervulkan, returning the transcript as a single
/// line (internal whitespace/newlines collapsed to single spaces).
pub fn transcribe(samples: &[f32], sample_rate: u32) -> Result<String, String> {
    let wav = wav_bytes(samples, sample_rate)?;
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

    let resp = ureq::post(&endpoint())
        .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .timeout(std::time::Duration::from_secs(120))
        .send_bytes(&body)
        .map_err(|e| format!("POST {}: {e}", endpoint()))?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    // Whisper's `text` format emits one line per segment, so a multi-sentence
    // utterance comes back with embedded newlines. Collapse all whitespace runs
    // (newlines included) into single spaces so dictation pastes as one clean
    // line — otherwise terminals/Claude show a "[Pasted +N lines]" chip instead
    // of the text.
    Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
}
