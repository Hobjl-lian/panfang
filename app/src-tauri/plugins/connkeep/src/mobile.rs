use serde::de::DeserializeOwned;
use tauri::{plugin::PluginApi, AppHandle, Runtime};

use crate::ConnKeep;

/// 安卓侧：Kotlin 类 `com.wristry.app.connkeep.ConnKeepPlugin`（见 android/ 模块）。
pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    api: PluginApi<R, C>,
) -> Result<ConnKeep<R>, Box<dyn std::error::Error>> {
    let handle = api.register_android_plugin("com.wristry.app.connkeep", "ConnKeepPlugin")?;
    Ok(ConnKeep { handle })
}
