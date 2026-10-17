//! apkinstaller：把下载好的 APK 交给系统安装器（安卓）。
//!
//! 为什么要有这个插件：Tauri 官方 updater 不支持 Android（安卓自更新 = 下载 APK + 调系统安装器），
//! 而这一步必须走 Kotlin（`PackageManager.canRequestPackageInstalls()` 未知来源授权、
//! `FileProvider` 的 content:// URI、`ACTION_VIEW` 安装意图）。
//!
//! 本插件**不向 JS 暴露命令**：界面侧统一调用主 crate 的 `app_update_*` 命令，
//! 由那边决定「先检查授权、再下载、最后装」的顺序。
//!
//! 桌面端：整套只在安卓上有意义 —— 插件在桌面构建里编译通过但不注册句柄，
//! 主 crate 里用到的命令都按 `#[cfg(target_os = "android")]` 分开写。

use serde::{Deserialize, Serialize};
use tauri::{
    plugin::{Builder, PluginHandle, TauriPlugin},
    Runtime,
};
#[cfg(target_os = "android")]
use tauri::Manager;

#[cfg(target_os = "android")]
mod mobile;

/// 本机安装信息：自己的 versionCode + 有没有「安装未知来源应用」的授权。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApkStatus {
    pub version_code: i64,
    pub can_install: bool,
}

#[derive(Debug, Serialize)]
struct InstallArg<'a> {
    path: &'a str,
}

/// 插件句柄（主 crate 通过 `app.apk_installer()` 拿到它）。
pub struct ApkInstaller<R: Runtime> {
    handle: PluginHandle<R>,
}

/// 安卓：真正调到 Kotlin 侧。
///
/// ⚠️ `run_mobile_plugin` 只在 mobile 目标上存在 —— 桌面构建必须走下面那套空实现，
/// 否则 `cargo test`/`cargo build`（Windows 主机）直接编译不过，而 binface 回归
/// 与单元测试都要在主机上编这个 crate。
#[cfg(target_os = "android")]
impl<R: Runtime> ApkInstaller<R> {
    /// 自己的 versionCode（拿不到算 0）+ 未知来源授权状态。
    pub fn status(&self) -> Result<ApkStatus, String> {
        self.handle
            .run_mobile_plugin::<ApkStatus>("status", ())
            .map_err(|e| e.to_string())
    }

    /// 打开本应用的「安装未知来源应用」系统设置页。
    pub fn open_install_settings(&self) -> Result<(), String> {
        self.handle
            .run_mobile_plugin::<()>("openInstallSettings", ())
            .map_err(|e| e.to_string())
    }

    /// 内部 cache 目录（`context.cacheDir`）：下载的 APK 必须落在这里。
    ///
    /// 不用 Tauri 的 `cache_dir()`：它在安卓上映射 **getExternalCacheDir**（外部存储），
    /// 而 FileProvider 的配置只认内部 `<cache-path>` → 真机上会「点更新没反应」。
    pub fn internal_cache_dir(&self) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        struct P {
            path: String,
        }
        self.handle
            .run_mobile_plugin::<P>("cacheDir", ())
            .map(|p| p.path)
            .map_err(|e| e.to_string())
    }

    /// 调起系统安装器（path 必须在本应用的 cacheDir 下，FileProvider 才认）。
    pub fn install(&self, path: &str) -> Result<(), String> {
        self.handle
            .run_mobile_plugin::<()>("install", InstallArg { path })
            .map_err(|e| e.to_string())
    }
}

/// 桌面：编译通过、调用即报错（系统安装器只在安卓上有意义）。
#[cfg(not(target_os = "android"))]
impl<R: Runtime> ApkInstaller<R> {
    pub fn status(&self) -> Result<ApkStatus, String> {
        Ok(ApkStatus::default())
    }
    pub fn open_install_settings(&self) -> Result<(), String> {
        Err("应用内安装仅安卓可用".into())
    }
    pub fn internal_cache_dir(&self) -> Result<String, String> {
        Err("应用内安装仅安卓可用".into())
    }
    pub fn install(&self, _path: &str) -> Result<(), String> {
        Err("应用内安装仅安卓可用".into())
    }
}

/// 取插件句柄：`app.apk_installer()`（仅安卓）。
#[cfg(target_os = "android")]
pub trait ApkInstallerExt<R: Runtime> {
    fn apk_installer(&self) -> &ApkInstaller<R>;
}

#[cfg(target_os = "android")]
impl<R: Runtime, T: tauri::Manager<R>> ApkInstallerExt<R> for T {
    fn apk_installer(&self) -> &ApkInstaller<R> {
        self.state::<ApkInstaller<R>>().inner()
    }
}

/// 初始化插件（安卓注册 Kotlin 插件；其他平台只是编译通过）。
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("apkinstaller")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                let installer = mobile::init(app, api)?;
                app.manage(installer);
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = (app, api);
            }
            Ok(())
        })
        .build()
}
