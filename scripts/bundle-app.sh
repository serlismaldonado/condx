#!/usr/bin/env bash
# Empaqueta la app GPUI como «Condx.app» (bundle mínimo, sin firma de Developer ID)
# en ~/Applications. Uso: scripts/bundle-app.sh [debug|release]
# APP_DIR cambia la carpeta (el release la arma en dist/); SKIP_BUILD=1 usa lo ya compilado.
set -euo pipefail
PROFILE="${1:-release}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
if [ -z "${SKIP_BUILD:-}" ]; then
  if [ "$PROFILE" = "release" ]; then cargo build --release -p devtool-app; else cargo build -p devtool-app; fi
fi
APP_DIR="${APP_DIR:-$HOME/Applications}"
mkdir -p "$APP_DIR"
APP="$APP_DIR/Condx.app"
# La del nombre viejo, si quedó.
rm -rf "$APP_DIR/Heskala Devtool.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp assets/brand/Condx.icns "$APP/Contents/Resources/Condx.icns"
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
  <key>CFBundleIconFile</key><string>Condx</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
# Con la identidad estable (scripts/create-signing-cert.sh) si existe; si no, ad hoc.
SIGN_ID="${CONDX_SIGN_IDENTITY:-Condx Dev}"
if security find-identity -v -p codesigning | grep -q "\"$SIGN_ID\""; then
  codesign --force --sign "$SIGN_ID" "$APP" >/dev/null
else
  codesign --force --sign - "$APP" >/dev/null
fi
echo "Listo: $APP"
