//! 壁纸方案 —— 照片表盘的「壁纸生成器」配置与素材。
//!
//! 一句话分工：**配置存库、素材落盘、App 拉下来自己长界面**（详见 `docs/方案-壁纸生成器.md`）。
//! - `editor_templates`：一个 watchface_id 一条方案（画布尺寸 / 每张照片的基准与可调范围 / 相框窗口与多色号素材）
//! - `editor_assets`：素材登记（文件名 + sha256 + 大小）；文件落 `<data_dir>/editor-assets/<watchface_id>/`
//! - App：`GET /v1/editor-template?watchface_id=` 拿配置与素材清单 → 按需 `GET /wp/<watchface_id>/<file>`
//!
//! **安全约束（务必保持）**：
//! 1. 素材路径只接受**相对文件名**（白名单字符、拒绝目录分隔与 `..`、限图片扩展名）；
//! 2. 配置里任何字符串**不许出现 `://`** —— 否则等于给了"让客户端去抓任意外链"的口子；
//! 3. 服务端自身**不做任何按 URL 抓取**；将来若要做，必须先校验仅 http/https、
//!    并**按 DNS 解析结果**拒绝 loopback / 私有 / 保留地址（防 DNS rebinding）。
//!
//! 这套约束是需求带的安全条件（配置与素材由后台写入、被所有 App 消费），不要为了省事放开。

use serde_json::Value;
use std::path::PathBuf;

use crate::state::{ApiError, ApiResult, AppState};

/// App 上看到的配置版本；形状变了就 +1，App 侧按它决定怎么解析。
pub const CONFIG_VERSION: i64 = 1;

/// 素材根目录
pub fn assets_root(state: &AppState) -> PathBuf {
    state.data_dir.join("editor-assets")
}

/// 单个方案的素材目录
pub fn assets_dir(state: &AppState, watchface_id: &str) -> PathBuf {
    assets_root(state).join(watchface_id)
}

/// watchface_id 必须是纯数字：它同时被当作素材目录名，绝不能带路径成分
pub fn safe_watchface_id(id: &str) -> ApiResult<String> {
    let id = id.trim();
    if id.len() < 6 || id.len() > 24 || !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(ApiError::bad_request("watchface_id 不合法（应为一串数字）"));
    }
    Ok(id.to_string())
}

/// 素材文件名白名单：只允许相对名（无目录、无 `..`、非隐藏），且必须是图片
pub fn safe_asset_name(name: &str) -> ApiResult<String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 64 {
        return Err(ApiError::bad_request(format!("素材文件名不合法：{name}")));
    }
    if n.contains('/') || n.contains('\\') || n.contains("..") || n.starts_with('.') {
        return Err(ApiError::bad_request(format!(
            "素材文件名不允许目录或 ..：{name}"
        )));
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(ApiError::bad_request(format!(
            "素材文件名只能用字母数字与 . _ -：{name}"
        )));
    }
    let ext = n.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        return Err(ApiError::bad_request(format!(
            "素材只允许图片（png/jpg/webp）：{name}"
        )));
    }
    Ok(n.to_string())
}

// ---------------------------------------------------------------- 配置校验

fn num(v: Option<&Value>, what: &str) -> ApiResult<f64> {
    v.and_then(|x| x.as_f64())
        .ok_or_else(|| ApiError::bad_request(format!("{what} 要是数字")))
}

/// 一个"可调量"：数字/布尔 = 固定值（界面上不出现）；对象 = {default,min?,max?,step?,adjustable?}
fn check_control(v: &Value, what: &str) -> ApiResult<()> {
    if v.is_number() || v.is_boolean() {
        return Ok(());
    }
    let o = v.as_object().ok_or_else(|| {
        ApiError::bad_request(format!(
            "{what} 要是数字/布尔，或 {{default,min,max,step,adjustable}}"
        ))
    })?;
    match o.get("default") {
        Some(d) if d.is_number() || d.is_boolean() => {}
        _ => return Err(ApiError::bad_request(format!("{what}.default 缺失或类型不对"))),
    }
    for k in ["min", "max", "step"] {
        if let Some(x) = o.get(k) {
            if !x.is_number() {
                return Err(ApiError::bad_request(format!("{what}.{k} 要是数字")));
            }
        }
    }
    if let Some(a) = o.get("adjustable") {
        if !a.is_boolean() {
            return Err(ApiError::bad_request(format!("{what}.adjustable 要是布尔")));
        }
    }
    let mn = o.get("min").and_then(|x| x.as_f64());
    let mx = o.get("max").and_then(|x| x.as_f64());
    if let (Some(a), Some(b)) = (mn, mx) {
        if a > b {
            return Err(ApiError::bad_request(format!("{what}: min 比 max 大")));
        }
    }
    Ok(())
}

/// 扫一遍配置里的所有字符串：不许出现 `://`（防止外链塞进来让客户端去抓）
fn scan_no_url(v: &Value, path: &str) -> ApiResult<()> {
    match v {
        Value::String(s) => {
            if s.contains("://") {
                return Err(ApiError::bad_request(format!(
                    "{path} 里不许出现网址（{s}）：素材只用相对文件名"
                )));
            }
            Ok(())
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                scan_no_url(x, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        Value::Object(o) => {
            for (k, x) in o {
                scan_no_url(x, &format!("{path}.{k}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// 校验一份壁纸方案配置，返回它引用到的素材文件名（供保存后核对"图传齐了没"）
pub fn validate_config(cfg: &Value) -> ApiResult<Vec<String>> {
    scan_no_url(cfg, "config")?;

    let o = cfg
        .as_object()
        .ok_or_else(|| ApiError::bad_request("config 必须是一个对象"))?;

    let ver = o.get("version").and_then(|v| v.as_i64()).unwrap_or(-1);
    if ver != CONFIG_VERSION {
        return Err(ApiError::bad_request(format!(
            "config.version 必须是 {CONFIG_VERSION}（收到 {ver}）"
        )));
    }

    let canvas = o
        .get("canvas")
        .and_then(|v| v.as_object())
        .ok_or_else(|| ApiError::bad_request("缺 canvas"))?;
    let cw = num(canvas.get("w"), "canvas.w")?;
    let ch = num(canvas.get("h"), "canvas.h")?;
    if !(16.0..=4096.0).contains(&cw) || !(16.0..=4096.0).contains(&ch) {
        return Err(ApiError::bad_request("canvas 尺寸超出合理范围"));
    }

    let photos = o
        .get("photos")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::bad_request("缺 photos"))?;
    if photos.is_empty() || photos.len() > 4 {
        return Err(ApiError::bad_request("photos 要有 1~4 张（一般 1 张；拍立得那种 2 张）"));
    }

    // 有没有 frame：有 frame 才允许 photos 里出现 base = "frame"
    let frame = o.get("frame").and_then(|v| v.as_object());
    let mut refs: Vec<String> = Vec::new();
    if let Some(f) = frame {
        // window 是可选的：只有「有照片要放进画框」才需要它
        // （Edge-UI 那种单图 + 花边模板就只有 colors，没有窗口）
        if let Some(win) = f.get("window").and_then(|v| v.as_object()) {
            for k in ["x", "y", "w", "h"] {
                num(win.get(k), &format!("frame.window.{k}"))?;
            }
            for k in ["radius", "rot"] {
                if win.get(k).is_some() {
                    num(win.get(k), &format!("frame.window.{k}"))?;
                }
            }
        }
        if let Some(c) = f.get("colors").and_then(|v| v.as_array()) {
            for (i, item) in c.iter().enumerate() {
                let io = item
                    .as_object()
                    .ok_or_else(|| ApiError::bad_request(format!("frame.colors[{i}] 要是对象")))?;
                let src = io
                    .get("src")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ApiError::bad_request(format!("frame.colors[{i}].src 缺失")))?;
                refs.push(safe_asset_name(src)?);
            }
        }
    }

    for (i, p) in photos.iter().enumerate() {
        let po = p
            .as_object()
            .ok_or_else(|| ApiError::bad_request(format!("photos[{i}] 要是对象")))?;
        if po.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
            return Err(ApiError::bad_request(format!("photos[{i}].id 缺失")));
        }
        let base = po.get("base").and_then(|v| v.as_str()).unwrap_or("canvas");
        match base {
            "canvas" => {}
            "frame" => {
                let has_win = frame
                    .map(|f| f.get("window").and_then(|v| v.as_object()).is_some())
                    .unwrap_or(false);
                if !has_win {
                    return Err(ApiError::bad_request(format!(
                        "photos[{i}] 说 base=frame，但配置里没写 frame.window（相框窗口）"
                    )));
                }
            }
            other => {
                return Err(ApiError::bad_request(format!(
                    "photos[{i}].base 只能是 canvas 或 frame（收到 {other}）"
                )))
            }
        }
        let controls = po
            .get("controls")
            .and_then(|v| v.as_object())
            .ok_or_else(|| ApiError::bad_request(format!("photos[{i}].controls 缺失")))?;
        for (k, v) in controls {
            check_control(v, &format!("photos[{i}].controls.{k}"))?;
        }
    }

    refs.sort();
    refs.dedup();
    Ok(refs)
}
