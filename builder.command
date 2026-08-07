#!/bin/sh
set -u

project_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export PATH="$HOME/.cargo/bin:$PATH"

builder_root=${BUILDER_HOME:-}
if [ -z "$builder_root" ]; then
  search_root=$project_root
  while :; do
    if [ -f "$search_root/0_devControl/builder/Cargo.toml" ]; then
      builder_root=$search_root/0_devControl/builder
      break
    fi
    parent_root=$(dirname -- "$search_root")
    [ "$parent_root" != "$search_root" ] || break
    search_root=$parent_root
  done
fi

if [ -z "$builder_root" ] || [ ! -f "$builder_root/Cargo.toml" ]; then
  echo "Builder General not found. Move the project under the Code root or set BUILDER_HOME."
  exit 1
fi
builder_root=$(CDPATH= cd -- "$builder_root" && pwd)
export BUILDER_HOME=$builder_root
CARGO_TARGET_DIR=$builder_root/target
export CARGO_TARGET_DIR

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found. Install Rust from https://rustup.rs"
  exit 1
fi

receipt="$project_root/tools/builder/.builder-general-clean-receipt-$$-$(date +%s)"
if [ -e "$receipt" ] || [ -L "$receipt" ]; then
  echo "Refusing to reuse cleanup receipt: $receipt"
  exit 1
fi
export BUILDER_LAUNCHER_CLEAN_RECEIPT=$receipt

cargo run --quiet --manifest-path "$builder_root/Cargo.toml" -p builder-general -- \
  --project-root-hint "$project_root" syncdash "$@"
builder_rc=$?

if [ -f "$receipt" ]; then
  IFS= read -r receipt_token < "$receipt" || receipt_token=
  rm -f -- "$receipt"
  if [ "$builder_rc" -eq 0 ]; then
    if [ "$receipt_token" = "builder-general-clean-v1" ]; then
      cargo clean --quiet --manifest-path "$builder_root/Cargo.toml" || builder_rc=1
    else
      echo "Invalid Builder cleanup receipt; shared cache was preserved."
      builder_rc=1
    fi
  fi
fi
exit "$builder_rc"
