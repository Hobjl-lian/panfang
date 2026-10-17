//! 设备桥接层：把 bluetooth(StdImp) 传输层接到 corelib 的 ECS 设备栈上，
//! 并暴露连接 / 安装 / 表盘管理等 Tauri 命令。接线方式复刻 app_wasm 的公开实现。

use bluetooth::btinterface::{self, ConnectType};
use corelib::device::xiaomi::packet::dispatcher;
use corelib::device::xiaomi::packet::mass::MassDataType;
use corelib::device::xiaomi::resutils::{FileType, get_file_type};
use corelib::device::xiaomi::r#type::ConnectType as CoreConnectType;
use corelib::device::xiaomi::SendError as CoreSend;
use corelib::device::{DeviceConnectionInfo, DeviceKind, create_device};
use corelib::ecs::{self, Component};
use corelib::ecs::runtime::Runtime;
use std::io::Read;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::State;

pub struct AppState {
    /// corelib 专用 ECS/异步运行时（必须长期存活）
    #[allow(dead_code)]
    pub core_runtime: tokio::runtime::Runtime,
    pub core_handle: tokio::runtime::Handle,
    pub mi_token: parking_lot::Mutex<Option<account::xiaomi::MiAccountToken>>,
    pub mi_2fa: parking_lot::Mutex<Option<crate::cloud::TwoFaState>>,
    /// 移动端受控浏览器会话 id（2FA 验证页）
    pub mi_2fa_browser: parking_lot::Mutex<Option<u32>>,
    pub app_data_dir: Option<std::path::PathBuf>,
}

fn bt() -> anyhow::Result<&'static dyn btinterface::BluetoothInterface> {
    bluetooth::get_interface().ok_or_else(|| anyhow::anyhow!("蓝牙接口未初始化"))
}

fn map_send_error(err: btinterface::SendError) -> CoreSend {
    match err {
        btinterface::SendError::Disconnected => CoreSend::Disconnected,
        other => CoreSend::Io(other.to_string()),
    }
}

// ---------- ECS 访问辅助（复刻 app_wasm，适配原生多线程） ----------

async fn with_device_system<S, F, R>(addr: &str, f: F) -> anyhow::Result<R>
where
    S: Component,
    F: FnOnce(&mut S) -> R + Send + 'static,
    R: Send + 'static,
{
    let owned = addr.to_string();
    let result: Result<R, String> = ecs::with_rt_mut(move |rt: &mut Runtime| {
        rt.with_device_mut(&owned, |world, entity| {
            let mut sys = world
                .get_mut::<S>(entity)
                .ok_or_else(|| "组件未就绪（设备可能未连接）".to_string())?;
            Ok(f(&mut sys))
        })
        .ok_or_else(|| "设备不存在".to_string())?
    })
    .await;
    result.map_err(|e| anyhow::anyhow!(e))
}

async fn await_oneshot<T, E: std::fmt::Display>(
    rx: tokio::sync::oneshot::Receiver<Result<T, E>>,
    missing: &'static str,
) -> anyhow::Result<T> {
    let r = rx.await.map_err(|_| anyhow::anyhow!(missing))?;
    r.map_err(|e| anyhow::anyhow!("{e}"))
}

// ---------- 命令：扫描 ----------

#[tauri::command]
pub async fn start_scan(channel: Channel<btinterface::BluetoothDevice>) -> Result<(), String> {
    // stdimp 内部用 tauri::async_runtime::block_on，必须在阻塞线程执行
    tauri::async_runtime::spawn_blocking(move || {
        bt()
            .and_then(|iface| {
                iface
                    .start_scan(channel, Some(ConnectType::BLE))
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn stop_scan() -> Result<Vec<btinterface::BluetoothDevice>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        bt()
            .and_then(|iface| iface.stop_scan().map_err(|e| anyhow::anyhow!("{e}")))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- 命令：连接 / 断开 ----------

#[tauri::command]
pub async fn device_connect(
    state: State<'_, AppState>,
    name: String,
    addr: String,
    authkey: String,
) -> Result<DeviceConnectionInfo, String> {
    let info =
        connect_inner(state.core_handle.clone(), name.clone(), addr.clone(), authkey.clone()).await?;
    // 连接成功 → 登记期望连接：拉起前台服务保活（防后台冻结）+ 允许掉线自动重连。
    // 连接状态由 Rust 侧维持（后台 WebView 的定时器会被节流），前端只负责显示。
    if let Some(sup) = crate::conn::get() {
        sup.arm(&addr, &name, &authkey);
    }
    Ok(info)
}

/// 连接前确保「附近的设备」权限齐全（Android 12+ 的 SCAN + CONNECT）。
///
/// 单独一条命令、在 device_connect 之前调：系统权限弹窗等待用户选择期间不计入
/// 前端的 15 秒连接超时，用户慢慢点也不会被误判成「连接失败」。
/// 桌面端没有这套权限，直接返回 true。
#[tauri::command]
pub async fn device_ensure_bt_permissions(app: tauri::AppHandle) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use connkeep::ConnKeepExt;
        tauri::async_runtime::spawn_blocking(move || app.conn_keep().ensure_bt_permissions())
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(true)
    }
}

/// 连接一个设备（**手动连接与自动重连共用这一条路径**）。
/// stdimp 内部使用 tauri::async_runtime::block_on，必须在阻塞线程上调用。
pub(crate) async fn connect_inner(
    core_handle: tokio::runtime::Handle,
    name: String,
    addr: String,
    authkey: String,
) -> Result<DeviceConnectionInfo, String> {
    // 经典蓝牙 SPP 通道（channel 5）——与 AstroBox、环表工具箱完全一致的方案：
    // 手环「连接新手机」模式即经典蓝牙可被发现模式，SPP 连接稳定成熟。
    // （此前尝试的 BLE 路径为弯路：上游安卓 BLE 实现存在多处缺陷。）
    let connect_type = ConnectType::SPP;

    let connect_addr = addr.clone();
    let display_addr = addr.clone();
    log::info!("[device_connect] begin addr={addr} type={connect_type:?}");
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let iface = bt().map_err(|e| e.to_string())?;
        // Some(false)=保留已有配对关系直接连接（环表工具箱同款行为，免重新配对）
        iface
            .connect(connect_addr, connect_type, vec![], Some(false))
            .map_err(|e| match e {
                btinterface::ConnectError::DeviceNotFound => format!(
                    "蓝牙连接失败: 未发现设备 {display_addr}。\
                     请依次确认：① 手环处于「连接新手机」界面（经典蓝牙可被发现）；\
                     ② 手机上的小米运动健康已退出、没占着手环；\
                     ③ 本应用的「附近的设备」权限已允许（系统设置 → 应用 → 盘坊 → 权限）。\
                     第 ③ 条最容易被忽略：Android 12 起没有该权限时扫描结果恒为空，表现就是「哪个手环都连不上」。"
                ),
                other => format!("蓝牙连接失败: {other}"),
            })
    })
    .await
    .map_err(|e| e.to_string())??;
    log::info!("[device_connect] transport connected");

    // 订阅通知 → 喂给 corelib 分发器（认证握手在 create_device 内自动发起）
    let packet_handle = core_handle.clone();
    let sub_addr = addr.clone();
    let sub_cb_addr = addr.clone();
    let sub_cb: Arc<dyn Fn(Result<Vec<u8>, String>) + Send + Sync> = Arc::new(move |res| match res {
        Ok(data) => dispatcher::on_packet(packet_handle.clone(), sub_addr.clone(), data),
        Err(err) => {
            log::warn!("[ble] notify error: {err}");
            // 通知通道报错 = 链路已死：立刻交给连接总管（界面马上如实显示 + 自动重连）
            if let Some(sup) = crate::conn::get() {
                sup.note_lost(&sub_cb_addr, &format!("蓝牙通道断开: {err}"), true);
            }
        }
    });
    let sub_addr2 = addr.clone();
    log::info!("[device_connect] subscribing notifications");
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let iface = bt().map_err(|e| e.to_string())?;
        iface
            .subscribe(&sub_addr2, sub_cb, None)
            .map_err(|e| format!("订阅蓝牙通知失败: {e}"))
    })
    .await
    .map_err(|e| e.to_string())??;
    log::info!("[device_connect] subscribed");

    let core_handle = core_handle.clone();
    let send_addr = addr.clone();
    let sender = move |data: Vec<Vec<u8>>| {
        let addr = send_addr.clone();
        async move {
            let Some(iface) = bluetooth::get_interface() else {
                log::warn!("[sender] bluetooth interface missing");
                return Err(CoreSend::Disconnected);
            };
            log::info!(
                "[sender] send_many_async to {addr} packets={} total_len={}",
                data.len(),
                data.iter().map(|d| d.len()).sum::<usize>()
            );
            let result = iface.send_many_async(&addr, data, None).await;
            log::info!("[sender] result ok={}", result.is_ok());
            if let Err(e) = &result {
                // 发送失败 = socket 已死：上报连接总管（重连流程会处理；未连续时是空操作）
                if let Some(sup) = crate::conn::get() {
                    sup.note_lost(&addr, &format!("发送失败: {e:?}"), true);
                }
            }
            result.map_err(map_send_error)
        }
    };

    // 传输参数：SPP 用 977 帧宽（AstroBox 同款，安卓/桌面一致）
    let (ct_core, chunk_spp, chunk_ble) = (CoreConnectType::SPP, Some(977usize), None);

    log::info!("[device_connect] creating corelib device");
    // create_device 内部会等待认证握手完成；认证包发送失败时上游不通知等待者会永久挂起，
    // 这里加 30 秒超时兜底，避免连接命令永不返回（前端按钮永久禁用）。
    let info = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        create_device(
            core_handle,
            DeviceKind::Xiaomi,
            name,
            addr.clone(),
            authkey,
            2, // sar_version，参照 app_wasm/BandBurg 默认值
            ct_core,
            None,
            chunk_spp,
            chunk_ble,
            false,
            sender,
        ),
    )
    .await
    .map_err(|_| "认证握手超时（30 秒内设备未完成认证，请重试）".to_string())?
    .map_err(|e| format!("创建设备失败: {e}"))?;
    log::info!("[device_connect] corelib device ready, auth done");
    Ok(info)
}

#[tauri::command]
pub async fn device_disconnect(_state: State<'_, AppState>, addr: String) -> Result<(), String> {
    // 用户主动断开 / 切设备 / 清残留会话：撤销期望连接（停前台服务、作废在飞的重连）
    if let Some(sup) = crate::conn::get() {
        sup.disarm(&addr);
    }
    teardown_device(&addr).await;
    Ok(())
}

/// 拆掉一条连接：关 socket + 从 ECS 移除设备 + 清缓存状态。
/// **不含 disarm** —— 「期望连接」的撤销由调用方决定（用户主动断开要撤销；
/// 自动重连跑到一半、用户已经不要这条连接了，则拆掉不留活 socket）。
pub(crate) async fn teardown_device(addr: &str) {
    let disconnect_addr = addr.to_string();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        if let Ok(iface) = bt() {
            let _ = iface.disconnect(&disconnect_addr);
        }
    })
    .await;
    let owned = addr.to_string();
    let kind = corelib::ecs::with_rt_mut(move |rt: &mut Runtime| {
        let kind = rt
            .component_ref::<corelib::device::Device>(&owned)
            .map(|d| d.kind());
        rt.remove_device(&owned);
        kind
    })
    .await;
    if let Some(kind) = kind {
        corelib::device::cleanup_device_state(kind, addr);
    }
}

// ---------- 命令：设备数据 ----------

#[tauri::command]
pub async fn device_get_data(
    addr: String,
    data_type: String,
) -> Result<serde_json::Value, String> {
    use corelib::device::xiaomi::components::info::InfoSystem;
    // 三个请求的返回类型不同，分支间独立收发（与 app_wasm 一致）
    match data_type.as_str() {
        "info" => {
            let rx = with_device_system::<InfoSystem, _, _>(&addr, |sys| sys.request_device_info())
                .await
                .map_err(|e| e.to_string())?;
            let v = await_oneshot(rx, "设备未返回信息").await.map_err(|e| e.to_string())?;
            serde_json::to_value(&v).map_err(|e| e.to_string())
        }
        "status" => {
            let rx = with_device_system::<InfoSystem, _, _>(&addr, |sys| sys.request_device_status())
                .await
                .map_err(|e| e.to_string())?;
            let v = await_oneshot(rx, "设备未返回状态").await.map_err(|e| e.to_string())?;
            serde_json::to_value(&v).map_err(|e| e.to_string())
        }
        "storage" => {
            let rx = with_device_system::<InfoSystem, _, _>(&addr, |sys| sys.request_device_storage())
                .await
                .map_err(|e| e.to_string())?;
            let v = await_oneshot(rx, "设备未返回存储信息").await.map_err(|e| e.to_string())?;
            serde_json::to_value(&v).map_err(|e| e.to_string())
        }
        other => Err(format!("不支持的数据类型: {other}")),
    }
}

/// 轻量探测：查一次设备状态，返回电量（连不上 / 没回有效状态都算失败）。
/// 连接总管的心跳用它；`device_get_data("status")` 与它同一条链路。
pub(crate) async fn probe_device_status(addr: &str) -> Result<u32, String> {
    use corelib::device::xiaomi::components::info::InfoSystem;
    let rx = with_device_system::<InfoSystem, _, _>(addr, |sys| sys.request_device_status())
        .await
        .map_err(|e| e.to_string())?;
    let status = await_oneshot(rx, "设备未返回状态").await.map_err(|e| e.to_string())?;
    Ok(status.battery.capacity)
}

// ---------- 命令：连接状态（保活 / 自动重连）----------

/// 连接总管状态快照：界面启动 / WebView 重建后**用真实状态**重新挂载
/// （前台服务保活后连接可能比界面活得久，不能靠内存里的旧地址猜）
#[tauri::command]
pub async fn device_link_state() -> Result<serde_json::Value, String> {
    match crate::conn::get() {
        Some(sup) => serde_json::to_value(sup.state()).map_err(|e| e.to_string()),
        None => Ok(serde_json::json!({
            "addr": null,
            "name": null,
            "connected": false,
            "retrying": false,
            "gaveUp": false,
            "busy": false
        })),
    }
}

/// 重新连一次（回到前台 / 用户点「重新连接」）：重新武装退避重连，结果走 wf-link-* 事件。
/// 已经连着时是空操作（不会拆掉好链路）。
/// 返回 `{armed, retrying}`：**armed=false** 表示总管手里根本没有「要保住的连接」
/// （用户主动断开过）—— 前端据此把「重连中…」收掉，也不会再自动重连。
#[tauri::command]
pub async fn device_relink() -> Result<serde_json::Value, String> {
    match crate::conn::get() {
        Some(sup) => {
            sup.relink();
            let st = sup.state();
            Ok(serde_json::json!({
                "armed": st.addr.is_some(),
                "retrying": st.retrying,
                "connected": st.connected,
                "gaveUp": st.gave_up
            }))
        }
        None => Ok(serde_json::json!({
            "armed": false,
            "retrying": false,
            "connected": false,
            "gaveUp": false
        })),
    }
}

/// 「后台保持连接」一键入口（设置页那一行）：插件按最能帮到用户的一步走，
/// 返回实际做了什么 —— `already`（已允许）/ `dialog`（弹了系统对话框）/ `settings`（开了应用信息页）/ `failed`。
/// 前端按 result 给一句对应的提示（保证每次点都看得见反馈）。
#[tauri::command]
pub async fn device_open_battery_settings(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    #[cfg(target_os = "android")]
    {
        use connkeep::ConnKeepExt;
        let result = app.conn_keep().battery_help()?;
        Ok(serde_json::json!({ "result": result }))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err("仅安卓版可用".to_string())
    }
}

// ---------- 命令：安装 ----------

fn extract_install_watchface_bin(data: &[u8]) -> Option<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data)).ok()?;
    let mut root_bin = None;
    let mut any_bin = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).ok()?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_ascii_lowercase();
        if !name.ends_with(".bin") {
            continue;
        }
        if !name.contains('/') {
            root_bin.get_or_insert(index);
        }
        any_bin.get_or_insert(index);
    }
    let index = root_bin.or(any_bin)?;
    let mut entry = archive.by_index(index).ok()?;
    let mut out = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut out).ok()?;
    Some(out)
}

async fn resolve_watchface_id(addr: &str, file_data: &[u8]) -> Option<String> {
    let install_data =
        extract_install_watchface_bin(file_data).unwrap_or_else(|| file_data.to_vec());
    corelib::device::watchface::resolve_xiaomi_watchface_id(addr.to_string(), install_data)
        .await
        .ok()
        .flatten()
}

/// 安装核心：字节数据 + 全局事件上报进度（发射事件 wf-install-progress）。
/// 本地安装只当提示文案用（「安装完成」）；付费安装走下面那个返回表盘 id 的版本。
pub(crate) async fn install_file_bytes(
    app: &tauri::AppHandle,
    addr: &str,
    file_data: Vec<u8>,
    ext: String,
) -> Result<String, String> {
    install_file_bytes_id(app, addr, file_data, ext)
        .await
        .map(|_| "安装完成".to_string())
}

/// 安装并返回**装上去的表盘 id**（非表盘类型返回 None）。
/// 付费安装用它：一批多个包时，把**最后一个包**的表盘设为当前。
pub(crate) async fn install_file_bytes_id(
    app: &tauri::AppHandle,
    addr: &str,
    file_data: Vec<u8>,
    ext: String,
) -> Result<Option<String>, String> {
    use corelib::device::xiaomi::components::install::InstallSystem;
    use corelib::device::xiaomi::components::mass::SendMassCallbackData;
    use tauri::Emitter;

    log::info!("[install] addr={addr} ext={ext} len={}", file_data.len());
    // 长任务开始：心跳让路（把「忙」当掉线会误报），自动重连也等它收尾
    let _busy = crate::conn::busy_guard();

    // 内容级文件类型识别（zip 容器按扩展名/内容细分，与 app_wasm 一致）
    let mut file_type = get_file_type(&file_data);
    if file_type == FileType::Zip {
        if ext.eq_ignore_ascii_case("mwz") {
            file_type = FileType::WatchFace;
        } else if extract_install_watchface_bin(&file_data).is_some() {
            file_type = FileType::WatchFace;
        }
    }
    log::info!("[install] detected type={file_type:?}");
    let data_type = MassDataType::try_from(file_type as u8).map_err(|e| e.to_string())?;
    let watchface_id = if matches!(data_type, MassDataType::Watchface) {
        resolve_watchface_id(addr, &file_data).await
    } else {
        None
    };
    if let Some(id) = watchface_id.as_deref() {
        if id.trim().starts_with("1209") {
            return Err("表盘文件已损坏".to_string());
        }
    }

    // 留档：安装成功后把 .bin 存到 <data>/watchface_archive/<MAC>/<id>.bin
    // （编辑「已装表盘」的唯一数据源：设备上的表盘无法回读，必须安装时留档）
    let archive_copy: Option<Vec<u8>> = if matches!(data_type, MassDataType::Watchface) {
        Some(file_data.clone())
    } else {
        None
    };

    // 同 ID 表盘已存在 → **先卸载再装**。
    // 原因：corelib 的表盘安装请求把 version_code 写死成 65536（见
    // `build_watchface_install_request`），设备拿它跟已装表盘的版本比较，不比对方大就回
    // `PrepareStatus::Downgrade` —— 也就是「同一个表盘装第二次」必然失败。
    // 卸载再装 = 每次安装都是干净结果（与 AstroBox 的安装语义一致）。
    if let Some(id) = watchface_id.clone() {
        match uninstall_if_exists(addr, &id).await {
            Ok(true) => log::info!("[install] 设备已有同 ID 表盘 {id}，已先卸载再安装"),
            Ok(false) => {}
            Err(e) => log::warn!("[install] 预处理（同 ID 卸载）失败，继续尝试安装: {e}"),
        }
    }

    let (progress_tx, mut progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<SendMassCallbackData>();
    let progress_cb: Arc<dyn Fn(SendMassCallbackData) + Send + Sync> = {
        let tx = progress_tx.clone();
        Arc::new(move |payload| {
            let _ = tx.send(payload);
        })
    };

    let addr_owned = addr.to_string();
    let wf_id_for_install = watchface_id.clone();
    let install_future = with_device_system::<InstallSystem, _, _>(&addr_owned, move |sys| {
        sys.send_install_request_with_progress(
            data_type,
            file_data,
            None,
            progress_cb,
            wf_id_for_install.as_deref(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    // 进度通过全局事件上报（二进制 raw body 命令无法携带 Channel 参数）
    let app_handle = app.clone();
    let forward = tauri::async_runtime::spawn(async move {
        while let Some(payload) = progress_rx.recv().await {
            let js = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
            let _ = app_handle.emit("wf-install-progress", js);
        }
    });

    let result = install_future.await;
    drop(progress_tx);
    let _ = forward.await;
    let mut resolved_id = watchface_id;
    if result.is_ok() {
        // 表盘 id 是设备往返解析出来的，偶发失败不该让「装成功」变成「失败」：
        // 装完再补两次（留档与「设为当前」都靠它；都没有就只是少这两步，安装结果不受影响）
        if resolved_id.is_none() {
            if let Some(bytes) = archive_copy.as_ref() {
                for _ in 0..2 {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    if let Some(id) = resolve_watchface_id(addr, bytes).await {
                        resolved_id = Some(id);
                        break;
                    }
                }
            }
        }
        if let (Some(bytes), Some(id)) = (archive_copy, resolved_id.as_deref()) {
            if let Err(e) = archive_watchface(app, addr, id, &bytes, &ext) {
                log::warn!("[install] 留档失败: {e:#}");
            }
        }
    }
    result
        .map(|_: ()| resolved_id)
        .map_err(|e| format!("{e}"))
}

/// 设备上若已有同 ID 表盘 → 卸载它，返回是否真的卸了。
/// 用于「重装同一个表盘」（设备判 Downgrade）与「更新表盘」两条路径。
async fn uninstall_if_exists(addr: &str, face_id: &str) -> anyhow::Result<bool> {
    use corelib::device::xiaomi::components::resource::ResourceSystem;
    use corelib::device::xiaomi::components::watchface::WatchfaceSystem;

    let list = with_device_system::<ResourceSystem, _, _>(addr, |sys| {
        sys.request_watchface_list()
    })
    .await?;
    let items = await_oneshot(list, "设备未返回表盘列表").await?;
    if !items.iter().any(|it| it.id == face_id) {
        return Ok(false);
    }
    let id = face_id.to_string();
    let rx = with_device_system::<WatchfaceSystem, _, _>(addr, move |sys| {
        sys.request_uninstall_watchface(&id)
    })
    .await?;
    await_oneshot(rx, "设备未确认卸载").await?;
    Ok(true)
}

/// 把安装过的表盘写到本地档案（供后续「编辑已装表盘」使用）
fn archive_watchface(
    app: &tauri::AppHandle,
    addr: &str,
    id: &str,
    data: &[u8],
    ext: &str,
) -> anyhow::Result<()> {
    let dir = archive_dir(app, addr)?;
    std::fs::create_dir_all(&dir)?;
    let ext = if ext.is_empty() { "bin" } else { ext };
    let path = dir.join(format!("{id}.{ext}"));
    // **留档加密**（付费表盘必须不落明文）：用设备密钥派生，只有本机能解开，
    // 编辑器读取时（watchface_archive_get）在内存里解密。
    let sealed = match crate::paid::seal_archive(&paid_dir(app)?, data) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[install] 留档加密失败，放弃留档（不写明文）: {e:#}");
            return Ok(());
        }
    };
    std::fs::write(&path, &sealed)?;
    log::info!(
        "[install] 已留档（密文）{} 明文 {} 字节 → 密文 {} 字节",
        path.display(),
        data.len(),
        sealed.len()
    );
    Ok(())
}

/// 付费模块目录（设备密钥所在；留档加解密要用它取设备密钥）
fn paid_dir(app: &tauri::AppHandle) -> anyhow::Result<std::path::PathBuf> {
    use tauri::Manager;
    Ok(crate::paid::paid_dir(&app.path().app_data_dir()?))
}

/// 留档目录：`<app_data>/watchface_archive/<MAC>/`（MAC 里的 ':' 归一化成 '_'）
fn archive_dir(app: &tauri::AppHandle, addr: &str) -> anyhow::Result<std::path::PathBuf> {
    use tauri::Manager;
    let mac = addr
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>();
    Ok(app
        .path()
        .app_data_dir()?
        .join("watchface_archive")
        .join(mac))
}

/// 留档条目（前端用它决定设备表盘列表里哪些能直接「编辑」）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveEntry {
    pub id: String,
    pub ext: String,
    pub size: u64,
}

/// 列出该设备的本地留档（= 通过本应用安装过的表盘）
#[tauri::command]
pub async fn watchface_archive_list(
    app: tauri::AppHandle,
    addr: String,
) -> Result<Vec<ArchiveEntry>, String> {
    let dir = archive_dir(&app, &addr).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out); // 目录不存在 = 没留档
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("bin")
            .to_string();
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(ArchiveEntry {
            id: stem.to_string(),
            ext,
            size,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    log::info!("[archive] {addr} 留档 {} 个", out.len());
    Ok(out)
}

/// 留档字节（编辑页入口：直接拿它进编辑页，不用用户再选文件）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveData {
    pub id: String,
    pub ext: String,
    pub size: u64,
    pub data_b64: String,
}

/// 读某个表的留档字节（编辑页的入口：不用再让用户选一次 .bin）
#[tauri::command]
pub async fn watchface_archive_get(
    app: tauri::AppHandle,
    addr: String,
    face_id: String,
) -> Result<ArchiveData, String> {
    use base64::Engine as _;
    let dir = archive_dir(&app, &addr).map_err(|e| e.to_string())?;
    // 逐个扩展名找（.bin / .mwz / .rpk）
    let mut hit = None;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.file_stem().and_then(|s| s.to_str()) == Some(face_id.as_str()) {
                hit = Some(p);
                break;
            }
        }
    }
    let path = hit.ok_or_else(|| {
        format!("本地没有 {face_id} 的留档（该表盘不是通过本应用安装的，请用「选择 .bin 文件」打开它）")
    })?;
    let raw = std::fs::read(&path).map_err(|e| format!("读取留档失败: {e}"))?;
    // 留档是密文（付费包必须不落明文）→ 读出来在内存里解密；老版本明文留档自动兼容
    let was_plain = !crate::paid::is_sealed(&raw);
    let bytes = match paid_dir(&app).and_then(|d| crate::paid::open_archive(&d, &raw).map_err(|e| e)) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[archive] 留档解密失败 {}: {e:#}", path.display());
            return Err("本地留档无法解密（通常是重装过应用）。到「我的表盘」或「本地表盘安装」重装一次这个表盘，之后就能编辑了".into());
        }
    };
    // 加密前的老版本留下的是明文 .bin（手机里躺着可直接拷走的素材）→ 顺手升级成密文
    if was_plain {
        match paid_dir(&app).and_then(|d| crate::paid::seal_archive(&d, &bytes)) {
            Ok(sealed) => match std::fs::write(&path, &sealed) {
                Ok(_) => log::info!("[archive] 明文留档已升级为密文 {}", path.display()),
                Err(e) => log::warn!("[archive] 留档回写失败（仍是明文）{}: {e}", path.display()),
            },
            Err(e) => log::warn!("[archive] 留档升级加密失败（仍是明文）{}: {e:#}", path.display()),
        }
    }
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin")
        .to_string();
    log::info!("[archive] 载入留档 {} ({} 字节，已解密)", path.display(), bytes.len());
    Ok(ArchiveData {
        id: face_id,
        ext,
        size: bytes.len() as u64,
        data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// 安装（路径版；桌面兼容保留）。
#[tauri::command]
pub async fn device_install(
    app: tauri::AppHandle,
    addr: String,
    path: String,
) -> Result<String, String> {
    let mut file = std::fs::File::open(&path).map_err(|e| format!("打开文件失败: {e}"))?;
    let mut file_data = Vec::new();
    file.read_to_end(&mut file_data)
        .map_err(|e| format!("读取文件失败: {e}"))?;
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    install_file_bytes(&app, &addr, file_data, ext).await
}

/// 安装（字节版）：安卓系统文件选择器返回 content:// URI，无法用文件路径打开；
/// 前端用 <input type="file"> 读取文件字节并 base64 编码后传入（安卓/桌面通用）。
#[tauri::command]
pub async fn device_install_payload(
    app: tauri::AppHandle,
    addr: String,
    data_b64: String,
    ext: String,
) -> Result<String, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .map_err(|e| format!("安装数据解码失败: {e}"))?;
    install_file_bytes(&app, &addr, bytes, ext).await
}

// ---------- 命令：表盘管理 ----------

#[tauri::command]
pub async fn watchface_get_list(addr: String) -> Result<serde_json::Value, String> {
    use corelib::device::xiaomi::components::resource::ResourceSystem;
    let rx = with_device_system::<ResourceSystem, _, _>(&addr, |sys| {
        sys.request_watchface_list()
    })
    .await
    .map_err(|e| e.to_string())?;
    let list = await_oneshot(rx, "设备未返回表盘列表")
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(&list).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn watchface_set_current(
    addr: String,
    watchface_id: String,
) -> Result<(), String> {
    use corelib::device::xiaomi::components::watchface::WatchfaceSystem;
    let wid = watchface_id.clone();
    let rx = with_device_system::<WatchfaceSystem, _, _>(&addr, move |sys| {
        sys.request_set_watchface(&wid)
    })
    .await
    .map_err(|e| e.to_string())?;
    await_oneshot(rx, "设备未确认切换")
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn watchface_uninstall(
    addr: String,
    watchface_id: String,
) -> Result<(), String> {
    use corelib::device::xiaomi::components::watchface::WatchfaceSystem;
    let wid = watchface_id.clone();
    let rx = with_device_system::<WatchfaceSystem, _, _>(&addr, move |sys| {
        sys.request_uninstall_watchface(&wid)
    })
    .await
    .map_err(|e| e.to_string())?;
    await_oneshot(rx, "设备未确认卸载")
        .await
        .map_err(|e| e.to_string())
}

// ---------- 命令：表盘编辑应用（EditRequest 两阶段 + 图片上传）----------

/// 设备上报 id 的格式（.bin 规则：槽位 = 0x08<<24 | idx，动作 = 0x09<<24 | idx）
#[derive(Debug, Clone, Copy, PartialEq)]
enum DeviceIdFormat {
    /// 8 位以内十六进制字符串，如 "09000005"
    Hex,
    /// 完整 u32 十进制字符串，如 "150994945"
    Dec,
}

fn detect_id_format(samples: &[&str]) -> DeviceIdFormat {
    for s in samples {
        let t = s.trim();
        if t.len() > 8 && t.chars().all(|c| c.is_ascii_digit()) {
            return DeviceIdFormat::Dec;
        }
    }
    DeviceIdFormat::Hex
}

fn fmt_device_id(f: DeviceIdFormat, high: u32, idx: u32) -> String {
    let v = (high << 24) | (idx & 0x00FF_FFFF);
    match f {
        DeviceIdFormat::Hex => format!("{v:08x}"),
        DeviceIdFormat::Dec => v.to_string(),
    }
}

/// 把设备 id 解析回低 24 位序号（用于格式校准：与默认候选对照）
fn parse_device_id(s: &str) -> Option<u32> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if t.len() <= 8 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        if let Ok(v) = u32::from_str_radix(t, 16) {
            return Some(v & 0x00FF_FFFF);
        }
    }
    t.parse::<u32>().ok().map(|v| v & 0x00FF_FFFF)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyEditReport {
    pub first_code: i32,
    pub can_accept_image_count: u32,
    pub slice_length: u32,
    pub uploaded: usize,
    pub image_ids: Vec<String>,
    pub final_code: i32,
    pub support_image_group: bool,
    /// 设备对壁纸上传回的 BgImageResult.code（None = 本次没传图；0 = 成功）
    pub image_code: Option<i32>,
    /// 设备回报的壁纸 md5
    pub image_md5: String,
}

/// 编辑进度事件载荷
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditProgress {
    pub phase: String, // prepare / upload / finalize / done
    pub detail: String,
    pub sent: Option<u64>,
    pub total: Option<u64>,
}

/// 设备上某表盘已有的壁纸（相册）——只能拿到 md5，拿不到图像数据
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumInfo {
    pub support_image_group: bool,
    /// 设备上报的组上限（support_max_count，0 = 未上报）
    pub max_count: u32,
    pub images: Vec<AlbumImageInfo>,
    /// 设备当前配色序号（多配色表盘：编辑页用它预选，避免一进来就默认配色1）
    pub style_color_index: Option<u32>,
    /// 设备当前样式名
    pub style: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumImageInfo {
    /// 图片 id（相册里就是 md5）
    pub id: String,
    /// 组 id / 背景图 md5
    pub bg_md5: String,
    pub pos_index: u32,
    pub size: Option<u32>,
}

/// 当前 App 版本（关于页显示用；Tauri 的 JS 全局 API 不一定带 app 模块，所以走命令）
#[tauri::command]
pub async fn app_version(app: tauri::AppHandle) -> Result<String, String> {
    Ok(app.package_info().version.to_string())
}

/// 用系统浏览器打开外部链接（App 内的「关于 / 开源许可」里的仓库、许可证等）
#[tauri::command]
pub async fn open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

/// 本地相册存档（每张照片的原始字节 + 该张自己的裁剪变换）。
/// 与 AstroBox 同款：`<app_data>/watchface_edit_photos/<表盘id>/NNNN.png` + index.json。
/// 用途：① 相册跨 App 重启保留；② 应用时按「整本相册」声明 + 全量重传（设备要求一一对应）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedPhoto {
    pub name: String,
    /// 原始图片（PNG/JPEG）的 base64
    pub b64: String,
    /// 该张的裁剪变换（前端 PhotoTransform 的 JSON，可为 null）
    #[serde(default)]
    pub crop: Option<serde_json::Value>,
}

fn photos_dir(app: &tauri::AppHandle, face_id: &str) -> anyhow::Result<std::path::PathBuf> {
    use tauri::Manager;
    let dir = app
        .path()
        .app_data_dir()?
        .join("watchface_edit_photos")
        .join(face_id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 读本地相册存档
#[tauri::command]
pub async fn watchface_photos_load(
    app: tauri::AppHandle,
    face_id: String,
) -> Result<Vec<SavedPhoto>, String> {
    let dir = photos_dir(&app, &face_id).map_err(|e| e.to_string())?;
    let index = dir.join("index.json");
    if !index.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&index).map_err(|e| e.to_string())?;
    let list: Vec<SavedPhoto> = serde_json::from_str(&text).unwrap_or_default();
    log::info!("[photos] 载入本地相册 {} 张（{face_id}）", list.len());
    Ok(list)
}

/// 覆盖保存本地相册存档
#[tauri::command]
pub async fn watchface_photos_save(
    app: tauri::AppHandle,
    face_id: String,
    photos: Vec<SavedPhoto>,
) -> Result<(), String> {
    use base64::Engine as _;
    let dir = photos_dir(&app, &face_id).map_err(|e| e.to_string())?;
    // 清掉旧的 png（数量可能变少），再写新的
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "png").unwrap_or(false) {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    for (i, ph) in photos.iter().enumerate() {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&ph.b64) {
            let _ = std::fs::write(dir.join(format!("{i:04}.png")), bytes);
        }
    }
    let text = serde_json::to_string_pretty(&photos).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("index.json"), text).map_err(|e| e.to_string())?;
    log::info!("[photos] 保存本地相册 {} 张（{face_id}）", photos.len());
    Ok(())
}

/// 读设备上该表盘已存的壁纸（相册）列表：EditRequest 的差量更新需要它
/// （开发期命令：正式包界面不调用；留着是为了排查设备上报状态）
#[tauri::command]
pub async fn watchface_album_get(addr: String, face_id: String) -> Result<AlbumInfo, String> {
    crate::dev_only("读取设备相册状态")?;
    use corelib::device::xiaomi::components::resource::ResourceSystem;
    let list = with_device_system::<ResourceSystem, _, _>(&addr, |sys| {
        sys.request_watchface_list()
    })
    .await
    .map_err(|e| e.to_string())?;
    let items = await_oneshot(list, "设备未返回表盘列表")
        .await
        .map_err(|e| e.to_string())?;
    let item = items
        .iter()
        .find(|it| it.id == face_id)
        .ok_or_else(|| format!("设备上没有表盘 {face_id}"))?;
    // 诊断日志：把设备上报的原始状态打出来（容器/相册/样式），排查排版问题用
    log::info!(
        "[device] face={} style={:?} slots={:?} groups={:?} bg_list={:?} fmt={:?}",
        item.id,
        item.style,
        item.slot_item_list
            .iter()
            .map(|s| (s.slot_id.clone(), s.widget_id.clone()))
            .collect::<Vec<_>>(),
        item.image_group_list
            .as_ref()
            .map(|g| g
                .list
                .iter()
                .map(|x| (x.content_pos_index, x.bg_image.id.clone(), x.bg_image.size))
                .collect::<Vec<_>>()),
        item.background_image_list,
        item.support_image_format
    );
    Ok(album_from_item(item))
}

fn album_from_item(item: &pb::xiaomi::protocol::WatchFaceItem) -> AlbumInfo {
    let mut images = Vec::new();
    let mut max_count = 0;
    if let Some(groups) = item.image_group_list.as_ref() {
        max_count = groups.support_max_count;
        for g in &groups.list {
            images.push(AlbumImageInfo {
                // 相册里「图片 id」就是 md5（无 fg 时 group.id = bg md5）
                id: g.bg_image.id.clone(),
                bg_md5: g.bg_image.id.clone(),
                pos_index: g.content_pos_index,
                size: g.bg_image.size,
            });
        }
    }
    if images.is_empty() {
        for id in &item.background_image_list {
            if id.trim().is_empty() {
                continue;
            }
            images.push(AlbumImageInfo {
                id: id.clone(),
                bg_md5: id.clone(),
                pos_index: images.len() as u32,
                size: None,
            });
        }
    }
    AlbumInfo {
        support_image_group: item.support_image_group.unwrap_or(false),
        max_count,
        images,
        style_color_index: item.style_color_index,
        style: item.style.trim().to_string(),
    }
}

/// 应用表盘编辑：
/// 1) 有壁纸时：逐张合成画布 → 编码成设备格式（手环 10 实测 ARGB_8565_LE=3 字节/像素）
///    → 算 md5（**客户端算**，设备端会回同一 md5）
/// 2) EditRequest：一次性声明整本相册（image_group_list + order_image_list）
/// 3) 逐张上传图片（MassData WatchfaceImage=48），设备按 md5 落盘
///
/// `photo_png_b64`：单张（兼容旧前端）；`photos_png_b64`：多张（相册，按顺序）。
/// 两者都给时以 `photos_png_b64` 为准。前端已把裁剪变换烘进画布 PNG。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn watchface_apply_edit(
    app: tauri::AppHandle,
    addr: String,
    face_id: String,
    style: Option<String>,
    style_color_index: Option<u32>,
    // 可编辑容器的用户选择：模型槽位序号 → 动作序号（设备 id 格式由后端按上报样本自动映射）
    slot_choices: Option<std::collections::HashMap<u32, u32>>,
    // .bin 模型里全部槽位的默认候选：槽位序号 → 动作序号（用于把设备没上报的槽位也补上，
    // 见下面 slot_list 的注释：手环按列表顺序对应样式布局里的槽位，少发槽位会让元素错位）
    slot_defaults: Option<std::collections::HashMap<u32, u32>>,
    photo_png_b64: Option<String>,
    photos_png_b64: Option<Vec<String>>,
    // 相册模式：append（保留设备已有壁纸，追加）/ replace（清空后只留这次上传的）
    album_mode: Option<String>,
    // 清空设备相册（独立操作：deleteAll=true 且**不带任何图片**——实测带图清空时设备会拒绝保存）
    clear_album: Option<bool>,
    set_current: Option<bool>,
) -> Result<ApplyEditReport, String> {
    use corelib::device::watchface::{
        self, WatchfaceEditParams, EditImageGroup, EditSlotItem,
    };
    use corelib::device::xiaomi::components::resource::ResourceSystem;
    use tauri::Emitter;

    // 长任务开始：心跳让路（把「忙」当掉线会误报），自动重连也等它收尾
    let _busy = crate::conn::busy_guard();

    let emit = |phase: &str, detail: String, sent: Option<u64>, total: Option<u64>| {
        let _ = app.emit(
            "wf-edit-progress",
            EditProgress {
                phase: phase.to_string(),
                detail,
                sent,
                total,
            },
        );
    };

    // 0) 拉设备表盘列表，取该表盘的当前状态（style/slot_item_list/背景色/是否支持图组）
    emit("prepare", "读取设备表盘状态…".into(), None, None);
    let list = with_device_system::<ResourceSystem, _, _>(&addr, |sys| {
        sys.request_watchface_list()
    })
    .await
    .map_err(|e| e.to_string())?;
    let items = await_oneshot(list, "设备未返回表盘列表")
        .await
        .map_err(|e| e.to_string())?;
    let device_item = items
        .iter()
        .find(|it| it.id == face_id)
        .ok_or_else(|| format!("设备上没有表盘 {face_id}（请先安装后再编辑）"))?
        .clone();
    let support_group = device_item.support_image_group.unwrap_or(false);
    let style = style.or_else(|| {
        let s = device_item.style.trim();
        (!s.is_empty()).then(|| s.to_string())
    });
    let style_color_index = style_color_index.or(device_item.style_color_index);
    let background_color = {
        let c = device_item.background_color.trim();
        (!c.is_empty()).then(|| c.to_string())
    };
    // 槽位：以设备现值为底，用户选择覆盖。
    // 设备 id 格式未知 → 从上报样本推断（hex/dec），并假设上报顺序 = .bin 槽位序号；
    // 上报条目数不足时按 .bin 规则（0x08<<24|idx）构造。
    let reported = &device_item.slot_item_list;
    let fmt = detect_id_format(
        &reported.iter().map(|it| it.widget_id.as_str()).collect::<Vec<_>>(),
    );
    let mut slot_list: Vec<EditSlotItem> = reported
        .iter()
        .map(|it| EditSlotItem {
            slot_id: it.slot_id.clone(),
            widget_id: it.widget_id.clone(),
        })
        .collect();
    // 容器（可编辑组件）：**必须发全**——手环把列表顺序对应到样式布局里的槽位，
    // 少发一个槽位（实测：样式3 只发 1/3/4、漏了 0）就会让时间/日期丢坐标挤到顶部。
    // AstroBox 的做法也是「累积型」列表（样式3 的请求里仍带着样式1 用过的 slot 0）。
    // 组装顺序：设备上报的现值 → 用 .bin 模型默认候选补齐缺的槽位 → 用户覆盖。
    if let Some(defaults) = &slot_defaults {
        for (slot_idx, action_idx) in defaults {
            let sid = fmt_device_id(fmt, 0x08, *slot_idx);
            if !slot_list.iter().any(|it| it.slot_id == sid) {
                slot_list.push(EditSlotItem {
                    slot_id: sid,
                    widget_id: fmt_device_id(fmt, 0x09, *action_idx),
                });
            }
        }
    }
    if let Some(choices) = &slot_choices {
        for (slot_idx, action_idx) in choices {
            let sid = fmt_device_id(fmt, 0x08, *slot_idx);
            match slot_list.iter_mut().find(|it| it.slot_id == sid) {
                Some(it) => it.widget_id = fmt_device_id(fmt, 0x09, *action_idx),
                None => {
                    slot_list.push(EditSlotItem {
                        slot_id: sid,
                        widget_id: fmt_device_id(fmt, 0x09, *action_idx),
                    });
                }
            }
        }
    }
    log::info!(
        "[edit] slot format={fmt:?} choices={slot_choices:?} -> {:?}",
        slot_list.iter().map(|i| (i.slot_id.clone(), i.widget_id.clone())).collect::<Vec<_>>()
    );

    let set_current = set_current.unwrap_or(true);

    // 设备上报的 dataList / 前景色要**原样回传**（AstroBox 的 apply 就是这么做的：
    // `dataList: t.dataList ?? []`、`foregroundColor: t.foregroundColor ?? void 0`）。
    // 留空会让设备以为表盘没有数据项，时间/日期等元素会丢布局（实测：元素挤到顶部）。
    let device_data_list: Vec<i32> = device_item.data_list.clone();
    let device_fg_hex: Option<String> = device_item.foreground_color.as_ref().and_then(|b| match b.len() {
        4 => Some(format!("#{:02x}{:02x}{:02x}", b[1], b[2], b[3])),
        3 => Some(format!("#{:02x}{:02x}{:02x}", b[0], b[1], b[2])),
        _ => None,
    });
    log::info!(
        "[edit] 设备上报 data_list={device_data_list:?} fg={device_fg_hex:?} bg={background_color:?}"
    );

    // 1) 逐张：把画布编码成设备要的像素格式并算 md5（**客户端**算，设备端会回同一 md5）。
    //    设备要求「先声明整本相册（EditRequest）→ 再逐张传图」。
    let mut photo_b64s: Vec<String> = photos_png_b64
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    if photo_b64s.is_empty() {
        if let Some(single) = photo_png_b64.clone().filter(|s| !s.is_empty()) {
            photo_b64s.push(single);
        }
    }
    let fmt = device_item.support_image_format.unwrap_or(7);
    let mut encoded_photos: Vec<(Vec<u8>, String)> = Vec::new();
    for (i, b64) in photo_b64s.iter().enumerate() {
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("壁纸 #{i} 数据解码失败: {e}"))?;
        let encoded = if fmt == 6 {
            log::info!("[edit] 设备要求 PNG(6)，原样上传 {} 字节", png.len());
            png
        } else {
            // 旧版本存下的相册里可能有未缩过的手机原图（新选图已缩到 2560）：
            // 解码前先挡超大图，否则一张 4000 万像素就 ~160MB，低内存手机上会被系统杀掉。
            let dims = image::ImageReader::new(std::io::Cursor::new(&png))
                .with_guessed_format()
                .map_err(|e| format!("壁纸 #{i} 解析失败: {e}"))?
                .into_dimensions()
                .map_err(|e| format!("壁纸 #{i} 解析失败: {e}"))?;
            if dims.0 as u64 * dims.1 as u64 > 24_000_000 {
                return Err(format!("壁纸 #{i} 分辨率太高，请换一张照片"));
            }
            let img =
                image::load_from_memory(&png).map_err(|e| format!("壁纸 #{i} PNG 解析失败: {e}"))?;
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            let bytes = crate::watchface_imgcodec::encode_canvas(rgba.as_raw(), w, h, fmt)
                .map_err(|e| e.to_string())?;
            log::info!(
                "[edit] encoded photo #{i}: {w}x{h} format={fmt} bytes={}",
                bytes.len()
            );
            bytes
        };
        let md5 = crate::watchface_imgcodec::md5_hex(&encoded);
        log::info!("[edit] photo #{i} md5={md5}");
        encoded_photos.push((encoded, md5));
    }

    // 相册现状：设备已有壁纸（md5 列表）——追加模式下要一起写进 order_image_list
    let album_before = album_from_item(&device_item);
    let album_mode = album_mode.unwrap_or_else(|| "append".to_string());
    let replace_all = album_mode == "replace";
    log::info!(
        "[edit] album mode={album_mode} 已有 {} 张 {:?}",
        album_before.images.len(),
        album_before.images.iter().map(|i| i.id.clone()).collect::<Vec<_>>()
    );

    // 2) EditRequest：一次性声明整本相册（图组 + 顺序表）
    emit("prepare", "发送编辑请求…".into(), None, None);
    let mut params = WatchfaceEditParams {
        id: face_id.clone(),
        set_current,
        background_color,
        style: style.clone().unwrap_or_default(),
        style_color_index,
        slot_item_list: slot_list,
        data_list: device_data_list.clone(),
        foreground_color: device_fg_hex.clone(),
        ..Default::default()
    };
    let mut order: Vec<String> = Vec::new();
    if clear_album.unwrap_or(false) {
        // AstroBox 的 `deleteAllImages` 只在「照片列表为空」时为 true：清空 = 一次独立的空请求
        params.delete_all_images = Some(true);
        log::info!("[edit] 清空设备相册（deleteAll=true，不带图片）");
    } else if !encoded_photos.is_empty() {
        // 相册声明规则（2026-10-06 真机结论）：
        //  ① **不能把相册缩小**：手环记着「当前正在看的照片位置」，位置越界时表盘元素会
        //     被挤到顶部（实测：声明 1、2 号位而缺 0 号位 / 相册从 5 张缩到 2 张，都会错位）。
        //     所以默认把设备已有壁纸原样带上（md5+size），新图追加在后。
        //  ② content_pos_index 从 0 连续编号，order_image_list 与 image_group_list 同长同内容
        //     （AstroBox 5 张时：photos=5 image_groups=5 order_list=5）。
        //  ③ 只上传新图（设备按 md5 去重，已有图不必重传）。
        params.delete_all_images = Some(replace_all);
        // 已有壁纸要带上「真实字节数」——设备上报的 size 是空的（None），
        // 之前填 0 会让整次请求的图片保存全部失败（IMAGE_SAVE_FAILED，实测 13:50/13:58）。
        // 同一表盘的图片都是整画布同格式，字节数相同 → 直接用本次编码出的长度。
        // ★ 声明 + 上传 = 同一批（本地相册的全部照片）。
        //   实测：声明了但没上传的图组会让整次保存失败（IMAGE_SAVE_FAILED）——
        //   设备要求「每个声明的图组都要有对应上传」，AstroBox 也是每次都把整本存档重传一遍。
        // ★ content_pos_index 恒为 0：它是「相册里第几个照片容器」而不是第几张照片。
        //   本表盘只有一个照片容器，AstroBox 的 7 张全填 0（设备上报的图组也全是 0）；
        //   之前填 0,1,2…（越界）会让表盘元素丢坐标挤到顶部——这就是错位的真正原因。
        for (encoded, md5) in encoded_photos.iter() {
            if support_group {
                params.image_groups.push(EditImageGroup {
                    id: md5.clone(),
                    content_pos_index: 0,
                    bg_image: md5.clone(),
                    bg_image_size: encoded.len() as u32,
                    fg_image: None,
                    fg_image_size: None,
                });
            } else {
                params.background_image_list.push(md5.clone());
                params
                    .background_image_size_list
                    .push(encoded.len() as u32);
            }
            order.push(md5.clone());
        }
        params.order_image_list = order.clone();
        log::info!(
            "[edit] 相册声明+上传 {} 张（设备上原有 {} 张，本次不保留）",
            order.len(),
            album_before.images.len()
        );
    }
    let first = watchface::edit_watchface(addr.clone(), params)
        .await
        .map_err(|e| format!("{e:#}"))?;
    log::info!(
        "[edit] response: code={} can_accept={} slice={} (support_group={}) order={:?}",
        first.code,
        first.can_accept_image_count,
        first.expected_slice_length,
        support_group,
        order
    );

    // 3) 逐张传图（分片长度取 EditResponse.expected_slice_length，0 时由 core 兜底 4096）
    let mut image_ids: Vec<String> = Vec::new();
    let mut image_code: Option<i32> = None;
    let mut image_md5 = String::new();
    let n_photos = encoded_photos.len();
    for (idx, (encoded, md5)) in encoded_photos.into_iter().enumerate() {
        let total = encoded.len() as u64;
        emit(
            "upload",
            if n_photos > 1 {
                format!("上传壁纸 {}/{}（{} KB）…", idx + 1, n_photos, total / 1024)
            } else {
                format!("上传壁纸（{} KB）…", total / 1024)
            },
            Some(0),
            Some(total),
        );
        let app2 = app.clone();
        let result = watchface::transfer_watchface_image(
            addr.clone(),
            encoded,
            first.expected_slice_length as usize,
            Some(Arc::new(move |d| {
                let _ = app2.emit(
                    "wf-edit-progress",
                    EditProgress {
                        phase: "upload".into(),
                        detail: if n_photos > 1 {
                            format!("壁纸 {}/{} 传输中", idx + 1, n_photos)
                        } else {
                            "壁纸传输中".into()
                        },
                        // progress 是 0.0~1.0；total_parts/current_part_num 是分片序号
                        sent: Some((d.progress * 1000.0) as u64),
                        total: Some(1000),
                    },
                );
            })),
        )
        .await
        .map_err(|e| format!("{e:#}"))?;
        log::info!(
            "[edit] bg image #{idx} result: code={} id={} md5={} (声明 md5={})",
            result.code,
            result.id,
            result.background_image,
            md5
        );
        image_code = Some(result.code);
        if !result.background_image.is_empty() {
            image_md5 = result.background_image.clone();
        }
        if result.code == 0 {
            image_ids.push(if result.background_image.trim().is_empty() {
                md5.clone()
            } else {
                result.background_image.clone()
            });
        }
    }

    emit("done", "应用完成".into(), None, None);

    Ok(ApplyEditReport {
        first_code: first.code,
        can_accept_image_count: first.can_accept_image_count,
        slice_length: first.expected_slice_length,
        uploaded: image_ids.len(),
        image_ids,
        final_code: first.code,
        support_image_group: support_group,
        image_code,
        image_md5,
    })
}

// ---------------------------------------------------------------- 安装前识别（型号防呆）

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinInfoLite {
    pub name: String,
    pub watchface_id: String,
    pub style_count: usize,
    /// 画布尺寸（= 表盘适配的屏幕）；认不出为 None
    pub canvas: Option<(u32, u32)>,
}

/// 轻量识别 .bin：表盘名 / id / **画布尺寸**。
/// 安装前用它做「型号防呆」：文件画布 ≠ 本机屏幕就直接阻断
/// （例如小米手环 10 Pro（336×480）不能装手环 10（212×520）或红米手表 5（432×514）的表盘）。
#[tauri::command]
pub async fn bin_inspect_info(data_b64: String) -> Result<BinInfoLite, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|e| format!("文件字节不是合法 base64: {e}"))?;
    let h = crate::watchface_bin::parse_header(&bytes).map_err(|e| e.to_string())?;
    let canvas = canvas_of(&bytes, &h);
    Ok(BinInfoLite {
        name: h.name,
        watchface_id: h.watchface_id,
        style_count: h.style_count,
        canvas,
    })
}

/// 画布尺寸：优先「样式头 back_image_id 指向的图」，否则取面积最大的图
/// （与服务端 `server/src/bin_inspect.rs` 的 `canvas_of` 同判据）。
fn canvas_of(d: &[u8], h: &crate::watchface_bin::BinHeader) -> Option<(u32, u32)> {
    use crate::watchface_bin::{block_records, parse_image_head, walk_styles};
    let styles = walk_styles(d, h).ok()?;
    for (_, s) in &styles {
        let low = s.back_image_id & 0x00FF_FFFF;
        let hi = (s.back_image_id >> 24) & 0x7F;
        if (hi == 0x02 || hi == 0x82) && low != 0 {
            for bt in &s.blocks {
                if bt.index != 2 && bt.index != 3 {
                    continue;
                }
                for r in block_records(d, bt.count, bt.offset) {
                    if r.idx as u32 == low {
                        if let Ok(head) = parse_image_head(d, r.data_offset as usize) {
                            let (w, h2) = (head.width as u32, head.height as u32);
                            if w > 0 && h2 > 0 && w <= 4096 && h2 <= 4096 {
                                return Some((w, h2));
                            }
                        }
                    }
                }
            }
        }
    }
    // 退化：取面积最大的图
    let mut best = None;
    let mut best_area = 0u64;
    for (_, s) in &styles {
        for bt in &s.blocks {
            if bt.index != 2 && bt.index != 3 {
                continue;
            }
            for r in block_records(d, bt.count, bt.offset) {
                if let Ok(head) = parse_image_head(d, r.data_offset as usize) {
                    let (w, h2) = (head.width as u32, head.height as u32);
                    if w == 0 || h2 == 0 || w > 4096 || h2 > 4096 {
                        continue;
                    }
                    let area = w as u64 * h2 as u64;
                    if area > best_area {
                        best_area = area;
                        best = Some((w, h2));
                    }
                }
            }
        }
    }
    best
}
