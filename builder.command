#!/bin/bash
# SyncDash mac builder — dist/ 前端产物随 git 提供，Mac 无需 node/npm。
# Tauri 在编译期把 ../dist 嵌进二进制，纯 cargo 即可产出完整 GUI 应用。
cd "$(dirname "$0")"
set -e

echo "== SyncDash mac builder =="
if [ ! -d dist ]; then
    echo "dist/ 不存在 —— 在 Windows 上跑一次 'npm run build' 并提交，再 git pull"
    exit 1
fi

echo "[1/2] cargo build --release -p syncdash-desktop (GUI)"
cargo build --release -p syncdash-desktop
echo "      -> target/release/syncdash-desktop"

echo "[2/2] cargo build --release -p syncdash (CLI)"
cargo build --release -p syncdash
echo "      -> target/release/syncdash"

echo "完成。GUI 直接运行 ./target/release/syncdash-desktop 即可（个人使用无需打 .app 包）"
