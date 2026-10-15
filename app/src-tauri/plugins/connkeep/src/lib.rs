//! connkeep：连接保活（安卓）。
//!
//! 为什么要有这个插件：经典蓝牙 SPP 的 socket 由 App 进程内的 Kotlin 插件持有，
//! 进程一旦进入 cached 状态就可能被系统（Android 12+ 的 app freezer，及各 ROM 的省电策略）
//! 冻结 —— 冻结期间读线程停摆、传输停在半路，甚至 socket 被系统回收。
//! 唯一可靠的解法是**前台服务**：带一条低优先级静默通知，让系统别把进程当 cached 处理。
//! 服务与本插件同进程，只是「钉住」进程，不接管 socket。
//!
//! 命令（都只从 Rust 侧调用，不暴露给网页层）：
//!  - start：拉起/刷新前台服务（通知里带设备名）
//!  - stop：停掉前台服务（断开连接时调）
//!  - isRunning：服务在不在（查询用）
//!  - openAppSettings：打开本应用的应用详情页（国产 ROM 的后台/电池设置入口）
//!  - requestIgnoreBattery：尝试弹「不限制电池优化」系统对话框；弹不出来返回 false
//!
//! 桌面端：整套只在安卓上有意义 —— 插件在桌面构建里编译通过但不注册句柄。

use serde::{Deserialize, Serialize};
use tauri::{
    plugin::{Builder, PluginHandle, TauriPlugin},
    Runtime,
};
#[cfg(target_os = "android")]
use tauri::Manager;

#[cfg(target_os = "android")]
mod mobile;

/// 前台服务状态（给 Rust 侧核对用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeepStatus {
    pub running: bool,
    pub notification_allowed: bool,
}

#[derive(Debug, Serialize)]
struct StartArg<'a> {
    name: &'a str,
    addr: &'a str,
}

/// 插件句柄（主 crate 通过 `app.conn_keep()` 拿到它）。
pub struct ConnKeep<R: Runtime> {
    handle: PluginHandle<R>,
}

impl<R: Runtime> ConnKeep<R> {
    /// 拉起/刷新前台服务（同一进程，服务在 = 进程不会被当成 cached 冻结）。
    pub fn start(&self, device_name: &str, addr: &str) -> Result<KeepStatus, String> {
        self.handle
            .run_mobile_plugin::<KeepStatus>(
                "start",
                StartArg {
                    name: device_name,
                    addr,
                },
            )
            .map_err(|e| e.to_string())
    }

    /// 停掉前台服务（用户主动断开 / 切换设备时调）。
    pub fn stop(&self) -> Result<(), String> {
        self.handle
            .run_mobile_plugin::<()>("stop", ())
            .map_err(|e| e.to_string())
    }

    /// 前台服务当前在不在。
    pub fn is_running(&self) -> Result<bool, String> {
        #[derive(serde::Deserialize)]
        struct P {
            running: bool,
        }
        self.handle
            .run_mobile_plugin::<P>("isRunning", ())
            .map(|p| p.running)
            .map_err(|e| e.to_string())
    }

    /// 打开本应用的「应用信息」系统页（电池 / 后台运行设置都在里面）。
    pub fn open_app_settings(&self) -> Result<(), String> {
        self.handle
            .run_mobile_plugin::<()>("openAppSettings", ())
            .map_err(|e| e.to_string())
    }

    /// 「后台保持连接」一键入口：插件按最有用的一步走（弹系统对话框 / 开应用信息页 / 本来就已经允许），
    /// 返回实际做了什么：`"already" | "dialog" | "settings" | "failed"`（前端据此给一句提示）。
    /// 详见 Kotlin 侧 `batteryHelp` 的判定顺序注释。
    pub fn battery_help(&self) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        struct P {
            result: String,
        }
        self.handle
            .run_mobile_plugin::<P>("batteryHelp", ())
            .map(|p| p.result)
            .map_err(|e| e.to_string())
    }
}

/// 取插件句柄：`app.conn_keep()`（仅安卓）。
#[cfg(target_os = "android")]
pub trait ConnKeepExt<R: Runtime> {
    fn conn_keep(&self) -> &ConnKeep<R>;
}

#[cfg(target_os = "android")]
impl<R: Runtime, T: tauri::Manager<R>> ConnKeepExt<R> for T {
    fn conn_keep(&self) -> &ConnKeep<R> {
        self.state::<ConnKeep<R>>().inner()
    }
}

/// 初始化插件（安卓注册 Kotlin 插件；其他平台只是编译通过）。
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("connkeep")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                let keep = mobile::init(app, api)?;
                app.manage(keep);
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = (app, api);
            }
            Ok(())
        })
        .build()
}
