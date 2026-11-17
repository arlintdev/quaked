# quaked

Drives the DECOKEE Quake desk screen directly, with no DK-Suite. Four pages (order and
visibility set in the config), each with its own color on the knob's light ring:

- **AI**: a column per Claude Code session showing state (working / needs you / done / idle),
  the prompt, why it needs you or Claude's last reply, and its latest steps: commands,
  files edited or read, searches, MCP calls, with running / done / failed status. Open
  one for its full step timeline.
- **Music**: Now Playing from Spotify or Apple Music with artwork, progress,
  previous / play-pause / next, and the player's volume.
- **Shortcuts**: up to 16 tiles with SF Symbols icons. A tile can type a key combo (the
  Quake types it as a real USB keyboard, so no macOS permission is needed), open an app /
  file / URL, run a macOS Shortcut or a shell command, mute your soundboard mic, control
  music, play a clip or switch page.
- **Sounds**: a soundboard. Clips play into your call through a virtual mic, mixed with
  your real mic, and on your own output so you hear them too.
- **League**: follows the League client (live or PBE) through its local API: lobby,
  time in queue, champ select countdown, in game. Auto-accepts the ready check (after a
  short, configurable delay) and jumps to this page with a chirp and a gold ring when the
  queue pops. Accept / Decline / Find match / Cancel buttons.
- **Docker**: containers on Kiri (its Docker-compatible socket, `~/.kiri/run/docker.sock`)
  or any Docker engine: state, CPU, memory, ports, engine summary; open one for logs and
  Start / Stop / Restart / Pause.

The ring shows the page color, and breathes amber while a Claude session needs you (the
buzzer chirps too).

## Soundboard setup

1. BlackHole 2ch (virtual audio device) must be installed: `brew install blackhole-2ch`.
2. In Zoom / Meet / Teams, pick **BlackHole 2ch** as the microphone.
3. Put clips (wav, mp3, m4a, aiff, flac, caf) in `~/Music/Quaked Soundboard`, or add them
   in Quaked Companion.

With `live: "auto"` (default) quaked only opens your real mic while another app is
listening to BlackHole, so the mic indicator is on during calls only. macOS asks once for
microphone access. Clips always play for you; the Sounds page says whether the call hears
them.

## Knob and touch

| Where | Turn | Press | Double-click | Hold |
|---|---|---|---|---|
| Top level | switch page (AI ⇄ Music) | focus the page | — | dim / undim |
| AI, focused | choose a session | open it | back | top |
| Session detail | scroll steps | open Claude | back | top |
| Music, focused | volume | play / pause | back | top |

Touch: tap a tab to switch pages, a session to open it, Back / Open in Claude in the
detail view, and the music buttons or volume bar. 45 seconds without input returns to the
top level. The top bar always shows where you are and what the knob does there.

## Settings and the companion app

Everything is in `~/Library/Application Support/Quaked/config.json` (pages, ring colors,
shortcuts, soundboard devices and clips, League auto-accept, Docker socket, the Quake's
own mic (off by default, which also turns its indicator light off), touch mapping,
buzzer, idle timeout). quaked reloads it when it
changes. **Quaked Companion** (`companion/`, a small SwiftUI app) edits it and shows a live
mirror of the screen you can click on, with knob buttons.

## How the Quake works

| Part | What macOS sees | How quaked uses it |
|---|---|---|
| Screen | display "DK-QUAKE", 1920x480 | borderless window pinned over it, above the menu bar |
| Controls | HID `4158:514B`, raw page `0xFF60`/`0x61` | keepalive ping, buzzer, luminance, knob events, keyboard emulation |
| Light ring | same interface, VIA v3 (`qmk_rgb_matrix`, channel 3) | color / effect / brightness, set live and never saved to the device |
| Touch | HID `0712:0010`, page `0xFF73`/`0x71` | touch frames, in screen pixels, turned into taps |

Frame formats are documented in `src/proto.rs`, worked out from DK-Suite 0.4.95. The
commands that reboot the screen (`0x04`) or enter firmware-update mode (`0x2F 0x03`) are
refused in code.

## Pieces

- `~/Applications/Quaked.app`: the daemon, a background app with no Dock icon, signed with
  your Developer ID so its Input Monitoring grant survives rebuilds.
- `~/Library/LaunchAgents/dev.arlint.quaked.plist`: starts it at login and restarts it if it
  crashes. Log: `~/Library/Logs/quaked.log`.
- `~/.local/bin/quaked`: symlink to the daemon's binary, used by the hooks.
- `~/.claude/settings.json` hooks (SessionStart, UserPromptSubmit, PreToolUse, PostToolUse,
  PostToolUseFailure, Notification, Stop, SessionEnd) run `quaked hook`, which keeps one
  JSON file per session in `~/.claude/quaked/sessions/` (state, prompt, last 60 steps,
  Claude's last reply from the transcript). The board polls that folder every second.
- Music uses Spotify's and Music's AppleScript interfaces; macOS asks once to let Quaked
  control them (Privacy & Security > Automation). Cover art is cached in
  `~/Library/Caches/quaked/art`.

DK-Suite must not run at the same time: both want the same display and HID devices.

## Commands

```
./install.sh                 build, bundle, sign, (re)start the LaunchAgent
quaked ctl turn 1|-1         drive the running board like the knob
quaked ctl press|double|hold
quaked ctl tap X Y           tap at screen coordinates (1920x480)
quaked ctl page <id>         ai | music | shortcuts | soundboard
quaked ctl shortcut <n>      run shortcut tile n (from 0)
quaked ctl play <title>      play a clip; `ctl stop` stops all
quaked ctl status            JSON status; `ctl frame` = PNG of the screen
quaked ctl audio-devices     input / output device names
quaked ctl reload            re-read the config now
quaked via                   read the light ring's current settings
quaked symbol <name>         render an SF Symbol to PNG
quaked probe [secs]          raw HID traffic: touch the screen or turn the knob
quaked preview <dir>         render every view with sample data to PNGs
quaked beep                  play the "needs you" chirp
quaked luminance <0-255>     set screen luminance
```

Stop it: `launchctl bootout gui/$(id -u)/dev.arlint.quaked`
Start it: `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.arlint.quaked.plist`
