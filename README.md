# 盘坊（Panfang）

面向小米 Vela 穿戴设备的第三方表盘安装工具。目标设备：小米手环 9 / 9 Pro / 10 / 10 Pro / 11、REDMI 手表 5 / 6 等。

功能：

- 小米账号登录 → 自动读取账号下的设备列表（含 authkey），一键连接手环
- 安装表盘 / 快应用（`.bin` 等）到设备，安装时自动留档
- **表盘编辑**：换样式、改可编辑容器（时间/日期/功能位组件）、上传本地照片当壁纸（可多张成一本相册、逐张裁剪、实时预览）
- 本地 `.bin` 解包预览：样式列表、预览图、素材图、图层模型推导
- 全部功能仅安卓端为目标平台；桌面端可跑（用于调试与预览）

> 非官方工具，与小米公司无关联。所有操作在你的设备与账号下进行，风险自担。

---

## 开源许可与署名（重要）

本项目以 **AGPL-3.0** 分发（见根目录 [`LICENSE`](./LICENSE)）。

它以下列方式复用了 **[AstroBox-NG](https://github.com/AstralSightStudios/AstroBox-NG)** 的代码（作者：**AstralSight Studios**）：

- 以 Cargo 路径依赖方式链接其公开模块 `core`（corelib）、`pb`、`account`、`bluetooth`、`plugins/inappbrowser`
- 上游提交（本项目验证通过）：`4735de617785701985b4bc99fc5842bf71730bd3`（对应 AstroBox-NG 2.2.0）

**上游额外署名要求**（AGPL-3.0 所述可选附加条款，以中文版为准）：
> 根据 AGPL 3.0 所述可选附加条款，本项目额外附加署名要求，使用此项目需在遵守 AGPL 3.0 条款后额外为此项目添加署名，署名包括但不限于本项目仓库地址，作者名等。

本项目已按该要求保留署名 —— **仓库地址与作者名同时出现在本 README、`app/README.md`、以及 App 内「关于盘坊 → 开源许可与致谢」页面**：

- 本仓库：<https://github.com/Hobjl-lian/panfang>
- 作者：Hobjl（盘坊）

AGPL-3.0 合规说明：

- 本项目向任何获得二进制副本的人提供**完整对应源码**（本仓库 + `scripts/init-upstream.sh` 所指的上游公开模块），且不附加任何额外限制；你可以自由再分发。
- 本项目**不**包含任何私钥、授权服务凭据或第三方素材；商业化（授权/兑换码/加密分发）依赖的服务端代码不含 AGPL 组件。
- 上游 `netcfg` 模块为**私有仓库**，本项目以自写的等价占位实现（[`vendor/netcfg/`](./vendor/netcfg)）替代，该占位代码同样以 AGPL-3.0 分发。

---

## 目录结构

```
app/                         应用（Tauri 2 + React；安卓为目标平台）
  ui-react/src/App.tsx       前端全部界面（React 源码；构建产物进 ui-react/dist）
  src-tauri/src/             Rust 侧：设备桥接、.bin 编解码、编辑模型、下载解密、自更新
  src-tauri/plugins/         自有 Tauri 插件：apkinstaller（调系统安装器）、connkeep（连接保活前台服务）
  src-tauri/gen/android/     安卓工程（含 MainActivity 定制）
server/                      授权服务（Rust + axum + SQLite）
  src/                       订单同步 / 授权 / 爱发电 OAuth / 管理接口 / 客户端接口 / 加密
  static/                    管理页与协议页（编译进二进制）
  deploy/                    安装脚本与 systemd / Nginx 配置（install.sh 幂等）
vendor/netcfg/               上游私有模块的等价占位实现（必须，account 依赖）
scripts/init-upstream.sh     拉取上游开源核心并放置占位模块
docs/                        接口契约与上游补丁（内部设计/现场笔记不随仓库公开）
```

---

## 构建

### 0. 准备源码

```bash
git clone <本仓库> && cd <本仓库>
bash scripts/init-upstream.sh      # 拉 AstroBox-NG + 公开模块 + 放置 netcfg 占位
```

> Windows 上用 Git Bash 执行同一脚本；上游模块准备需要 `git` 与 `python`。

### 1. 桌面（调试用）

前置：Rust（1.90+）、Node.js 20+。

```bash
cd app
npx tauri dev          # 或 npx tauri build 出桌面安装包
```

### 2. 安卓（正式交付形态）

前置：

| 组件 | 版本（本项目验证环境） |
|---|---|
| Rust + 安卓目标 | `rustup target add aarch64-linux-android` |
| Node.js | 24.x（用 `npx tauri` 调 Tauri CLI 2.12） |
| JDK | 17 |
| Android SDK | build-tools 36.0.0、platform-tools |
| Android NDK | 27.0.12077973 |

```bash
cd app
export JAVA_HOME=/path/to/jdk17
export ANDROID_HOME=/path/to/Android/Sdk
export NDK_HOME="$ANDROID_HOME/ndk/27.0.12077973"
npx tauri android build --apk --target aarch64     # 产物在 src-tauri/gen/android/app/build/outputs/apk/
```

产物是未签名 APK。对齐 + 签名（注意 16KB 页大小）：

```bash
# 1) 对齐（16KB 页）
zipalign -P 16 -f 4 app-universal-release-unsigned.apk /tmp/panfang-aligned.apk
# 2) 签名（用你自己的 keystore 与口令）
apksigner sign --ks <你的.keystore> --ks-key-alias <别名> --out 盘坊-release.apk /tmp/panfang-aligned.apk
```

签名密钥是**应用身份**：一旦丢失，将无法再对已安装的 App 做覆盖更新 —— 请单独离线备份。
密钥与口令一律**不入库**（本项目所有敏感配置都走环境变量或仓库外的本地文件）。

---

## 安全与隐私

- 账号密码只用于向小米服务器登录换取服务令牌；令牌与设备 authkey 保存在应用私有目录，用于与手环建立加密通道。
- 应用**不上传**任何用户数据到第三方；本仓库不含任何埋点与云端上报。
- 提交代码前请确认没有把私钥、签名口令、账号凭据写进仓库（本项目所有敏感配置均走环境变量/本地文件）。
