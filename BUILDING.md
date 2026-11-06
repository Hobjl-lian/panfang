# 构建说明（Building）

本仓库是「盘坊」App 的完整源码（Tauri 2 + React，安卓为目标平台）。

## 一、App

### 1. 准备上游依赖（必须）

App 通过 **Cargo 路径依赖** 复用 [AstroBox-NG](https://github.com/AstralSightStudios/AstroBox-NG) 的部分模块。
本仓库不含其源码：

```bash
bash scripts/init-upstream.sh      # 克隆 AstroBox-NG、钉住各模块提交、打本地补丁
```

钉住的提交已包含本项目需要的上游改动；`init-upstream.sh` 还会把
`docs/patches/*-local-changes.patch` 打到对应的上游仓库
（bluetooth / core / btclassic-spp / inappbrowser 各一份，本项目依赖的少量行为改动，
脚本自动完成，无需手工操作）。

（与上游的关系见 README「开源许可与署名」一节。）

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

## 二、许可

- 本项目以 **AGPL-3.0** 分发（见 [`LICENSE`](./LICENSE)）。
- 上游署名与附加条款见 [`README.md`](./README.md) 的「开源许可与署名」一节。
