//! 付费表盘客户端（App 侧）：设备密钥、绑定、列表、下载解密、直装。
//!
//! 与 `docs/接口契约-授权服务-v1.md` v1.2 对应。要点：
//! - **明文不落盘**：下载的是密文，密钥包裹用设备私钥解开，解密在内存完成，直接喂给安装通道
//! - 设备身份：本机生成一对 X25519 密钥（`device.key`）+ 一个随机 `device_uuid`；
//!   `device_fp = sha256(device_uuid)`，公钥随每次请求上报（服务端用它包裹文件钥匙）
//! - 登录态：绑定成功后拿到 `license_token`，用**设备私钥派生**的密钥加密后存盘（`session.enc`）
//!
//! ⚠️ 待加固（M4）：私钥目前存应用私有目录，后续应改为 Android Keystore 托管（需 JNI 代码）。
//! 现阶段威胁模型是「防普通用户翻文件」，明文不落盘 + 服务端设备绑定已经覆盖。

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Emitter;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::bridge::AppState;

/// 密钥包裹的 KDF 上下文，必须与服务端一致（`server/src/crypto.rs`）
const WRAP_INFO: &[u8] = b"wf-cek-wrap-v1";
const SESSION_INFO: &[u8] = b"wf-session-v1";
/// 本地留档（已装表盘的 .bin）加解密用；与「会话」分开的上下文，避免密钥复用
const ARCHIVE_INFO: &[u8] = b"wf-archive-v1";
/// 留档密文头：老版本存的是明文（没有这个头）→ 读取时自动兼容
const ARCHIVE_MAGIC: &[u8] = b"WFENC1\0";
const DEFAULT_BASE: &str = "https://api.hobjl-shop.top";

// ---------------------------------------------------------------- 小工具

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}
fn b64d(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(s.trim().as_bytes())
        .context("base64 解码失败")
}
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// 与授权服务通信用的 HTTP 客户端（付费链路与 App 自更新共用）。
pub(crate) fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Panfang/0.2 (watchface-installer)")
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("创建 HTTP 客户端失败")
}

// ---------------------------------------------------------------- 登录态

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Session {
    pub license_token: String,
    pub expires_at: i64,
    pub mi_user_id: String,
    pub display: String,
}

// ---------------------------------------------------------------- 本机身份与状态

pub struct Ctx {
    pub dir: PathBuf,
    pub base: String,
    pub secret: StaticSecret,
    pub pub_b64: String,
    pub fp: String,
    pub session: Option<Session>,
}

impl Ctx {
    /// 载入或创建：设备密钥、设备 id、已保存的登录态。
    pub fn load_or_create(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let key_path = dir.join("device.key");
        let secret = match std::fs::read(&key_path) {
            Ok(raw) if raw.len() == 32 => {
                let mut k = [0u8; 32];
                k.copy_from_slice(&raw);
                StaticSecret::from(k)
            }
            _ => {
                let mut k = [0u8; 32];
                rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut k);
                let _ = std::fs::write(&key_path, k);
                StaticSecret::from(k)
            }
        };
        let pub_b64 = b64(PublicKey::from(&secret).as_bytes());

        let id_path = dir.join("device_uuid");
        let uuid = match std::fs::read_to_string(&id_path) {
            Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                let mut raw = [0u8; 16];
                rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut raw);
                let s = hex::encode(raw);
                let _ = std::fs::write(&id_path, &s);
                s
            }
        };
        let fp = sha256_hex(uuid.as_bytes());

        let base = std::fs::read_to_string(dir.join("base.txt"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE.to_string());

        let mut ctx = Self {
            dir,
            base,
            secret,
            pub_b64,
            fp,
            session: None,
        };
        ctx.session = ctx.load_session();
        ctx
    }

    fn session_key(&self) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(None, self.secret.as_bytes());
        let mut k = [0u8; 32];
        hk.expand(SESSION_INFO, &mut k).expect("hkdf");
        k
    }

    fn load_session(&self) -> Option<Session> {
        let raw = std::fs::read(self.dir.join("session.enc")).ok()?;
        if raw.len() < 13 {
            return None;
        }
        let key = self.session_key();
        let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
        let plain = cipher
            .decrypt(Nonce::from_slice(&raw[..12]), Payload { msg: &raw[12..], aad: &[] })
            .ok()?;
        serde_json::from_slice(&plain).ok()
    }

    pub fn save_session(&mut self, s: Session) -> Result<()> {
        let key = self.session_key();
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("AES: {e}"))?;
        let mut nonce = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
        let plain = serde_json::to_vec(&s)?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(
            &cipher
                .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: &[] })
                .map_err(|_| anyhow!("登录态加密失败"))?,
        );
        std::fs::write(self.dir.join("session.enc"), out)?;
        self.session = Some(s);
        Ok(())
    }

    pub fn clear_session(&mut self) {
        let _ = std::fs::remove_file(self.dir.join("session.enc"));
        self.session = None;
    }

    /// 登录态是否可用（未过期）
    pub fn token(&self) -> Option<&str> {
        let s = self.session.as_ref()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if s.expires_at > now && !s.license_token.is_empty() {
            Some(s.license_token.as_str())
        } else {
            None
        }
    }
}

/// 付费模块的数据目录（设备密钥、登录态都放这里）
pub fn paid_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("paid")
}

// ---------------------------------------------------------------- 本机安装记录（更新提醒用）

/// 一次安装的账：某个设备行（variant）上次装到手环上的**是哪个版本**。
/// `sha` = 那次装的包体内容指纹（与服务端 `/v1/me` 的 `latest_sha` 同算法），判「可更新」比它；
/// `version` 只用于界面显示 —— 版本号是人工填的字符串，管理页「强制替换」可以不改版本号换内容。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InstallRecord {
    pub product_id: String,
    pub variant_id: String,
    pub version: String,
    pub sha: String,
    /// 这次装上车机（手环）的表盘 id：管理页标「可更新」时按它对上
    pub dials: Vec<String>,
    /// 安装时间（unix 秒）
    pub at: i64,
}

fn installs_path(dir: &Path) -> PathBuf {
    dir.join("installs.json")
}

fn load_installs(dir: &Path) -> Vec<InstallRecord> {
    std::fs::read(installs_path(dir))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<InstallRecord>>(&b).ok())
        .unwrap_or_default()
}

/// 记下/覆盖一条安装记录（同一个设备行只留最新一次）。
/// **失败不影响安装本身**（用户已经装好了），所以调用方忽略返回值。
fn save_install(dir: &Path, rec: InstallRecord) {
    let mut all = load_installs(dir);
    all.retain(|r| !(r.product_id == rec.product_id && r.variant_id == rec.variant_id));
    all.push(rec);
    if let Ok(b) = serde_json::to_vec_pretty(&all) {
        let _ = std::fs::write(installs_path(dir), b);
    }
}

/// 版本内容指纹：按文件顺序把各包 `sha256_plain` 用 `|` 连起来再 sha256。
/// ⚠️ 必须与服务端 `subject_products` 的 `latest_sha` 用同一套算法（顺序、分隔符都要一致），
/// 否则界面会一直显示「可更新」。
fn manifest_sha(files: &[Value]) -> String {
    let joined = files
        .iter()
        .map(|f| f["sha256_plain"].as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("|");
    hex::encode(Sha256::digest(joined.as_bytes()))
}

static CTX: OnceLock<Mutex<Option<Ctx>>> = OnceLock::new();

/// 取本机上下文（首次调用时按数据目录初始化）。
///
/// ⚠️ 入参是**付费模块的数据目录**（`paid_dir(app_data_dir)`，不是 app_data_dir 本身）。
/// 这是进程级 `OnceLock`：**第一个调用者决定用哪个目录**，传错就会读到/新建另一个
/// `device.key`，于是留档与登录态全都解不开（2026-10-08 真机踩过：诊断打点模块先跑、
/// 少传了一层 `paid/`）。现在启动时会先用正确目录初始化一次，之后调用者传什么都不再影响结果。
pub fn ctx(dir: &Path) -> &'static Mutex<Option<Ctx>> {
    CTX.get_or_init(|| Mutex::new(Some(Ctx::load_or_create(dir.to_path_buf()))))
}

fn dir_of(state: &AppState) -> PathBuf {
    state
        .app_data_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir)
        .join("paid")
}

fn with_ctx<T>(state: &AppState, f: impl FnOnce(&mut Ctx) -> Result<T>) -> Result<T> {
    let m = ctx(&dir_of(state));
    let mut g = m.lock();
    let c = g.as_mut().ok_or_else(|| anyhow!("付费模块未初始化"))?;
    f(c)
}

/// 当前授权服务地址（App 自更新也用它 —— 与付费链路同一个 base，联调时改一处即可）。
pub fn service_base(state: &AppState) -> Result<String, String> {
    with_ctx(state, |c| Ok(c.base.clone())).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- 服务端交互

/// GET /v1/bind/mode —— 绑定页显示什么（oauth / dev / none）+ 店铺链接
pub async fn bind_mode(base: &str) -> Result<Value> {
    let r = http()?
        .get(format!("{}/v1/bind/mode", base.trim_end_matches('/')))
        .send()
        .await?;
    Ok(r.json().await?)
}

/// POST /v1/bind/dev —— 联调绑定（服务端需开启 WF_DEV_BIND=1）
pub async fn bind_dev(base: &str, mi_user_id: &str, pub_b64: &str, device_name: &str) -> Result<Session> {
    let r = http()?
        .post(format!("{}/v1/bind/dev", base.trim_end_matches('/')))
        .json(&json!({
            "mi_user_id": mi_user_id,
            "device_fp": "",
            "device_pubkey": pub_b64,
            "device_name": device_name,
        }))
        .send()
        .await?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        bail!(
            "绑定失败：{}",
            v["message"].as_str().unwrap_or("服务端未开放联调绑定")
        );
    }
    Ok(Session {
        license_token: v["license_token"].as_str().unwrap_or_default().to_string(),
        expires_at: v["expires_at"].as_i64().unwrap_or(0),
        mi_user_id: v["subject"]["mi_user_id"].as_str().unwrap_or_default().to_string(),
        display: v["subject"]["display"].as_str().unwrap_or_default().to_string(),
    })
}

async fn get_json(base: &str, path: &str, token: Option<&str>) -> Result<Value> {
    let mut req = http()?.get(format!("{}{}", base.trim_end_matches('/'), path));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let r = req.send().await?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_else(|_| json!({}));
    if status.as_u16() == 401 {
        bail!("登录已过期，请重新绑定");
    }
    if !status.is_success() {
        bail!("{}", v["message"].as_str().unwrap_or("请求失败"));
    }
    Ok(v)
}

/// GET /v1/me —— 我买到的资源（只含有包体的设备行）
pub async fn me(base: &str, token: &str) -> Result<Value> {
    get_json(base, "/v1/me", Some(token)).await
}

/// GET /v1/manifest —— 某设备行的版本清单
pub async fn manifest(base: &str, token: &str, product_id: &str, variant_id: &str) -> Result<Value> {
    get_json(
        base,
        &format!(
            "/v1/manifest?product_id={}&variant_id={}",
            urlencode(product_id),
            urlencode(variant_id)
        ),
        Some(token),
    )
    .await
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32 as u8),
        })
        .collect()
}

/// 设备密钥解包 + 内存解密（返回明文 + 与服务端一致的哈希校验）
pub fn unwrap_and_decrypt(
    secret: &StaticSecret,
    key_wrap: &Value,
    file_nonce_b64: &str,
    cipher: &[u8],
) -> Result<Vec<u8>> {
    let epk: [u8; 32] = b64d(key_wrap["epk"].as_str().unwrap_or_default())?
        .try_into()
        .map_err(|_| anyhow!("设备公钥长度异常"))?;
    let salt = b64d(key_wrap["salt"].as_str().unwrap_or_default())?;
    let wrap_nonce = b64d(key_wrap["wrap_nonce"].as_str().unwrap_or_default())?;
    let wrapped = b64d(key_wrap["cipher"].as_str().unwrap_or_default())?;
    if wrap_nonce.len() != 12 {
        bail!("密钥包裹格式异常");
    }

    let shared = secret.diffie_hellman(&PublicKey::from(epk));
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut kek = [0u8; 32];
    hk.expand(WRAP_INFO, &mut kek).map_err(|_| anyhow!("派生失败"))?;

    let cipher_kek = Aes256Gcm::new_from_slice(&kek).map_err(|e| anyhow!("AES: {e}"))?;
    let cek = cipher_kek
        .decrypt(Nonce::from_slice(&wrap_nonce), Payload { msg: &wrapped, aad: &[] })
        .map_err(|_| anyhow!("解不开密钥（设备不匹配？）"))?;
    if cek.len() != 32 {
        bail!("密钥长度异常");
    }

    let nonce = b64d(file_nonce_b64)?;
    if nonce.len() != 12 {
        bail!("文件随机数异常");
    }
    let cipher_file = Aes256Gcm::new_from_slice(&cek).map_err(|e| anyhow!("AES: {e}"))?;
    cipher_file
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload { msg: cipher, aad: b"wf-file-v1" },
        )
        .map_err(|_| anyhow!("文件解密失败"))
}

/// 下载一个文件并解密（内存），返回明文 + 期望哈希
pub async fn fetch_decrypt(
    base: &str,
    token: &str,
    secret: &StaticSecret,
    device_fp: &str,
    device_pub_b64: &str,
    device_name: &str,
    file_id: &str,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<Vec<u8>> {
    // 1) 换取直链 + 密钥包裹
    let r = http()?
        .post(format!("{}/v1/download", base.trim_end_matches('/')))
        .bearer_auth(token)
        .json(&json!({
            "file_id": file_id,
            "device_fp": device_fp,
            "device_pubkey": device_pub_b64,
            "device_name": device_name,
        }))
        .send()
        .await?;
    let status = r.status();
    let d: Value = r.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        bail!("{}", d["message"].as_str().unwrap_or("获取下载凭证失败"));
    }
    let url = d["url"].as_str().ok_or_else(|| anyhow!("响应缺少下载地址"))?;
    let sha_plain = d["file"]["sha256_plain"].as_str().unwrap_or_default().to_string();
    let total = d["file"]["size"].as_u64().unwrap_or(0);

    // 2) 下载密文（流式，报进度）
    let mut resp = http()?.get(url).send().await?;
    if !resp.status().is_success() {
        bail!("下载失败（HTTP {}）", resp.status().as_u16());
    }
    let mut cipher: Vec<u8> = Vec::with_capacity(total as usize + 32);
    while let Some(chunk) = resp.chunk().await? {
        cipher.extend_from_slice(&chunk);
        on_progress(cipher.len() as u64, total);
    }
    on_progress(cipher.len() as u64, total.max(cipher.len() as u64));

    // 3) 设备私钥解包 + 内存解密 + 校验
    let plain = unwrap_and_decrypt(secret, &d["key_wrap"], d["file_nonce"].as_str().unwrap_or_default(), &cipher)?;
    if !sha_plain.is_empty() && sha256_hex(&plain) != sha_plain {
        bail!("文件校验失败（内容与服务端不一致）");
    }
    Ok(plain)
}

// ------------------------------------------------------- Tauri 命令（供界面调用）

fn emit_progress(app: &tauri::AppHandle, phase: &str, detail: &str, sent: Option<u64>, total: Option<u64>) {
    let percent = match (sent, total) {
        // 密文比明文多 16 字节校验码，先算出的比例可能略超 100% → 封顶
        (Some(s), Some(t)) if t > 0 => Some((((s as f64 / t as f64) * 100.0).min(100.0)).round() as u64),
        _ => None,
    };
    let _ = app.emit(
        "wf-paid-progress",
        json!({ "phase": phase, "detail": detail, "sent": sent, "total": total, "percent": percent }),
    );
}

/// 设备型号（代号，如 o66）→ 屏幕尺寸；列表页用它判断「这份表盘能不能装」
// ---------------------------------------------------------------- 本地留档加解密

fn archive_key(secret: &StaticSecret) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, secret.as_bytes());
    let mut k = [0u8; 32];
    hk.expand(ARCHIVE_INFO, &mut k).expect("hkdf");
    k
}

/// 留档落盘前加密：WFENC1\0 || nonce(12) || AES-256-GCM(明文)`。
/// 密钥由**设备密钥**派生，所以留档只在装它的这台手机上能解开。
pub fn seal_archive(paid_dir: &Path, plain: &[u8]) -> Result<Vec<u8>> {
    let m = ctx(paid_dir);
    let g = m.lock();
    let c = g.as_ref().ok_or_else(|| anyhow!("付费模块未初始化"))?;
    let key = archive_key(&c.secret);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("AES: {e}"))?;
    let mut nonce = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload { msg: plain, aad: ARCHIVE_MAGIC },
        )
        .map_err(|_| anyhow!("留档加密失败"))?;
    let mut out = Vec::with_capacity(ARCHIVE_MAGIC.len() + 12 + ct.len());
    out.extend_from_slice(ARCHIVE_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// 这段字节是不是「已加密的留档」（带 WFENC1 头）。用来顺手把老的明文留档升级成密文。
pub fn is_sealed(blob: &[u8]) -> bool {
    blob.starts_with(ARCHIVE_MAGIC)
}

/// 读留档：带密文头 → 解密；没头（老版本明文留档）→ 原样返回。
pub fn open_archive(paid_dir: &Path, blob: &[u8]) -> Result<Vec<u8>> {
    if !blob.starts_with(ARCHIVE_MAGIC) {
        return Ok(blob.to_vec());
    }
    let body = &blob[ARCHIVE_MAGIC.len()..];
    if body.len() < 13 {
        bail!("留档数据损坏");
    }
    let m = ctx(paid_dir);
    let g = m.lock();
    let c = g.as_ref().ok_or_else(|| anyhow!("付费模块未初始化"))?;
    let key = archive_key(&c.secret);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("AES: {e}"))?;
    cipher
        .decrypt(
            Nonce::from_slice(&body[..12]),
            Payload { msg: &body[12..], aad: ARCHIVE_MAGIC },
        )
        .map_err(|_| anyhow!("留档无法解密（设备密钥已变化？）"))
}

#[tauri::command]
pub async fn paid_device_screen(model: Option<String>) -> Result<Value, String> {
    let scr = model
        .as_deref()
        .and_then(crate::watchface_edit::device_screen);
    Ok(json!({ "screen": scr.map(|(w, h)| vec![w, h]) }))
}

#[tauri::command]
pub async fn paid_status(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    with_ctx(&state, |c| {
        Ok(json!({
            "base": c.base,
            "device_fp": c.fp,
            "bound": c.token().is_some(),
            "mi_user_id": c.session.as_ref().map(|s| s.mi_user_id.clone()),
            "display": c.session.as_ref().map(|s| s.display.clone()),
            "expires_at": c.session.as_ref().map(|s| s.expires_at),
        }))
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn paid_bind_mode(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    let base = with_ctx(&state, |c| Ok(c.base.clone())).map_err(|e| e.to_string())?;
    bind_mode(&base).await.map_err(|e| e.to_string())
}

/// 切换授权服务地址（**开发期命令**：联调时把 App 指向本机服务器用，正式包关闭）
#[tauri::command]
pub async fn paid_set_base(state: tauri::State<'_, AppState>, base: String) -> Result<(), String> {
    crate::dev_only("切换服务地址")?;
    with_ctx(&state, |c| {
        c.base = base.clone();
        let _ = std::fs::write(c.dir.join("base.txt"), &base);
        Ok(())
    })
    .map_err(|e| e.to_string())
}

/// 绑定（当前为联调模式；爱发电审批后此处改为打开授权页 + 回调换绑）
#[tauri::command]
pub async fn paid_bind(
    state: tauri::State<'_, AppState>,
    mi_user_id: String,
    device_name: Option<String>,
) -> Result<Value, String> {
    // 没显式给 UID 就用当前登录的小米账号（绑定主体 = 小米账号，复用现有登录）
    let mi_user_id = if mi_user_id.trim().is_empty() {
        state
            .mi_token
            .lock()
            .as_ref()
            .map(|t| t.user_id.clone())
            .unwrap_or_default()
    } else {
        mi_user_id.trim().to_string()
    };
    if mi_user_id.is_empty() {
        return Err("请先用小米账号登录".into());
    }
    let (base, pub_b64) = with_ctx(&state, |c| Ok((c.base.clone(), c.pub_b64.clone())))
        .map_err(|e| e.to_string())?;
    let s = bind_dev(
        &base,
        &mi_user_id,
        &pub_b64,
        device_name.as_deref().unwrap_or("盘坊 App"),
    )
    .await
    .map_err(|e| e.to_string())?;
    with_ctx(&state, |c| c.save_session(s.clone()))
        .map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "bound": true, "display": s.display, "expires_at": s.expires_at }))
}

/// 当前登录的小米账号 UID（绑定的主体就是它）
fn current_mi_uid(state: &tauri::State<'_, AppState>) -> String {
    state
        .mi_token
        .lock()
        .as_ref()
        .map(|t| t.user_id.clone())
        .unwrap_or_default()
}

/// POST /v1/afdian/oauth/start —— 发起爱发电授权：拿到让用户去点的授权页地址 + 会话号
pub async fn bind_oauth_start(
    base: &str,
    mi_user_id: &str,
    fp: &str,
    pub_b64: &str,
    device_name: &str,
) -> Result<Value> {
    let r = http()?
        .post(format!("{}/v1/afdian/oauth/start", base.trim_end_matches('/')))
        .json(&json!({
            "mi_user_id": mi_user_id,
            "device_fp": fp,
            "device_pubkey": pub_b64,
            "device_name": device_name,
        }))
        .send()
        .await?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        bail!(
            "{}",
            v["message"].as_str().unwrap_or("暂时无法发起爱发电授权")
        );
    }
    Ok(v)
}

/// GET /v1/afdian/oauth/poll?state=… —— 用户在浏览器里授权完没有
pub async fn bind_oauth_poll(base: &str, state: &str) -> Result<Value> {
    let v = get_json(base, &format!("/v1/afdian/oauth/poll?state={state}"), None).await?;
    Ok(v)
}

/// 轮询到授权完成 → 把服务端备好的登录态存到本机（与联调绑定同一套存储）
pub fn save_oauth_session(state: &tauri::State<'_, AppState>, v: &Value) -> Result<Session> {
    let s = Session {
        license_token: v["license_token"].as_str().unwrap_or_default().to_string(),
        expires_at: v["expires_at"].as_i64().unwrap_or(0),
        mi_user_id: v["mi_user_id"].as_str().unwrap_or_default().to_string(),
        display: v["display"].as_str().unwrap_or_default().to_string(),
    };
    if s.license_token.is_empty() {
        bail!("授权结果不完整，请重试");
    }
    with_ctx(state, |c| c.save_session(s.clone()))
        .map_err(|e| anyhow!("{e}"))?;
    Ok(s)
}

/// 发起绑定：返回 { state, url }，前端用系统浏览器打开 url
#[tauri::command]
pub async fn paid_bind_oauth_start(
    state: tauri::State<'_, AppState>,
    device_name: Option<String>,
) -> Result<Value, String> {
    let mi_uid = current_mi_uid(&state);
    if mi_uid.is_empty() {
        return Err("请先用小米账号登录".into());
    }
    let (base, fp, pub_b64) =
        with_ctx(&state, |c| Ok((c.base.clone(), c.fp.clone(), c.pub_b64.clone())))
            .map_err(|e| e.to_string())?;
    let v = bind_oauth_start(
        &base,
        &mi_uid,
        &fp,
        &pub_b64,
        device_name.as_deref().unwrap_or("盘坊 App"),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(v)
}

/// 轮询授权结果；拿到 ok 时顺手把登录态存下（省得前端再调一次）
#[tauri::command]
pub async fn paid_bind_oauth_poll(
    state: tauri::State<'_, AppState>,
    state_key: String,
) -> Result<Value, String> {
    let base = with_ctx(&state, |c| Ok(c.base.clone())).map_err(|e| e.to_string())?;
    let v = bind_oauth_poll(&base, &state_key)
        .await
        .map_err(|e| e.to_string())?;
    if v["status"] == json!("ok") {
        let s = save_oauth_session(&state, &v).map_err(|e| e.to_string())?;
        return Ok(json!({
            "status": "ok",
            "bound": true,
            "display": s.display,
            "expires_at": s.expires_at,
        }));
    }
    Ok(json!({
        "status": v["status"].as_str().unwrap_or("pending"),
        "message": v["message"].as_str().unwrap_or(""),
    }))
}

/// POST /v1/unbind —— 通知服务端把当前登录态作废（尽力而为：失败不报错，本地退出照常进行）。
async fn unbind_remote(base: &str, token: &str) -> Result<()> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?
        .post(format!("{}/v1/unbind", base.trim_end_matches('/')))
        .bearer_auth(token)
        .send()
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn paid_unbind(state: tauri::State<'_, AppState>) -> Result<(), String> {
    // 2026-10-15：退出登录改成**两端都退** —— 先在服务端把令牌作废（否则泄露的令牌能一直用到过期），
    // 再清本地。网络失败不阻塞退出：用户点了「退出登录」就必须退得出去。
    if let Ok((base, token)) = with_ctx(&state, |c| Ok((c.base.clone(), c.token().map(|t| t.to_string())))) {
        if let Some(t) = token {
            let _ = unbind_remote(&base, &t).await;
        }
    }
    with_ctx(&state, |c| {
        c.clear_session();
        Ok(())
    })
    .map_err(|e| e.to_string())
}

/// 我买到的资源列表（App 列表页的数据源）。
/// 顺手把**本机安装记录**贴到每个设备行上：界面据此显示「可更新 / 已装 vX」。
#[tauri::command]
pub async fn paid_list(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    let (base, token) = with_ctx(&state, |c| {
        Ok((
            c.base.clone(),
            c.token().map(|t| t.to_string()),
        ))
    })
    .map_err(|e| e.to_string())?;
    let token = token.ok_or_else(|| "NOT_BOUND".to_string())?;
    let mut v = me(&base, &token).await.map_err(|e| e.to_string())?;
    let records = with_ctx(&state, |c| Ok(load_installs(&c.dir))).map_err(|e| e.to_string())?;
    if let Some(products) = v["products"].as_array_mut() {
        for p in products.iter_mut() {
            let pid = p["product_id"].as_str().unwrap_or("").to_string();
            let Some(vars) = p["variants"].as_array_mut() else { continue };
            for var in vars.iter_mut() {
                let vid = var["variant_id"].as_str().unwrap_or("").to_string();
                let rec = records
                    .iter()
                    .find(|r| r.product_id == pid && r.variant_id == vid);
                let (installed, installed_version, has_update) = match rec {
                    None => (false, Value::Null, false),
                    Some(r) => {
                        let latest_sha = var["latest_sha"].as_str().unwrap_or("");
                        let latest_ver = var["latest_version"].as_str().unwrap_or("");
                        // 优先比内容指纹（sha）；服务端没给 latest_sha 时退回比版本号
                        let up = if !latest_sha.is_empty() && !r.sha.is_empty() {
                            latest_sha != r.sha
                        } else {
                            !latest_ver.is_empty() && latest_ver != r.version
                        };
                        (true, json!(r.version), up)
                    }
                };
                var["installed"] = json!(installed);
                var["installed_version"] = installed_version;
                var["has_update"] = json!(has_update);
            }
        }
    }
    Ok(v)
}

/// 本机安装记录（**纯本地读，不联网**）。
/// 管理页据此把「设备上的表盘」对应的资源 id / 设备行 id 对上，标记哪些表盘有新版本可更新。
#[tauri::command]
pub async fn paid_installs_get(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    let records = with_ctx(&state, |c| Ok(load_installs(&c.dir))).map_err(|e| e.to_string())?;
    serde_json::to_value(records).map_err(|e| e.to_string())
}

/// 某设备行最新版本的**全部包**（App 点安装时用）。
/// 一个版本可能有多个包：同一设备因内存限制拆成两个独立表盘，按 sort_order 依次安装。
#[tauri::command]
pub async fn paid_latest_file(
    state: tauri::State<'_, AppState>,
    product_id: String,
    variant_id: String,
) -> Result<Value, String> {
    let (base, token) = with_ctx(&state, |c| Ok((c.base.clone(), c.token().map(|t| t.to_string()))))
        .map_err(|e| e.to_string())?;
    let token = token.ok_or_else(|| "NOT_BOUND".to_string())?;
    let m = manifest(&base, &token, &product_id, &variant_id)
        .await
        .map_err(|e| e.to_string())?;
    let latest = m["versions"]
        .as_array()
        .and_then(|a| a.iter().find(|v| v["is_latest"] == json!(true)))
        .or_else(|| m["versions"].as_array().and_then(|a| a.first()))
        .cloned()
        .ok_or_else(|| "该设备暂无可用包体".to_string())?;
    let files = latest["files"]
        .as_array()
        .cloned()
        .ok_or_else(|| "该版本没有文件".to_string())?;
    if files.is_empty() {
        return Err("该版本没有文件".to_string());
    }
    let total: u64 = files.iter().filter_map(|f| f["size"].as_u64()).sum();
    Ok(json!({
        "version": latest["version"],
        "count": files.len(),
        "totalSize": total,
        "file": files[0],
        "files": files,
    }))
}

/// 下载 → 解密（内存）→ 直装到设备；全程发 `wf-paid-progress`，安装阶段另有 `wf-install-progress`。
/// **一批多个包时按顺序依次安装，返回最后一个包的表盘 id**（前端拿它设为当前表盘）；
/// 表盘 id 是设备往返解析出来的，个别情况下解析不出来 —— 那种情况**安装本身已经成功**，
/// 返回 `None` 让界面照常走成功页（不能反过来报「安装失败」骗用户重装）。
/// `file_ids` = 只装选中的这些包（多包体「勾选安装」）；None = 全装。
/// 记账的版本指纹始终按**全量包体**算（与服务端 latest_sha 同口径），勾选装一部分不影响「可更新」判断。
#[tauri::command]
pub async fn paid_install(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    addr: String,
    product_id: String,
    variant_id: String,
    device_name: Option<String>,
    file_ids: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    let (base, token) = with_ctx(&state, |c| Ok((c.base.clone(), c.token().map(|t| t.to_string()))))
        .map_err(|e| e.to_string())?;
    let token = token.ok_or_else(|| "NOT_BOUND".to_string())?;

    emit_progress(&app, "auth", "验证授权…", None, None);
    let (files, version) = {
        let m = manifest(&base, &token, &product_id, &variant_id)
            .await
            .map_err(|e| e.to_string())?;
        let latest = m["versions"]
            .as_array()
            .and_then(|a| a.iter().find(|v| v["is_latest"] == json!(true)))
            .or_else(|| m["versions"].as_array().and_then(|a| a.first()))
            .cloned()
            .ok_or_else(|| "该设备暂无可用包体".to_string())?;
        let files = latest["files"]
            .as_array()
            .cloned()
            .ok_or_else(|| "该版本没有文件".to_string())?;
        if files.is_empty() {
            return Err("该版本没有文件".to_string());
        }
        let version = latest["version"].as_str().unwrap_or("").to_string();
        (files, version)
    };

    // 服务端已按 sort_order（文件名自然序）返回，这里按序装。
    // 勾选安装时只装选中的包，进度里的「第 N/M 个表盘」仍按全量序号报 —— 与选择列表的
    // 表盘1..表盘M 编号一一对应（用户勾的是「表盘2」，进度就报「第 2/M 个」）。
    let total = files.len();
    let install_files: Vec<Value> = match &file_ids {
        Some(ids) if !ids.is_empty() => files
            .iter()
            .filter(|f| ids.iter().any(|id| id == f["file_id"].as_str().unwrap_or("")))
            .cloned()
            .collect(),
        _ => files.clone(),
    };
    if install_files.is_empty() {
        return Err("选中的表盘已不存在，请重新安装".to_string());
    }
    let (secret, fp, pub_b64) = with_ctx(&state, |c| {
        Ok((c.secret.clone(), c.fp.clone(), c.pub_b64.clone()))
    })
    .map_err(|e| e.to_string())?;

    let mut last_id: Option<String> = None;
    // 这一批装上去的表盘 id（记账用：管理页据此标「可更新」）
    let mut dial_ids: Vec<String> = Vec::new();
    for (idx, file) in install_files.iter().enumerate() {
        let file_id = file["file_id"].as_str().unwrap_or_default().to_string();
        let tag = if total > 1 {
            let n = files
                .iter()
                .position(|f| f["file_id"] == file["file_id"])
                .map(|i| i + 1)
                .unwrap_or(idx + 1);
            format!("第 {}/{} 个表盘", n, total)
        } else {
            String::new()
        };
        let phase = |base: &str| -> String {
            if tag.is_empty() {
                base.to_string()
            } else {
                format!("{tag} {base}")
            }
        };

        emit_progress(
            &app,
            "download",
            &phase("开始下载…"),
            Some(0),
            file["size"].as_u64(),
        );
        let app2 = app.clone();
        let tag2 = tag.clone();
        let plain = {
            let mut last_emit = std::time::Instant::now();
            let r = fetch_decrypt(
                &base,
                &token,
                &secret,
                &fp,
                &pub_b64,
                device_name.as_deref().unwrap_or("盘坊 App"),
                &file_id,
                |sent, total_bytes| {
                    if last_emit.elapsed().as_millis() >= 120 || sent == total_bytes {
                        last_emit = std::time::Instant::now();
                        let p = if tag2.is_empty() {
                            "下载中…".to_string()
                        } else {
                            format!("{tag2} 下载中…")
                        };
                        emit_progress(&app2, "download", &p, Some(sent), Some(total_bytes));
                    }
                },
            )
            .await;
            match r {
                Ok(v) => v,
                Err(e) => {
                    emit_progress(&app2, "error", &format!("{e}"), None, None);
                    return Err(e.to_string());
                }
            }
        };
        emit_progress(&app, "decrypt", &phase("解密完成，校验通过"), None, None);

        // 交给现有安装通道（它会发 wf-install-progress，并按表盘 id 各自留档 → 两个包可分别编辑）
        emit_progress(&app, "transfer", &phase("正在传输到手环…"), None, None);
        let id = crate::bridge::install_file_bytes_id(&app, &addr, plain, "bin".to_string())
            .await
            .map_err(|e| {
                let msg = phase(&format!("安装失败：{e}"));
                emit_progress(&app, "error", &msg, None, None);
                msg
            })?;
        if let Some(id) = id {
            dial_ids.push(id.clone());
            last_id = Some(id);
        }
    }

    // 记账：这个设备行现在装着的是哪个版本（更新提醒靠它）。
    // 写失败不影响安装结果（用户已经装好了），所以忽略错误。
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rec = InstallRecord {
        product_id: product_id.clone(),
        variant_id: variant_id.clone(),
        version: version.clone(),
        sha: manifest_sha(&files),
        dials: dial_ids,
        at,
    };
    let _ = with_ctx(&state, |c| {
        save_install(&c.dir, rec);
        Ok(())
    });

    let done = if total > 1 {
        format!("已安装 {} 个表盘（{version}）", install_files.len())
    } else {
        format!("已安装 {version}")
    };
    emit_progress(&app, "done", &done, None, None);
    Ok(last_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_seal_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wf-archive-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let plain = b"PANFANG-FACE-BYTES-0123456789".to_vec();
        let sealed = seal_archive(&dir, &plain).unwrap();
        assert!(sealed.starts_with(ARCHIVE_MAGIC), "应带密文头");
        assert!(!sealed.windows(8).any(|w| w == &plain[..8]), "密文里不该出现明文");
        let back = open_archive(&dir, &sealed).unwrap();
        assert_eq!(back, plain);
        // 老版本明文留档：没头 → 原样返回
        assert_eq!(open_archive(&dir, &plain).unwrap(), plain);
        // is_sealed 用来判断「要不要顺手升级成密文」
        assert!(is_sealed(&sealed));
        assert!(!is_sealed(&plain));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn url_encode_basic() {
        // RFC3986：'.' 属非保留字符，不转义；'@' 要转义
        assert_eq!(urlencode("a@b.c"), "a%40b.c");
    }
}
