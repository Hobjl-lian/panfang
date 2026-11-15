//! vivo 蓝河手表桥接层。
//!
//! 移植自 `D:\zcode工作空间\蓝河表盘\vivo表盘助手` v0.1.0（已真机验证：连接、推送、安装）。
//! 与小米那条线的差别：
//!  - 凭证不是 authkey，而是手表机身记录的 openId。openId 不是密钥（握手签名用协议内置固定密钥），
//!    所以连上后用 BindAuth/BindInit 两条只读消息就能把它读出来，**不需要 vivo 账号**；
//!  - 传输以 BLE 为主、SPP 兜底（小米只有 SPP）；
//!  - 表盘是 `.rpk`（zip + manifest.json + router.watchfaces），安装走 file_v2 + DialInstall。
//!
//! 只读探测不发 BindBindRequest，对已绑定的手表没有副作用。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use bluetooth::btinterface::{self, ConnectType};
use corelib::device::Device;
use corelib::device::data::{DeviceStatusData, StorageStatusData};
use corelib::device::vivo::crypto::bind_aes_sign;
use corelib::device::vivo::dial_manifest::parse_vivo_dial_manifest;
use corelib::device::vivo::packet as vivo_packet;
use corelib::device::vivo::transport::vscp::{VscpMessage, VscpV2Pdu};
use corelib::device::vivo::{
    SendError as VivoSend, VivoConnectType, VivoDevice, VivoDeviceConfig,
    components::info::InfoSystem as VivoInfoSystem,
    components::resource::ResourceSystem as VivoResourceSystem,
};
use corelib::device::{DeviceConnectionInfo, DeviceKind, create_vivo_device};
use corelib::ecs::{self, runtime::Runtime};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::runtime::Handle;
use vivo_msgpack::messages::bind::{
    BID_BIND, BindAuthRequest, BindAuthResponse, BindInitRequest, BindInitResponse, CID_AUTH,
    CID_INIT, DEFAULT_BID_VERSION,
};

/// 单次绑定握手预算（与助手一致：vivo 手表回包偶发偏慢）
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
/// 只读探测里等单条回包的时间
const PROBE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

fn bt() -> anyhow::Result<&'static dyn btinterface::BluetoothInterface> {
    bluetooth::get_interface().ok_or_else(|| anyhow::anyhow!("蓝牙接口未初始化"))
}

fn map_send_error(err: btinterface::SendError) -> VivoSend {
    match err {
        btinterface::SendError::Disconnected => VivoSend::Disconnected,
        other => VivoSend::Io(other.to_string()),
    }
}

// ---------- openId 本地存储（按 MAC） ----------

#[derive(Debug, Default, Serialize, Deserialize)]
struct OpenIdStore {
    #[serde(default)]
    map: HashMap<String, String>,
}

fn store_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("vivo_openid.json"))
}

fn norm_addr(addr: &str) -> String {
    addr.trim().to_ascii_uppercase()
}

fn load_store(app: &AppHandle) -> OpenIdStore {
    store_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 已记录的 openId（没有就是空串）
pub fn stored_open_id(app: &AppHandle, addr: &str) -> String {
    load_store(app)
        .map
        .get(&norm_addr(addr))
        .cloned()
        .unwrap_or_default()
}

fn save_open_id(app: &AppHandle, addr: &str, open_id: &str) {
    let Some(path) = store_path(app) else { return };
    let mut store = load_store(app);
    let key = norm_addr(addr);
    if store.map.get(&key).map(String::as_str) == Some(open_id) {
        return;
    }
    store.map.insert(key, open_id.to_string());
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&store) {
        let _ = std::fs::write(path, text);
    }
}

// ---------- 只读探测：从 BindInitResponse 抄手表记录的 openId ----------

static SNOOP: LazyLock<Mutex<HashMap<u8, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn snoop_reset() {
    SNOOP.lock().unwrap().clear();
}

/// 从一条 VSCP 原始帧里抄下绑定域（BID 15）响应的业务载荷。
fn snoop_record(data: &[u8]) {
    let Ok(pdu) = VscpV2Pdu::parse(data) else { return };
    if pdu.frame_index() != 0 {
        return;
    }
    let (Some(bid), Some(cid)) = (pdu.bid(), pdu.cid()) else {
        return;
    };
    if bid != BID_BIND {
        return;
    }
    let chunk = pdu.chunk();
    if chunk.len() < 3 {
        return;
    }
    SNOOP.lock().unwrap().insert(cid, chunk[3..].to_vec());
}

fn snoop_take(cid: u8) -> Option<Vec<u8>> {
    SNOOP.lock().unwrap().remove(&cid)
}

async fn snoop_wait(cid: u8, timeout: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(payload) = snoop_take(cid) {
            return Some(payload);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

fn snoop_watch_open_id() -> Option<String> {
    let payload = snoop_take(BindInitResponse::CID)?;
    let resp = BindInitResponse::decode(&payload).ok()?;
    let open_id = resp.open_id.trim().to_string();
    (!open_id.is_empty()).then_some(open_id)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchBindInfo {
    pub open_id: String,
    /// 0=未绑定(出厂) 1=已绑定 2/3/4=中间态(连接/工厂/维修)
    pub state: i32,
}

// ---------- 传输 ----------

async fn transport_connect(addr: &str, ct: ConnectType) -> Result<(), String> {
    let addr_owned = addr.to_string();
    let display_addr = addr.to_string();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let iface = bt().map_err(|e| e.to_string())?;
        // Some(false)=保留已有配对直接连接
        iface.connect(addr_owned, ct, vec![], Some(false)).map_err(|e| match e {
            btinterface::ConnectError::DeviceNotFound => format!(
                "手机没有发现这台手表（{display_addr}）。\
                 请确认：① 手表已用「vivo 健康」连接过；\
                 ② vivo 健康已从最近任务退出、没占着手表；\
                 ③ 本应用的「附近的设备」与「定位」权限已允许"
            ),
            btinterface::ConnectError::TargetRejected => format!(
                "手表没有接受连接（{display_addr}）。请确认 vivo 健康没有正在连接它"
            ),
            other => format!("蓝牙连接失败（{display_addr}）：{other}"),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn transport_close(addr: &str) {
    let a = addr.to_string();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        if let Ok(iface) = bt() {
            let _ = iface.disconnect(&a);
        }
    })
    .await;
}

fn attempts_for(mode: &str) -> Vec<&'static str> {
    match mode {
        "spp" => vec!["spp"],
        "ble" => vec!["ble"],
        // 与已验证的助手一致：SPP 优先，失败回退 BLE
        _ => vec!["spp", "ble"],
    }
}

fn connect_type_for(attempt: &str) -> ConnectType {
    if attempt == "ble" {
        ConnectType::BLE
    } else {
        ConnectType::SPP
    }
}

fn vivo_connect_type_for(attempt: &str) -> VivoConnectType {
    if attempt == "ble" {
        VivoConnectType::BLE
    } else {
        VivoConnectType::SPP
    }
}

/// 只读探测：完成 BindAuth + BindInit 两条消息即停，不发绑定确认。
pub async fn probe_bind_info(name: &str, addr: &str, mode: &str) -> Result<WatchBindInfo, String> {
    let mut last_err = String::new();
    for attempt in attempts_for(mode) {
        let ct = connect_type_for(attempt);
        snoop_reset();
        if let Err(err) = transport_connect(addr, ct).await {
            last_err = err;
            continue;
        }

        let probe_handle = tokio::runtime::Handle::current();
        let probe_addr = addr.to_string();
        let sub_cb: Arc<dyn Fn(Result<Vec<u8>, String>) + Send + Sync> =
            Arc::new(move |res| match res {
                Ok(data) => {
                    snoop_record(&data);
                    vivo_packet::on_packet(probe_handle.clone(), probe_addr.clone(), data);
                }
                Err(err) => log::warn!("[vivo/probe] notify error: {err}"),
            });
        let sub_addr = addr.to_string();
        if let Err(err) = tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            let iface = bt().map_err(|e| e.to_string())?;
            iface
                .subscribe(&sub_addr, sub_cb, None)
                .map_err(|e| format!("订阅蓝牙通知失败: {e}"))
        })
        .await
        .map_err(|e| e.to_string())?
        {
            last_err = err;
            transport_close(addr).await;
            continue;
        }

        // 编码器：独立 VivoDevice 实例（不进 ECS），只借它做 VSCP 帧编码
        let dummy_sender = |_data: Vec<Vec<u8>>| async move { Ok::<(), VivoSend>(()) };
        let dev = VivoDevice::new(
            name.to_string(),
            addr.to_string(),
            vivo_connect_type_for(attempt),
            VivoDeviceConfig::default(),
            dummy_sender,
        );

        let send_frames = async |bid: u8, cid: u8, payload: Vec<u8>| -> Result<(), String> {
            let packets = dev
                .encode_message_packets(VscpMessage::new(bid, cid, payload))
                .map_err(|e| format!("编码失败: {e:?}"))?;
            let Some(iface) = bluetooth::get_interface() else {
                return Err("蓝牙接口未初始化".to_string());
            };
            iface
                .send_many_async(addr, packets, None)
                .await
                .map_err(|e| format!("发送失败: {e:?}"))
        };

        // ① BindAuthRequest（探测用占位 openId；签名仍按协议正常计算）
        let random: u16 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_millis() as u16)
            .unwrap_or(0x1234);
        let probe_open_id = "panfang-probe";
        let Ok(aes_sign) = bind_aes_sign(probe_open_id, random) else {
            transport_close(addr).await;
            last_err = "生成绑定签名失败".to_string();
            continue;
        };
        let Ok(auth_payload) = (BindAuthRequest {
            open_id: probe_open_id.to_string(),
            random: i32::from(random),
            aes_sign,
            bid_version: DEFAULT_BID_VERSION,
            magic_phone: String::new(),
        })
        .payload()
        else {
            transport_close(addr).await;
            last_err = "编码绑定请求失败".to_string();
            continue;
        };
        if let Err(err) = send_frames(BID_BIND, CID_AUTH, auth_payload).await {
            last_err = err;
            transport_close(addr).await;
            continue;
        }

        // ② 等 BindAuthResponse 取 seq
        let Some(raw) = snoop_wait(BindAuthResponse::CID, PROBE_REPLY_TIMEOUT).await else {
            last_err = "手表未响应绑定握手（5 秒超时）".to_string();
            transport_close(addr).await;
            continue;
        };
        let Ok(auth_resp) = BindAuthResponse::decode(&raw) else {
            last_err = "解析绑定握手回包失败".to_string();
            transport_close(addr).await;
            continue;
        };
        if auth_resp.code != 0 {
            last_err = format!("手表拒绝绑定握手: code={}", auth_resp.code);
            transport_close(addr).await;
            continue;
        }

        // ③ BindInitRequest（seq = auth_seq + 21，与官方 App 一致）
        let Ok(init_payload) = (BindInitRequest {
            os: 1,
            is_bind: true,
            version: "1".to_string(),
            module: "panfang".to_string(),
            phone_device_id: "panfang-phone".to_string(),
            seq: auth_resp.seq + 21,
        })
        .payload()
        else {
            transport_close(addr).await;
            last_err = "编码绑定信息请求失败".to_string();
            continue;
        };
        if let Err(err) = send_frames(BID_BIND, CID_INIT, init_payload).await {
            last_err = err;
            transport_close(addr).await;
            continue;
        }

        // ④ BindInitResponse 里就是手表记录的 openId 与绑定状态
        let Some(raw) = snoop_wait(BindInitResponse::CID, PROBE_REPLY_TIMEOUT).await else {
            last_err = "手表未返回绑定信息（5 秒超时）".to_string();
            transport_close(addr).await;
            continue;
        };
        transport_close(addr).await;
        match BindInitResponse::decode(&raw) {
            Ok(resp) => {
                log::info!(
                    "[vivo/probe] 手表绑定信息 state={} open_id_len={}",
                    resp.state,
                    resp.open_id.trim().len()
                );
                return Ok(WatchBindInfo {
                    open_id: resp.open_id.trim().to_string(),
                    state: resp.state,
                });
            }
            Err(_) => last_err = "解析绑定信息回包失败".to_string(),
        }
    }
    Err(last_err)
}

// ---------- 连接 ----------

async fn connect_once(
    handle: &Handle,
    name: &str,
    addr: &str,
    open_id: &str,
    attempt: &str,
) -> Result<DeviceConnectionInfo, String> {
    let ct = connect_type_for(attempt);
    transport_connect(addr, ct).await?;
    log::info!("[vivo] transport connected via {attempt}");

    // 订阅通知：入站帧先抄进嗅探缓存，再喂给 corelib 分发器
    let packet_handle = handle.clone();
    let sub_addr = addr.to_string();
    let lost_addr = addr.to_string();
    let sub_cb: Arc<dyn Fn(Result<Vec<u8>, String>) + Send + Sync> =
        Arc::new(move |res| match res {
            Ok(data) => {
                snoop_record(&data);
                vivo_packet::on_packet(packet_handle.clone(), sub_addr.clone(), data);
            }
            Err(err) => {
                log::warn!("[vivo] notify error: {err}");
                if let Some(sup) = crate::conn::get() {
                    sup.note_lost(&lost_addr, &format!("蓝牙通道断开: {err}"), true);
                }
            }
        });
    let sub_addr2 = addr.to_string();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let iface = bt().map_err(|e| e.to_string())?;
        iface
            .subscribe(&sub_addr2, sub_cb, None)
            .map_err(|e| format!("订阅蓝牙通知失败: {e}"))
    })
    .await
    .map_err(|e| e.to_string())??;

    let send_addr = addr.to_string();
    let sender = move |data: Vec<Vec<u8>>| {
        let addr = send_addr.clone();
        async move {
            let Some(iface) = bluetooth::get_interface() else {
                return Err(VivoSend::Disconnected);
            };
            let result = iface.send_many_async(&addr, data, None).await;
            if let Err(e) = &result {
                if let Some(sup) = crate::conn::get() {
                    sup.note_lost(&addr, &format!("发送失败: {e:?}"), true);
                }
            }
            result.map_err(map_send_error)
        }
    };

    let config = VivoDeviceConfig {
        open_id: open_id.to_string(),
        phone_device_id: "panfang-phone".to_string(),
        app_version: "1".to_string(),
        phone_model: "Panfang".to_string(),
        ..Default::default()
    };

    let info = tokio::time::timeout(
        AUTH_TIMEOUT,
        create_vivo_device(
            handle.clone(),
            name.to_string(),
            addr.to_string(),
            vivo_connect_type_for(attempt),
            config,
            sender,
        ),
    )
    .await
    .map_err(|_| "绑定握手超时（30 秒内手表未完成绑定）".to_string())?
    .map_err(|e| format!("绑定握手失败: {e}"))?;
    Ok(info)
}

/// 连接 vivo 手表。openId 的取法（按优先级）：
/// 1. 调用方给的 hint；2. 本地已记录；3. 只读探测手表记录的 openId。
/// 握手因 openId 不一致失败时，会自动改用手表记录的值重试一次。
pub async fn connect(
    app: &AppHandle,
    handle: &Handle,
    name: &str,
    addr: &str,
    hint: Option<&str>,
) -> Result<DeviceConnectionInfo, String> {
    let mut open_id = hint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| stored_open_id(app, addr));

    if open_id.is_empty() {
        log::info!("[vivo] 本地没有 openId，先做只读探测 addr={addr}");
        let info = probe_bind_info(name, addr, "auto").await?;
        if info.open_id.is_empty() {
            if info.state == 0 {
                return Err(
                    "手表还未绑定过账号。请先用「vivo 健康」连接一次手表，再回来重试".to_string(),
                );
            }
            return Err("没有读到手表的绑定信息。请确认 vivo 健康已退出后重试".to_string());
        }
        open_id = info.open_id;
        save_open_id(app, addr, &open_id);
        // 探测刚拆过链，给蓝牙栈一点时间再重连（Android 上立刻重连偶发 GATT busy）
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let mut last_err = String::new();
    for pass in 0..2 {
        for (idx, attempt) in attempts_for("auto").into_iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            snoop_reset();
            match connect_once(handle, name, addr, &open_id, attempt).await {
                Ok(info) => {
                    save_open_id(app, addr, &open_id);
                    log::info!("[vivo] 连接成功 addr={addr} mode={attempt}");
                    return Ok(info);
                }
                Err(err) => {
                    log::warn!("[vivo] pass={pass} mode={attempt} 失败: {err}");
                    last_err = err;
                    transport_close(addr).await;
                }
            }
        }
        if pass == 0 {
            if let Some(watch_open_id) = snoop_watch_open_id() {
                if watch_open_id != open_id {
                    log::info!("[vivo] 改用手表记录的 openId 重试");
                    open_id = watch_open_id;
                    save_open_id(app, addr, &open_id);
                    continue;
                }
            }
        }
    }
    Err(humanize_connect_error(&last_err))
}

fn humanize_connect_error(err: &str) -> String {
    if err.contains("openId") || err.contains("open_id") || err.contains("bind reset") {
        return "连接被拒：手表的绑定信息与预期不一致。请确认手表是用「vivo 健康」正常绑定过的，然后重试"
            .to_string();
    }
    if err.is_empty() {
        return "连接失败，请确认 vivo 健康已退出后重试".to_string();
    }
    if err.chars().count() > 160 {
        let short: String = err.chars().take(160).collect();
        return format!("{short}…");
    }
    err.to_string()
}

// ---------- 设备类型判定 ----------

/// 这个地址上挂的设备是不是 vivo（未连接 / 不存在都返回 false）
pub async fn is_vivo(addr: &str) -> bool {
    let owned = addr.to_string();
    let kind = ecs::with_rt_mut(move |rt: &mut Runtime| {
        rt.component_ref::<Device>(&owned).map(|d| d.kind())
    })
    .await;
    matches!(kind, Some(DeviceKind::Vivo))
}

// ---------- 设备数据 ----------

pub async fn device_info(addr: &str) -> anyhow::Result<serde_json::Value> {
    let rx = crate::bridge::with_device_system::<VivoInfoSystem, _, _>(addr, |sys| {
        sys.request_device_info()
    })
    .await?;
    let v = crate::bridge::await_oneshot(rx, "设备未返回信息").await?;
    Ok(serde_json::to_value(&v)?)
}

pub async fn device_status(addr: &str) -> anyhow::Result<DeviceStatusData> {
    let rx = crate::bridge::with_device_system::<VivoInfoSystem, _, _>(addr, |sys| {
        sys.request_device_status()
    })
    .await?;
    crate::bridge::await_oneshot(rx, "设备未返回状态").await
}

pub async fn device_storage(addr: &str) -> anyhow::Result<StorageStatusData> {
    let rx = crate::bridge::with_device_system::<VivoInfoSystem, _, _>(addr, |sys| {
        sys.request_device_storage()
    })
    .await?;
    crate::bridge::await_oneshot(rx, "设备未返回存储信息").await
}

pub async fn dial_list(addr: &str) -> anyhow::Result<serde_json::Value> {
    let rx = crate::bridge::with_device_system::<VivoResourceSystem, _, _>(addr, |sys| {
        sys.request_watchface_list()
    })
    .await?;
    let list = crate::bridge::await_oneshot(rx, "设备未返回表盘列表").await?;
    Ok(serde_json::to_value(&list)?)
}

// ---------- 表盘安装 / 切换 / 卸载 ----------

/// 是不是 vivo 表盘包：manifest.json 里有 `router.watchfaces`。
/// `.rpk` 扩展名不可靠（小米快应用也是 rpk），必须看内容。
pub fn is_dial_rpk(bytes: &[u8]) -> bool {
    let cursor = std::io::Cursor::new(bytes);
    let Ok(mut archive) = zip::ZipArchive::new(cursor) else {
        return false;
    };
    let Ok(mut entry) = archive.by_name("manifest.json") else {
        return false;
    };
    let mut text = String::new();
    if std::io::Read::read_to_string(&mut entry, &mut text).is_err() {
        return false;
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    json.get("router")
        .and_then(|r| r.get("watchfaces"))
        .is_some()
}

/// 本地 rpk 的 dialId / 包名 / 版本名 / 版本号（确认页展示用）
pub fn inspect_dial(bytes: &[u8]) -> anyhow::Result<(i64, String, String, i32)> {
    let m = parse_vivo_dial_manifest(bytes)?;
    Ok((m.dial_id, m.package, m.version_name, m.version_code))
}

pub async fn install_dial(
    addr: &str,
    data: Vec<u8>,
    progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
) -> anyhow::Result<()> {
    corelib::device::watchface::install_local_zip_vivo(addr.to_string(), data, None, progress)
        .await
}

// ---------- 快应用 ----------

/// vivo 快应用包里有 `<包名>.vru`（应用本体，magic = "vivo union file"）。
/// 两端快应用都是 zip + manifest.json，靠这个条目区分（小米手环的 rpk 没有 .vru）。
pub fn has_vru_entry(bytes: &[u8]) -> bool {
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return false;
    };
    (0..archive.len()).any(|i| {
        archive
            .by_index(i)
            .map(|entry| {
                entry
                    .name()
                    .trim_end()
                    .to_ascii_lowercase()
                    .ends_with(".vru")
            })
            .unwrap_or(false)
    })
}

/// 是不是 vivo 快应用包：不是表盘，且带 `<包名>.vru`。
pub fn is_quick_app_rpk(bytes: &[u8]) -> bool {
    !is_dial_rpk(bytes) && has_vru_entry(bytes)
}

/// 本地 vivo 快应用的 manifest 信息（确认页展示用）
pub fn inspect_quick_app(
    bytes: &[u8],
) -> anyhow::Result<corelib::device::vivo::quickapp_manifest::VivoQuickAppManifest> {
    corelib::device::vivo::quickapp_manifest::parse_vivo_quick_app_manifest(bytes)
}

/// vivo 快应用安装：file_v2 推到 `/sdcard/apps/apparch/<包名>.rpk`，
/// 再发 BleAppInstallReq（BID 40 / CID 1）让手表就地安装。
pub async fn install_quick_app(
    addr: &str,
    data: Vec<u8>,
    progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
) -> anyhow::Result<()> {
    corelib::device::install::install_vivo_quick_app_local(
        addr.to_string(),
        data,
        None,
        None,
        None,
        progress,
    )
    .await
}

pub async fn set_current(addr: &str, dial_id: &str) -> anyhow::Result<()> {
    corelib::device::watchface::set_current(addr.to_string(), dial_id.to_string()).await
}

pub async fn uninstall(addr: &str, dial_id: &str) -> anyhow::Result<()> {
    corelib::device::watchface::uninstall(addr.to_string(), dial_id.to_string()).await
}
