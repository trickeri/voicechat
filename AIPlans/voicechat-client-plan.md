# voicechat — headless dictation daemon (speech → text)

**Repo:** `trickeri/voicechat` → `~/programming/voicechat`
**Status:** BUILT — headless Rust daemon running as a `systemd --user` service; taskbar `VoiceMonitor.qml` live.
**Depends on:** `whispermodel` daemon (see `~/programming/Models/whispermodel/AIPlans/`) for all STT.
**Scope:** Lean **speech-to-text only**. No conversation, no LLM, no TTS. **One hidden background
process**, no window ever — the *only* visible surface is the taskbar hex
(`trikeri_taskbar/VOICE_INDICATOR_PLAN.md`), which shows state + audio reactivity.
**Flow:** `Meta+Escape` (or click the hex) → speak → transcript → clipboard + smart-paste into the
focused window → idle.

It does **no** transcription itself: it captures mic audio and POSTs it to `whispermodel`.

---

## 1. Why this exists

"open whisper" uses ~1 GB RAM, shows popups, and steals a window we can't place. We want **none of
that window**: a tiny headless Rust process that sits idle, wakes on a hotkey, dictates into whatever
is focused, and whose *only* visible surface is the taskbar hex we already control. STT is handled by
the **shared** `whispermodel` daemon (model warm in VRAM, not re-loaded per app). `whispermodel` is
the engine; `voicechat` is the glue (capture → STT → paste) + the state-file the taskbar renders.

## 2. Stack decision

**Headless Rust binary. No Tauri, no Svelte, no webview, no window.** (Updated from an earlier
Tauri+Svelte idea — there is no GUI to host, so a webview would be pure overhead.)

- Everything the app does is backend work Rust already suits: PipeWire mic capture (`cpal`/`pw`),
  16 kHz resample, HTTP to whispermodel (`reqwest`/`ureq`), key injection (`wtype`/`ydotool`),
  atomic state-file writes.
- Ships as a small daemon binary + a `voicechat-dictate` CLI verb (`toggle`/`start`/`stop`) that the
  taskbar hex and the `Meta+Escape` global shortcut invoke. Runs under `systemd --user`.
- Idle footprint is a sleeping Rust process (a few MB) — the whole point vs. open-whisper's ~1 GB.
- **Preserve Windows builds** (memory `preserve-windows-builds`): gate Linux-only bits (PipeWire,
  wtype/ydotool, wl-copy) behind `cfg!`/traits so a future Windows port isn't blocked.

> Alternative considered & rejected: Tauri+Svelte windowed client (reuse Trik_Klip's solved
> KDE-Wayland config). Dropped — confirmed there will be no window; only the taskbar icon reacts.
> If a settings UI is ever wanted, prefer a plain `config.toml` over bringing a webview back.

## 3. No UI — control surface

There is **no voicechat window**. Control and feedback are entirely external:

- **Toggle:** `Meta+Escape` global shortcut (primary) or clicking the taskbar hex — both call
  `voicechat-dictate toggle`. See §7 hotkey + the taskbar plan.
- **Feedback:** the taskbar hex (idle / listening + audio-reactive bars / processing / done),
  driven by the state file in §4a. **No OS popups.**
- **Push-to-talk vs toggle-to-talk:** support both — hold the key to talk + release to send, or tap
  to start and tap to stop. Config flag for the default.
- The mic "level" the bars react to is written to the state file (§4a), not shown in any voicechat
  window (there is none).

## 4a. Active indicator → honeycomb taskbar integration

The voice-active indicator is a new component in the **`trikeri_taskbar` honeycomb plasmoid**
(`plasmoid/com.nuldrums.honeycomb/`), reusing the established **fast-cache-reader** pattern so the
QML never blocks plasmashell (same architecture as `SystemMonitor.qml` / `AiUsageMonitor.qml`, which
read a local script/cache via `P5Support.DataSource` executable engine — see that repo's
`AI_USAGE_METERS_PLAN.md`).

**Contract — voicechat is the writer, taskbar is the reader:**
- voicechat writes a tiny state file on every transition, e.g. `~/.cache/voicechat/state.json`:
  `{ "state": "idle|listening|processing|done", "level": 0.0, "ts": <epoch> }`.
  Writing a small file is cheap and atomic (write temp + rename); no IPC server needed on the client.
  `level` is 0..1 mic RMS, written at ~30–60 Hz **while listening**, so the taskbar's 3-line
  equalizer can be audio-reactive (see `trikeri_taskbar/VOICE_INDICATOR_PLAN.md`). `processing` =
  transcribing (waiting on whispermodel); `done` = brief flash after paste, then back to `idle`.
- **Everything is driven from the taskbar** (no window): the headless `voicechat-dictate` daemon does
  capture → whispermodel → clipboard + smart paste, toggled by the taskbar hex / `Meta+Escape`.
- A new `plasmoid/.../contents/ui/VoiceMonitor.qml` (+ a fast `contents/scripts/voice-state.sh`
  cat-the-cache reader, matching the SystemMonitor/AiUsage pattern) renders the indicator in the
  Nuldrums palette: still cyan when `idle`, cyan-filled with black audio-reactive bars when
  `listening`, purple/black when `processing`, brief flash on `done`. Stale `ts` (e.g. >3 s in a live
  state) → fall back to idle so a crashed client doesn't leave it stuck "listening".
- **Why a state file, not the daemon's state:** the engine (`whispermodel`) only knows
  "transcribing"; voicechat owns the capture→paste pipeline and is the only thing that also knows
  `listening` (and the live mic `level`). So the client owns the indicator state.
- Clicking the taskbar indicator toggles dictation (same as `Meta+Escape`). Placement within the
  honeycomb is a taskbar-repo decision; this plan just defines the **state-file contract**.

> Cross-repo work item: the actual `VoiceMonitor.qml` lands in **`trikeri_taskbar`**, not here.
> Track it there; voicechat's only obligation is writing `~/.cache/voicechat/state.json`.

## 4. Audio capture

- Capture mic via PipeWire (`pw-record`/`parec` present, or `cpal` crate in Rust). Record to a buffer.
- Resample to **16 kHz mono WAV** (whisper's native rate) before sending — ffmpeg present, or do it
  in-process. Per the daemon contract, **the client owns capture + resample** (whispermodel plan §6).
- VAD/endpointing: simple silence detection to auto-stop on release, or rely on push-to-talk
  key-up = stop. (Daemon also has optional silero VAD for trimming.)

## 5. Flow — the whole app

1. `Meta+Escape` / click the hex → `state=listening`, start capturing mic; stream `level` to the
   state file. (Push-to-talk: hold = listen, release = send. Toggle-to-talk: tap to start, tap to stop.)
2. Stop → finalize 16 kHz mono WAV → `state=processing`.
3. `POST http://127.0.0.1:48450/inference` (file=wav, `response_format=text`) → transcript.
4. **Smart paste into the focused window** (full logic in taskbar plan §6):
   - `wl-copy` (**persistent**, never `-o`/one-shot — that clears the clipboard on first paste).
   - Detect if the focused window is **ghostty** (active window class — the plasmoid already has it
     from `TasksModel`); send **`Ctrl+Shift+V`** in ghostty, else normal **`Ctrl+V`**.
   - Injection via `wtype` or `ydotool` — **decide during build which is reliable on this KDE-Wayland
     box**, verify against a real focused field, record in a memory.
5. `state=done` (brief flash) → `idle`.

## 6. Repo layout

```
~/programming/voicechat/
├── AIPlans/
├── Cargo.toml
├── src/
│   ├── main.rs                  # daemon: own the mic-toggle state machine, write state.json
│   ├── bin/voicechat-dictate.rs # tiny CLI: toggle|start|stop (invoked by taskbar hex + hotkey)
│   ├── capture.rs               # PipeWire mic capture + RMS level + 16 kHz resample
│   ├── stt.rs                   # whispermodel HTTP client
│   ├── paste.rs                 # wl-copy (persistent) + focus detect + wtype/ydotool smart paste
│   └── state.rs                 # atomic ~/.cache/voicechat/state.json writer
├── config.toml                  # gitignored: whisper url, hotkey, ptt-vs-toggle default
├── voicechat.service            # systemd --user unit
└── README.md
```

- `config.toml` keys: `WHISPER_HTTP_URL=http://127.0.0.1:48450/inference`, hotkey,
  ptt-vs-toggle default, language.
- Lifecycle: `systemd --user` like `whispermodel`; expose `~/.local/bin/voicechat-dictate` shim
  (PATH per memory `local-bin-graphical-path`) so the hex/hotkey can invoke it.

## 7. Dependency on whispermodel

- voicechat is useless without the daemon running. Before a toggle, probe `GET /health` (or a tiny
  `/inference`); if down, set state to an `error` flash on the taskbar hex and (optionally) run
  `systemctl --user start whispermodel` itself — no window to show a button in.
- Build `whispermodel` **first**; voicechat's STT calls are just the curl contract in that plan §6.

## 8. Build order / milestones

1. **Scaffold** Rust daemon repo (`cargo init`). `git init`, create `trickeri/voicechat`. The
   `voicechat-dictate toggle` CLI verb + a `systemd --user` unit. No window, ever.
2. **Mic capture → 16 kHz WAV** in Rust, emitting RMS `level`.
3. **State-file writer** — emit `~/.cache/voicechat/state.json` on every transition + `level` while
   listening (§4a). Cheap, atomic. Unblocks the taskbar indicator independently.
4. **whispermodel client** — POST wav, get transcript. (Requires the daemon.)
5. **Global hotkey** — `Meta+Escape` → `voicechat-dictate toggle` (see §5 / taskbar plan).
6. **Taskbar `VoiceMonitor.qml`** — in the **`trikeri_taskbar`** repo: new QML component + fast
   `voice-state.sh` reader, animated by state, stale-`ts` → idle. (Cross-repo; track there.)
7. **Smart paste** — persistent `wl-copy` + focus-detect ghostty + `Ctrl+Shift+V`/`Ctrl+V` via
   wtype/ydotool (taskbar plan §6). Pick the reliable injector, verify, record in memory. **Ships it.**

## 9. Open questions / decide during build

- Push-to-talk vs toggle-to-talk default; the `Meta+Escape` global-capture confirmation (else the
  `Meta+Tab` rebind — see taskbar plan §7). Avoid clashing with SystemFlow / KWin schemes.
- Text-injection / paste mechanism on this KDE-Wayland box (wtype vs ydotool) — empirical, shared
  decision with the taskbar paste step.

## Related memory
`trik-klip-linux-dev` (whisper.cpp Vulkan build recipe — the engine side),
`preserve-windows-builds`, `local-bin-graphical-path`.
`systemflow` (avoid hotkey clashes), `rice` (honeycomb taskbar). Engine plan: `whispermodel`.
Cross-repo: `trikeri_taskbar` (`plasmoid/com.nuldrums.honeycomb/`, `AI_USAGE_METERS_PLAN.md` — the
fast-cache-reader pattern the `VoiceMonitor.qml` indicator follows).
