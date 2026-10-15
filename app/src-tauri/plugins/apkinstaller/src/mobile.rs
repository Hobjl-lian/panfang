use serde::de::DeserializeOwned;
use tauri::{plugin::PluginApi, AppHandle, Runtime};

use crate::ApkInstaller;

/// 安卓侧：Kotlin 类 `com.wristry.app.apkinstaller.ApkInstallerPlugin`（见 android/ 模块）。
pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    api: PluginApi<R, C>,
) -> Result<ApkInstaller<R>, Box<dyn std::error::Error>> {
    let handle = api.register_android_plugin("com.wristry.app.apkinstaller", "ApkInstallerPlugin")?;
    Ok(ApkInstaller { handle })
}
