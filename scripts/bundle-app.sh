#!/usr/bin/env bash
# Empaqueta la app GPUI como «Condx.app» (bundle mínimo, sin firma de Developer ID)
# en ~/Applications. Uso: tools/devtool/scripts/bundle-app.sh [debug|release]
set -euo pipefail
PROFILE="${1:-release}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
if [ "$PROFILE" = "release" ]; then cargo build --release -p devtool-app; else cargo build -p devtool-app; fi
APP="$HOME/Applications/Condx.app"
# La del nombre viejo, si quedó.
rm -rf "$HOME/Applications/Heskala Devtool.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "target/$PROFILE/condx" "$APP/Contents/MacOS/condx"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>dev.condx.app</string>
  <key>CFBundleName</key><string>Condx</string>
  <key>CFBundleDisplayName</key><string>Condx</string>
  <key>CFBundleExecutable</key><string>condx</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$APP" >/dev/null
echo "Listo: $APP"
