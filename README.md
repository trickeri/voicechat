# voicechat

Headless **speech-to-text dictation** daemon for Linux. No window. Press your bound shortcut,
speak, press again — your words get transcribed and pasted into the focused window. STT is
served by the [`NulSpeech2Text`](https://github.com/trickeri/NulSpeech2Text) daemon
(engine-swappable; currently the Parakeet TDT 0.6B backend).

(The name is legacy — there's no chat/LLM/TTS, just dictation.)

## How it works

```
shortcut ── voicechat toggle ──▶ voicechat daemon
   capture mic (parec, 16 kHz mono)
   on stop: POST wav ──▶ NulSpeech2Text /inference ──▶ transcript
            broadcast transcript on the Unix socket (every transcript, always)
            then per-app routing (rules.conf), by focused app:
              paste     -> wl-copy (persistent) + synthesize combo  (default)
              clipboard -> copy only, no keystroke
              emit      -> don't paste; the app consumes the socket  (e.g. Kdenlive/Krita/Inkscape)
              system    -> no window focused (the desktop): broadcast only, for a
                           system-wide voice-command service
```

It publishes a small JSON status file (`~/.cache/voicechat/status.json`) and an event log
(`~/.cache/voicechat/events.jsonl`) that any taskbar/widget/visualizer can read — but the
daemon owns no UI itself, so none of that is required to use it.

### Tapping into transcripts (per-app behavior)

Other applications can consume voicechat's output directly. Every finished transcript is
broadcast on a Unix domain socket (`$XDG_RUNTIME_DIR/voicechat.sock`) as one JSON line:

```json
{"text":"hello world","app":"kdenlive","mode":"emit","ts":1718755200.123}
```

Connect and read lines (`socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/voicechat.sock`, or a socket
client in your app) to receive transcripts. The broadcast is unconditional — it fires in every
mode, so loggers and observers see paste-mode dictation too.

What voicechat does *locally* with each transcript is set per focused app by a rules file
(`~/.config/voicechat/rules.conf` — see [`rules.conf.example`](rules.conf.example)). Each rule
picks a **mode**: `paste` (copy + keystroke, the default), `clipboard` (copy only), or `emit`
(don't paste — the app reads it off the socket instead). So normal apps get pasted into, while
focusing **Kdenlive / Krita / Inkscape** suppresses the paste and lets your integration handle the
text. When **no window is focused** (the desktop), the transcript is automatically `system` mode —
broadcast only, for a system-wide voice-command service to act on (see
`~/programming/Plasma/systemvoicecommands`, which runs e.g. "start stream" as a shell command).
Rules are re-read each dictation, so edits apply immediately. Matching needs the focus
hint (`VOICECHAT_ACTIVE_WINDOW_FILE`); without a rules file, voicechat keeps its historical
built-in defaults.

---

## Install with Claude Code

> **If you are an AI coding agent (e.g. Claude Code) installing this for a user, follow the
> steps below in order. Stop and ask the user the 🟢 question with your interactive question
> tool — don't assume. Use the marked default if the user has no preference.**

1. **Install the STT server first.** voicechat is just the client; it needs
   [`NulSpeech2Text`](https://github.com/trickeri/NulSpeech2Text) running. Follow that repo's
   "Install with Claude Code" section — it handles the GPU/CPU choice and the STT model
   download. Confirm it's up before continuing:
   ```bash
   curl -fsS http://127.0.0.1:48450/ >/dev/null && echo "NulSpeech2Text is up"
   ```

2. **Install runtime dependencies** (names vary by distro):
   - `parec` (PipeWire/PulseAudio — Arch `pipewire-pulse`, Debian `pipewire-pulse`/`pulseaudio-utils`)
   - `wl-clipboard` (provides `wl-copy`) — Wayland clipboard
   - `ydotool` **and** the `ydotoold` daemon — synthesizes the paste keystroke
   - a Rust toolchain (`cargo`) to build

   Make sure `ydotoold` is running (it needs access to `/dev/uinput`); enable it as a user
   service or per your distro's instructions. For **any-key-stop** (step 6) the user also
   needs read access to `/dev/input` — i.e. membership in the `input` group
   (`sudo usermod -aG input "$USER"`, then re-login). Without it dictation still works; only
   the any-key shortcut is disabled.

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

5. 🟢 **Ask: which push-to-talk hotkey?** — *"What hotkey should toggle dictation? (e.g.
   Meta+Esc)"* The daemon does the work; the shortcut just runs `voicechat toggle`. Bind the
   user's chosen combo to that command:
   - **KDE Plasma** → see [Global shortcut on KDE](#global-shortcut-on-kde). Put the chosen
     combo in `voicechat-toggle.desktop`'s `X-KDE-Shortcuts=` line; the live `gdbus`
     registration there is written for Meta+Esc, so for a different key either adjust the
     keycode or just set it in *System Settings → Shortcuts* after `kbuildsycoca6`.
   - **GNOME / Sway / others** → bind the chosen combo to `voicechat toggle` in the DE's
     keyboard-shortcut settings.

6. 🟢 **Ask: end recording with any key?** — *"Let any key (not just the hotkey) finish a
   recording? It transcribes and pastes immediately."* Default **yes**.
   - **Yes** → nothing to set (it's the default). Make sure the user is in the `input` group
     (step 2) so voicechat can watch the keyboard.
   - **No** → set `Environment=VOICECHAT_ANYKEY_STOP=0` in the service (only the hotkey ends
     a recording).

7. 🟢 **Ask: default paste shortcut?** — *"What key combo should paste the transcript? The
   default is Ctrl+V, which works in most apps."* Default **Ctrl+V**.
   - Set `Environment=VOICECHAT_PASTE_KEY=ctrl+v` (or the chosen combo) in the service. Combos
     are written like `ctrl+v` / `ctrl+shift+v` / `shift+insert`.
   - **Tell the user:** some apps need a *different* paste shortcut — or no paste at all (an
     app that consumes transcripts off the socket). Both are set per app in a rules file. Copy
     [`rules.conf.example`](rules.conf.example) to `~/.config/voicechat/rules.conf` and edit:
     each line is `pattern  mode  [combo]`, where `mode` is `paste`, `clipboard` (copy only),
     or `emit` (don't paste — deliver on the socket). For example terminals paste with
     `ctrl+shift+v`, while `kdenlive`/`krita`/`inkscape` use `emit`. Per-app rules require the focus
     hint (`VOICECHAT_ACTIVE_WINDOW_FILE`) to be populated by the user's setup. (The legacy
     `VOICECHAT_PASTE_RULES` env var still works when no rules file is present.)

8. 🟢 **Ask: start/stop notification sounds?** — *"Play a short sound when dictation starts and
   stops? It's on by default, using the sounds shipped in the repo — you can swap in your own
   or turn it off."* Default **yes**.
   - **Yes** → nothing to set. The service ships `VOICECHAT_SOUND_START`/`VOICECHAT_SOUND_STOP`
     enabled, pointing at `sounds/PushToTalkStartSFX.mp3` and `PushToTalkStopSFX.mp3`. Playback
     uses `pw-play` (PipeWire); confirm it's installed.
   - **To use their own sounds** → replace those two files (any format `pw-play` supports), or
     point the two `Environment=VOICECHAT_SOUND_*` lines in the service at other files.
   - **No** → comment out both `Environment=VOICECHAT_SOUND_*` lines in the service (unset =
     silent).

9. **Test:** run `voicechat toggle`, say a sentence, then press the hotkey (or, with any-key
   enabled, any key) — the text should paste into the focused window. `VOICECHAT_DRY_PASTE=1`
   copies to the clipboard without synthesizing the keystroke if you want to test without
   pasting.

---

## Manual build & install

```bash
cargo build --release
ln -sf "$PWD/target/release/voicechat" ~/.local/bin/voicechat
ln -sf "$PWD/voicechat.service" ~/.config/systemd/user/voicechat.service
systemctl --user daemon-reload
systemctl --user enable --now voicechat
```

Requires: `NulSpeech2Text` running, `parec` (pipewire-pulse), `wl-copy` (wl-clipboard), and
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
- `WHISPER_HTTP_URL` — NulSpeech2Text endpoint (default `http://127.0.0.1:48450/inference`).
  (Legacy env-var name — kept because the whole stack shares it; the engine is Parakeet now.)
- `YDOTOOL_SOCKET` — default `$XDG_RUNTIME_DIR/.ydotool_socket`.
- `VOICECHAT_PASTE_KEY` — default paste combo (default `ctrl+v`). Written like `ctrl+v` /
  `ctrl+shift+v` / `shift+insert` (modifiers: ctrl, shift, alt, super; plus a–z or insert).
  Used as the default combo for `paste` rules that don't specify their own.
- `VOICECHAT_RULES_FILE` — per-app routing rules file (default `~/.config/voicechat/rules.conf`).
  Lines are `pattern  mode  [combo]`; `mode` is `paste` / `clipboard` / `emit`. Re-read each
  dictation. See [`rules.conf.example`](rules.conf.example). Needs the focus hint
  (`VOICECHAT_ACTIVE_WINDOW_FILE`).
- `VOICECHAT_SOCKET` — Unix socket transcripts are broadcast on (default
  `$XDG_RUNTIME_DIR/voicechat.sock`). Each transcript is one JSON line
  `{"text","app","mode","ts"}`; any app can connect and read to consume voicechat output.
- `VOICECHAT_PASTE_RULES` — legacy per-app paste overrides, `;`-separated `app-substring=combo`.
  Only consulted when no rules file exists. Default `ghostty=ctrl+shift+v`. Prefer the rules file.
- `VOICECHAT_ANYKEY_STOP` — let any key (not just the hotkey) finish a recording. On by
  default; set `0`/`false`/`off` to require the hotkey. Needs read access to `/dev/input`
  (the `input` group); silently disables itself if unavailable.
- `VOICECHAT_SOUND_START` / `VOICECHAT_SOUND_STOP` — start/stop notification sounds played via
  `pw-play`. **Enabled by default**, pointing at the sounds shipped in [`sounds/`](sounds/).
  Replace `sounds/PushToTalkStartSFX.mp3` and `PushToTalkStopSFX.mp3` with your own audio to
  change them, point these vars elsewhere, or comment both lines out (unset = silent).
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
