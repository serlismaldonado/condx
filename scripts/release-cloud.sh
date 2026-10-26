#!/usr/bin/env bash
# El release en la nube (respaldo de scripts/release.sh cuando no hay una Mac): sube la versión, crea
# el tag y lanza el workflow de GitHub (macOS), que compila, firma el manifiesto con el secreto
# CONDX_UPDATE_KEY y publica. Tarda más (15–20 min); los daemons lo ofrecen igual cuando sale.
#
#   scripts/release-cloud.sh 0.2.1    # o sin versión: la siguiente de parche
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
REPO="${CONDX_RELEASE_REPO:-serlismaldonado/condx}"
CURRENT="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
VERSION="${1:-}"
VERSION="${VERSION#v}"
if [ -z "$VERSION" ]; then
  IFS=. read -r MA MI PA <<<"$CURRENT"
  VERSION="$MA.$MI.$((PA + 1))"
fi
TAG="v$VERSION"
[ -z "$(git status --porcelain -- . ':!target')" ] || { echo "the repo has uncommitted changes: commit them first" >&2; exit 2; }
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then echo "tag $TAG already exists" >&2; exit 2; fi
# Cargo.lock sigue a Cargo.toml (el CI compila con --locked implícito en los tests).
sed -i.bak "s/^version = \"$CURRENT\"/version = \"$VERSION\"/" Cargo.toml && rm -f Cargo.toml.bak
cargo update -w -q 2>/dev/null || true
git add Cargo.toml Cargo.lock
git commit -q -m "release: $TAG"
git tag -a "$TAG" -m "Condx $TAG"
git -c credential.helper= -c credential.helper='!gh auth git-credential' push -q origin HEAD "$TAG"
gh workflow run release.yml -R "$REPO" -f tag="$TAG"
echo "Cloud build of $TAG started on GitHub Actions (15–20 min)."
