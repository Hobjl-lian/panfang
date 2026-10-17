//! 失败打点：把「哪一步失败了、什么码、什么机型」送到自己的服务器。
//!
//! 为什么要有它：管理页原来只看得到**成功**（激活数、下载数、订单），买家装不上时
//! 卖家这边什么都收不到——那是整条付费链路上唯一完全瞎的一环。
//!
//! 边界（与既定原则一致）：
//!   · 界面上仍然只给一句操作指引，技术细节进 console；这里只是把同一份细节**也**送给开发者；
//!   · 上报绝不能变成新的失败点：节流、短超时、失败静默丢弃、不重试、不冒泡；
//!   · 不采集与排障无关的东西（没有账号密码、没有表盘内容、没有照片）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// 「反馈」页要复制给开发者的最近几条
const RECENT_MAX: usize = 30;
/// 同一 (step, code) 的最小上报间隔（秒）：重试循环不会灌爆服务器
const THROTTLE_SECS: i64 = 30;

static DIR: OnceLock<PathBuf> = OnceLock::new();
static RECENT: OnceLock<Mutex<Vec<Value>>> = OnceLock::new();
static LAST_SENT: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
/// 本机安装标识：App 第一次启动时随机生成后落盘，之后每次上报都带。
/// 用途只有一个 —— 后台把「同一台手机的一次安装」和别的手机分开，不至于把两个人看成一个人。
static DEVICE_FP: OnceLock<String> = OnceLock::new();
/// 手机厂商/型号与系统（启动时由 connkeep 插件从 Build 里取；取不到就留空）。
static PHONE: OnceLock<(String, String)> = OnceLock::new();

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 启动时登记数据目录（命令行诊断信息也在付费模块的 ctx 里取 base/token）。
///
/// 顺带准备「本机安装标识」：已有就复用，没有就随机生成后落盘。
/// 只用于后台区分不同手机，不含任何账号信息；重装 App 会变成新标识（这是预期）。
pub fn init(dir: PathBuf) {
    let path = dir.join("diag_device_fp");
    let fp = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let generated = format!("{:016x}", rand::random::<u64>());
            let _ = std::fs::write(&path, &generated);
            generated
        });
    let _ = DEVICE_FP.set(fp);
    let _ = DIR.set(dir);
}

/// 启动时登记手机型号/系统（安卓由 connkeep 插件读 Build.*；桌面不调）。
pub fn set_phone_info(model: &str, os: &str) {
    let _ = PHONE.set((model.to_string(), os.to_string()));
}

fn recent_buf() -> &'static Mutex<Vec<Value>> {
    RECENT.get_or_init(|| Mutex::new(Vec::new()))
}

/// 最近几条打点（新的在前）——「反馈」页原样复制给开发者。
pub fn recent() -> Vec<Value> {
    recent_buf().lock().map(|v| v.clone()).unwrap_or_default()
}

/// 上报一次失败（界面上那句「操作指引」之外的细节走这里）。
pub fn report(step: &str, code: &str, message: &str, device_model: Option<&str>) {
    report_kind("fail", step, code, message, device_model)
}

/// 上报一条信息（不是失败，但值得知道，如「留档已升级为密文」）。
pub fn info(step: &str, code: &str, message: &str, device_model: Option<&str>) {
    report_kind("info", step, code, message, device_model)
}

/// 上报前把可能混进来的密钥长串洗掉（≥32 位连续 [A-Za-z0-9_-] → [已隐藏]）。
/// 服务端入库前还会再洗一遍（那道对所有旧版本都生效）；这里先洗，是为了本地 logcat
/// 与「反馈」页复制出来的文本里也不留令牌。2026-10-15 加。
fn scrub_secretish(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            run.push(ch);
            continue;
        }
        if run.len() >= 32 {
            out.push_str("[已隐藏]");
        } else {
            out.push_str(&run);
        }
        run.clear();
        out.push(ch);
    }
    if run.len() >= 32 {
        out.push_str("[已隐藏]");
    } else {
        out.push_str(&run);
    }
    out
}

pub fn report_kind(kind: &str, step: &str, code: &str, message: &str, device_model: Option<&str>) {
    let ts = unix_now();
    let scrubbed = scrub_secretish(message);
    let message: &str = &scrubbed;
    // 1) console 永远有（logcat / app.log 是排障第一现场）
    log::warn!("[diag] {step} {code}: {message}");
    // 2) 内存里留一份（反馈页复制用；不落盘，重启即清空）
    let entry = json!({
        "ts": ts,
        "kind": kind,
        "step": step,
        "code": code,
        "message": message,
        "device_model": device_model,
    });
    if let Ok(mut buf) = recent_buf().lock() {
        buf.insert(0, entry);
        buf.truncate(RECENT_MAX);
    }
    // 3) 节流后发给服务器
    if !throttled(ts, step, code) {
        return;
    }
    let Some((base, token)) = transport() else { return };
    // 「是谁」：设备指纹 + 手机型号/系统。第一次连不上时 device_model 还是空的，
    // 以前后台只能靠 IP 猜人（CGNAT 下两个买家会同 IP）——现在至少分得开不同手机。
    let device_fp = DEVICE_FP.get().map(|s| s.as_str());
    let (phone_model, phone_os) = PHONE
        .get()
        .map(|(m, o)| (Some(m.as_str()), Some(o.as_str())))
        .unwrap_or((None, None));
    let body = json!({ "events": [ {
        "kind": kind,
        "step": step,
        "code": code,
        "message": message,
        "device_model": device_model,
        "device_fp": device_fp,
        "phone_model": phone_model,
        "phone_os": phone_os,
        "app_version": env!("CARGO_PKG_VERSION"),
    } ] })
    .to_string();

    // 后台发，绝不阻塞调用方（连手环的操作都在这条路径上，慢一点都不行）
    tauri::async_runtime::spawn(async move {
        let client = match reqwest::Client::builder()
            .user_agent(concat!("Panfang/", env!("CARGO_PKG_VERSION"), " (watchface-installer)"))
            .timeout(std::time::Duration::from_secs(6))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut req = client
            .post(format!("{}/v1/events", base.trim_end_matches('/')))
            .header("content-type", "application/json")
            .body(body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let _ = req.send().await; // 失败就算了：上报本身不该有任何存在感
    });
}

/// 同一 (step, code) 在节流窗口内只上报一次；顺带清掉过期条目，避免 map 无限增长。
fn throttled(ts: i64, step: &str, code: &str) -> bool {
    let key = format!("{step}|{code}");
    let m = LAST_SENT.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut g) = m.lock() else { return true };
    if let Some(last) = g.get(&key) {
        if ts - *last < THROTTLE_SECS {
            return false;
        }
    }
    if g.len() > 200 {
        g.retain(|_, v| ts - *v < THROTTLE_SECS);
    }
    g.insert(key, ts);
    true
}

/// 从付费模块的上下文里取「服务器地址 + 登录态」（没登录也要能上报）。
///
/// 注意：`paid::ctx` 收的是**付费目录**（`paid_dir`），它与付费模块共用同一个进程级上下文；
/// 少传一层 `paid/` 会让谁先调用谁定音、留档与登录态全解不开（2026-10-08 真机踩过）。
fn transport() -> Option<(String, Option<String>)> {
    let dir = DIR.get()?;
    let m = crate::paid::ctx(&crate::paid::paid_dir(dir));
    let g = m.lock(); // 付费模块用的是 parking_lot（无 poisoning），直接拿 guard
    let c = g.as_ref()?;
    let token = c.token().map(|s| s.to_string());
    Some((c.base.clone(), token))
}
