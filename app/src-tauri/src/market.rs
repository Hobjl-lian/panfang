//! 集市（免费表盘）客户端：列表、下载解密、直装。
//!
//! 与付费链路的关系：**通道完全相同**（服务端把 CEK 用本机设备公钥重新包裹，
//! 这里在内存里解密，明文不落盘），只是不需要授权令牌；安装仍然走
//! `bridge::install_file_bytes_id`，所以装完同样会自动留档、可在管理页继续编辑。

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use x25519_dalek::StaticSecret;

use crate::bridge::AppState;
use crate::paid::{self, http, sha256_hex, unwrap_and_decrypt};

/// 单包上限：正常表盘最大 20MB 左右，留足余量但别让异常响应吃光内存
const MAX_BYTES: u64 = 128 * 1024 * 1024;

/// GET /v1/market/list —— 免费内容，不需要登录态；`w/h` 给当前设备画布。
/// `app` 是本机版本：服务端据此决定要不要把付费条目放进来（老版本只会显示「安装」）。
/// `model` 是设备型号（小米云端给的值，形如 `miwear.watch.n67cn`）：9 Pro 与 10 Pro
/// 同屏，服务端只能靠它把「屏蔽 9 Pro」的资源筛掉。
pub async fn list(
    base: &str,
    w: Option<u32>,
    h: Option<u32>,
    app: &str,
    model: Option<&str>,
    device: Option<&str>,
    member: Option<&str>,
) -> Result<Value> {
    let mut url = format!("{}/v1/market/list", base.trim_end_matches('/'));
    let mut sep = '?';
    if let (Some(w), Some(h)) = (w, h) {
        url.push_str(&format!("{sep}w={w}&h={h}"));
        sep = '&';
    }
    url.push_str(&format!("{sep}app={}", urlencode(app)));
    if let Some(m) = model.map(str::trim).filter(|s| !s.is_empty()) {
        url.push_str(&format!("&model={}", urlencode(m)));
    }
    if let Some(d) = device.map(str::trim).filter(|s| !s.is_empty()) {
        url.push_str(&format!("&device={}", urlencode(d)));
    }
    // 会员档位：top = 尊享/金主，basic = 轻悦。服务端只拿它算「显示价」
    if let Some(m) = member.map(str::trim).filter(|s| !s.is_empty()) {
        url.push_str(&format!("&member={}", urlencode(m)));
    }
    let r = http()?.get(url).send().await?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        bail!("{}", paid::api_error_text(&v, "加载失败"));
    }
    Ok(v)
}

/// URL 查询参数编码：按 UTF-8 字节百分号编码（设备名可能是中文，逐字符编码会写坏）。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 换直链 → 下载密文 → 设备私钥解包 → 内存解密 + 校验
async fn fetch_decrypt(
    base: &str,
    secret: &StaticSecret,
    device_fp: &str,
    device_pub_b64: &str,
    device_name: &str,
    device_model: Option<&str>,
    variant_id: &str,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(Vec<u8>, String)> {
    let r = http()?
        .post(format!(
            "{}/v1/market/download",
            base.trim_end_matches('/')
        ))
        .json(&json!({
            "variant_id": variant_id,
            "device_fp": device_fp,
            "device_pubkey": device_pub_b64,
            "device_name": device_name,
            "device_model": device_model,
        }))
        .send()
        .await?;
    let status = r.status();
    let d: Value = r.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        bail!("{}", paid::api_error_text(&d, "获取下载凭证失败"));
    }
    let url = d["url"].as_str().ok_or_else(|| anyhow!("响应缺少下载地址"))?;
    let sha_plain = d["file"]["sha256_plain"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let total = d["file"]["size"].as_u64().unwrap_or(0);

    let mut resp = http()?.get(url).send().await?;
    if !resp.status().is_success() {
        bail!("下载失败（HTTP {}）", resp.status().as_u16());
    }
    let mut cipher: Vec<u8> = Vec::with_capacity(total.min(MAX_BYTES) as usize + 32);
    while let Some(chunk) = resp.chunk().await? {
        cipher.extend_from_slice(&chunk);
        if cipher.len() as u64 > MAX_BYTES {
            bail!("文件过大，已停止下载");
        }
        on_progress(cipher.len() as u64, total);
    }
    on_progress(cipher.len() as u64, total.max(cipher.len() as u64));

    let plain = unwrap_and_decrypt(
        secret,
        &d["key_wrap"],
        d["file_nonce"].as_str().unwrap_or_default(),
        &cipher,
    )?;
    if !sha_plain.is_empty() && sha256_hex(&plain) != sha_plain {
        bail!("文件校验失败（内容与服务端不一致）");
    }
    let sha = if sha_plain.is_empty() {
        sha256_hex(&plain)
    } else {
        sha_plain
    };
    Ok((plain, sha))
}

// ---------------------------------------------------------------- 本机安装记录（更新提示用）

/// 一次集市安装：记住装的是哪个资源、哪个设备行、哪个内容指纹。
/// 下次拉列表时比对服务端的 `sha`，不一致就是「可更新」。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MarketInstall {
    /// 资源 id（market_products.id）
    #[serde(default)]
    pub product_id: String,
    /// 设备行 id（market_variants.id）：更新判断按它对
    pub variant_id: String,
    pub watchface_id: String,
    pub sha: String,
    pub name: String,
    pub at: i64,
}

fn market_dir(state: &AppState) -> PathBuf {
    state
        .app_data_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir)
        .join("market")
}

fn installs_path(dir: &Path) -> PathBuf {
    dir.join("installs.json")
}

fn load_installs(dir: &Path) -> Vec<MarketInstall> {
    std::fs::read(installs_path(dir))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<MarketInstall>>(&b).ok())
        .unwrap_or_default()
}

/// 记下/覆盖一条安装记录。失败不影响安装本身（用户已经装好了）。
fn record_install(dir: &Path, rec: MarketInstall) {
    let mut all = load_installs(dir);
    all.retain(|r| r.variant_id != rec.variant_id);
    all.push(rec);
    if let Ok(b) = serde_json::to_vec_pretty(&all) {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(installs_path(dir), b);
    }
}

/// 给服务端列表补上 `installed` / `has_update`：UI 只读这两个字段。
/// 列表里一条 = 一个资源 + 当前设备对应的设备行，安装记录按 `variantId` 对。
fn annotate_installs(v: &mut Value, dir: &Path) {
    let records = load_installs(dir);
    let Some(faces) = v["faces"].as_array_mut() else {
        return;
    };
    for f in faces.iter_mut() {
        let variant_id = f["variantId"].as_str().unwrap_or("").to_string();
        let sha = f["sha"].as_str().unwrap_or("").to_string();
        let rec = records.iter().find(|r| r.variant_id == variant_id);
        let has_update = rec
            .map(|r| !sha.is_empty() && !r.sha.is_empty() && r.sha != sha)
            .unwrap_or(false);
        f["installed"] = json!(rec.is_some());
        f["has_update"] = json!(has_update);
    }
}

#[tauri::command]
pub async fn market_list(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    w: Option<u32>,
    h: Option<u32>,
    model: Option<String>,
    device: Option<String>,
    member: Option<String>,
) -> Result<Value, String> {
    let base = paid::service_base(&state)?;
    let version = app.package_info().version.to_string();
    let mut v = list(
        &base,
        w,
        h,
        &version,
        model.as_deref(),
        device.as_deref(),
        member.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())?;
    annotate_installs(&mut v, &market_dir(&state));
    Ok(v)
}

/// 下载 → 解密（内存）→ 装到设备（安装通道会按表盘 id 自动留档，之后可编辑）。
#[tauri::command]
pub async fn market_install(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    addr: String,
    variant_id: String,
    product_id: Option<String>,
    device_name: Option<String>,
    device_model: Option<String>,
    face_name: Option<String>,
) -> Result<Option<String>, String> {
    let base = paid::service_base(&state)?;
    let (secret, fp, pub_b64) = paid::device_identity(&state).map_err(|e| e.to_string())?;

    paid::emit_progress(&app, "download", "开始下载…", Some(0), None);
    let (plain, sha) = {
        let app2 = app.clone();
        let mut last_emit = std::time::Instant::now();
        let r = fetch_decrypt(
            &base,
            &secret,
            &fp,
            &pub_b64,
            device_name.as_deref().unwrap_or("盘坊 App"),
            device_model.as_deref(),
            &variant_id,
            |sent, total| {
                if last_emit.elapsed().as_millis() >= 120 || sent == total {
                    last_emit = std::time::Instant::now();
                    paid::emit_progress(&app2, "download", "下载中…", Some(sent), Some(total));
                }
            },
        )
        .await;
        match r {
            Ok((p, s)) => (p, s),
            Err(e) => {
                paid::emit_progress(&app, "error", &format!("{e}"), None, None);
                return Err(e.to_string());
            }
        }
    };
    paid::emit_progress(&app, "decrypt", "解密完成，校验通过", None, None);
    paid::emit_progress(&app, "transfer", "正在传输到手环…", None, None);

    // 安装前先认头：记安装账号（更新提示）要用表盘 id 与名称。
    // 复用了 .bin 头部，不另外让界面传。
    let header = crate::watchface_bin::parse_header(&plain).ok();
    let watchface_id = header
        .as_ref()
        .map(|h| h.watchface_id.clone())
        .unwrap_or_default();
    let name = face_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            header
                .as_ref()
                .map(|h| h.name.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default();

    let id = crate::bridge::install_file_bytes_id(&app, &addr, plain, "bin".to_string(), None)
        .await
        .map_err(|e| {
            let msg = format!("安装失败：{e}");
            paid::emit_progress(&app, "error", &msg, None, None);
            msg
        })?;

    // 安装成功才记账：内容指纹用于「可更新」判断（与付费资源的 has_update 同一套语义）
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    record_install(
        &market_dir(&state),
        MarketInstall {
            product_id: product_id.unwrap_or_default(),
            variant_id,
            watchface_id,
            sha,
            name,
            at,
        },
    );

    paid::emit_progress(&app, "done", "安装完成，正在激活…", None, None);
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(variant_id: &str, sha: &str) -> MarketInstall {
        MarketInstall {
            product_id: "mkt_test".to_string(),
            variant_id: variant_id.to_string(),
            watchface_id: "466000107".to_string(),
            sha: sha.to_string(),
            name: "香".to_string(),
            at: 1,
        }
    }

    #[test]
    fn annotate_installs_marks_update_only_when_sha_changed() {
        let dir = std::env::temp_dir().join(format!("market-installs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        record_install(&dir, rec("mv_a", "old"));
        let mut v = json!({
            "faces": [
                {"id": "mkt_a", "variantId": "mv_a", "sha": "new"},
                {"id": "mkt_b", "variantId": "mv_b", "sha": "x"},
            ]
        });
        annotate_installs(&mut v, &dir);
        assert_eq!(v["faces"][0]["installed"], json!(true));
        assert_eq!(v["faces"][0]["has_update"], json!(true));
        assert_eq!(v["faces"][1]["installed"], json!(false));
        assert_eq!(v["faces"][1]["has_update"], json!(false));

        // 装到最新后：installed 仍为 true，has_update 变 false
        record_install(&dir, rec("mv_a", "new"));
        annotate_installs(&mut v, &dir);
        assert_eq!(v["faces"][0]["installed"], json!(true));
        assert_eq!(v["faces"][0]["has_update"], json!(false));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
