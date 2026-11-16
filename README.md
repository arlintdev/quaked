# quaked

Drives the DECOKEE Quake desk screen directly, with no DK-Suite. It shows a Claude Code
status board: one card per session (working / needs you / done / idle), counts and a clock.
Tapping a card brings the Claude app forward, the buzzer chirps when a session starts
waiting on you, and the knob sets screen luminance (press to dim / undim).

## How the Quake works

| Part | What macOS sees | How quaked uses it |
|---|---|---|
| Screen | display "DK-QUAKE", 1920x480 | borderless window pinned over it, above the menu bar |
| Controls | HID `4158:514B`, raw page `0xFF60`/`0x61` | keepalive ping, buzzer, luminance, knob events |
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
- `~/.claude/settings.json` hooks (SessionStart, UserPromptSubmit, PostToolUse, Notification,
  Stop, SessionEnd) run `quaked hook`, which writes one JSON file per session to
  `~/.claude/quaked/sessions/`. The board polls that folder every second.

DK-Suite must not run at the same time: both want the same display and HID devices.

## Commands

```
./install.sh                 build, bundle, sign, (re)start the LaunchAgent
quaked probe [secs]          raw HID traffic: touch the screen or turn the knob
quaked preview out.png       render the board with sample sessions
quaked beep                  play the "needs you" chirp
quaked luminance <0-255>     set screen luminance
quaked run --no-buzzer       run in the foreground without the chirp
```

Stop it: `launchctl bootout gui/$(id -u)/dev.arlint.quaked`
Start it: `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.arlint.quaked.plist`
