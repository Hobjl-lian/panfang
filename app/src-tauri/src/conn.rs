//! 连接总管：掉线检测 + 自动重连 + 保活（前台服务）。
//!
//! 分工：
//!  - **保活**（connkeep 插件的前台服务）钉住进程，让下面这些逻辑在后台也有机会跑；
//!  - **掉线检测**三条线：① 蓝牙通知通道报错（Kotlin 读线程 IOException → 这里）；
//!    ② 发送失败（socket 已死）；③ 心跳（每 15s 查一次设备状态，连续两次失败再复核一次）。
//!  - **自动重连**：确认掉线后按退避表重试（1/3/6/12/20/30s…约 2.4 分钟），
//!    成功发 `wf-link-restored`，全失败发 `wf-link-give-up`（界面给一句操作指引 + 手动重连）。
//!
//! 为什么放在 Rust 而不是前端：后台 WebView 的定时器会被 Chromium 节流，
//! JS 驱动的重连在后台根本不会跑；Rust 线程在前台服务下正常运行。
//!
//! 真机结论（2026-10-10）：手环配对过一次且没被别的 App 占用时，重新连回来**通常不需要**
//! 手环进「连接新手机」；少数连不回来 —— 所以退避表跑完就放弃，把选择权交回用户。
//!
//! 状态一律内存态、不持久化（用户的铁律：瞬时状态不落盘）。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};
use tokio::runtime::Handle;

use crate::bridge;

/// 心跳间隔
const HEARTBEAT: Duration = Duration::from_secs(15);
/// 单次心跳探测超时（设备刚连上/正在拉列表时会忙，别卡太紧 —— 真机实测 8s 偏紧）
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 判死前的复核探测超时（复核要更宽容：宁可多等一下，也别误杀好链路）
const VERIFY_TIMEOUT: Duration = Duration::from_secs(15);
/// 判死前要连续失败几次（每次心跳算一次；避免单次抖动误报）
const FAILS_BEFORE_LOST: u32 = 2;
/// 重连退避（秒）：先快后慢；跑完即放弃（约 2.4 分钟）
const BACKOFF_SECS: &[u64] = &[1, 3, 6, 12, 20, 30, 30, 30];
/// 重连前若还有长任务在收尾（安装/应用），最多等这么久
const BUSY_WAIT_LIMIT: Duration = Duration::from_secs(30);
/// 短命连接熔断：连接成功后在这么短时间内又掉线，算一次「空闲反复断开」
/// （买家录屏：连上后约 1 秒就断、再连再断；诊断 30 秒节流后看起来像 60~100 秒一次）
const FLAP_WINDOW: Duration = Duration::from_secs(120);
/// 连续几次短命连接后停止自动重连。买家被小米运动健康/另一台手机占着时，
/// 我们越自动重连越是在抢链路，不如停下并提示用户去关闭占用。
const FLAP_LIMIT: u32 = 3;

static SUPERVISOR: OnceLock<Arc<Supervisor>> = OnceLock::new();

/// 期望保持的连接（连接成功后登记；用户主动断开/切换设备时清掉）
#[derive(Clone, Debug)]
pub struct DesiredConn {
    pub addr: String,
    pub name: String,
    pub authkey: String,
}

/// 连接总管对外暴露的状态快照（`device_link_state` 命令 / 前端重挂载用）
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkState {
    pub addr: Option<String>,
    pub name: Option<String>,
    pub connected: bool,
    pub retrying: bool,
    pub gave_up: bool,
    pub busy: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkLostPayload {
    addr: String,
    reason: String,
    will_retry: bool,
    attempt: u32,
    /// true = 短命连接熔断导致的掉线（前端别弹普通的「已断开，正在重连」）
    flap: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkRetryPayload {
    addr: String,
    attempt: u32,
    delay_ms: u64,
    /// true = 这次重连会先解除系统配对（短命掉线后的第一次重连，用来顶掉占用方）
    unpair: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkRestoredPayload {
    addr: String,
    name: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkGiveUpPayload {
    addr: String,
    reason: String,
    /// true = 短命连接熔断（不是退避跑完）；前端给「去关闭小米运动健康」的提示
    flap: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceStatusPayload {
    addr: String,
    battery: u32,
}

pub struct Supervisor {
    app: AppHandle,
    core: Handle,
    desired: Mutex<Option<DesiredConn>>,
    /// 期望连接的代际：用户换设备 / 主动断开都会 +1，让在飞的重连尝试自觉作废
    generation: AtomicU64,
    connected: AtomicBool,
    retrying: AtomicBool,
    gave_up: AtomicBool,
    /// 本次掉线是否已经报过（一个掉线周期只报一次，避免刷屏）
    lost_notified: AtomicBool,
    /// 长任务（安装 / 应用编辑）计数：>0 时不做心跳，也不启动重连
    busy: AtomicUsize,
    /// 本次连接成功的时刻（判「短命连接」用；掉线时取走）
    connected_at: Mutex<Option<Instant>>,
    /// 连续「连接成功后在 120 秒内又断」的次数（空闲掉线才计；传输中/长连接清零）
    flap_count: AtomicU32,
    probe_failures: AtomicU32,
    /// 保活通知是否已显示（Android 13+ 通知权限：首次连接时才弹框，
    /// 弹框之前 startForeground 的通知会被系统静默吞掉 → 权限给到后要补发一次）
    notif_ok: AtomicBool,
    /// 补发通知的尝试次数（放弃上限见 NOTIF_REFRESH_LIMIT）
    notif_refresh: AtomicU32,
}

/// 通知权限补发的尝试上限（15s × 40 ≈ 10 分钟；此后不再打扰）
const NOTIF_REFRESH_LIMIT: u32 = 40;

/// 长任务期间的心跳豁免（RAII）：命令进入时取一个，返回时自动放掉。
pub struct BusyGuard(Arc<Supervisor>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Supervisor {
    fn is_desired(&self, addr: &str) -> bool {
        self.desired.lock().as_ref().is_some_and(|d| d.addr == addr)
    }

    fn emit<T: serde::Serialize + Clone>(&self, event: &str, payload: T) {
        let _ = self.app.emit(event, payload);
    }

    /// 保活前台服务：拉起 / 刷新（失败只记日志，不影响连接本身）
    fn start_keepalive(&self, name: &str, addr: &str) {
        #[cfg(target_os = "android")]
        {
            use connkeep::ConnKeepExt;
            match self.app.conn_keep().start(name, addr) {
                Ok(st) => {
                    let was = self.notif_ok.swap(st.notification_allowed, Ordering::SeqCst);
                    if !was && st.notification_allowed {
                        log::info!("[conn] 保活服务在跑，通知已显示（running={}）", st.running);
                    } else if !st.notification_allowed {
                        log::debug!("[conn] 保活服务在跑，但通知权限未允许（通知暂不显示）");
                    }
                }
                Err(e) => log::warn!("[conn] 前台服务启动失败（不影响连接）: {e}"),
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (name, addr);
        }
    }

    fn stop_keepalive(&self) {
        #[cfg(target_os = "android")]
        {
            use connkeep::ConnKeepExt;
            if let Err(e) = self.app.conn_keep().stop() {
                log::warn!("[conn] 前台服务停止失败: {e}");
            }
        }
    }

    /// 连接成功：登记期望连接、开始保活（前端手动连接成功后调）
    pub fn arm(&self, addr: &str, name: &str, authkey: &str) {
        *self.desired.lock() = Some(DesiredConn {
            addr: addr.to_string(),
            name: name.to_string(),
            authkey: authkey.to_string(),
        });
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.connected.store(true, Ordering::SeqCst);
        *self.connected_at.lock() = Some(Instant::now());
        self.flap_count.store(0, Ordering::SeqCst);
        self.retrying.store(false, Ordering::SeqCst);
        self.gave_up.store(false, Ordering::SeqCst);
        self.lost_notified.store(false, Ordering::SeqCst);
        self.probe_failures.store(0, Ordering::SeqCst);
        self.notif_refresh.store(0, Ordering::SeqCst);
        self.start_keepalive(name, addr);
    }

    /// 用户主动断开 / 切换设备 / 清残留会话：撤销期望连接并停保活
    pub fn disarm(&self, addr: &str) {
        let hit = {
            let mut d = self.desired.lock();
            if d.as_ref().is_some_and(|x| x.addr == addr) {
                *d = None;
                true
            } else {
                false
            }
        };
        if !hit {
            return;
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
        *self.connected_at.lock() = None;
        self.flap_count.store(0, Ordering::SeqCst);
        self.retrying.store(false, Ordering::SeqCst);
        self.gave_up.store(false, Ordering::SeqCst);
        self.lost_notified.store(false, Ordering::SeqCst);
        self.probe_failures.store(0, Ordering::SeqCst);
        self.stop_keepalive();
        log::info!("[conn] 已撤销期望连接 {addr}，保活服务停止");
    }

    /// 掉线上报（三条检测线都汇到这里）
    ///
    /// `hard = true`：链路确实死了（通知通道报错 / 发送失败）→ 重连时不必先验证；
    /// `hard = false`：心跳探测失败（可能是抖动）→ 重连前先复核一次，别误杀好链路。
    pub fn note_lost(self: &Arc<Self>, addr: &str, reason: &str, hard: bool) {
        if !self.is_desired(addr) {
            return; // 不是我们要保的连接（比如刚切走的那台）
        }
        if !self.connected.load(Ordering::SeqCst) && self.retrying.load(Ordering::SeqCst) {
            return; // 已经在重连流程里了
        }
        if self.gave_up.load(Ordering::SeqCst) {
            // 已放弃自动重连（等用户手动重连）：死 socket 上迟到的 read/write 报错不该把这一轮重新点着
            return;
        }
        let was_connected = self.connected.swap(false, Ordering::SeqCst);
        self.probe_failures.store(0, Ordering::SeqCst);

        // 短命连接熔断：只在「本来连着、且不是传输中」的空闲掉线上计数。
        // 买家被小米运动健康/另一台手机占着时，链路会连上 1 秒就被顶掉；
        // 以前每次重连成功都把退避重置，于是无限循环「连上→断→1 秒后重连」。
        let alive = self.connected_at.lock().take();
        let busy = self.busy.load(Ordering::SeqCst) > 0;
        let short_idle = was_connected
            && !busy
            && alive.is_some_and(|t| t.elapsed() < FLAP_WINDOW);
        let flaps = if short_idle {
            self.flap_count.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            // 长连接（>=120s）或传输中掉线 = 不是空闲抢占，清零重来
            self.flap_count.store(0, Ordering::SeqCst);
            0
        };
        if flaps >= FLAP_LIMIT {
            let alive_secs = alive.map(|t| t.elapsed().as_secs()).unwrap_or(0);
            let msg = format!(
                "{reason}（连接仅维持 {alive_secs} 秒，连续 {flaps} 次；多为小米运动健康/另一台手机占用）"
            );
            self.retrying.store(false, Ordering::SeqCst);
            self.gave_up.store(true, Ordering::SeqCst);
            self.lost_notified.store(true, Ordering::SeqCst);
            log::warn!("[conn] 短命连接熔断：{addr} {msg}");
            self.emit(
                "wf-link-lost",
                LinkLostPayload {
                    addr: addr.to_string(),
                    reason: msg.clone(),
                    will_retry: false,
                    attempt: 0,
                    flap: true,
                },
            );
            self.emit(
                "wf-link-give-up",
                LinkGiveUpPayload {
                    addr: addr.to_string(),
                    reason: msg,
                    flap: true,
                },
            );
            return;
        }
        if !self.lost_notified.swap(true, Ordering::SeqCst) {
            log::warn!("[conn] 判定掉线（{}）{addr}，开始自动重连", if hard { "硬" } else { "软" });
            self.emit(
                "wf-link-lost",
                LinkLostPayload {
                    addr: addr.to_string(),
                    reason: reason.to_string(),
                    will_retry: true,
                    attempt: 0,
                    flap: false,
                },
            );
        }
        self.start_retry(hard);
    }

    /// 重新武装一轮重连（回前台 / 用户点重连时调）：已连着就是空操作。
    /// 注意这里**不发 wf-link-lost**：界面此刻已经是红的（掉线时发过一次），
    /// 这一轮的第一步是先复核探测 —— 复核成功直接发 restored，不会来回闪。
    ///
    /// `force = false`（前端的自动兜底：60 秒探测失败 / 回前台）在**已放弃**时是空操作 ——
    /// 否则放弃后每 60 秒又重头跑满 8 次退避，买家看到的就是「一直一直在重连」。
    /// `force = true` 只在有证据说明链路可能还活着时用（探测成功但仍显示未连接：
    /// 请总管复核一次），这时允许无视 give-up。
    pub fn relink(self: &Arc<Self>, force: bool) {
        if self.desired.lock().is_none() {
            return;
        }
        if self.connected.load(Ordering::SeqCst) {
            return;
        }
        if self.gave_up.load(Ordering::SeqCst) && !force {
            log::info!("[conn] 已放弃自动重连，等用户手动重连（忽略本次自动兜底）");
            return;
        }
        self.gave_up.store(false, Ordering::SeqCst);
        self.start_retry(false);
    }

    fn start_retry(self: &Arc<Self>, hard: bool) {
        if self.retrying.swap(true, Ordering::SeqCst) {
            return; // 已经有一轮在跑
        }
        let sup = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            sup.retry_loop(hard).await;
        });
    }

    async fn retry_loop(self: Arc<Self>, hard: bool) {
        let gen_id = self.generation.load(Ordering::SeqCst);
        let mut last_err = String::new();

        // 软化掉线先复核一次：真的还活着就直接恢复，别把好链路拆了重连
        if !hard {
            let snapshot = self.desired.lock().clone();
            if let Some(d) = snapshot {
                let verified = matches!(
                    tokio::time::timeout(VERIFY_TIMEOUT, bridge::probe_device_status(&d.addr)).await,
                    Ok(Ok(_))
                );
                if verified {
                    log::info!("[conn] 复核探测成功，链路仍在 → 不重连");
                    self.connected.store(true, Ordering::SeqCst);
                    self.probe_failures.store(0, Ordering::SeqCst);
                    self.lost_notified.store(false, Ordering::SeqCst);
                    self.retrying.store(false, Ordering::SeqCst);
                    self.emit("wf-link-restored", LinkRestoredPayload { addr: d.addr, name: d.name });
                    return;
                }
            }
        }

        for (i, delay) in BACKOFF_SECS.iter().enumerate() {
            let attempt = (i + 1) as u32;
            if self.generation.load(Ordering::SeqCst) != gen_id {
                log::info!("[conn] 期望连接已变化，自动重连作废");
                self.retrying.store(false, Ordering::SeqCst);
                return;
            }
            let Some(d) = self.desired.lock().clone() else {
                self.retrying.store(false, Ordering::SeqCst);
                return;
            };
            // 短命掉线后的第一次重连先解配对：买家实测连上 6 秒就被手环掐断，
            // 多半是小米运动健康/另一台手机占着；解除系统配对能把占用方顶掉。
            // 同一轮里的后续重试改回保留配对，避免每轮都 removeBond 互相踩绑定。
            let unpair_first = attempt == 1 && self.flap_count.load(Ordering::SeqCst) > 0;
            self.emit(
                "wf-link-retry",
                LinkRetryPayload {
                    addr: d.addr.clone(),
                    attempt,
                    delay_ms: delay * 1000,
                    unpair: unpair_first,
                },
            );
            log::info!(
                "[conn] 第 {attempt} 次重连将在 {delay}s 后尝试（{}，unpair={unpair_first}）",
                d.addr
            );
            tokio::time::sleep(Duration::from_secs(*delay)).await;

            // 长任务收尾期间不动 socket（最多等 BUSY_WAIT_LIMIT）
            let wait_start = std::time::Instant::now();
            while self.busy.load(Ordering::SeqCst) > 0 {
                if self.generation.load(Ordering::SeqCst) != gen_id || wait_start.elapsed() > BUSY_WAIT_LIMIT {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if self.generation.load(Ordering::SeqCst) != gen_id {
                self.retrying.store(false, Ordering::SeqCst);
                return;
            }

            let bond = if unpair_first {
                bridge::BondPolicy::UnpairFirst
            } else {
                bridge::BondPolicy::Keep
            };
            match bridge::connect_inner(
                self.core.clone(),
                d.name.clone(),
                d.addr.clone(),
                d.authkey.clone(),
                bond,
            )
            .await
            {
                Ok(_) => {
                    if self.generation.load(Ordering::SeqCst) != gen_id {
                        // 用户在这轮重连跑到一半时主动断开/切了设备：这条刚建立的连接不属于任何人，
                        // 拆掉它（否则会出现「界面显示未连接、其实 socket 还活着」的脏状态）
                        log::warn!("[conn] 重连成功但用户已不要这条连接，拆掉 {}", d.addr);
                        bridge::teardown_device(&d.addr).await;
                        self.retrying.store(false, Ordering::SeqCst);
                        return;
                    }
                    log::info!("[conn] 自动重连成功（第 {attempt} 次）{}", d.addr);
                    self.connected.store(true, Ordering::SeqCst);
                    *self.connected_at.lock() = Some(Instant::now());
                    self.retrying.store(false, Ordering::SeqCst);
                    self.gave_up.store(false, Ordering::SeqCst);
                    self.lost_notified.store(false, Ordering::SeqCst);
                    self.probe_failures.store(0, Ordering::SeqCst);
                    self.start_keepalive(&d.name, &d.addr);
                    self.emit(
                        "wf-link-restored",
                        LinkRestoredPayload {
                            addr: d.addr,
                            name: d.name,
                        },
                    );
                    return;
                }
                Err(e) => {
                    log::warn!("[conn] 第 {attempt} 次重连失败: {e}");
                    last_err = e;
                }
            }
        }

        self.retrying.store(false, Ordering::SeqCst);
        if self.generation.load(Ordering::SeqCst) != gen_id {
            return;
        }
        self.gave_up.store(true, Ordering::SeqCst);
        let addr = self.desired.lock().as_ref().map(|d| d.addr.clone()).unwrap_or_default();
        log::warn!("[conn] 自动重连放弃（{addr}）：{last_err}");
        self.emit(
            "wf-link-give-up",
            LinkGiveUpPayload {
                addr,
                reason: last_err,
                flap: false,
            },
        );
    }

    /// 心跳一轮：正连着、不忙，就查一次设备状态
    async fn tick(self: &Arc<Self>) {
        if self.busy.load(Ordering::SeqCst) > 0 {
            return;
        }
        let Some(d) = self.desired.lock().clone() else {
            return;
        };
        if !self.connected.load(Ordering::SeqCst) {
            return; // 掉线中：重连流程在管
        }
        // 通知权限可能是服务起来之后才允许的（首次连接才弹框）→ 补发，让保活通知显示出来
        if !self.notif_ok.load(Ordering::SeqCst)
            && self.notif_refresh.fetch_add(1, Ordering::SeqCst) < NOTIF_REFRESH_LIMIT
        {
            self.start_keepalive(&d.name, &d.addr);
        }
        match tokio::time::timeout(PROBE_TIMEOUT, bridge::probe_device_status(&d.addr)).await {
            Ok(Ok(battery)) => {
                self.probe_failures.store(0, Ordering::SeqCst);
                self.emit(
                    "wf-device-status",
                    DeviceStatusPayload {
                        addr: d.addr,
                        battery,
                    },
                );
            }
            Ok(Err(e)) => {
                let n = self.probe_failures.fetch_add(1, Ordering::SeqCst) + 1;
                log::warn!("[conn] 心跳 #{n} 失败: {e}");
                if n >= FAILS_BEFORE_LOST {
                    self.note_lost(&d.addr, "心跳连续失败", false);
                }
            }
            Err(_) => {
                let n = self.probe_failures.fetch_add(1, Ordering::SeqCst) + 1;
                log::warn!("[conn] 心跳 #{n} 超时");
                if n >= FAILS_BEFORE_LOST {
                    self.note_lost(&d.addr, "心跳连续超时", false);
                }
            }
        }
    }

    /// 最近是否处于「连上几秒就被掐」的短命掉线状态（flap_count 只在长连接掉线 /
    /// 主动断开 / 新一轮手动连接成功时清零）。手动连接据此决定要不要先解配对。
    pub fn in_flap_episode(&self) -> bool {
        self.flap_count.load(Ordering::SeqCst) > 0
    }

    pub fn state(&self) -> LinkState {
        let d = self.desired.lock().clone();
        LinkState {
            addr: d.as_ref().map(|x| x.addr.clone()),
            name: d.as_ref().map(|x| x.name.clone()),
            connected: self.connected.load(Ordering::SeqCst),
            retrying: self.retrying.load(Ordering::SeqCst),
            gave_up: self.gave_up.load(Ordering::SeqCst),
            busy: self.busy.load(Ordering::SeqCst) > 0,
        }
    }
}

/// 取总管（未初始化返回 None；setup 里初始化一次）
pub fn get() -> Option<Arc<Supervisor>> {
    SUPERVISOR.get().cloned()
}

/// 长任务标记：安装 / 应用编辑开始时取，结束自动放
pub fn busy_guard() -> Option<BusyGuard> {
    get().map(|s| {
        s.busy.fetch_add(1, Ordering::SeqCst);
        BusyGuard(s)
    })
}

/// 初始化：登记全局总管并启动心跳循环（setup 里调一次）
pub fn init(app: AppHandle, core: Handle) {
    let sup = Arc::new(Supervisor {
        app,
        core,
        desired: Mutex::new(None),
        generation: AtomicU64::new(0),
        connected: AtomicBool::new(false),
        retrying: AtomicBool::new(false),
        gave_up: AtomicBool::new(false),
        lost_notified: AtomicBool::new(false),
        busy: AtomicUsize::new(0),
        connected_at: Mutex::new(None),
        flap_count: AtomicU32::new(0),
        probe_failures: AtomicU32::new(0),
        notif_ok: AtomicBool::new(false),
        notif_refresh: AtomicU32::new(0),
    });
    if SUPERVISOR.set(Arc::clone(&sup)).is_err() {
        return; // 已经初始化过
    }
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(HEARTBEAT).await;
            sup.tick().await;
        }
    });
    log::info!("[conn] 连接总管就绪（心跳 {}s / 退避重连 / 前台服务保活）", HEARTBEAT.as_secs());
}
