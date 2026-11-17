#!/bin/sh
# Builds a release of quaked for Apple Silicon on this Mac and packs it as
# dist/quaked-<version>-macos-arm64.zip: Quaked.app and Quaked Settings.app (signed with
# your Developer ID when you have one), the LaunchAgent template and an installer.
# Publish it with: gh release create v<version> dist/quaked-<version>-macos-arm64.zip
set -e
cd "$(dirname "$0")/.."
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
BUILD="$(git rev-parse --short HEAD)"
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "warning: uncommitted changes; the build will say $BUILD but include them" >&2
fi
TARGET=aarch64-apple-darwin
NAME="quaked-$VERSION-macos-arm64"
OUT="dist/$NAME"
echo "Packaging quaked $VERSION ($BUILD) for $TARGET"
cargo build --release --target "$TARGET"
rm -rf "$OUT" "dist/$NAME.zip"
mkdir -p "$OUT"

APP="$OUT/Quaked.app"
mkdir -p "$APP/Contents/MacOS"
cp bundle/Info.plist "$APP/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$BUILD" "$APP/Contents/Info.plist"
install -m 755 "target/$TARGET/release/quaked" "$APP/Contents/MacOS/quaked"

SETTINGS="$OUT/Quaked Settings.app"
mkdir -p "$SETTINGS/Contents/MacOS"
cp bundle/settings/Info.plist "$SETTINGS/Contents/Info.plist"
install -m 755 bundle/settings/quaked-settings "$SETTINGS/Contents/MacOS/quaked-settings"

IDENTITY="${QUAKED_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk '/Developer ID Application/ {print $2; exit}')}"
if [ -n "$IDENTITY" ]; then
  codesign --force --timestamp --sign "$IDENTITY" --identifier dev.arlint.quaked "$APP"
  codesign --force --timestamp --sign "$IDENTITY" --identifier dev.arlint.quaked.settings "$SETTINGS"
else
  echo "warning: no Developer ID identity; signing ad hoc" >&2
  codesign --force --sign - --identifier dev.arlint.quaked "$APP"
  codesign --force --sign - --identifier dev.arlint.quaked.settings "$SETTINGS"
fi
codesign --verify --strict "$APP" "$SETTINGS"

cp launchd/dev.arlint.quaked.plist "$OUT/dev.arlint.quaked.plist"
install -m 755 release/install.sh "$OUT/install.sh"
printf '%s\n' "$BUILD" > "$OUT/BUILD"
cat > "$OUT/README.txt" <<EOF
quaked $VERSION ($BUILD) for Apple Silicon Macs (macOS 13 or later).

Install: open Terminal in this folder and run ./install.sh
It puts Quaked.app and Quaked Settings.app in ~/Applications, links ~/.local/bin/quaked,
adds quaked's Claude Code hooks to ~/.claude/settings.json, and starts quaked at login.

Then open http://127.0.0.1:47380 (or Quaked Settings) to set it up.
More: https://github.com/arlintdev/quaked
EOF

# ditto keeps the code signatures and bundle structure intact.
(cd dist && ditto -c -k --keepParent "$NAME" "$NAME.zip")
shasum -a 256 "dist/$NAME.zip" | tee "dist/$NAME.zip.sha256"
echo "dist/$NAME.zip"
