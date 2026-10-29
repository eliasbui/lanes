#!/bin/sh
# Builds "dist/Lanes.app": the release binary, the icon, and the app metadata.
#
#   scripts/bundle-macos.sh               # for this Mac
#   UNIVERSAL=1 scripts/bundle-macos.sh   # Apple silicon and Intel in one binary (what a release ships)
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP="dist/Lanes.app"

# Each build carries the ffmpeg of its own processor (scripts/build-ffmpeg.sh).
if [ "${UNIVERSAL:-}" = 1 ]; then
    for TARGET in aarch64-apple-darwin x86_64-apple-darwin; do
        FFMPEG=$(scripts/build-ffmpeg.sh "$TARGET")
        LANES_FFMPEG=$FFMPEG cargo build --release --target "$TARGET"
    done
    BIN=$(mktemp -d)/lanes
    lipo -create -output "$BIN" target/aarch64-apple-darwin/release/lanes target/x86_64-apple-darwin/release/lanes
else
    FFMPEG=$(scripts/build-ffmpeg.sh "$(rustc -vV | sed -n 's/^host: //p')")
    LANES_FFMPEG=$FFMPEG cargo build --release
    BIN=target/release/lanes
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/lanes"

ICONSET=$(mktemp -d)/AppIcon.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -z "$s" "$s" assets/icon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) assets/icon.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat >"$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Lanes</string>
    <key>CFBundleDisplayName</key><string>Lanes</string>
    <key>CFBundleIdentifier</key><string>com.eliasbui.lanes</string>
    <key>CFBundleExecutable</key><string>lanes</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

# An ad-hoc signature with a stable identifier, so the download permissions and the update check hold across launches.
codesign --force --deep --sign - --identifier com.eliasbui.lanes "$APP"
echo "$APP ($VERSION)"
