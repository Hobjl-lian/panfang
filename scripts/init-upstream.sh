#!/usr/bin/env bash
# 拉取本项目依赖的上游开源核心（AstroBox-NG，AGPL-3.0 + 附加署名条款）
#
# 用法：在仓库根目录执行  bash scripts/init-upstream.sh
#
# 做的事：
#   1) 把 AstroBox-NG 克隆到 repos/AstroBox-NG 并切到本项目验证过的提交
#   2) 用上游自带的 abtools 拉取公开模块（core / pb / account / bluetooth / inappbrowser …）
#   3) 补上 netcfg 占位模块（上游 netcfg 是私有仓库，account 只用到它的两个入口；
#      占位实现见 vendor/netcfg/，属于本项目源码，同样以 AGPL-3.0 分发）
set -euo pipefail

PINNED_COMMIT="4735de617785701985b4bc99fc5842bf71730bd3"   # AstroBox-NG 2.2.0
UPSTREAM_URL="https://github.com/AstralSightStudios/AstroBox-NG.git"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/repos/AstroBox-NG"

echo "==> 1/3 拉取 AstroBox-NG @ ${PINNED_COMMIT:0:9}"
if [ -d "$DEST/.git" ]; then
  git -C "$DEST" fetch --all --tags
else
  mkdir -p "$ROOT/repos"
  git clone "$UPSTREAM_URL" "$DEST"
fi
git -C "$DEST" checkout "$PINNED_COMMIT"

echo "==> 2/3 拉取公开模块（abtools sync）"
if command -v python >/dev/null 2>&1; then PY=python; else PY=python3; fi
( cd "$DEST" && "$PY" abtools.py sync ) || {
  echo "!! abtools sync 未完全成功（私有模块会失败，属正常）。请确认 core/pb/account/bluetooth 已存在。"
}

echo "==> 3/3 放置 netcfg 占位模块"
mkdir -p "$DEST/src-tauri/modules/netcfg/src"
cp "$ROOT/vendor/netcfg/Cargo.toml" "$DEST/src-tauri/modules/netcfg/Cargo.toml"
cp "$ROOT/vendor/netcfg/src/lib.rs"  "$DEST/src-tauri/modules/netcfg/src/lib.rs"

echo
echo "完成。接下来："
echo "  桌面调试：  cd app && npx tauri dev"
echo "  安卓构建：  cd app && npx tauri android build --apk --target aarch64"
echo "（构建细节见 README「构建」一节；本脚本只准备源码依赖）"
