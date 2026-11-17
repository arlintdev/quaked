#!/bin/sh
# Installs a quaked release (this folder, unzipped) on an Apple Silicon Mac: Quaked.app and
# Quaked Settings.app into ~/Applications, ~/.local/bin/quaked, the Claude Code hooks, and
# the LaunchAgent that starts quaked at login. Run it again to update.
set -e
cd "$(dirname "$0")"
if [ "$(uname -m)" != "arm64" ]; then
  echo "This release is for Apple Silicon Macs (arm64); this Mac is $(uname -m)." >&2
  exit 1
fi
BUILD="$(cat BUILD 2>/dev/null || echo unknown)"
echo "Installing quaked $BUILD"
APP="$HOME/Applications/Quaked.app"
SETTINGS="$HOME/Applications/Quaked Settings.app"
mkdir -p "$HOME/Applications" "$HOME/.local/bin" "$HOME/Library/LaunchAgents"
rm -rf "$APP" "$SETTINGS"
ditto "Quaked.app" "$APP"
ditto "Quaked Settings.app" "$SETTINGS"
# The release isn't notarized: clear the download quarantine on the two apps you're
# installing, so macOS lets the LaunchAgent start quaked.
xattr -dr com.apple.quarantine "$APP" "$SETTINGS" 2>/dev/null || true
ln -sf "$APP/Contents/MacOS/quaked" "$HOME/.local/bin/quaked"
# Claude Code reports sessions through hooks in ~/.claude/settings.json.
"$HOME/.local/bin/quaked" hooks
# The template says __HOME__ wherever this Mac's home folder goes.
AGENT="$HOME/Library/LaunchAgents/dev.arlint.quaked.plist"
HOME_SED="$(printf '%s' "$HOME" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/[\\&|]/\\&/g')"
sed "s|__HOME__|$HOME_SED|g" dev.arlint.quaked.plist > "$AGENT.tmp"
plutil -lint -s "$AGENT.tmp"
mv "$AGENT.tmp" "$AGENT"
launchctl bootout "gui/$(id -u)/dev.arlint.quaked" 2>/dev/null || true
for _ in 1 2 3 4 5 6 7 8 9 10; do
  launchctl print "gui/$(id -u)/dev.arlint.quaked" >/dev/null 2>&1 || break
  sleep 0.5
done
# A daemon started another way would keep running the old build; hooks are left alone.
for pid in $(pgrep -x quaked || true); do
  case "$(ps -o args= -p "$pid" 2>/dev/null)" in
    *"quaked run") echo "Stopping an older quaked (pid $pid)"; kill "$pid" 2>/dev/null || true ;;
  esac
done
sleep 0.5
launchctl bootstrap "gui/$(id -u)" "$AGENT"
RUNNING=""
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  RUNNING="$("$HOME/.local/bin/quaked" ctl status 2>/dev/null | sed -n 's/.*"build":"\([^"]*\)".*/\1/p')"
  [ -n "$RUNNING" ] && break
  sleep 0.5
done
if [ "$RUNNING" = "$BUILD" ]; then
  echo "quaked $BUILD is running. Set it up at http://127.0.0.1:47380 (or open Quaked Settings)."
  echo "macOS will ask for Input Monitoring (the Quake's touch panel) and other permissions as you use them."
else
  echo "warning: expected quaked $BUILD to be running, but it answered with '${RUNNING:-nothing}'." >&2
  echo "Last lines of ~/Library/Logs/quaked.log:" >&2
  tail -n 15 "$HOME/Library/Logs/quaked.log" >&2 || true
  exit 1
fi
