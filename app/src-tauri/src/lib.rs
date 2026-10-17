pub mod app_update;
pub mod binface_cmd;
pub mod bridge;
pub mod cloud;
pub mod conn;
pub mod diag;
pub mod generator_cmd;
pub mod paid;
pub mod watchface_bin;
pub mod watchface_edit;
pub mod watchface_imgcodec;
pub mod watchface_render;
pub mod wallpaper_gen;

use bridge::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(inappbrowser::init())
        .plugin(apkinstaller::init())
        .plugin(connkeep::init())
        .setup(|app| {
            use tauri::Manager;

            // 日志：安卓 → logcat（adb 实时可读）；桌面 → 应用数据目录文件
            #[cfg(target_os = "android")]
            android_logger::init_once(
                android_logger::Config::default()
                    .with_max_level(log::LevelFilter::Debug)
                    .with_tag("WFInstaller"),
            );
            #[cfg(not(target_os = "android"))]
            {
                let log_path = app
                    .path()
                    .app_data_dir()
                    .map(|dir| dir.join("app.log"))
                    .ok();
                let file = log_path.and_then(|path| {
                    std::fs::create_dir_all(path.parent()?).ok()?;
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .ok()
                });
                let mut builder = env_logger::Builder::from_env(
                    env_logger::Env::default().default_filter_or("debug"),
                );
                builder.format_timestamp_millis();
                if let Some(file) = file {
                    builder.target(env_logger::Target::Pipe(Box::new(file)));
                }
                let _ = builder.try_init();
            }
            log::info!("[setup] logger ready");

            // corelib ECS 运行时（进程内一次性初始化）
            static CORE_INIT: std::sync::Once = std::sync::Once::new();
            CORE_INIT.call_once(|| {
                corelib::ecs::init_runtime_default();
            });
            let core_runtime = corelib::asyncrt::build_runtime();
            let core_handle = core_runtime.handle().clone();

            // 蓝牙传输层：StdImp（bluest BLE + btclassic-spp 插件）
            bluetooth::stdimp::init(app.handle().clone())?;
            let _ = bluetooth::set_interface(Box::new(bluetooth::stdimp::StdImp::new(
                bluetooth::btinterface::ConnectType::BLE,
            )));

            let app_data_dir = app.path().app_data_dir().ok();

            // 失败打点：登记数据目录（之后任何一步失败都能上报，不需要再传路径）
            if let Some(dir) = app_data_dir.clone() {
                diag::init(dir.clone());
                // **启动时就用正确目录把付费上下文钉死**：它是进程级 OnceLock，
                // 谁先初始化谁定音——若让诊断打点抢先用了别的目录，留档/登录态会全部解不开
                // （2026-10-08 真机踩过）。这里先初始化一次，之后所有调用者都不可能改变它。
                let _ = paid::ctx(&paid::paid_dir(&dir));
            }

            app.manage(AppState {
                core_runtime,
                core_handle: core_handle.clone(),
                mi_token: parking_lot::Mutex::new(None),
                mi_2fa: parking_lot::Mutex::new(None),
                mi_2fa_browser: parking_lot::Mutex::new(None),
                app_data_dir,
            });
            // 连接总管：掉线检测 + 自动重连 + 前台服务保活（心跳循环在它内部起）
            conn::init(app.handle().clone(), core_handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bridge::start_scan,
            bridge::stop_scan,
            bridge::device_connect,
            bridge::device_disconnect,
            bridge::device_get_data,
            bridge::device_link_state,
            bridge::device_relink,
            bridge::device_open_battery_settings,
            bridge::device_install,
            paid::paid_device_screen,
            paid::paid_status,
            paid::paid_bind_mode,
            paid::paid_set_base,
            paid::paid_bind,
            paid::paid_bind_oauth_start,
            paid::paid_bind_oauth_poll,
            paid::paid_unbind,
            paid::paid_list,
            paid::paid_installs_get,
            paid::paid_latest_file,
            paid::paid_install,
            bridge::device_install_payload,
            bridge::bin_inspect_info,
            bridge::watchface_get_list,
            bridge::watchface_set_current,
            bridge::watchface_uninstall,
            cloud::xiaomi_login,
            cloud::xiaomi_2fa_url,
            cloud::xiaomi_2fa_open,
            cloud::xiaomi_complete_2fa,
            cloud::xiaomi_complete_2fa_sts,
            cloud::xiaomi_restore,
            cloud::xiaomi_logout,
            binface_cmd::watchface_bin_inspect,
            binface_cmd::watchface_bin_style_thumbs,
            binface_cmd::watchface_bin_extract_style,
            binface_cmd::watchface_edit_model,
            binface_cmd::watchface_layer_model,
            binface_cmd::watchface_render_preview,
            binface_cmd::watchface_compose_photo,
            bridge::watchface_apply_edit,
            bridge::watchface_album_get,
            bridge::watchface_archive_list,
            bridge::watchface_archive_get,
            bridge::open_external,
            bridge::app_version,
            bridge::watchface_photos_load,
            bridge::watchface_photos_save,
            diag_recent,
            diag_report,
            system_bars_light,
            install_back_lock,
            keep_screen_on,
            open_webview_settings,
            generator_cmd::watchface_generator_plan,
            generator_cmd::watchface_generator_render,
            generator_cmd::watchface_generator_frame,
            generator_cmd::watchface_generator_cached,
            generator_cmd::watchface_generator_clear,
            app_update::app_update_check,
            app_update::app_update_download,
            app_update::app_update_install,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 系统栏图标明暗（仅移动端有实际效果，桌面端 no-op）：
/// 浅色页传 true（深色图标），深色页（裁剪页）传 false（白色图标）。
#[tauri::command]
fn system_bars_light(app: tauri::AppHandle, light: bool) -> Result<(), String> {
    use inappbrowser::InappbrowserExt;
    app.inappbrowser()
        .set_system_bars(light)
        .map_err(|err| err.to_string())
}

/// 安装进行中锁死返回键（安卓的物理返回与侧滑手势都吞掉，App 退不出去）：
/// 前端在进度页置 true、装完或出错置 false。只锁返回键，不拦 Home（切后台不打断传输）。
#[tauri::command]
fn install_back_lock(app: tauri::AppHandle, locked: bool) -> Result<(), String> {
    use inappbrowser::InappbrowserExt;
    app.inappbrowser()
        .set_back_lock(locked)
        .map_err(|err| err.to_string())
}

/// 安装进行中保持屏幕常亮（安卓；桌面端 no-op）：
/// 用户放着不动等安装时屏幕会按系统超时熄掉，息屏后 App 进入 cached 状态、
/// 进程可能被系统（及各 ROM 省电策略）冻结，传输就停在半路。
/// 与 install_back_lock 同一处调用、同一生命周期，装完或出错立刻放开。
#[tauri::command]
fn keep_screen_on(app: tauri::AppHandle, on: bool) -> Result<(), String> {
    use inappbrowser::InappbrowserExt;
    app.inappbrowser()
        .set_keep_screen_on(on)
        .map_err(|err| err.to_string())
}

/// 打开系统 WebView 的应用详情页（安卓版本门禁页的「去更新」按钮；桌面端 no-op）。
#[tauri::command]
fn open_webview_settings(app: tauri::AppHandle) -> Result<(), String> {
    use inappbrowser::InappbrowserExt;
    app.inappbrowser()
        .open_webview_settings()
        .map_err(|err| err.to_string())
}

/// 「反馈」页用：最近几条失败打点 + 版本号，让买家一键复制给开发者。
/// 只含排障字段（步骤/错误码/一句话/机型），不含账号、不含表盘内容。
#[tauri::command]
fn diag_recent() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "app_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "recent": diag::recent(),
    }))
}

/// 前端上报一次失败（前端才是「所有错误都被翻译成一句操作指引」的地方）。
#[tauri::command]
fn diag_report(step: String, code: String, message: String, device_model: Option<String>) {
    diag::report(&step, &code, &message, device_model.as_deref());
}

/// **开发期命令门闩**：这些命令是设计流水线/联调用的（批量导出素材图、切测试服务器等），
/// 界面上没有入口，但注册进正式包就等于「拿到包的人都能调用」。release 构建一律拒绝。
///
/// 正式包里需要放行某条命令时，把它调用处的 `dev_only(...)` 去掉即可。
pub(crate) fn dev_only(what: &str) -> Result<(), String> {
    if cfg!(debug_assertions) {
        Ok(())
    } else {
        Err(format!("{what}在当前版本不可用"))
    }
}
