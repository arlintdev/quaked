#!/bin/sh
# Builds quaked, wraps it in ~/Applications/Quaked.app (so macOS privacy settings show it by
# name), signs it with your Developer ID so permission grants survive rebuilds, links
# ~/.local/bin/quaked to it, adds the Claude Code hooks, and (re)starts the LaunchAgent.
set -e
cd "$(dirname "$0")"
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
cp launchd/dev.arlint.quaked.plist "$HOME/Library/LaunchAgents/"
launchctl bootout "gui/$(id -u)/dev.arlint.quaked" 2>/dev/null || true
# bootout returns before the old instance has exited; bootstrap fails until it has.
for _ in 1 2 3 4 5 6 7 8 9 10; do
  launchctl print "gui/$(id -u)/dev.arlint.quaked" >/dev/null 2>&1 || break
  sleep 0.5
done
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/dev.arlint.quaked.plist"
echo "quaked installed; settings: http://127.0.0.1:47380 (or open Quaked Settings); log: ~/Library/Logs/quaked.log"
