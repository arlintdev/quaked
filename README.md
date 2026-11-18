# quaked

Drives the DECOKEE Quake desk screen directly, with no DK-Suite. Eighteen pages (turn them
on and off and reorder them on the settings page's Panes tab), each with its own color on the
knob's light ring:

- **Dashboard** (the grid icon, first by default): a tile per pane, each drawn for what
  matters there: Claude session chips and a 20-minute activity histogram; album art with
  progress; Digin mentions, inbox and chat counts; ON AIR / OFF AIR with the call app and clip chips; the
  League phase as a Lobby → Queue → Found → Pick → Game stepper with the timer; a CPU
  sparkline per container; WARP, host dots and a latency sparkline per site with TLS
  days; a stacked CI bar across your PRs and the review waiting on you. Each has a health
  dot. Tap a tile (or press the knob on it) to go there. After the idle
  time (default 45 s, set on the board's Settings page or the settings page) the board comes back
  here; switch "Home when idle" off to stay on the current page instead.

- **AI**: a column per Claude Code session showing state (working / needs you / done / idle),
  the prompt, why it needs you or Claude's last reply, and its latest steps: commands,
  files edited or read, searches, MCP calls, with running / done / failed status. Open
  one for its full step timeline.
- **Music**: Now Playing from Spotify or Apple Music with artwork, progress,
  previous / play-pause / next, shuffle on / off, and the player's volume.
- **Shortcuts**: up to 16 tiles with SF Symbols icons. A tile can type a key combo (the
  Quake types it as a real USB keyboard, so no macOS permission is needed), open an app /
  file / URL, run a macOS Shortcut or a shell command, mute your soundboard mic, control
  music, play a clip or switch page.
- **Sounds**: a soundboard. Clips play into your call through a virtual mic, mixed with
  your real mic, and on your own output so you hear them too.
- **League**: follows the League client (live or PBE) through its local API: lobby,
  time in queue, champ select countdown, in game. Auto-accepts the ready check (after a
  short, configurable delay) and jumps to this page with a chirp and a gold ring when the
  queue pops. Accept / Decline / Find match / Cancel buttons. A **Played with** panel keeps
  your record with the people you play with: every finished matched game from the
  client's history is saved to `league-history.json` (so it grows past the client's short
  history), your top partners (friends, or 3+ games together) show with wins, losses and
  win rate, and in your party, champ select and the game the ones with you are on top
  (the dashboard tile says "with Sam 12–7" while you play). Teammates you don't know stay
  unnamed: the client hides their names in game.
  The pane follows what you're playing (from the lobby, champ select and the game): ARAM
  is icy blue, ARAM: Mayhem purple, Summoner's Rift gold, Arena coral, with your record in
  that mode (ARAM and Mayhem together) on the left. During a game the middle shows the game
  itself, from Riot's Live Client Data API (served by the game on 127.0.0.1:2999): clock,
  team kill score, your K/D/A and gold, then per mode: kill participation and towers /
  inhibitors in ARAM; CS per minute, vision, dragons by type, grubs, herald, baron and towers
  on the Rift. Below that a feed of first blood, objectives, towers, aces, multikills and
  your kills and deaths, colored by side. While you're dead it turns into a respawn
  countdown with who killed you and your gold (in ARAM: "shop before you're back"). The
  right side becomes a scoreboard of both teams (champion, level, K/D/A, respawn timers,
  and your record next to people you know); between games it's back to Played with, with
  this session's record and streak in the middle. Nothing it shows goes beyond what the
  game shows you (no enemy cooldowns or timers).
- **Docker**: containers on Kiri (its Docker-compatible socket, `~/.kiri/run/docker.sock`)
  or any Docker engine: state, CPU, memory, ports, engine summary; open one for logs and
  Start / Stop / Restart / Pause.
- **Ops**: WARP on/off (tap to toggle), AppGate, public IP and Cloudflare colo; a card per
  SSH host (reachability and latency, plus load / memory / disk / GPU over SSH with key
  login; hosts behind a ProxyCommand are checked over SSH only); and your sites with HTTP
  status, response time and days until the TLS certificate expires. Tap a host to SSH in,
  a site to open it.
- **Code**: your open GitHub pull requests (CI, review, conflicts, drafts) and the reviews
  waiting on you, through `gh`; GitLab merge requests too, through `glab`, for every host
  it is signed in to. Tap one to open it.
- **Digin**: your open issues (priority, key, status), inbox (mentions, DMs, assignments,
  comments) and recently changed pages from one or more Digin servers, with unread chat
  counts on top. Tap or press a row to open it on the board: an issue shows its
  description, checklist and newest comments with a button per status in the project's
  order (press, press moves it one step; Done and Canceled need a second press); an inbox
  item shows the whole message with Mark read; a page shows its text. Turn to scroll,
  double-click to go back, "Open in Digin" opens it in the browser. Sign in on the
  settings page's Digin tab with your email and a code (or a token from Digin's Settings ›
  Access). quaked asks for issues and chat write (to move issues and mark chats read; it
  never sends messages), pages read and profile write (to mark notifications read), and
  keeps the token in the Keychain of the Mac with the Quake.
- **Calendar** (calendar icon): the next 24 hours from macOS Calendar as a timeline, the
  next meeting big with its time, place and a Join button for Zoom / Meet / Teams /
  FaceTime / Webex links, and the list coming up, merged from both Macs' calendars. The
  board jumps here 15, 10 and 5 minutes before each meeting and as it starts (set the
  marks in Extras).
- **Focus** (timer icon): a countdown you set on the knob (25 minutes by default), shown
  in the ring wherever the board is, with today's blocks, the week, and whether a block
  fits before your next meeting. Optional macOS Shortcuts run when a block starts and ends
  (e.g. Do Not Disturb). The tally lives in `focus.json`.
- **Health** (pulse icon): CPU, memory, GPU, network, disk and uptime for this Mac and the
  paired ones, with sparklines, plus the batteries of your Bluetooth keyboard, mouse,
  trackpad and AirPods (LOW under 20%).
- **Ship** (paper plane): your projects with an executable `deploy.sh` under `~/ArlintDev`
  (or the list in Extras): branch, ahead / behind, changed files, last commit, the latest
  GitHub Actions run, and a deploy button. A deploy only starts from the board, press to
  arm and press again within a few seconds; its output streams onto the page and it can
  be cancelled.
- **Clipboard** (clipboard icon): your last 20 copies, kept in memory only; pick one and
  the app in front pastes it. Copies from password managers, copies made in front of
  password apps and strings that look like keys are skipped.
- **Stocks** (chart icon): your symbols (stocks, ETFs, indexes like `^GSPC`, crypto like
  `BTC-USD`) with price, today's change and today's chart against yesterday's close; press
  one for 1D / 5D / 1M / 1Y charts (turn to switch, press again for its Yahoo Finance page).
  Outside market hours a pill says PRE-MARKET, AFTER HOURS or CLOSED, the pre-market or
  after-hours price shows with its move from the close, and the 1D chart carries on past
  the close in a dimmer line. Prices come from Yahoo Finance with no account (only your symbols are sent), every
  minute while the page is up. Pick symbols on the settings page (Extras › Stocks): search
  by company name, reorder, remove.
- **Windows** (windows icon): your apps front to back with their window counts; press
  one to bring it forward. It only looks while the page is on screen.
- **Settings** (the gear): screen brightness, ring brightness, idle timeout, and switches
  for the ring, the alert pulse, blink / jump on new stuff, buzzer, the Quake's mic,
  League auto-accept and snap.

The ring shows the page color, and breathes amber while a Claude session needs you (the
buzzer chirps too).

Three things appear over the pages on their own:

- **Call mode**: while a call app (Zoom, Teams, FaceTime, Webex, Slack, Discord, or a
  browser call) holds the mic, or something listens to the soundboard mic, the board shows
  the call: app and timer, a mute control (the soundboard mic, the app's mute shortcut, or
  bringing the app forward), the meeting from your calendar with Join, and your clips.
  Hide puts it away until the call ends.
- **Night mode**: after 10 idle minutes between 22:00 and 07:00 the board dims to a slowly
  drifting clock with the weather (Open-Meteo, located from the settings or the
  Cloudflare colo, never IP geolocation) and the first thing on tomorrow's calendar. The
  ring goes dark; a touch or a turn wakes it.
- **Music visualizer**: a spectrum behind the Music page from what the Mac plays (a Core
  Audio process tap, macOS 14.2+), with the ring pulsing on the beat. It only listens
  while the Music page shows and something plays; the first time, macOS asks to let
  Quaked record system audio.

The Extras tab of the settings page holds their options.

**New stuff**: when a session starts waiting on you, a review is requested, CI fails, a PR
is approved / gets changes requested / conflicts, a host goes down, a site starts failing,
a container stops, a deploy finishes, a Mac runs low on disk or memory, or a meeting is
15, 10 or 5 minutes away or starting, the ring blinks in that page's color, the top bar
says what happened, and the board jumps to the page. Touching the board stops the
blinking. `quaked ctl alert code "test"` tries it.

**What matters wins**: each alert has an urgency (a meeting 5 minutes out or starting is
critical; something waiting on you is high; a container stopping or a deploy passing is
normal). Nothing jumps while you're using the board; something urgent jumps once you've
left it for 15 seconds. A League game, a focus block or a meeting that's starting only
give way to critical things, and a page the board just jumped to only gives way to
something more urgent for a minute. Left alone, the board goes to what's going on (Go to
what matters): your game, a meeting about to start or just started, a Claude session
waiting on you, a meeting in the next 15 minutes, a running deploy, a focus block, and
otherwise the dashboard.

## Install

**From a release** (Apple Silicon, macOS 13+): download `quaked-<version>-macos-arm64.zip`
from [Releases](https://github.com/arlintdev/quaked/releases), unzip it, and run
`./install.sh` in that folder. The apps are signed but not notarized, so the installer
clears the download quarantine on them.

**From source**: `./install.sh` in a clone (needs Rust). `scripts/package.sh` builds a
release zip into `dist/` the same way.

## Claude's questions and permissions on the Quake

When Claude asks you to choose (merge or hold, which approach...) or needs permission
(a command, a file edit, a fetch), the prompt also shows on the Quake, next to Claude's
own prompt on the Mac: whichever you answer first applies, and the Quake's copy clears
itself once you answer on the Mac.

- Questions get a card per option: turn to choose, press to answer; multi-select takes
  presses to tick options, then Done.
- Permission prompts show the exact command or path with **Allow**, **Always allow**
  (when Claude Code offers a "don't ask again" rule) and **Deny**. Presses in the first
  second are ignored, so a stray press can't approve something you haven't read.
- The last card or a double-click dismisses it from the Quake; so does no answer within
  the timeout (2 minutes; settings page → General). Claude's prompt stays up either way.
- A prompt from a session on your other Mac shows on whichever Mac has the Quake.

- In the desktop app Claude often asks in its **next-step box** at the end of a turn
  ("Push it?" with buttons). That question shows on the Quake too, with the same buttons;
  picking one on the knob wakes the session with your answer. Replying in the app first
  clears the Quake's copy.

Questions and permissions run as a `PermissionRequest` hook (`quaked permit`); next-step
questions as a background `PostToolUse` hook on the app's next-step tool (`quaked
nextstep`, with `asyncRewake`, so it never holds the turn open). `./install.sh` adds both.

AI cards also show what Claude is saying as it works (its narration between tool calls,
read from the session transcript), and sessions that have been done or idle for 5 minutes
drop off the board (settings page → General) until you prompt them again.

## Two Macs

Every quaked advertises `_quaked._tcp` with Bonjour. Paired Macs share two things:

- **Claude sessions.** The Mac with the Quake attached pulls the others' sessions every
  2 seconds and shows them with the Mac's name; a session waiting on another Mac alerts
  here like a local one.
- **One set of settings.** Each config carries `updatedAt`; a Mac that sees newer
  settings on a peer takes them, keeping only its own `sync` section (name and key). Edit
  on either Mac, by hand or on the settings page, and the other follows within seconds.
  Whichever Mac has the Quake plugged in uses the same settings.

To add the second Mac: install quaked there (`./install.sh`; it runs fine with no Quake
attached), open its settings page, go to **Macs** and click **Pair** next to this Mac.
Allow it here by pressing the knob (or clicking Allow on this Mac's settings page) and
the key is handed over. macOS asks once to allow Quaked on the local network; that is
needed for this and for the Ops page's LAN hosts.

## Soundboard setup

1. BlackHole 2ch (virtual audio device) must be installed: `brew install blackhole-2ch`.
2. In Zoom / Meet / Teams, pick **BlackHole 2ch** as the microphone.
3. Put clips (wav, mp3, m4a, aiff, flac, caf) in `~/Music/Quaked Soundboard`, or add them
   on the settings page's Soundboard tab.

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

## Menu bar

quaked runs in the background (a LaunchAgent starts it at login), so its menu bar icon
is where you find it: a dial, filled when the Quake is attached to either Mac, with a
count when Claude sessions are waiting on you. The menu shows the Quake, Claude and music
status, pairing requests (Allow / Refuse), Open Settings…, a page switcher for the Quake,
Restart quaked and Quit quaked (stops it until your next login). Turn the icon off on the
settings page (General → Show quaked in the menu bar).

## Settings

quaked serves its own settings page at **http://127.0.0.1:47380** (this Mac only; open
**Quaked Settings** from Spotlight, or `quaked settings`). Tabs: Live (a mirror of the
board you can click, knob and page buttons, status), Panes, Shortcuts, Soundboard,
Lighting, General and Macs (pairing). On a Mac without the Quake, the live view and board
controls go through to the Mac that has it. It replaced the old SwiftUI companion app.

Everything is in `~/Library/Application Support/Quaked/config.json` (pages, ring colors,
shortcuts, soundboard devices and clips, League auto-accept, Docker socket, the Quake's
own mic (off by default, which also turns its indicator light off), screen brightness,
touch mapping, buzzer, idle timeout, Ops hosts and sites, Code refresh and GitLab, new
stuff alerts, two-Mac sync). quaked reloads it when it changes, and refuses to use a file
it can't parse (it keeps the last good settings instead).

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
  crashes. Log: `~/Library/Logs/quaked.log`. `install.sh` writes it from
  `launchd/dev.arlint.quaked.plist`, filling in `__HOME__` with this Mac's home folder.
- `~/.local/bin/quaked`: symlink to the daemon's binary, used by the hooks.
- `~/.claude/settings.json` hooks (SessionStart, UserPromptSubmit, PreToolUse, PostToolUse,
  PostToolUseFailure, Notification, Stop, SessionEnd) run `quaked hook`, which keeps one
  JSON file per session in `~/.claude/quaked/sessions/` (state, prompt, last 60 steps,
  Claude's last reply from the transcript). The board polls that folder every second.
- Music uses Spotify's and Music's AppleScript interfaces; macOS asks once to let Quaked
  control them (Privacy & Security > Automation). Cover art is cached in
  `~/Library/Caches/quaked/art`.
- macOS also asks once for Calendars (the Calendar page, call mode and night mode) and for
  System Audio Recording (the Music visualizer, the first time it listens). Saying no only
  turns that piece off.

DK-Suite must not run at the same time: both want the same display and HID devices.

## Commands

```
./install.sh                 build, bundle, sign, add Claude Code hooks, (re)start the LaunchAgent
scripts/package.sh           build the Apple Silicon release zip into dist/
quaked settings              open the settings page (http://127.0.0.1:47380)
quaked hooks                 add the Claude Code hooks to ~/.claude/settings.json
quaked ctl turn 1|-1         drive the running board like the knob
quaked ctl press|double|hold
quaked ctl tap X Y           tap at screen coordinates (1920x480)
quaked ctl page <id>         dash | ai | music | shortcuts | soundboard | league | docker | ops | code | settings
quaked ctl shortcut <n>      run shortcut tile n (from 0)
quaked ctl play <title>      play a clip; `ctl stop` stops all
quaked ctl status            JSON status; `ctl frame [page]` = PNG of the screen (or another page, off-screen)
quaked ctl alert <page> [why]  blink the ring and jump like new stuff does
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
