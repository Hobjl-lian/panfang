# 盘坊授权服务（wf-auth-server）

付费表盘的**加密分发 + 兑换码 + 授权**服务端。实现 `docs/接口契约-授权服务-v1.md`。

- 技术栈：Rust + axum + SQLite（rusqlite bundled）+ 本地密文目录（可平移到对象存储）
- 单文件二进制，服务器上无需额外运行时
- **明文永不落盘**：上传即加密（AES-256-GCM），密钥以 KEK 包裹入库；下载时用**设备公钥**包裹 CEK
- 本机跑通 → 换 Base URL 即可上线（契约里 Base URL 是配置项）

---

## 快速开始（本机）

```bash
cd server
export PATH="$HOME/.cargo/bin:$PATH"
cargo run                        # 默认 127.0.0.1:8787
# 打开 http://127.0.0.1:8787/   （管理页；首次需在右上角填管理令牌）
```

环境变量：

| 变量 | 默认 | 说明 |
|---|---|---|
| `WF_BIND` | `127.0.0.1:8787` | 监听地址；手机联调改 `0.0.0.0:8787` |
| `WF_DATA_DIR` | `./data` | 数据库、主密钥、密文目录 |
| `WF_ADMIN_TOKEN` | `dev-admin-token`（会告警） | 管理端令牌（管理页与 `/admin/*`） |
| `WF_PUBLIC_BASE` | 由 WF_BIND 推导 | 生成下载直链用的对外地址；手机联调要写 `http://<电脑局域网IP>:8787` |
| `WF_KEK` | 自动生成 `<data>/kek.bin` | 主密钥（64 位 hex）。**上线务必显式设置并备份** |
| `WF_URL_SECRET` | 自动生成 | 直链签名密钥 |
| `WF_DEVICE_LIMIT` | `2` | 每个账号每产品可激活的设备数 |

## 管理页（`/`）

1. **产品**：建表盘商品（可用商城 `wf-XX` 关联）
2. **设备变体**：一份 `.bin` 覆盖的机型集合（如「手环10/11」→ 小米手环10、小米手环11）
3. **上传 .bin**：拖入文件或整个版本文件夹 → **自动识别**设备变体与版本号（见下）→ 确认上传（上传即加密）
4. **版本**：切换「设为最新」（用户默认装最新版）
5. **兑换码**：批量生成 → 复制/下载 CSV → 贴到爱发电「自动回复 → 随机发放」
6. **授权与设备**：查激活、吊销（退款用）

## 自动识别规则（上传时）

| 目标 | 依据（从强到弱） |
|---|---|
| 设备变体 | ① `.bin` 头部**表盘 id**（0x28，同产品同设备族稳定）② **画布尺寸**（背景图推导：212×520→手环10/11，192×490→手环9，336×480→9Pro/10Pro，432×514→RW5/6）③ 文件名标记（`-9P10P` / `-1011` / `-RW5RW6`） |
| 版本号 | ① 文件夹/文件名里的 `vN`（如 `3.0/AWOS-9-v3.bin`）② **内容哈希相同** → 复用已有版本号 ③ 否则按该变体**历史步长**递增（1.0→2.0 就继续大版本，3.0→3.1 就继续小版本） |
| 新旧判定 | 头部 0x04「头部版本」（同一工程每次导出 +1）：比最近上传大 = 新版本；相等但内容不同 = 重构建（提示）；更小 = 旧构建（提示不阻断） |

识别结果在管理页逐条展示（含依据），任何一条都可人工改。

## 客户端 API（App 用）

```
POST /v1/redeem      {code, mi_user_id, device_fp, device_pubkey, device_name}   → license_token
GET  /v1/me          [Bearer]                                                    → 我的授权 + 设备
POST /v1/activate    {product_id, device_fp, device_pubkey, device_name}         → 换机
GET  /v1/manifest    ?product_id=&variant_id=   [Bearer]                          → 版本清单
POST /v1/download    {file_id, device_fp?}      [Bearer]                          → 直链 + 包裹密钥
GET  /dl/:file_id    ?exp=&uid=&sig=                                             → 密文（5 分钟签名直链）
```

## 自测（本机全链路）

```bash
# 终端 1：起服务
WF_ADMIN_TOKEN=dev-admin-token cargo run
# 终端 2：跑端到端自测（会把真实 .bin 走完 上传→加密→发码→兑换→下载→设备侧解密→哈希校验）
WF_BIN_DIR="D:/设计相关/AWOS-照片表盘/表盘文件" cargo run --bin selftest
# 单文件检查器（识别 id/名称/画布/版本）
cargo run --bin wfbin -- "D:/设计相关/AWOS-照片表盘/表盘文件/3.0/AWOS-1011-v3.bin"
```

## 上线（自有服务器）

```bash
# 1) 构建
cargo build --release          # 产物 target/release/wf-auth-server

# 2) 主密钥与令牌（务必显式设置，别用默认值）
export WF_KEK=$(openssl rand -hex 32)     # 备份到安全处；丢了则所有密文无法解开
export WF_ADMIN_TOKEN=$(openssl rand -hex 24)
export WF_BIND=127.0.0.1:8787             # 由 Caddy/nginx 反代并加 TLS
export WF_PUBLIC_BASE=https://api.example.com
export WF_DATA_DIR=/var/lib/wf-auth

# 3) 常驻（systemd）
#    ExecStart=/usr/local/bin/wf-auth-server
```

反代要点：

- 反代到 8787，开启 HTTPS；`/dl/` 走同一域名即可（量大可把密文目录挂在对象存储/CDN 后面，改 `download` 里生成 URL 的一处）
- 备份：`data/wf.db`（授权/码）+ `data/cipher`（密文）+ `WF_KEK`

## 目录结构

```
src/
  main.rs        路由、配置、请求日志
  crypto.rs      KEK/CEK、AES-256-GCM、X25519 包裹、直链签名、兑换码
  bin_inspect.rs .bin 元数据检查器（表盘 id/名称/画布/头部版本 + 版本号推断）
  derive.rs      自动识别：变体匹配（单次取数，避免锁重入）
  db.rs          SQLite 表结构
  state.rs       状态、统一错误、鉴权
  api_admin.rs   管理端接口
  api_client.rs  客户端接口 + 直链
  bin/selftest.rs 端到端自测（含设备侧解密）
  bin/wfbin.rs   .bin 检查器 CLI
static/admin.html 管理页（无构建链）
```

## 已知边界

- 该服务防的是「翻文件夹 / 分享链接 / 抓包直接拿文件」；root+Frida 级别的内存逆向防不住（所有客户端方案同理）
- 下载直链目前的密文就是同一份（未做 per-user 重加密），泄露追溯靠下载日志（`download_logs`）
- 管理页无多用户/角色，只有一个 admin 令牌；够个人规模用
