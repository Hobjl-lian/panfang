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

# 模块在**各自的仓库**里，abtools sync 拿到的是它们的 HEAD —— 会随时间漂移
#（例如 account 后来加了 hostio 依赖，头一天能编、第二天就编不过）。
# 所以这里把本版本验证过的每个模块提交钉死；改依赖版本时按实际构建结果更新这张表。
PINNED_MODULES=$(cat <<'EOF'
modules/account c427e2d75b7e148f76ffc2d6db37458cf69f8857
modules/app_wasm ec015807597f9dd9d8061b66d0229ea9060e15ac
modules/bluetooth be9d420adbfbefb568a2f96f3c2cfba3e3c9f46c
modules/core e0a5e7887390135241f75023f60b81ccf88f2fb9
modules/frontbridge ef64fb69a50ffb97d78b29923789085251327ebb
modules/pb 03a92010056dd41af114f6f46fd612104b27bd7b
modules/pluginsystem 2d3e1273d24b96383534f9888ca1fd7c08410150
modules/provider 836fafde407d0111ddba65a370a127cc814b620c
modules/vivo_msgpack baa814b3d90454c127008a85ac307acf92b4914f
plugins/inappbrowser a369eca21ad1d7277568851bb039ee366343b2af
plugins/btclassic-spp 685d1647dce16597c91fe81736c85a9020a71c4a
EOF
)

echo "==> 1/3 拉取 AstroBox-NG @ ${PINNED_COMMIT:0:9}"
if [ -d "$DEST/.git" ]; then
  git -C "$DEST" fetch --all --tags
else
  mkdir -p "$ROOT/repos"
  git clone "$UPSTREAM_URL" "$DEST"
fi
git -C "$DEST" checkout "$PINNED_COMMIT"

echo "==> 2/5 拉取公开模块（abtools sync）"
if command -v python >/dev/null 2>&1; then PY=python; else PY=python3; fi
( cd "$DEST" && "$PY" abtools.py sync ) || {
  echo "!! abtools sync 未完全成功（私有模块会失败，属正常）。请确认 core/pb/account/bluetooth 已存在。"
}

echo "==> 3/5 把各模块钉到本版本验证过的提交"
while read -r rel sha; do
  [ -z "${rel:-}" ] && continue
  if [ -d "$DEST/src-tauri/$rel/.git" ]; then
    # 必须能切到钉住的提交；切不到说明上游改写了历史，先停下来别编出一个不一样的包
    if git -C "$DEST/src-tauri/$rel" checkout -q "$sha" 2>/dev/null; then
      echo "    $rel @ ${sha:0:7}"
    else
      echo "!! $rel 切不到 ${sha:0:7}（上游历史可能被改写）" >&2
      exit 1
    fi
  else
    echo "!! 缺少模块 $rel" >&2
    exit 1
  fi
done <<< "$PINNED_MODULES"

echo "==> 4/5 放置 netcfg 占位模块"
mkdir -p "$DEST/src-tauri/modules/netcfg/src"
cp "$ROOT/vendor/netcfg/Cargo.toml" "$DEST/src-tauri/modules/netcfg/Cargo.toml"
cp "$ROOT/vendor/netcfg/src/lib.rs"  "$DEST/src-tauri/modules/netcfg/src/lib.rs"

echo "==> 5/5 应用各上游仓库的本地改动"
# App 侧直接依赖的上游改动（`ConnectError::Failed`、inappbrowser 的 set_back_lock 等）
# 只存在于本地工作树，上游仓库里没有。补丁路径相对各自仓库根，所以在仓库目录里 apply；
# 已经打过的再跑一次会失败，按幂等处理（reverse --check 命中就跳过）。
apply_patch() {
  local repo="$DEST/src-tauri/$1" patch="$ROOT/docs/patches/$2"
  if [ ! -f "$patch" ]; then
    echo "!! 找不到 $patch" >&2
    exit 1
  fi
  if git -C "$repo" apply --check "$patch" 2>/dev/null; then
    git -C "$repo" apply "$patch"
    echo "    $1 ← $2"
  elif git -C "$repo" apply --reverse --check "$patch" 2>/dev/null; then
    echo "    $1 ← $2（已应用，跳过）"
  else
    echo "!! $1 的补丁与钉住的提交对不上（先核对 PINNED_MODULES）" >&2
    exit 1
  fi
}
apply_patch modules/bluetooth bluetooth-local-changes.patch
apply_patch modules/core core-local-changes.patch
apply_patch plugins/btclassic-spp btclassic-spp-local-changes.patch
apply_patch plugins/inappbrowser inappbrowser-local-changes.patch

echo
echo "完成。接下来："
echo "  桌面调试：  cd app && npx tauri dev"
echo "  安卓构建：  cd app && npx tauri android build --apk --target aarch64"
echo "（构建细节见 README「构建」一节；本脚本只准备源码依赖）"
