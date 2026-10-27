#!/usr/bin/env bash
# Installs Condx from the latest GitHub release: the app into ~/Applications and the daemon
# (condxd) as a launchd service. Usage:
#
#   curl -fsSL https://condx.app/install.sh | bash
#
# CONDX_VERSION=v0.2.0 picks a release; CONDX_APP_DIR changes where the app goes;
# CONDX_DRY_RUN=1 downloads and checks everything but installs nothing.
# The builds aren't signed with a Developer ID yet, so this clears macOS's quarantine flag on
# what it downloaded (only after checking its SHA-256 against the release).
set -euo pipefail

REPO="serlismaldonado/condx"
APP_DIR="${CONDX_APP_DIR:-$HOME/Applications}"

say() { printf '\033[1;33m›\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "Condx runs on macOS."
[ "$(uname -m)" = "arm64" ] || die "Release builds are for Apple Silicon. On Intel, build from source: https://github.com/$REPO#install-from-source"
major="$(sw_vers -productVersion | cut -d. -f1)"
[ "$major" -ge 15 ] || die "Condx needs macOS 15 or later."
for tool in curl tar shasum; do command -v "$tool" >/dev/null || die "missing $tool"; done

if [ -n "${CONDX_VERSION:-}" ]; then
  api="https://api.github.com/repos/$REPO/releases/tags/$CONDX_VERSION"
else
  api="https://api.github.com/repos/$REPO/releases/latest"
fi
say "Looking for the release…"
json="$(curl -fsSL -H 'Accept: application/vnd.github+json' "$api")" || die "couldn't reach GitHub ($api)"
# Only download from this repo's release assets.
prefix="https://github.com/$REPO/releases/download/"
url="$(printf '%s' "$json" | grep -o "\"browser_download_url\": *\"[^\"]*macos-arm64\.tar\.gz\"" | head -1 | sed 's/.*"\(https[^"]*\)"/\1/')"
[ -n "$url" ] || die "this release has no macOS build"
case "$url" in "$prefix"*) ;; *) die "unexpected download URL: $url" ;; esac
tag="$(printf '%s' "$url" | sed "s#^$prefix##" | cut -d/ -f1)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
file="$tmp/$(basename "$url")"
say "Downloading Condx $tag…"
curl -fsSL -o "$file" "$url"
curl -fsSL -o "$file.sha256" "$url.sha256"
expected="$(cut -d' ' -f1 < "$file.sha256")"
actual="$(shasum -a 256 "$file" | cut -d' ' -f1)"
[ -n "$expected" ] && [ "$expected" = "$actual" ] || die "checksum mismatch (expected $expected, got $actual)"

tar -xzf "$file" -C "$tmp"
[ -d "$tmp/Condx.app" ] && [ -x "$tmp/condxd" ] || die "the archive doesn't have Condx.app and condxd"
xattr -dr com.apple.quarantine "$tmp/Condx.app" "$tmp/condxd" 2>/dev/null || true
if [ -n "${CONDX_DRY_RUN:-}" ]; then
  "$tmp/condxd" --help >/dev/null || die "condxd doesn't run"
  say "Dry run: Condx $tag downloaded and verified ($actual); nothing installed."
  exit 0
fi

say "Installing the daemon (launchd service dev.condx.daemon)…"
"$tmp/condxd" install

say "Installing the app in $APP_DIR…"
mkdir -p "$APP_DIR"
rm -rf "$APP_DIR/Condx.app"
mv "$tmp/Condx.app" "$APP_DIR/Condx.app"

printf '\n\033[1;32m✓\033[0m Condx %s is installed. Open it with:\n\n  open "%s/Condx.app"\n\n' "$tag" "$APP_DIR"
printf 'With no projects yet, the app walks you through the first steps.\n'
