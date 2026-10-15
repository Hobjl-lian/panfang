#!/usr/bin/env bash
# 一键部署/升级 盘坊授权服务（幂等：重复执行＝升级重建）
#
# 用法（在服务器上）：
#   bash /opt/wf-auth/deploy/install.sh [域名]
# 默认域名 api.hobjl-shop.top
set -euo pipefail

DOMAIN="${1:-api.hobjl-shop.top}"
SRC_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DATA_DIR=/var/lib/wf-auth
ENV_FILE=/etc/wf-auth.env
BIN=/usr/local/bin/wf-auth-server

echo "==> 源目录 $SRC_DIR / 域名 $DOMAIN"

# 0) 国内镜像（服务器在国内，直连官方源会卡住）
export RUSTUP_DIST_SERVER="${RUSTUP_DIST_SERVER:-https://mirrors.tuna.tsinghua.edu.cn/rustup}"
export RUSTUP_UPDATE_ROOT="${RUSTUP_UPDATE_ROOT:-https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup}"
mkdir -p "$HOME/.cargo"
if [ ! -f "$HOME/.cargo/config.toml" ]; then
  cat >"$HOME/.cargo/config.toml" <<'TOML'
[source.crates-io]
replace-with = "tuna"
[source.tuna]
registry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"
[net]
git-fetch-with-cli = true
TOML
  echo "已配置 crates 清华镜像"
fi

# 1) Rust 工具链（如果没有就装）
if ! command -v cargo >/dev/null 2>&1; then
  echo "==> 安装 Rust 工具链"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || export PATH="$HOME/.cargo/bin:$PATH"

# 2) 构建（首次较慢，之后增量很快）
echo "==> cargo build --release"
cd "$SRC_DIR"
cargo build --release --bin wf-auth-server

# 3) 停服务再替换二进制（避免占用）
systemctl stop wf-auth 2>/dev/null || true
install -m 0755 "$SRC_DIR/target/release/wf-auth-server" "$BIN"
install -d -m 0755 "$DATA_DIR"

# 4) 环境文件（首次生成密钥；已存在则保留，绝不覆盖以免丢 KEK）
if [ ! -f "$ENV_FILE" ]; then
  echo "==> 生成 $ENV_FILE（含主密钥，请备份）"
  # openssl 一定有（certbot 依赖）；xxd 可能没装
  KEK=$(openssl rand -hex 32)
  ADMIN_TOKEN=$(openssl rand -hex 24)
  cat >"$ENV_FILE" <<EOF
# 盘坊授权服务配置（本文件含密钥，请勿外传；备份它！）
WF_DATA_DIR=$DATA_DIR
WF_PUBLIC_BASE=https://$DOMAIN
WF_ADMIN_TOKEN=$ADMIN_TOKEN
WF_KEK=$KEK
WF_DEVICE_LIMIT=2
EOF
  chmod 600 "$ENV_FILE"
else
  echo "==> 复用已存在的 $ENV_FILE（密钥不变）"
fi

# 5) systemd
install -m 0644 "$SRC_DIR/deploy/wf-auth.service" /etc/systemd/system/wf-auth.service
systemctl daemon-reload
systemctl enable wf-auth
systemctl restart wf-auth
sleep 2
systemctl --no-pager --lines=5 status wf-auth || true

# 5.5) 每日自动备份（wf.db + cipher/ + 密钥环境文件，保留 14 份）
#      以前只在这里打印一句「请备份」—— 没人会记得手动备份，出事就是全丢。
install -m 0755 "$SRC_DIR/deploy/wf-auth-backup.sh" /usr/local/bin/wf-auth-backup.sh
install -m 0644 "$SRC_DIR/deploy/wf-auth-backup.service" /etc/systemd/system/wf-auth-backup.service
install -m 0644 "$SRC_DIR/deploy/wf-auth-backup.timer" /etc/systemd/system/wf-auth-backup.timer
systemctl daemon-reload
systemctl enable --now wf-auth-backup.timer >/dev/null 2>&1 || true
echo "==> 每日备份已启用：$(systemctl list-timers wf-auth-backup.timer --no-pager 2>/dev/null | sed -n 2p | awk '{print $1" "$2" "$3}')（目录 ${WF_BACKUP_DIR:-/var/backups/wf-auth}）"

# 6) Nginx server 块 + 证书
if [ ! -f /etc/nginx/sites-available/wf-auth.conf ]; then
  echo "==> 安装 Nginx 配置"
  install -m 0644 "$SRC_DIR/deploy/nginx-api.conf" /etc/nginx/sites-available/wf-auth.conf
  ln -sf /etc/nginx/sites-available/wf-auth.conf /etc/nginx/sites-enabled/wf-auth.conf
  nginx -t && systemctl reload nginx
fi

if ! [ -d "/etc/letsencrypt/live/$DOMAIN" ]; then
  echo "==> 申请证书（需要 DNS 已解析到本机）"
  certbot --nginx -d "$DOMAIN" --non-interactive --agree-tos --register-unsafely-without-email --redirect
else
  echo "==> 证书已存在，跳过"
fi

echo
echo "==> 本地自检"
curl -s "http://127.0.0.1:8787/healthz" && echo "  (127.0.0.1 OK)"
curl -s "https://$DOMAIN/healthz" && echo "  (https OK)"
echo
echo "管理页： https://$DOMAIN/"
echo "令牌：   grep WF_ADMIN_TOKEN $ENV_FILE"
echo "备份：   已自动（每天 04:30 → ${WF_BACKUP_DIR:-/var/backups/wf-auth}，保留 14 份）"
echo "         手工备一份命令： bash /usr/local/bin/wf-auth-backup.sh"
