# voicechat

Headless **speech-to-text dictation** daemon for Linux. No window. Press your bound shortcut,
speak, press again — your words get transcribed and pasted into the focused window. STT is
served by the [`whispervulkan`](https://github.com/trickeri/whispervulkan) daemon.

(The name is legacy — there's no chat/LLM/TTS, just dictation.)

## How it works

```
shortcut ── voicechat toggle ──▶ voicechat daemon
   capture mic (parec, 16 kHz mono)
   on stop: POST wav ──▶ whispervulkan /inference ──▶ transcript
            wl-copy (persistent) + smart paste:
              terminal (ghostty) -> Ctrl+Shift+V
              else               -> Ctrl+V
```

It publishes a small JSON status file (`~/.cache/voicechat/status.json`) and an event log
(`~/.cache/voicechat/events.jsonl`) that any taskbar/widget/visualizer can read — but the
daemon owns no UI itself, so none of that is required to use it.

---

## Install with Claude Code

> **If you are an AI coding agent (e.g. Claude Code) installing this for a user, follow the
> steps below in order. Stop and ask the user the 🟢 question with your interactive question
> tool — don't assume. Use the marked default if the user has no preference.**

1. **Install the STT server first.** voicechat is just the client; it needs
   [`whispervulkan`](https://github.com/trickeri/whispervulkan) running. Follow that repo's
   "Install with Claude Code" section — it handles the GPU/CPU choice and the Whisper model
   download. Confirm it's up before continuing:
   ```bash
   curl -fsS http://127.0.0.1:48450/ >/dev/null && echo "whispervulkan is up"
   ```

2. **Install runtime dependencies** (names vary by distro):
   - `parec` (PipeWire/PulseAudio — Arch `pipewire-pulse`, Debian `pipewire-pulse`/`pulseaudio-utils`)
   - `wl-clipboard` (provides `wl-copy`) — Wayland clipboard
   - `ydotool` **and** the `ydotoold` daemon — synthesizes the paste keystroke
   - a Rust toolchain (`cargo`) to build

   Make sure `ydotoold` is running (it needs access to `/dev/uinput`); enable it as a user
   service or per your distro's instructions.

3. **Clone and build:**
   ```bash
   mkdir -p ~/programming && cd ~/programming
   git clone https://github.com/trickeri/voicechat.git
   cd voicechat
   cargo build --release
   ln -sf "$PWD/target/release/voicechat" ~/.local/bin/voicechat
   ```
   (Ensure `~/.local/bin` is on `PATH`.)

4. 🟢 **Ask: auto-start on login?** — *"Start voicechat automatically on login? (recommended)"*
   Default **yes**.
   - **Yes** → install and enable the user service:
     ```bash
     ln -sf "$PWD/voicechat.service" ~/.config/systemd/user/voicechat.service
     systemctl --user daemon-reload
     systemctl --user enable --now voicechat
     ```
   - **No** → the user runs `voicechat` (the daemon) manually when needed.

   By default it records from the **system default** microphone. To use a specific mic, set
   `VOICECHAT_SOURCE` in the service (a name from `pactl list sources short`).

5. **Bind a shortcut** to toggle dictation. The daemon does the work; the shortcut just runs
   `voicechat toggle`. Bind that command to a hotkey in the user's desktop environment
   (GNOME/KDE/Sway/etc. all support custom shortcuts). On **KDE Plasma**, see
   [Global shortcut on KDE](#global-shortcut-on-kde) for a kglobalaccel registration that
   works system-wide.

6. **Test:** run `voicechat toggle`, say a sentence, run it again — the text should paste into
   the focused window. `VOICECHAT_DRY_PASTE=1` copies to the clipboard without synthesizing
   the keystroke if you want to test without pasting.

---

## Manual build & install

```bash
cargo build --release
ln -sf "$PWD/target/release/voicechat" ~/.local/bin/voicechat
ln -sf "$PWD/voicechat.service" ~/.config/systemd/user/voicechat.service
systemctl --user daemon-reload
systemctl --user enable --now voicechat
```

Requires: `whispervulkan` running, `parec` (pipewire-pulse), `wl-copy` (wl-clipboard), and
`ydotool` (+ `ydotoold`).

### Global shortcut on KDE

A plasmoid QML `Shortcut` only fires when plasmashell has focus, so on KDE the toggle should
be a real kglobalaccel shortcut. `voicechat-toggle.desktop` carries `X-KDE-Shortcuts=Meta+Esc`,
which KDE registers at login. Ship it and edit the `Exec` path:

```bash
cp voicechat-toggle.desktop ~/.local/share/applications/   # edit Exec= to your voicechat path
kbuildsycoca6
AID="['voicechat-toggle.desktop','_launch','Voice Dictation Toggle','Voice Dictation Toggle']"
gdbus call --session --dest org.kde.kglobalaccel --object-path /kglobalaccel \
  --method org.kde.KGlobalAccel.doRegister "$AID"
gdbus call --session --dest org.kde.kglobalaccel --object-path /kglobalaccel \
  --method org.kde.KGlobalAccel.setForeignShortcut "$AID" "[285212672]"   # 285212672 = Meta+Esc
```

(If Meta+Esc is already taken, rebind the conflicting shortcut first, or choose another key.)
On non-KDE desktops, just bind `voicechat toggle` to a key in your DE's keyboard settings.

## Config (env, set in the service)

- `VOICECHAT_SOURCE` — PipeWire/PulseAudio source to record from (`pactl list sources short`).
  Unset = system default source.
- `WHISPER_HTTP_URL` — whispervulkan endpoint (default `http://127.0.0.1:48450/inference`).
- `YDOTOOL_SOCKET` — default `$XDG_RUNTIME_DIR/.ydotool_socket`.
- `VOICECHAT_SOUND_START` / `VOICECHAT_SOUND_STOP` — notification sounds played via `pw-play`.
  Default to `~/Music/PushToTalkStartSFX.mp3` / `PushToTalkStopSFX.mp3` if present; set these
  to point elsewhere, or to silence them point at a non-existent path.
- `VOICECHAT_ACTIVE_WINDOW_FILE` — optional file an external focus listener writes with the
  focused app id; when it reads as empty (no app focused) voicechat skips the paste and leaves
  the text on the clipboard. Unset = always paste.
- `VOICECHAT_STATUS_FILE` / `VOICECHAT_EVENTS_FILE` — override the status/event output paths.
- `VOICECHAT_DRY_PASTE=1` — copy to clipboard but don't synthesize the paste keystroke.

## Commands

```
voicechat            run the daemon (the service does this)
voicechat toggle     start/stop listening (bind this to your shortcut)
```
