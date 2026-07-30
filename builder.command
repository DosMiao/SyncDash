#!/bin/sh
set -u

project_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found. Install Rust from https://rustup.rs"
  exit 1
fi

exec cargo run --quiet --manifest-path "$project_root/tools/builder/Cargo.toml" -- "$@"
