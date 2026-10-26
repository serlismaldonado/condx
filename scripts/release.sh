#!/usr/bin/env bash
# Un release de Condx desde esta Mac (local primero; docs/design/cloud-site.md §9.3): sube la versión,
# compila en release (con la caché de esta Mac, minutos en vez de los 15+ del CI), arma el paquete
# (app, daemon, host de agentes), firma el manifiesto con la llave de actualización local y publica el
# release en GitHub. El workflow de GitHub queda de respaldo (manual) para cuando no hay Mac.
#
#   scripts/release.sh 0.2.1          # esa versión
#   scripts/release.sh                # la siguiente de parche
#   scripts/release.sh 0.2.1 --dry-run
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
KEY="${CONDX_UPDATE_KEY_FILE:-$HOME/.condx/update-signing-key.pem}"
OPENSSL="${OPENSSL:-/opt/homebrew/opt/openssl@3/bin/openssl}"
REPO="${CONDX_RELEASE_REPO:-serlismaldonado/condx}"
DRY=""
VERSION=""
for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    *) VERSION="${a#v}" ;;
  esac
done

CURRENT="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
if [ -z "$VERSION" ]; then
  IFS=. read -r MA MI PA <<<"$CURRENT"
  VERSION="$MA.$MI.$((PA + 1))"
fi
TAG="v$VERSION"
NAME="condx-$TAG-macos-arm64"

# Preflight: lo que hace falta para compilar y firmar aquí (si falta, el release va por la nube).
[ "$(uname -s)" = "Darwin" ] || { echo "release.sh builds the macOS app: run it on a Mac (or use the cloud workflow)" >&2; exit 2; }
[ -f "$KEY" ] || { echo "no update key at $KEY" >&2; exit 2; }
[ -x "$OPENSSL" ] || { echo "no OpenSSL 3 at $OPENSSL (brew install openssl@3)" >&2; exit 2; }
command -v gh >/dev/null || { echo "no gh" >&2; exit 2; }
[ -z "$(git status --porcelain -- . ':!target')" ] || { echo "the repo has uncommitted changes: commit them first" >&2; exit 2; }
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then echo "tag $TAG already exists" >&2; exit 2; fi

echo "Condx $CURRENT → $VERSION ($TAG), built on this Mac"
if [ -n "$DRY" ]; then
  echo "Would: bump Cargo.toml, cargo test + build, package $NAME.tar.gz, sign manifest.json, tag $TAG, push, gh release create"
  exit 0
fi

sed -i '' "s/^version = \"$CURRENT\"/version = \"$VERSION\"/" Cargo.toml
cargo build --release -p devtool-app -p devtool-daemon
cargo test --workspace -q

DIST="$(mktemp -d)"
trap 'rm -rf "$DIST"' EXIT
APP_DIR="$DIST" SKIP_BUILD=1 scripts/bundle-app.sh release >/dev/null
cp target/release/condxd "$DIST/condxd"
codesign --force --sign - "$DIST/condxd"
mkdir -p "$DIST/agent-host" && cp -R agent-host/src agent-host/package.json agent-host/package-lock.json "$DIST/agent-host/"
OUT="$ROOT/target/release-dist"
mkdir -p "$OUT"
tar -czf "$OUT/$NAME.tar.gz" -C "$DIST" Condx.app condxd agent-host
( cd "$OUT" && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256" )
SUM="$(cut -d' ' -f1 "$OUT/$NAME.tar.gz.sha256")"
printf '{"version":"%s","tag":"%s","asset":"%s","url":"https://github.com/%s/releases/download/%s/%s","sha256":"%s","published":"%s","builtOn":"mac"}' \
  "$VERSION" "$TAG" "$NAME.tar.gz" "$REPO" "$TAG" "$NAME.tar.gz" "$SUM" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$OUT/manifest.json"
"$OPENSSL" pkeyutl -sign -inkey "$KEY" -rawin -in "$OUT/manifest.json" -out "$OUT/manifest.json.sig"

git add Cargo.toml Cargo.lock
git commit -q -m "release: $TAG"
git tag -a "$TAG" -m "Condx $TAG"
git -c credential.helper= -c credential.helper='!gh auth git-credential' push -q origin HEAD "$TAG"
gh release create "$TAG" -R "$REPO" --title "Condx $TAG" --generate-notes \
  "$OUT/$NAME.tar.gz" "$OUT/$NAME.tar.gz.sha256" "$OUT/manifest.json" "$OUT/manifest.json.sig" install.sh
echo "Released $TAG from this Mac."
