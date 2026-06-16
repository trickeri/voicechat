# voicechat

Headless **speech-to-text dictation** daemon. No window. Press `Meta+Escape` (or click the voice
hex in the honeycomb taskbar), speak, press again — your words get transcribed and pasted into the
focused window. STT is served by the [`whispervulkan`](../whispervulkan) daemon; the only UI is the
taskbar indicator (`trikeri_taskbar` → `VoiceMonitor.qml`).

(The name is legacy — there's no chat/LLM/TTS, just dictation. See `AIPlans/`.)

## How it works

```
Meta+Escape / hex click ── voicechat toggle ──▶ voicechat daemon
   capture mic (parec, 16 kHz mono)
   stream RMS level ──▶ ~/.cache/voicechat/state.json  ──▶ taskbar VoiceMonitor.qml (reactive bars)
   on stop: POST wav ──▶ whispervulkan /inference ──▶ transcript
            wl-copy (persistent) + smart paste:
              ghostty  -> Ctrl+Shift+V   (terminal paste)
              else     -> Ctrl+V
```

Focus detection is free: the taskbar writes the active window's app id to
`~/.cache/voicechat/active-window`, which the daemon reads.

## Build & install

```bash
cargo build --release
ln -sf "$PWD/target/release/voicechat" ~/.local/bin/voicechat
ln -sf "$PWD/voicechat.service" ~/.config/systemd/user/voicechat.service
systemctl --user daemon-reload
systemctl --user enable --now voicechat
```

Requires: `whispervulkan` running, `parec` (pipewire-pulse), `wl-copy`, and `ydotool` (+ `ydotoold`).

## Config (env, set in the service)

- `VOICECHAT_SOURCE` — PulseAudio/PipeWire source to record from (`pactl list sources short`).
  Unset = system default source. The shipped unit points at the Focusrite Scarlett Mic1; change it.
- `WHISPER_HTTP_URL` — default `http://127.0.0.1:48450/inference`.
- `YDOTOOL_SOCKET` — default `$XDG_RUNTIME_DIR/.ydotool_socket`.
- `VOICECHAT_DRY_PASTE=1` — copy to clipboard but don't synthesize the paste keystroke (testing).

## Commands

```
voicechat            run the daemon (the service does this)
voicechat toggle     start/stop listening (taskbar hex + Meta+Escape call this)
```
