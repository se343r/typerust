#!/bin/sh
# Builds the Typerust panel plug-in for PhotoCraft from this directory.
# Needs the wasm32-unknown-unknown target: `rustup target add wasm32-unknown-unknown`.
set -eu
cd "$(dirname "$0")"
cargo build --release --target wasm32-unknown-unknown
out=target/wasm32-unknown-unknown/release/typerust.wasm
cp "$out" typerust.wasm
echo "built $out ($(wc -c < "$out") bytes)"
echo "copied to $(pwd)/typerust.wasm"
echo "install it in PhotoCraft with: plugin.install {\"path\": \"$(pwd)/typerust.wasm\"}"
