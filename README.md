# quaked

Drives the DECOKEE Quake desk screen directly, with no DK-Suite. Two pages:

- **AI**: a column per Claude Code session showing state (working / needs you / done / idle),
  the prompt, why it needs you or Claude's last reply, and its latest steps: commands,
  files edited or read, searches, MCP calls, with running / done / failed status. Open
  one for its full step timeline.
- **Music**: Now Playing from Spotify or Apple Music with artwork, progress,
  previous / play-pause / next, and the player's volume.

The buzzer chirps when a session starts waiting on you.

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
quaked probe [secs]          raw HID traffic: touch the screen or turn the knob
quaked preview <dir>         render every view with sample data to PNGs
quaked beep                  play the "needs you" chirp
quaked luminance <0-255>     set screen luminance
quaked run --no-buzzer       run in the foreground without the chirp
```

Stop it: `launchctl bootout gui/$(id -u)/dev.arlint.quaked`
Start it: `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.arlint.quaked.plist`
