#!/bin/sh
# Builds quaked, wraps it in ~/Applications/Quaked.app (so macOS privacy settings show it by
# name), signs it with your Developer ID so permission grants survive rebuilds, links
# ~/.local/bin/quaked to it, adds the Claude Code hooks, and (re)starts the LaunchAgent.
set -e
cd "$(dirname "$0")"
BUILD="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
echo "Building quaked $BUILD"
cargo build --release
APP="$HOME/Applications/Quaked.app"
mkdir -p "$APP/Contents/MacOS" "$HOME/.local/bin" "$HOME/Library/LaunchAgents"
cp bundle/Info.plist "$APP/Contents/Info.plist"
install -m 755 target/release/quaked "$APP/Contents/MacOS/quaked"
IDENTITY="${QUAKED_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk '/Developer ID Application/ {print $2; exit}')}"
if [ -n "$IDENTITY" ]; then
  codesign --force --sign "$IDENTITY" --identifier dev.arlint.quaked "$APP"
else
  echo "warning: no Developer ID identity; signing ad hoc (Input Monitoring must be re-granted after each rebuild)"
  codesign --force --sign - --identifier dev.arlint.quaked "$APP"
fi
ln -sf "$APP/Contents/MacOS/quaked" "$HOME/.local/bin/quaked"
# Claude Code reports sessions through hooks in ~/.claude/settings.json.
"$HOME/.local/bin/quaked" hooks
# "Quaked Settings" in ~/Applications (and Spotlight) opens the settings page.
SETTINGS="$HOME/Applications/Quaked Settings.app"
mkdir -p "$SETTINGS/Contents/MacOS"
cp bundle/settings/Info.plist "$SETTINGS/Contents/Info.plist"
install -m 755 bundle/settings/quaked-settings "$SETTINGS/Contents/MacOS/quaked-settings"
codesign --force --sign "${IDENTITY:--}" --identifier dev.arlint.quaked.settings "$SETTINGS" 2>/dev/null || true
# The template says __HOME__ wherever this Mac's home folder goes, so one file serves every Mac.
AGENT="$HOME/Library/LaunchAgents/dev.arlint.quaked.plist"
# XML-escape the path, then escape it for sed's replacement text.
HOME_SED="$(printf '%s' "$HOME" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/[\\&|]/\\&/g')"
sed "s|__HOME__|$HOME_SED|g" launchd/dev.arlint.quaked.plist > "$AGENT.tmp"
plutil -lint -s "$AGENT.tmp"
mv "$AGENT.tmp" "$AGENT"
launchctl bootout "gui/$(id -u)/dev.arlint.quaked" 2>/dev/null || true
# bootout returns before the old instance has exited; bootstrap fails until it has.
for _ in 1 2 3 4 5 6 7 8 9 10; do
  launchctl print "gui/$(id -u)/dev.arlint.quaked" >/dev/null 2>&1 || break
  sleep 0.5
done
# A daemon started some other way (from Terminal, or an old install) would keep running
# the old build and keep its place on the network. Hooks (quaked hook / permit) are left
# alone: only "quaked run" is the daemon.
for pid in $(pgrep -x quaked || true); do
  case "$(ps -o args= -p "$pid" 2>/dev/null)" in
    *"quaked run") echo "Stopping an older quaked (pid $pid)"; kill "$pid" 2>/dev/null || true ;;
  esac
done
sleep 0.5
launchctl bootstrap "gui/$(id -u)" "$AGENT"
# Check the new build is the one answering.
RUNNING=""
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  RUNNING="$("$HOME/.local/bin/quaked" ctl status 2>/dev/null | sed -n 's/.*"build":"\([^"]*\)".*/\1/p')"
  [ -n "$RUNNING" ] && break
  sleep 0.5
done
if [ "$RUNNING" = "$BUILD" ]; then
  echo "quaked $BUILD is running; settings: http://127.0.0.1:47380 (or open Quaked Settings); log: ~/Library/Logs/quaked.log"
else
  echo "warning: expected quaked $BUILD to be running, but it answered with build '${RUNNING:-nothing}'." >&2
  echo "Last lines of ~/Library/Logs/quaked.log:" >&2
  tail -n 15 "$HOME/Library/Logs/quaked.log" >&2 || true
  exit 1
fi
