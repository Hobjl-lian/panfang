#!/usr/bin/env bash
# 盘坊授权服务：每日备份（授权库 + 加密包体 + 密钥环境文件）
#
# 为什么三样都要备：wf.db 是「谁买过什么」，cipher/ 是所有付费表盘的密文，
# /etc/wf-auth.env 里的 WF_KEK 是解开这些密文的唯一钥匙 —— 少一样都恢复不了。
#
# 保留最近 14 份，落在 /var/backups/wf-auth/（root 专属权限）。
# 由 systemd timer（wf-auth-backup.timer）每天 04:30 左右触发。
#
# 注意：App 发版的安装包（$DATA_DIR/app/*.apk，单个约 23MB）**不进备份** ——
# 保留 14 份会撑爆小盘；发布通道的权威副本请自行另存（发版时上传的那份 APK 即为权威件）。
set -euo pipefail
umask 077

DATA_DIR="${WF_DATA_DIR:-/var/lib/wf-auth}"
ENV_FILE="${WF_ENV_FILE:-/etc/wf-auth.env}"
DEST="${WF_BACKUP_DIR:-/var/backups/wf-auth}"
KEEP="${WF_BACKUP_KEEP:-14}"

if [ ! -f "$DATA_DIR/wf.db" ]; then
  echo "[backup] 找不到 $DATA_DIR/wf.db，跳过" >&2
  exit 1
fi

mkdir -p "$DEST"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 1) 数据库：能用 sqlite3 就用在线 .backup（带 WAL 也安全）；否则复制三件套
if command -v sqlite3 >/dev/null 2>&1; then
  sqlite3 "$DATA_DIR/wf.db" ".backup '$TMP/wf.db'"
else
  cp -a "$DATA_DIR/wf.db" "$TMP/wf.db"
  for side in wf.db-wal wf.db-shm; do
    [ -f "$DATA_DIR/$side" ] && cp -a "$DATA_DIR/$side" "$TMP/$side"
  done
fi

# 2) 加密包体
if [ -d "$DATA_DIR/cipher" ]; then
  mkdir -p "$TMP/cipher"
  cp -a "$DATA_DIR/cipher/." "$TMP/cipher/" 2>/dev/null || true
fi

# 3) 环境文件（含 WF_KEK / 管理令牌）
[ -f "$ENV_FILE" ] && cp -a "$ENV_FILE" "$TMP/wf-auth.env"

STAMP="$(date +%Y-%m-%d_%H%M)"
OUT="$DEST/wf-auth-$STAMP.tar.gz"
tar -czf "$OUT" -C "$TMP" .

# 4) 只留最近 KEEP 份
ls -1t "$DEST"/wf-auth-*.tar.gz 2>/dev/null | tail -n +$((KEEP + 1)) | xargs -r rm -f

# 5) 证书到期体检（2026-10-15 加）：每天顺手看一眼，剩不到 21 天就在日志里喊、并落一份告警文件。
#    为什么放在这：证书一过期，全 App 的 HTTPS 直接挂 —— 这件事必须有「每天有人看一眼」的兜底。
#    注意：`certbot renew --dry-run` 走的是 staging 端点，这台机器到 staging 不稳，dry-run 失败
#    **不代表**生产续期有问题；要验证真实续期路径就用上面告警里那条 certonly 命令。
CERT="/etc/letsencrypt/live/api.hobjl-shop.top/fullchain.pem"
if [ -f "$CERT" ]; then
  END="$(openssl x509 -in "$CERT" -noout -enddate 2>/dev/null | cut -d= -f2)"
  if [ -n "$END" ]; then
    LEFT=$(( ( $(date -d "$END" +%s) - $(date +%s) ) / 86400 ))
    if [ "$LEFT" -lt 21 ]; then
      echo "[backup] ⚠️ 证书只剩 ${LEFT} 天（${END}）—— 立刻跑：certbot certonly --cert-name api.hobjl-shop.top --nginx -d api.hobjl-shop.top --force-renewal && systemctl reload nginx" >&2
      echo "$(date '+%F %T') 证书只剩 ${LEFT} 天（${END}）" >> "$DEST/CERT-EXPIRY-ALERT.txt"
    else
      echo "[backup] 证书还有 ${LEFT} 天（${END}）"
    fi
  fi
fi

echo "[backup] $(date '+%F %T') → $OUT（$(du -h "$OUT" | cut -f1)，共保留 $(ls -1 "$DEST"/wf-auth-*.tar.gz | wc -l) 份）"
