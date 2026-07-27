#!/bin/bash
# SyncDash mac builder — the dist/ frontend build ships via git, so Mac needs no node/npm.
# Tauri embeds ../dist into the binary at compile time; plain cargo produces the full GUI app.
cd "$(dirname "$0")"
set -e

echo "== SyncDash mac builder =="
if [ ! -d dist ]; then
    echo "dist/ is missing — run 'npm run build' once on Windows and commit it, then git pull"
    exit 1
fi

echo "[1/2] cargo build --release -p syncdash-desktop (GUI)"
cargo build --release -p syncdash-desktop
echo "      -> target/release/syncdash-desktop"

echo "[2/2] cargo build --release -p syncdash (CLI)"
cargo build --release -p syncdash
echo "      -> target/release/syncdash"

echo "Done. Run the GUI directly with ./target/release/syncdash-desktop (no .app bundle needed for personal use)"
