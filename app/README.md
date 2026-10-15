# 盘坊（Panfang）

面向小米 Vela 穿戴设备的第三方表盘/快应用安装工具。目标设备：小米手环 9 / 9 Pro / 10、红米手表 5 / 6。

## 技术来源与署名（AGPL-3.0 合规）

- **本仓库（源码地址）**：<https://github.com/Hobjl-lian/panfang> —— 作者：Hobjl（盘坊）
- 协议栈与账号登录核心：[AstroBox-NG](https://github.com/AstralSightStudios/AstroBox-NG)（AGPL-3.0 + 额外署名条款），本工具以路径依赖方式复用其 `corelib` / `pb` / `account` / `bluetooth` 模块，源码位于 `../repos/AstroBox-NG/`。
- 「登录小米账号拉取设备列表（get_source_list）」调用形态：参考开源逆向样本（环表工具箱），实现于 `src-tauri/src/cloud.rs`。
- 依据 AGPL-3.0 及其附加署名条款：向任何获得本工具二进制的人提供本仓库（或含上述源的完整工程）源码，并保留 AstroBox 原项目署名。

## 目录结构

```
app/
  ui/index.html          前端（静态页，window.__TAURI__ API，无需构建链）
  src-tauri/
    src/lib.rs           Tauri 入口（运行时与蓝牙接口初始化）
    src/bridge.rs        设备桥接：连接/安装/表盘管理命令
    src/cloud.rs         小米账号登录 + get_source_list 设备列表补丁
  src-tauri/Cargo.toml   以路径依赖接入 repos/AstroBox-NG 模块
repos/
  AstroBox-NG/           上游开源核心（abtools init 已拉取公开模块；含本地 netcfg 占位）
  bandburg/              参考实现（网页版，未用于本 App）
```

## 当前能力（里程碑 1）

1. 小米账号密码登录 → 自动拉取账号下绑定设备列表（含 authkey）
2. BLE 直连手环（认证握手由核心自动完成）
3. 安装 .bin 表盘 / .mwz 表盘包 / .rpk 快应用（分包进度回调）
4. 查看设备上的表盘列表、切换当前表盘、卸载
5. 备用路径：BLE 扫描 + 手动填 key

## 构建与运行

前置：Rust (stable, msvc)、VS Build Tools (C++ workload)、Node.js。

```bash
cd app
npx tauri dev      # 开发运行
npx tauri build    # 产出 NSIS 安装包
```

直接跑二进制（跳过 tauri CLI）：

```bash
cd app/src-tauri
cargo run
```

## 安卓构建（arm64）

Git Bash 下环境变量必须显式导出，缺一会失败：

```bash
export JAVA_HOME='<你的 JDK 17 路径>'      # Temurin 17；路径必须 ASCII
export CARGO_TARGET_DIR='<纯 ASCII 目录>'  # 工作区路径含中文时 NDK lld 会链接失败，目标目录必须是 ASCII

cd app
npx tauri android build --target aarch64    # 只编 arm64；模拟器自测改用 --target x86_64
```

产物：`app/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`（未签名）。

签名流程（**顺序固定**，zipalign 必须在 apksigner 之前，否则签名作废）：

```bash
BT="$LOCALAPPDATA/Android/Sdk/build-tools/35.0.0"
"$BT/zipalign.exe" -P 16 -f 4 <unsigned.apk> aligned.apk    # -P 16 = 16KB 页对齐，Android 15+ 必需
"$BT/apksigner.bat" sign --ks <你的.keystore> --ks-key-alias <别名> \
  --ks-pass pass:*** --key-pass pass:*** --out 盘坊-release.apk aligned.apk
```

用**你自己的** keystore 签名即可（作者的签名密钥不公开，也不需要）。换签名等于另一个应用，只影响对已安装用户的覆盖更新。
**keystore 一旦丢失就无法再对已安装用户做覆盖更新，请单独离线备份，不要把密码写进仓库。**

## 后续路线（里程碑 2+）

- 表盘编辑：照片表盘换壁纸、相册图组管理、文字/字体（协议与编排已在 corelib 公开 API：`edit_watchface` / `transfer_watchface_image` / `transfer_watchface_font`）
- Android APK / iOS TestFlight（Tauri v2 mobile，同一 Rust 核心）
- 凭证持久化与 token 刷新（account 模块已含 `refresh_mi_account_token`）
- 2FA 兜底方案：手环端快应用读日志回传 key
