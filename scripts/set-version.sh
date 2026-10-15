#!/usr/bin/env bash
# 版本号四处一起改（约定：版本号随每次更新一起升）
# 用法：bash scripts/set-version.sh 0.3.13
#   1. app/src-tauri/tauri.conf.json   "version"
#   2. app/src-tauri/Cargo.toml        version =
#   3. app/ui-react/package.json       "version"
#   4. app/ui-react/src/App.tsx        appVersion ?? "x.y.z" 兜底文案（多处）
# Cargo.lock 里 wf_installer 的版本行构建时自动刷新，这里不动。
set -eu

NEW="${1:?用法: bash scripts/set-version.sh <新版本号，如 0.3.13>}"
case "$NEW" in
  *[!0-9.]* | .* | *..*) echo "版本号格式不对：$NEW（期望 x.y.z）" >&2; exit 1 ;;
esac
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

sed -i "s/\"version\": *\"[0-9][^\"]*\"/\"version\": \"$NEW\"/" "$ROOT/app/src-tauri/tauri.conf.json"
sed -i "s/^version = \"[0-9][^\"]*\"/version = \"$NEW\"/" "$ROOT/app/src-tauri/Cargo.toml"
sed -i "s/\"version\": *\"[0-9][^\"]*\"/\"version\": \"$NEW\"/" "$ROOT/app/ui-react/package.json"
sed -i "s/appVersion ?? \"[0-9][^\"]*\"/appVersion ?? \"$NEW\"/g" "$ROOT/app/ui-react/src/App.tsx"

echo "已改到 $NEW："
grep -m1 -o '"version": *"[^"]*"' "$ROOT/app/src-tauri/tauri.conf.json"
grep -m1 '^version = ' "$ROOT/app/src-tauri/Cargo.toml"
grep -m1 -o '"version": *"[^"]*"' "$ROOT/app/ui-react/package.json"
grep -o 'appVersion ?? "[^"]*"' "$ROOT/app/ui-react/src/App.tsx" | sort | uniq -c
