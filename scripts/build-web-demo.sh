#!/usr/bin/env bash
# Compila la app a WebAssembly (con el daemon de ejemplo) y deja la demo en site/demo/ para la
# landing. Necesita: rustup target add wasm32-unknown-unknown y wasm-bindgen-cli (la misma
# versión que el crate wasm-bindgen del Cargo.lock).
# RUSTC_BOOTSTRAP=1: gpui-web usa wasm_thread, que pide una característica de nightly.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
RUSTC_BOOTSTRAP=1 cargo build -p devtool-app --lib --target wasm32-unknown-unknown --profile web
OUT="$ROOT/site/demo"
rm -rf "$OUT/pkg"
wasm-bindgen --target web --out-dir "$OUT/pkg" --no-typescript target/wasm32-unknown-unknown/web/devtool_app.wasm
# Los íconos de GPUI Kit se piden a /assets/icons/ en la web.
KIT_ASSETS="$(find ~/.cargo/registry/src -maxdepth 1 -type d -name 'index.crates.io-*' | head -1)/gpui-kit-assets-0.8.0/assets"
mkdir -p "$ROOT/site/assets"
rm -rf "$ROOT/site/assets/icons"
cp -R "$KIT_ASSETS/icons" "$ROOT/site/assets/icons"
echo "Demo: $OUT ($(du -sh "$OUT/pkg" | cut -f1))"
# El instalador también se sirve desde la web (condx.vercel.app/install.sh).
cp "$ROOT/install.sh" "$ROOT/site/install.sh"
