//! App 自身更新（安卓自更新通道）：检查 → 下载（校验 sha256）→ 调系统安装器。
//!
//! 为什么不走 Tauri 官方 updater：它的平台表里 **Android = ✗**（Play 政策禁止自更新），
//! 安卓只能自研「下载 APK + 调系统安装器」这两步；调安装器必须过 Kotlin，见
//! `plugins/apkinstaller`（未知来源授权 + FileProvider + ACTION_VIEW）。
//!
//! 服务端侧：`GET /v1/app/latest` 给版本信息，`/app/<file>` 直发 APK，
//! `/app/latest.apk` 是永久链接（爱发电商品详情里贴的那条）。
//!
//! 界面按顺序调三个命令：`app_update_check` → `app_update_download` → `app_update_install`。

use serde_json::{json, Value};
use tauri::{Emitter, Manager};

use crate::bridge::AppState;

/// 本机版本（版本号三处同步：tauri.conf.json / Cargo.toml / ui-react 的 package.json）
const LOCAL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 下载进度事件（界面监听它画进度）
const PROGRESS_EVENT: &str = "wf-app-update-progress";

fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.trim().trim_start_matches('v').split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it
        .next()
        .unwrap_or("0")
        .split(|ch: char| !ch.is_ascii_digit())
        .next()?
        .parse()
        .unwrap_or(0);
    Some((a, b, c))
}

fn semver_gt(a: &str, b: &str) -> bool {
    match (parse_semver(a), parse_semver(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

/// 本机安装信息（versionCode + 未知来源授权）。桌面端返回默认值，好让「检查更新」也能在桌面联调。
#[cfg(target_os = "android")]
fn local_status(app: &tauri::AppHandle) -> Result<apkinstaller::ApkStatus, String> {
    use apkinstaller::ApkInstallerExt;
    app.apk_installer().status()
}

#[cfg(not(target_os = "android"))]
fn local_status(_app: &tauri::AppHandle) -> Result<apkinstaller::ApkStatus, String> {
    Ok(apkinstaller::ApkStatus::default())
}

/// GET /v1/app/latest：返回 Some(JSON) 或 None（服务端还没有发布过版本）。
async fn fetch_latest(base: &str) -> Result<Option<Value>, String> {
    let url = format!("{}/v1/app/latest", base.trim_end_matches('/'));
    let resp = crate::paid::http()
        .map_err(|e| format!("{e:#}"))?
        .get(&url)
        .send()
        .await
        .map_err(|_| "连不上更新服务器，请检查网络后重试".to_string())?;
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("更新服务器返回 HTTP {}", resp.status().as_u16()));
    }
    let d: Value = resp
        .json()
        .await
        .map_err(|_| "更新信息解析失败".to_string())?;
    Ok(Some(d))
}

/// 下载安装包用的客户端：超时放宽到 10 分钟。
/// 付费链路的客户端是 120 秒总超时（含收完响应体），23MB 在弱网下会中途被打断。
fn http_download() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Panfang/0.2 (watchface-installer)")
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("创建下载客户端失败：{e}"))
}

/// 安装包落盘的目录：内部 cacheDir（由插件给出）→ FileProvider 的 `<cache-path>` 正好覆盖它；
/// 插件调用失败或非安卓端时才退回 Tauri 的 cache_dir()（安卓上是**外部** cache，插件里还有一层兜底）。
fn download_dir(_app: &tauri::AppHandle) -> std::path::PathBuf {
    #[cfg(target_os = "android")]
    {
        use apkinstaller::ApkInstallerExt;
        if let Ok(p) = _app.apk_installer().internal_cache_dir() {
            if !p.trim().is_empty() {
                return std::path::PathBuf::from(p);
            }
        }
    }
    _app.path()
        .cache_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// 检查更新（不下载）。`available=false` 时界面显示「已是最新版本」。
#[tauri::command]
pub async fn app_update_check(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Value, String> {
    let base = crate::paid::service_base(&state)?;
    let st = local_status(&app).unwrap_or_default();
    let mut out = json!({
        "current": LOCAL_VERSION,
        "current_code": st.version_code,
        "can_install": st.can_install,
        "available": false,
        "mandatory": false,
    });
    let Some(d) = fetch_latest(&base).await? else {
        out["no_release"] = json!(true);
        return Ok(out);
    };
    let remote_name = d["version_name"].as_str().unwrap_or("").to_string();
    let remote_code = d["version_code"].as_i64().unwrap_or(0);
    let min_code = d["min_version_code"].as_i64().unwrap_or(0);
    // 兜底防「永远提示有新版本」：名字与本机相同就当已是最新（哪怕服务端 version_code 填错了）
    let same_name = remote_name == LOCAL_VERSION;
    let available = !same_name && (remote_code > st.version_code || semver_gt(&remote_name, LOCAL_VERSION));
    out["available"] = json!(available);
    out["mandatory"] = json!(available && min_code > st.version_code);
    out["version_name"] = d["version_name"].clone();
    out["version_code"] = d["version_code"].clone();
    out["notes"] = d["notes"].clone();
    out["size"] = d["size"].clone();
    out["sha256"] = d["sha256"].clone();
    Ok(out)
}

/// 下载最新版 APK 到 cacheDir（流式 + 进度事件 + sha256 校验），返回可交给安装器的路径。
#[tauri::command]
pub async fn app_update_download(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Value, String> {
    let base = crate::paid::service_base(&state)?;
    let d = fetch_latest(&base)
        .await?
        .ok_or_else(|| "服务端还没有发布过版本".to_string())?;
    let url = d["url"].as_str().unwrap_or_default().to_string();
    let want_sha = d["sha256"].as_str().unwrap_or_default().to_string();
    let total = d["size"].as_u64().unwrap_or(0);
    if url.is_empty() {
        return Err("更新信息缺少下载地址".into());
    }

    // 下载目录：优先内部 cacheDir（FileProvider 的 <cache-path> 覆盖它），
    // 拿不到才退回 Tauri 的 cache_dir()（安卓上是外部 cache，安装时插件会兜底复制）
    let dir = download_dir(&app).join("updates");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建缓存目录失败：{e}"))?;
    // 清掉上一次下载留下的包（一个 23MB，不清理会一直堆在缓存里）
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("apk") {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let file_name = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && s.ends_with(".apk"))
        .unwrap_or("panfang-update.apk")
        .to_string();
    let path = dir.join(&file_name);

    let mut resp = http_download()?
        .get(&url)
        .send()
        .await
        .map_err(|_| "下载安装包失败，请检查网络后重试".to_string())?;
    if !resp.status().is_success() {
        return Err(format!("下载安装包失败（HTTP {}）", resp.status().as_u16()));
    }

    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| "下载中断，请重试".to_string())?
    {
        buf.extend_from_slice(&chunk);
        let percent = if total > 0 {
            ((buf.len() as f64 / total as f64) * 100.0).min(100.0).round() as u64
        } else {
            0
        };
        let _ = app.emit(
            PROGRESS_EVENT,
            json!({ "sent": buf.len(), "total": total, "percent": percent }),
        );
    }

    if !want_sha.is_empty() && crate::paid::sha256_hex(&buf) != want_sha {
        let _ = std::fs::remove_file(&path);
        return Err("安装包校验失败（下载内容与服务端不一致），请重试".into());
    }
    std::fs::write(&path, &buf).map_err(|e| format!("保存安装包失败：{e}"))?;
    Ok(json!({
        "path": path.to_string_lossy(),
        "version_name": d["version_name"].clone(),
        "size": buf.len(),
    }))
}

/// 调起系统安装器。
/// - 还没给「安装未知来源应用」授权 → 打开设置页并回 `needs_permission=true`（界面提示回来再点一次）
/// - 已授权 → 直接调起安装器，回 `launched=true`
///
/// 注意：用户点完安装器里的「安装」，系统会杀掉本进程；重开就是新版本了。
#[tauri::command]
pub async fn app_update_install(app: tauri::AppHandle, path: String) -> Result<Value, String> {
    #[cfg(target_os = "android")]
    {
        use apkinstaller::ApkInstallerExt;
        let st = local_status(&app).unwrap_or_default();
        if !st.can_install {
            let _ = app.apk_installer().open_install_settings();
            return Ok(json!({ "needs_permission": true }));
        }
        app.apk_installer().install(&path)?;
        Ok(json!({ "launched": true }))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, path);
        Err("安装更新仅支持安卓版".into())
    }
}
