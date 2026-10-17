//! 照片表盘「壁纸生成器」的服务端对接：取方案 + 取素材（校验指纹）+ 按方案合成。
//!
//! 方案与素材都由服务端下发（`GET /v1/editor-template` 与 `GET /wp/<表盘id>/<文件名>`），
//! 所以**上新表盘/新模板不用发 App 版本**。素材不加密（它本来就不是付费内容），
//! 但路径只认「一个纯文件名」——配置里出现 URL / 分隔符 / 点点一律拒绝。
//!
//! 合成在 Rust（`wallpaper_gen`），预览与最终出图走同一条路：所见即手环上所得。

use anyhow::{anyhow, bail, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::bridge::AppState;
use crate::{paid, wallpaper_gen as wgen};

/// 素材缓存目录：`<应用数据>/paid/editor/<表盘id>/`
fn editor_dir(state: &AppState, wfid: &str) -> Result<PathBuf> {
    let dir = state
        .app_data_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir);
    let p = paid::paid_dir(&dir).join("editor").join(safe_id(wfid)?);
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

/// 表盘 id 只认 6~24 位数字（同时当目录名用）
fn safe_id(s: &str) -> Result<String> {
    let s = s.trim();
    if s.len() < 6 || s.len() > 24 || !s.chars().all(|c| c.is_ascii_digit()) {
        bail!("表盘 id 不合法");
    }
    Ok(s.to_string())
}

/// 素材名只认「一个纯文件名」（不许有路径分隔符、不许是 . 或 ..）
fn safe_asset(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() || n == "." || n == ".." || n.contains('/') || n.contains('\\') || n.contains('\0') {
        bail!("素材名不合法: {name}");
    }
    Ok(n.to_string())
}

fn decode_b64(data_b64: &str) -> Result<Vec<u8>> {
    let cleaned: String = data_b64
        .rsplit_once(',')
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_else(|| data_b64.to_string());
    Ok(base64::engine::general_purpose::STANDARD.decode(cleaned.trim())?)
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("Panfang/0.3 (watchface-installer)")
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

/// 当前授权服务地址 + 登录令牌（拿不到令牌也能取方案：素材是公开的）
fn base_and_token(state: &AppState) -> Result<(String, Option<String>)> {
    let dir = state
        .app_data_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir);
    let dir = paid::paid_dir(&dir);
    let m = paid::ctx(&dir);
    let g = m.lock();
    let c = g.as_ref().ok_or_else(|| anyhow!("付费模块未初始化"))?;
    let token = if c.session.as_ref().map(|s| s.license_token.is_empty()).unwrap_or(true) {
        None
    } else {
        Some(c.session.as_ref().unwrap().license_token.clone())
    };
    Ok((c.base.clone(), token))
}

/// 取方案 + 补齐素材。
///
/// 返回：
/// ```jsonc
/// { "enabled": true, "watchfaceId": "156700067433", "config": {...},
///   "offline": false,        // true = 用的本机缓存（断网/服务端不可达）
///   "missing": ["1.png"] }   // 没下齐的素材（App 侧提示「先联网下载一次」）
/// ```
/// 拿不到方案时返回 `{ "enabled": false, "code": "…", "reason": "…" }`，**不抛错**：
/// - `code = "none"`     = 服务端没有这个表盘的方案（App 用本地通用方案：只放照片、不套模板）
/// - `code = "network"`  = 服务端不可达且本机没有缓存（App 提示先联网打开一次）
/// - `code = "mismatch"` = 有方案但画布尺寸与这张表盘不一致（不许套用）
/// - `code = "bad"`      = 表盘 id / 方案内容本身有问题
/// 老客户端只认 `enabled`，多出来的字段不影响它。
#[tauri::command]
pub async fn watchface_generator_plan(
    state: tauri::State<'_, AppState>,
    watchface_id: String,
    canvas_w: Option<u32>,
    canvas_h: Option<u32>,
) -> Result<Value, String> {
    let wfid = match safe_id(&watchface_id) {
        Ok(v) => v,
        Err(e) => {
            return Ok(json!({ "enabled": false, "code": "bad", "reason": e.to_string() }))
        }
    };
    let cache = match editor_dir(&state, &wfid) {
        Ok(d) => d,
        Err(e) => {
            return Ok(json!({ "enabled": false, "code": "bad", "reason": e.to_string() }))
        }
    };
    let plan_file = cache.join("plan.json");

    // 1) 拉线上方案；拉不到就用本机缓存（断网也能继续做壁纸）
    let (base, token) = base_and_token(&state).map_err(|e| e.to_string())?;
    let remote = fetch_plan(&base, token.as_deref(), &wfid).await;
    let (plan, offline) = match remote {
        Ok(p) => {
            let _ = std::fs::write(&plan_file, serde_json::to_vec(&p).unwrap_or_default());
            (p, false)
        }
        Err(e) => match std::fs::read(&plan_file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            Some(p) => {
                log::warn!("取壁纸方案失败（{e}），用本机缓存");
                (p, true)
            }
            None => {
                return Ok(json!({
                    "enabled": false,
                    "code": "network",
                    "reason": format!("取方案失败: {e}")
                }));
            }
        },
    };
    if !plan["enabled"].as_bool().unwrap_or(false) {
        // 服务端明确回答「这个表盘没有方案」：App 走本地通用方案（不套模板）
        return Ok(json!({ "enabled": false, "code": "none", "watchfaceId": wfid }));
    }

    // 2) 方案必须与这张表盘对得上：画布尺寸不一致就直接不给用（9P 的方案不许套到 10）
    let config: wgen::GenConfig = match serde_json::from_value(plan["config"].clone()) {
        Ok(c) => c,
        Err(e) => {
            return Ok(json!({
                "enabled": false,
                "code": "bad",
                "reason": format!("方案读不出来: {e}")
            }));
        }
    };
    if let Err(e) = config.check() {
        return Ok(json!({
            "enabled": false,
            "code": "bad",
            "reason": format!("方案不合法: {e}")
        }));
    }
    if let (Some(w), Some(h)) = (canvas_w, canvas_h) {
        if w != config.canvas.w || h != config.canvas.h {
            return Ok(json!({
                "enabled": false,
                "code": "mismatch",
                "reason": format!("方案对应的是 {}x{}，这张表盘是 {}x{}", config.canvas.w, config.canvas.h, w, h)
            }));
        }
    }

    // 3) 素材：缺的补下来，按服务端给的 sha256 校验
    let mut want: Vec<String> = Vec::new();
    if let Some(f) = &config.frame {
        for c in &f.colors {
            if let Ok(n) = safe_asset(&c.src) {
                if !want.contains(&n) {
                    want.push(n);
                }
            }
        }
    }
    let mut sha_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(arr) = plan["assets"].as_array() {
        for a in arr {
            if let (Some(p), Some(s)) = (a["path"].as_str(), a["sha256"].as_str()) {
                sha_of.insert(p.to_string(), s.to_string());
            }
        }
    }
    let mut missing: Vec<String> = Vec::new();
    for name in &want {
        let path = cache.join(name);
        // 缓存对得上指纹就直接用；服务端换了素材 → 新方案里的 sha 变了 → 这里会重下
        if std::fs::read(&path).map(|b| verify(&b, sha_of.get(name))).unwrap_or(false) {
            continue;
        }
        match fetch_asset(&base, &wfid, name).await {
            Ok(bytes) => {
                if !verify(&bytes, sha_of.get(name)) {
                    missing.push(name.clone());
                    continue;
                }
                if let Err(e) = std::fs::write(&path, &bytes) {
                    log::warn!("素材写盘失败 {name}: {e}");
                    missing.push(name.clone());
                }
            }
            Err(e) => {
                log::warn!("素材取不到 {name}: {e}");
                missing.push(name.clone());
            }
        }
    }

    Ok(json!({
        "enabled": true,
        "watchfaceId": wfid,
        "config": plan["config"],
        "offline": offline,
        "missing": missing,
    }))
}

/// 指纹对不上就换（服务端给了 sha 才校验；没给就信内容）
fn verify(bytes: &[u8], want: Option<&String>) -> bool {
    match want {
        Some(s) if !s.is_empty() => paid::sha256_hex(bytes).eq_ignore_ascii_case(s),
        _ => true,
    }
}

async fn fetch_plan(base: &str, token: Option<&str>, wfid: &str) -> Result<Value> {
    let mut req = http()?
        .get(format!("{}/v1/editor-template", base.trim_end_matches('/')))
        .query(&[("watchface_id", wfid)]);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let r = req.send().await?;
    if !r.status().is_success() {
        bail!("服务端返回 {}", r.status().as_u16());
    }
    Ok(r.json().await?)
}

async fn fetch_asset(base: &str, wfid: &str, name: &str) -> Result<Vec<u8>> {
    let name = safe_asset(name)?;
    let url = format!("{}/wp/{}/{}", base.trim_end_matches('/'), wfid, name);
    let r = http()?.get(&url).send().await?;
    if !r.status().is_success() {
        bail!("素材 {} 返回 {}", name, r.status().as_u16());
    }
    let b = r.bytes().await?;
    // 一张模板图不该有大体积；超了就当异常（防被塞怪东西）
    if b.len() > 8 * 1024 * 1024 {
        bail!("素材 {} 体积异常（{} 字节）", name, b.len());
    }
    Ok(b.to_vec())
}

/// 按方案合成一张成品壁纸（预览与最终出图同一条路）。
///
/// - `config_json`：方案（前端从 plan 拿到的那份）
/// - `params_json`：`GenParams` 数组，与方案里的 photos 一一对应
/// - `photos_b64`：用户照片（同一顺序；缺 = 该层不画）
/// - `color_i`：相框色号（配置里的 `i`；不传 = 第一个）
///
/// 预览与最终出图**同一条路**：前端预览时传的是缩过的照片（取景与最终一致，
/// 因为取景只跟「填满参考框」的倍数有关），点「生成」时传原图。
#[tauri::command]
pub async fn watchface_generator_render(
    state: tauri::State<'_, AppState>,
    watchface_id: String,
    config_json: String,
    params_json: String,
    photos_b64: Vec<String>,
    color_i: Option<i64>,
) -> Result<String, String> {
    let cfg: wgen::GenConfig =
        serde_json::from_str(&config_json).map_err(|e| format!("方案读不出来: {e}"))?;
    cfg.check().map_err(|e| format!("{e:#}"))?;
    let params: Vec<wgen::GenParams> = serde_json::from_str(&params_json).unwrap_or_default();

    // 相框素材（按色号挑；文件名过白名单，只从缓存目录里读）。
    // 没有相框的方案（本地通用方案 / AWOS）不需要表盘 id，也不碰磁盘目录。
    let frame_png: Option<Vec<u8>> = match cfg.color(color_i) {
        Some(c) => {
            let wfid = safe_id(&watchface_id).map_err(|e| e.to_string())?;
            let dir = editor_dir(&state, &wfid).map_err(|e| e.to_string())?;
            let name = safe_asset(&c.src).map_err(|e| e.to_string())?;
            let path = dir.join(&name);
            match std::fs::read(&path) {
                Ok(b) => Some(b),
                Err(_) => {
                    return Err("相框素材还没下载好，请先连网打开一次".into());
                }
            }
        }
        None => None,
    };

    // 照片解码
    let mut photos: Vec<Option<(Vec<u8>, u32, u32)>> = Vec::new();
    for p in &photos_b64 {
        if p.is_empty() {
            photos.push(None);
            continue;
        }
        let raw = decode_b64(p).map_err(|e| format!("照片解码失败: {e}"))?;
        // 前端选图时已缩到 2048；这里再挡一道超大图：解码前先读尺寸，
        // 4000 万像素解码要 ~160MB（还要再拷一份），低内存手机会被系统直接杀掉。
        let dims = image::ImageReader::new(std::io::Cursor::new(&raw))
            .with_guessed_format()
            .map_err(|e| format!("照片读不出来: {e}"))?
            .into_dimensions()
            .map_err(|e| format!("照片解码失败: {e}"))?;
        if dims.0 as u64 * dims.1 as u64 > 24_000_000 {
            return Err("照片太大了，请换一张再试".into());
        }
        let im = image::load_from_memory(&raw).map_err(|e| format!("照片解码失败: {e}"))?;
        let rgba = im.to_rgba8();
        let (w, h) = rgba.dimensions();
        photos.push(Some((rgba.into_raw(), w, h)));
    }
    let frame_dec = match frame_png {
        Some(b) => {
            let im = image::load_from_memory(&b).map_err(|e| format!("相框素材读不出来: {e}"))?;
            let rgba = im.to_rgba8();
            let (w, h) = rgba.dimensions();
            Some((rgba.into_raw(), w, h))
        }
        None => None,
    };

    tauri::async_runtime::spawn_blocking(move || {
        let inputs: Vec<wgen::PhotoInput> = (0..cfg.photo_count())
            .map(|i| wgen::PhotoInput {
                rgba: photos
                    .get(i)
                    .and_then(|p| p.as_ref())
                    .map(|(d, w, h)| (d.as_slice(), *w, *h)),
                params: params.get(i).cloned().unwrap_or_default(),
            })
            .collect();
        let frame_ref = frame_dec.as_ref().map(|(d, w, h)| (d.as_slice(), *w, *h));
        let (rgba, w, h) = wgen::compose(&cfg, &inputs, frame_ref)?;
        crate::watchface_bin::encode_png_pub(&rgba, w, h)
    })
    .await
    .map_err(|e| format!("合成任务失败: {e}"))?
    .map_err(|e| format!("{e:#}"))
}

/// 相框素材（给前端**预览画布**用）：从本机缓存里读，直接回原始 PNG 的 data URL。
///
/// 预览在 WebView 里用 canvas 2D 画（跟网站同一套做法）——滑块和拖动要跟手，
/// 走 Rust 合成再编码过 IPC 太慢。点「生成」那条路仍然走 Rust（成品图尺寸/质量以它为准）。
#[tauri::command]
pub async fn watchface_generator_frame(
    state: tauri::State<'_, AppState>,
    watchface_id: String,
    config_json: String,
    color_i: Option<i64>,
) -> Result<Option<String>, String> {
    let cfg: wgen::GenConfig =
        serde_json::from_str(&config_json).map_err(|e| format!("方案读不出来: {e}"))?;
    // 没有相框色号的方案（本地通用方案 / AWOS）不需要表盘 id，直接回空
    let Some(c) = cfg.color(color_i) else {
        return Ok(None);
    };
    let wfid = safe_id(&watchface_id).map_err(|e| e.to_string())?;
    let dir = editor_dir(&state, &wfid).map_err(|e| e.to_string())?;
    let name = safe_asset(&c.src).map_err(|e| e.to_string())?;
    match std::fs::read(dir.join(&name)) {
        Ok(bytes) => {
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            Ok(Some(format!("data:image/png;base64,{b64}")))
        }
        Err(_) => Ok(None),
    }
}

/// 本机有没有这份方案的缓存（界面判断「要不要先联网下载」用）
#[tauri::command]
pub fn watchface_generator_cached(state: tauri::State<'_, AppState>, watchface_id: String) -> bool {
    let Ok(wfid) = safe_id(&watchface_id) else {
        return false;
    };
    let Ok(dir) = editor_dir(&state, &wfid) else {
        return false;
    };
    dir.join("plan.json").exists()
}

/// 清掉某张表盘的方案缓存（排障用：换方案后强制重取）
#[tauri::command]
pub fn watchface_generator_clear(state: tauri::State<'_, AppState>, watchface_id: String) -> bool {
    let Ok(wfid) = safe_id(&watchface_id) else {
        return false;
    };
    let Ok(dir) = editor_dir(&state, &wfid) else {
        return false;
    };
    std::fs::remove_dir_all(dir).is_ok()
}

/// 素材缓存目录（排障/诊断用）
pub fn cache_root_under(app_data_dir: &Path) -> PathBuf {
    paid::paid_dir(app_data_dir).join("editor")
}
