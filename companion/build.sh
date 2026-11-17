#!/bin/sh
# Builds Quaked Companion, wraps it in ~/Applications/Quaked Companion.app and signs it with
# your Developer ID (ad hoc when there is none). It doesn't touch the quaked daemon.
set -e
cd "$(dirname "$0")"
swift build -c release
BIN="$(swift build -c release --show-bin-path)/QuakedCompanion"
APP="$HOME/Applications/Quaked Companion.app"
mkdir -p "$APP/Contents/MacOS"
cp Info.plist "$APP/Contents/Info.plist"
install -m 755 "$BIN" "$APP/Contents/MacOS/QuakedCompanion"
IDENTITY="${QUAKED_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk '/Developer ID Application/ {print $2; exit}')}"
if [ -n "$IDENTITY" ]; then
  codesign --force --sign "$IDENTITY" --identifier dev.arlint.quaked.companion "$APP"
else
  echo "warning: no Developer ID identity; signing ad hoc"
  codesign --force --sign - --identifier dev.arlint.quaked.companion "$APP"
fi
echo "Quaked Companion installed: $APP"
