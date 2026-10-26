#!/usr/bin/env bash
# Instala en esta Mac el daemon (compilado en release) y el host de agentes, firmando con la
# identidad estable si existe (scripts/create-signing-cert.sh), y reinicia el servicio. El binario
# va a un archivo nuevo y se renombra: sobrescribirlo en el lugar hace que macOS lo mate (-9).
set -euo pipefail
cd "$(dirname "$0")/.."
NAME="${CONDX_SIGN_IDENTITY:-Condx Dev}"
BIN="$HOME/.condx/daemon/bin"
[ -n "${SKIP_BUILD:-}" ] || cargo build --release -p devtool-daemon
mkdir -p "$BIN" "$HOME/.condx/backups"
[ -f "$BIN/condxd" ] && cp "$BIN/condxd" "$HOME/.condx/backups/condxd-$(date +%Y%m%d-%H%M%S)"
cp target/release/condxd "$BIN/condxd.new"
if security find-identity -v -p codesigning | grep -q "\"$NAME\""; then
  codesign --force --sign "$NAME" --identifier dev.condx.daemon "$BIN/condxd.new"
  echo "Firmado con “$NAME”."
else
  echo "Sin la identidad “$NAME”: macOS volverá a pedir el Llavero (scripts/create-signing-cert.sh)."
fi
mv "$BIN/condxd.new" "$BIN/condxd"
rsync -a --delete agent-host/src/ "$HOME/.condx/daemon/agent-host/src/"
# Las dependencias del host de agentes: si cambió el lock, se instalan de nuevo (sin las de desarrollo).
HOST="$HOME/.condx/daemon/agent-host"
if ! cmp -s agent-host/package-lock.json "$HOST/package-lock.json"; then
  cp agent-host/package.json agent-host/package-lock.json "$HOST/"
  (cd "$HOST" && npm ci --omit=dev --silent) && echo "Dependencias del host de agentes instaladas."
fi
launchctl kickstart -k "gui/$(id -u)/dev.condx.daemon"
echo "Daemon instalado y reiniciado."
