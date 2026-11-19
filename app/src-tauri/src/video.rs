//! 视频表盘（动态表盘）客户端：
//! - 模板列表 / 任务提交 / 状态轮询 / 成品下载（全部走本机登录态，JS 不接触令牌）
//! - 手机端抽好的 PNG 帧先落到临时目录，提交时原生打成 zip 上传，避免 WebView 里做 zip
//! - 本地留档三件套（视频 / 帧包 / 成品）全部用设备密钥加密（`paid::seal_archive`），
//!   与付费表盘留档同一套 WFENC1 格式，只有本机能解开。
//!
//! 约束：成品 < 25MB、播放时长 = 帧数 / fps ≤ 5 秒（服务端与 worker 都会再校验）。

use base64::Engine as _;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::bridge::AppState;
use crate::paid;

const MAX_FRAMES: usize = 300;
const MAX_TEMP_PNG: usize = 12 * 1024 * 1024;
const MAX_VIDEO: usize = 80 * 1024 * 1024;
const MAX_FRAMES_ZIP: usize = 64 * 1024 * 1024;

fn b64d(s: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(s.trim().as_bytes())
        .map_err(|_| "数据不是合法 base64".to_string())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn app_data(state: &AppState) -> Result<PathBuf, String> {
    state
        .app_data_dir
        .clone()
        .ok_or_else(|| "应用数据目录不可用".to_string())
}

fn safe_id(id: &str) -> Result<(), String> {
    if id.len() < 8
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("项目 ID 非法".into());
    }
    Ok(())
}

/// 词法归一：不碰文件系统。
///
/// 以前这里用 canonicalize(parent)：抽帧目录 `video_tmp/<id>/frames` 的父目录还不存在，
/// canonicalize 失败后退回原路径，再和已解析的 root 比就误判成「路径越界」
/// （2026-11-18 真机：生成按钮永远停在第一步）。
fn clean_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(x) => out.push(x.as_os_str()),
            Component::RootDir => out.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(x) => out.push(x),
        }
    }
    out
}

fn safe_under(root: &Path, p: PathBuf) -> Result<PathBuf, String> {
    let r = clean_path(root);
    let t = clean_path(&p);
    if t != r && !t.starts_with(&r) {
        return Err("路径越界".into());
    }
    Ok(p)
}

fn project_dir(state: &AppState, id: &str) -> Result<PathBuf, String> {
    safe_id(id)?;
    let root = app_data(state)?.join("video_projects");
    std::fs::create_dir_all(&root).map_err(|e| format!("创建项目目录失败: {e}"))?;
    safe_under(&root, root.join(id))
}

fn temp_frames_dir(state: &AppState, id: &str) -> Result<PathBuf, String> {
    safe_id(id)?;
    let root = app_data(state)?.join("video_tmp");
    std::fs::create_dir_all(&root).map_err(|e| format!("创建临时目录失败: {e}"))?;
    safe_under(&root, root.join(id).join("frames"))
}

fn read_meta(dir: &Path) -> Result<Value, String> {
    let raw = std::fs::read(dir.join("meta.json")).map_err(|e| format!("读取项目失败: {e}"))?;
    serde_json::from_slice(&raw).map_err(|e| format!("项目数据损坏: {e}"))
}

fn write_meta(dir: &Path, meta: &Value) -> Result<(), String> {
    let raw = serde_json::to_vec_pretty(meta).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("meta.json"), raw).map_err(|e| format!("保存项目失败: {e}"))
}

fn file_size(p: &Path) -> i64 {
    std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(0)
}

fn summary(dir: &Path, meta: &Value) -> Value {
    let mut v = meta.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("video_size".into(), json!(file_size(&dir.join("video.enc"))));
        obj.insert("frames_size".into(), json!(file_size(&dir.join("frames.enc"))));
        obj.insert("bin_size".into(), json!(file_size(&dir.join("bin.enc"))));
        obj.insert("has_video".into(), json!(dir.join("video.enc").exists()));
        obj.insert("has_frames".into(), json!(dir.join("frames.enc").exists()));
        obj.insert("has_bin".into(), json!(dir.join("bin.enc").exists()));
        // 成品 bin 里官方工具自带的预览图（首帧 + 表盘布局）：历史记录用它当真实缩略图
        if let Ok(png) = std::fs::read(dir.join("preview.png")) {
            obj.insert(
                "preview_b64".into(),
                json!(base64::engine::general_purpose::STANDARD.encode(png)),
            );
        }
    }
    v
}

fn touch(meta: &mut Value) {
    if let Some(obj) = meta.as_object_mut() {
        obj.insert("updated_at".into(), json!(now()));
    }
}

fn png_size(head: &[u8]) -> Option<(u32, u32)> {
    if head.len() < 24 || &head[..8] != b"\x89PNG\r\n\x1a\n" || &head[12..16] != b"IHDR" {
        return None;
    }
    Some((
        u32::from_be_bytes([head[16], head[17], head[18], head[19]]),
        u32::from_be_bytes([head[20], head[21], head[22], head[23]]),
    ))
}

// ---------------------------------------------------------------- HTTP

fn token(state: &AppState) -> Result<String, String> {
    paid::license_token(state)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "请先在账号页绑定爱发电".to_string())
}

async fn json_or_err(resp: reqwest::Response) -> Result<Value, String> {
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = v["message"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&text);
        return Err(msg.to_string());
    }
    Ok(v)
}

/// 帧包/成品都是几十 MB，手机上行慢时 120 秒不够；任务接口单独用长超时。
fn http_long() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Panfang/0.2 (watchface-installer)")
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

async fn get_json(state: &AppState, path: &str) -> Result<Value, String> {
    let base = paid::service_base(state)?;
    let t = token(state)?;
    let resp = paid::http()
        .map_err(|e| e.to_string())?
        .get(format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/')))
        .bearer_auth(t)
        .send()
        .await
        .map_err(|e| format!("网络请求失败: {e}"))?;
    json_or_err(resp).await
}

// ---------------------------------------------------------------- 模板

#[tauri::command]
pub async fn video_templates(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    get_json(&state, "/v1/video/templates").await
}

// ---------------------------------------------------------------- 项目留档

#[tauri::command]
pub async fn video_project_create(
    state: tauri::State<'_, AppState>,
    meta_json: String,
    video_b64: Option<String>,
) -> Result<Value, String> {
    let mut meta: Value =
        serde_json::from_str(&meta_json).map_err(|e| format!("项目参数损坏: {e}"))?;
    let id = meta["id"].as_str().unwrap_or("").to_string();
    let dir = project_dir(&state, &id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建项目失败: {e}"))?;
    if let Some(b) = video_b64.filter(|s| !s.trim().is_empty()) {
        let raw = b64d(&b)?;
        if raw.len() > MAX_VIDEO {
            return Err("视频超过 80MB，请先在相册里裁剪到 5 秒以内再试".into());
        }
        let sealed =
            paid::seal_archive(&paid::paid_dir(&app_data(&state)?), &raw).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("video.enc"), sealed).map_err(|e| format!("保存视频失败: {e}"))?;
        // 换了视频，上一版成品的预览图就不再代表当前项目
        let _ = std::fs::remove_file(dir.join("preview.png"));
        if let Some(obj) = meta.as_object_mut() {
            obj.insert("video_bytes".into(), json!(raw.len()));
        }
    }
    touch(&mut meta);
    write_meta(&dir, &meta)?;
    Ok(summary(&dir, &meta))
}

/// 抽帧临时目录超过 24 小时就当废弃（App 被杀/用户中途退出留下的明文 PNG）。
fn cleanup_temp(state: &AppState) {
    let Ok(root) = app_data(state).map(|d| d.join("video_tmp")) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let cutoff = now() - 86_400;
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() as i64) < cutoff)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

#[tauri::command]
pub async fn video_project_list(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    cleanup_temp(&state);
    let app = app_data(&state)?;
    let paid_dir = paid::paid_dir(&app);
    let root = app.join("video_projects");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let dir = e.path();
            // 旧版本下载的成品没有 preview.png：列表时补一次。
            // 失败写 preview.bad，避免每 4 秒轮询都重复解密 20MB 的成品。
            if !dir.join("preview.png").exists()
                && dir.join("bin.enc").exists()
                && !dir.join("preview.bad").exists()
            {
                let backfill = || -> Result<(), String> {
                    let blob =
                        std::fs::read(dir.join("bin.enc")).map_err(|e| e.to_string())?;
                    let plain = paid::open_archive(&paid_dir, &blob)
                        .map_err(|e| format!("{e:#}"))?;
                    let sum = crate::watchface_bin::inspect(&plain)
                        .map_err(|e| format!("{e:#}"))?;
                    let p = sum.preview.ok_or_else(|| "成品里没有预览图".to_string())?;
                    let png = b64d(&p.png_base64)?;
                    std::fs::write(dir.join("preview.png"), png)
                        .map_err(|e| e.to_string())?;
                    Ok(())
                };
                if backfill().is_err() {
                    let _ = std::fs::write(dir.join("preview.bad"), b"");
                }
            }
            if let Ok(meta) = read_meta(&dir) {
                out.push(summary(&dir, &meta));
            }
        }
    }
    out.sort_by(|a, b| {
        b["updated_at"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&a["updated_at"].as_i64().unwrap_or(0))
    });
    Ok(json!({ "items": out }))
}

#[tauri::command]
pub async fn video_project_get(
    state: tauri::State<'_, AppState>,
    project_id: String,
    kind: String,
) -> Result<Value, String> {
    let dir = project_dir(&state, &project_id)?;
    let file = match kind.as_str() {
        "video" => "video.enc",
        "frames" => "frames.enc",
        "bin" => "bin.enc",
        _ => return Err("未知的留档类型".into()),
    };
    let raw = std::fs::read(dir.join(file)).map_err(|_| "本地没有这份留档".to_string())?;
    let plain = paid::open_archive(&paid::paid_dir(&app_data(&state)?), &raw)
        .map_err(|e| format!("留档解密失败: {e:#}"))?;
    Ok(json!({
        "kind": kind,
        "size": plain.len(),
        "data_b64": base64::engine::general_purpose::STANDARD.encode(plain),
    }))
}

#[tauri::command]
pub async fn video_project_clear(
    state: tauri::State<'_, AppState>,
    project_id: String,
    kinds: Vec<String>,
) -> Result<Value, String> {
    let dir = project_dir(&state, &project_id)?;
    let mut clear_bin = false;
    for k in &kinds {
        let file = match k.as_str() {
            "video" => "video.enc",
            "frames" => "frames.enc",
            "bin" => {
                clear_bin = true;
                "bin.enc"
            }
            _ => continue,
        };
        let _ = std::fs::remove_file(dir.join(file));
    }
    let mut meta = read_meta(&dir)?;
    if clear_bin {
        if let Some(obj) = meta.as_object_mut() {
            obj.insert("job_status".into(), json!("cleared"));
        }
        touch(&mut meta);
        write_meta(&dir, &meta)?;
    }
    Ok(summary(&dir, &meta))
}

#[tauri::command]
pub async fn video_project_delete(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<(), String> {
    let dir = project_dir(&state, &project_id)?;
    std::fs::remove_dir_all(dir).map_err(|e| format!("删除项目失败: {e}"))?;
    if let Ok(tmp) = temp_frames_dir(&state, &project_id) {
        if let Some(parent) = tmp.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn video_project_update_meta(
    state: tauri::State<'_, AppState>,
    project_id: String,
    patch_json: String,
) -> Result<Value, String> {
    let dir = project_dir(&state, &project_id)?;
    let mut meta = read_meta(&dir)?;
    let patch: Value =
        serde_json::from_str(&patch_json).map_err(|e| format!("参数损坏: {e}"))?;
    if let (Some(dst), Some(src)) = (meta.as_object_mut(), patch.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    touch(&mut meta);
    write_meta(&dir, &meta)?;
    Ok(summary(&dir, &meta))
}

// ---------------------------------------------------------------- 抽帧临时目录

#[tauri::command]
pub async fn video_frame_reset(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<(), String> {
    let dir = temp_frames_dir(&state, &project_id)?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时帧目录失败: {e}"))
}

#[tauri::command]
pub async fn video_frame_put(
    state: tauri::State<'_, AppState>,
    project_id: String,
    start_index: usize,
    frames_b64: Vec<String>,
) -> Result<usize, String> {
    let dir = temp_frames_dir(&state, &project_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时帧目录失败: {e}"))?;
    let pdir = project_dir(&state, &project_id)?;
    let meta = read_meta(&pdir)?;
    let cw = meta["canvas_w"].as_u64().unwrap_or(0) as u32;
    let ch = meta["canvas_h"].as_u64().unwrap_or(0) as u32;
    let paid_dir = paid::paid_dir(&app_data(&state)?);
    for (i, s) in frames_b64.iter().enumerate() {
        let idx = start_index + i;
        if idx >= MAX_FRAMES {
            return Err(format!("帧数超过 {MAX_FRAMES} 上限"));
        }
        let bytes = b64d(s)?;
        if bytes.len() > MAX_TEMP_PNG {
            return Err("单帧图片过大".into());
        }
        let (w, h) = png_size(&bytes).ok_or_else(|| "帧不是有效 PNG".to_string())?;
        if cw > 0 && ch > 0 && (w != cw || h != ch) {
            return Err(format!("帧尺寸 {w}×{h} 与模板 {cw}×{ch} 不一致"));
        }
        // 临时帧也不落明文：每帧单独用设备密钥加密，提交时在内存里解开再打 zip。
        let sealed = paid::seal_archive(&paid_dir, &bytes).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(format!("f{idx:03}.png.enc")), sealed)
            .map_err(|e| format!("写入帧失败: {e}"))?;
    }
    let count = std::fs::read_dir(&dir)
        .map(|it| {
            it.flatten()
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with('f') && n.ends_with(".png.enc")
                })
                .count()
        })
        .unwrap_or(0);
    Ok(count)
}

// ---------------------------------------------------------------- 提交任务

#[tauri::command]
pub async fn video_job_create(
    state: tauri::State<'_, AppState>,
    project_id: String,
    style_id: String,
    count: usize,
    fps: f64,
    mode: String,
) -> Result<Value, String> {
    if count == 0 || count > MAX_FRAMES {
        return Err(format!("帧数需在 1~{MAX_FRAMES}"));
    }
    if !(fps >= 1.0 && fps <= 60.0) {
        return Err("帧率需在 1~60".into());
    }
    if count as f64 / fps > 5.001 {
        return Err("视频时长不能超过 5 秒".into());
    }
    let mode = if mode == "once" { "once" } else { "loop" };
    let pdir = project_dir(&state, &project_id)?;
    let mut meta = read_meta(&pdir)?;
    let fdir = temp_frames_dir(&state, &project_id)?;
    let paid_dir = paid::paid_dir(&app_data(&state)?);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&fdir)
        .map_err(|_| "还没抽帧（临时目录不存在）".to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|x| x.to_str())
                .map(|x| x.starts_with('f') && x.ends_with(".png.enc"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    if files.len() < count {
        return Err(format!("抽到的帧不够：需要 {count}，实际 {}", files.len()));
    }

    // 原生打 zip（PNG 已压缩，用 store 模式；不打乱文件名顺序）。
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o644);
    for (i, p) in files.iter().take(count).enumerate() {
        let sealed = std::fs::read(p).map_err(|e| format!("读取帧失败: {e}"))?;
        let bytes = paid::open_archive(&paid_dir, &sealed)
            .map_err(|e| format!("帧解密失败: {e:#}"))?;
        zw.start_file(format!("f{i:03}.png"), opts)
            .map_err(|e| format!("打包帧失败: {e}"))?;
        zw.write_all(&bytes)
            .map_err(|e| format!("打包帧失败: {e}"))?;
    }
    let zip_bytes = zw
        .finish()
        .map_err(|e| format!("打包帧失败: {e}"))?
        .into_inner();
    if zip_bytes.len() > MAX_FRAMES_ZIP {
        return Err(format!(
            "帧包 {:.1}MB，超过 64MB 上限。请减少帧数或缩短视频后重试",
            zip_bytes.len() as f64 / 1048576.0
        ));
    }

    // 帧包留档（密文）先落盘：即使上传失败，用户重试/重新制作也不用再抽一次。
    let sealed_frames =
        paid::seal_archive(&paid::paid_dir(&app_data(&state)?), &zip_bytes).map_err(|e| e.to_string())?;
    std::fs::write(pdir.join("frames.enc"), sealed_frames)
        .map_err(|e| format!("保存帧包留档失败: {e}"))?;

    let base = paid::service_base(&state)?;
    let t = token(&state)?;
    // 成品只给创建任务的这台手机解：公钥随任务落库，服务端下载时用它包裹 CEK。
    let (_, _, device_pub) =
        paid::device_identity(&state).map_err(|e| format!("读取设备密钥失败: {e}"))?;
    let mut form = reqwest::multipart::Form::new()
        .text("style_id", style_id.clone())
        .text("count", count.to_string())
        .text("fps", fps.to_string())
        .text("mode", mode.to_string())
        .text("device_pubkey", device_pub);
    // 同一个作品继续制作：复用原来的表盘 ID，装到表上会顶掉旧版而不是新增一张
    if let Some(wid) = meta["watchface_id"].as_str().filter(|s| !s.trim().is_empty()) {
        form = form.text("watchface_id", wid.trim().to_string());
    }
    let form = form.part(
            "file",
            reqwest::multipart::Part::bytes(zip_bytes)
                .file_name("frames.zip")
                .mime_str("application/zip")
                .map_err(|e| e.to_string())?,
        );
    let resp = http_long()?
        .post(format!("{}/v1/video/jobs", base.trim_end_matches('/')))
        .bearer_auth(t)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("上传帧包失败: {e}"))?;
    let v = json_or_err(resp).await?;
    let job = &v["job"];
    if let Some(obj) = meta.as_object_mut() {
        obj.insert("job_id".into(), job["id"].clone());
        obj.insert("job_status".into(), job["status"].clone());
        obj.insert("watchface_id".into(), job["watchface_id"].clone());
        // 历史成品 ID 全部保留：老版本重做会生成新 ID，旧 ID 可能已经装在表上，
        // 管理页还要靠它把这张表盘认回这个作品。
        if let Some(wid) = job["watchface_id"].as_str() {
            let mut ids = obj
                .get("watchface_ids")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if !ids.iter().any(|v| v.as_str() == Some(wid)) {
                ids.push(json!(wid));
            }
            obj.insert("watchface_ids".into(), json!(ids));
        }
        obj.insert("frame_count".into(), json!(count));
        obj.insert("fps".into(), json!(fps));
        obj.insert("mode".into(), json!(mode));
        obj.insert("style_id".into(), json!(style_id));
    }
    touch(&mut meta);
    write_meta(&pdir, &meta)?;
    // 临时帧已进密文留档，删掉明文临时目录。
    let _ = std::fs::remove_dir_all(&fdir);
    Ok(v)
}

#[tauri::command]
pub async fn video_job_status(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<Value, String> {
    let pdir = project_dir(&state, &project_id)?;
    let mut meta = read_meta(&pdir)?;
    let job_id = meta["job_id"].as_str().unwrap_or("").to_string();
    if job_id.is_empty() {
        return Err("这个项目还没有提交过任务".into());
    }
    let v = match get_json(&state, &format!("/v1/video/jobs/{job_id}")).await {
        Ok(v) => v,
        Err(e) => {
            if e.contains("任务不存在") {
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("job_status".into(), json!("failed"));
                    obj.insert(
                        "job_error".into(),
                        json!("成品已过期（服务端保留 48 小时），请重新生成"),
                    );
                }
                touch(&mut meta);
                write_meta(&pdir, &meta)?;
            }
            return Err(e);
        }
    };
    let job = &v["job"];
    if let Some(obj) = meta.as_object_mut() {
        obj.insert(
            "job_status".into(),
            job["status"].clone(),
        );
        if let Some(sz) = job["result_size"].as_i64() {
            obj.insert("result_size".into(), json!(sz));
        }
        if let Some(err) = job["error"].as_str() {
            obj.insert("job_error".into(), json!(err));
        }
    }
    touch(&mut meta);
    write_meta(&pdir, &meta)?;
    Ok(v)
}

#[tauri::command]
pub async fn video_job_fetch(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<Value, String> {
    let pdir = project_dir(&state, &project_id)?;
    let mut meta = read_meta(&pdir)?;
    let job_id = meta["job_id"].as_str().unwrap_or("").to_string();
    if job_id.is_empty() {
        return Err("这个项目还没有提交过任务".into());
    }
    let base = paid::service_base(&state)?;
    let t = token(&state)?;
    let resp = http_long()?
        .get(format!(
            "{}/v1/video/jobs/{job_id}/bin",
            base.trim_end_matches('/')
        ))
        .bearer_auth(t)
        .send()
        .await
        .map_err(|e| format!("下载成品失败: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let v = json_or_err(resp).await.unwrap_or(Value::Null);
        return Err(v["message"]
            .as_str()
            .unwrap_or("成品还没准备好")
            .to_string());
    }
    // 服务端现在只回「密文 + 设备公钥包裹的 CEK」，私钥不出本机。
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("读取成品响应失败: {e}"))?;
    let cipher = b64d(v["cipher"].as_str().unwrap_or(""))?;
    let nonce = v["file_nonce"].as_str().unwrap_or("");
    let (secret, _, _) =
        paid::device_identity(&state).map_err(|e| format!("读取设备密钥失败: {e}"))?;
    let bytes = paid::unwrap_and_decrypt(&secret, &v["key_wrap"], nonce, &cipher)
        .map_err(|e| format!("解密成品失败: {e:#}"))?;
    if bytes.len() > 25 * 1024 * 1024 {
        return Err("成品超过 25MB，已拒绝保存".into());
    }
    if let Some(want) = v["sha256"].as_str().filter(|s| !s.is_empty()) {
        if paid::sha256_hex(&bytes) != want {
            return Err("成品校验失败（内容与服务端不一致）".into());
        }
    }
    // bin 自带的预览图先当兜底；服务端合成的「首帧 + 模板前景」优先覆盖它。
    if !pdir.join("preview.png").exists() {
        if let Ok(sum) = crate::watchface_bin::inspect(&bytes) {
            if let Some(p) = sum.preview {
                if let Ok(png) = b64d(&p.png_base64) {
                    let _ = std::fs::write(pdir.join("preview.png"), png);
                }
            }
        }
    }
    if let Some(pb) = v["preview_b64"].as_str().filter(|s| !s.trim().is_empty()) {
        if let Ok(png) = b64d(pb) {
            let _ = std::fs::write(pdir.join("preview.png"), png);
        }
    }
    let sealed =
        paid::seal_archive(&paid::paid_dir(&app_data(&state)?), &bytes).map_err(|e| e.to_string())?;
    std::fs::write(pdir.join("bin.enc"), sealed).map_err(|e| format!("保存成品失败: {e}"))?;
    if let Some(obj) = meta.as_object_mut() {
        obj.insert("job_status".into(), json!("done"));
        obj.insert("result_size".into(), json!(bytes.len()));
    }
    touch(&mut meta);
    write_meta(&pdir, &meta)?;
    Ok(json!({ "ok": true, "size": bytes.len() }))
}

#[tauri::command]
pub async fn video_job_delete(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<Value, String> {
    let pdir = project_dir(&state, &project_id)?;
    let mut meta = read_meta(&pdir)?;
    let job_id = meta["job_id"].as_str().unwrap_or("").to_string();
    if !job_id.is_empty() {
        let base = paid::service_base(&state)?;
        let t = token(&state)?;
        let _ = paid::http()
            .map_err(|e| e.to_string())?
            .delete(format!(
                "{}/v1/video/jobs/{job_id}",
                base.trim_end_matches('/')
            ))
            .bearer_auth(t)
            .send()
            .await;
    }
    if let Some(obj) = meta.as_object_mut() {
        obj.insert("job_status".into(), json!("deleted"));
    }
    touch(&mut meta);
    write_meta(&pdir, &meta)?;
    Ok(json!({ "ok": true }))
}
