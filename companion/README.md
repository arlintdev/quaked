# Quaked Companion

A small macOS app (SwiftUI, macOS 13+) for setting up and watching the `quaked` daemon that
drives the DECOKEE Quake desk screen.

- **Live**: a mirror of the Quake's 1920x480 screen, refreshed about three times a second
  while the page is on screen. Click it to tap the real screen. Under it are the knob
  (turn left or right, press, back, home), page buttons and the daemon's status: device,
  page, Claude sessions, now playing and soundboard state. Use the ← and → keys to turn the knob.
- **Shortcuts**: the Shortcuts page's tiles (up to 16). Add, delete, drag to reorder, and
  edit the title, SF Symbol, color and action. **Run** fires a tile on the Mac.
- **Soundboard**: the clips folder, adding clips (they're copied into the folder),
  per-clip title, symbol, color and volume, playing clips, and audio routing: microphone,
  output device, live-on-mic mode, passthrough, monitor and clip volume.
- **Lighting**: the knob's light ring: brightness, a color per page, and the alert
  breathing color.
- **General**: buzzer, idle timeout, and which pages the knob visits, in what order.

## Build

```
./build.sh
```

This runs `swift build -c release`, wraps the binary in `~/Applications/Quaked Companion.app`
and signs it with the first "Developer ID Application" identity in your keychain (set
`QUAKED_SIGN_IDENTITY` to pick another one). With no identity it signs ad hoc. It doesn't
touch the daemon or its LaunchAgent.

## Where things live

- Settings: `~/Library/Application Support/Quaked/config.json`, shared with the daemon
  (the schema is `../src/config.rs`). The app saves half a second after you stop editing,
  then sends `reload`. Keys it doesn't know are kept. If the file is missing, the app
  starts from the daemon's defaults and writes the file on your first edit.
- Control socket: `~/.claude/quaked/ctl.sock`. Each request uses its own connection: one
  line out, then the reply up to EOF (`status`, `frame`, `audio-devices`, `reload`,
  `turn ±1`, `press`, `double`, `hold`, `tap X Y`, `page <id>`, `shortcut <n>`,
  `play <title>`, `stop`).

When the daemon isn't running, a banner says so. You can keep editing, and the daemon
picks up the changes when it starts.
