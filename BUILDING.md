# 构建说明（Building）

本仓库是「盘坊」的完整源码。App 为 Tauri 2 + React（安卓为目标平台），服务端为 Rust + axum + SQLite。

## 一、App

### 1. 准备上游依赖（必须）

App 通过 **Cargo 路径依赖** 复用 [AstroBox-NG](https://github.com/AstralSightStudios/AstroBox-NG) 的部分模块。
本仓库不含其源码：

```bash
bash scripts/init-upstream.sh      # 克隆 AstroBox-NG 到 repos/AstroBox-NG，并按钉住的提交检出
```

⚠️ 另外必须应用本项目的本地补丁（对 `repos/AstroBox-NG` 的少量修改，均为我们依赖的行为改动）：

```bash
cd repos/AstroBox-NG && git apply ../../docs/patches/inappbrowser-local-changes.patch
```

（补丁内容见该文件；README「开源许可与署名」一节说明了与上游的关系。）

### 2. 工具链

- Rust stable（`rustup`）
- Node 20+ 与 npm
- JDK 17、Android SDK / NDK（交给 `npx tauri android` 使用）
- Tauri CLI：`npm --prefix app install` 会装好，或用 `cargo install tauri-cli`

### 3. 出包

```bash
cd app
npm --prefix ui-react ci          # 前端依赖
npx tauri android build --apk --target aarch64
```

产物在 `app/src-tauri/gen/android/app/build/outputs/apk/universal/release/`，用**你自己的** keystore 签名即可
（本项目作者的签名密钥不公开，你也无需使用；换签名=另一个 App，仅影响覆盖安装）。

> 经验之谈：Windows 下若工作区路径含中文，`lld` 链接可能失败——把构建输出目录放到纯 ASCII 路径
> （`CARGO_TARGET_DIR`）即可绕开。

## 二、服务端（授权服务）

```bash
cd server
cargo build --release          # 产出 target/release/wf-auth-server
cargo test                     # 单元/回归测试
```

配置项全部走环境变量（`server/README.md` 有完整表格），首次运行至少要：

| 变量 | 说明 |
|---|---|
| `WF_DATA_DIR` | 数据目录（SQLite + 密文） |
| `WF_PUBLIC_BASE` | 对外地址（生成下载直链用，如 `https://你的域名`） |
| `WF_ADMIN_TOKEN` | 管理页与 `/admin/*` 的令牌（缺失则拒绝启动） |
| `WF_KEK` | 主密钥（64 位 hex）——**丢了则所有密文无法解开，务必离线备份** |

部署到 Linux 服务器：`bash server/deploy/install.sh <域名>`（幂等：装 Rust → 编译 → systemd → Nginx → certbot）。

## 三、许可

- 本项目以 **AGPL-3.0** 分发（见 [`LICENSE`](./LICENSE)）。
- 上游署名与附加条款见 [`README.md`](./README.md) 的「开源许可与署名」一节。

更多说明见本仓库首页。
