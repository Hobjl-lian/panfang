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
use std::sync::atomic::{AtomicBool, Ordering};
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

/// 原子写文件（同目录写 `.tmp` → `sync_all` → `rename`），防止移动端强杀/断电截断半截 JSON 或密文。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("dat")
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, path)
}

/// 与授权服务通信用的 HTTP 客户端（付费链路与 App 自更新共用连接池）。
pub(crate) fn http() -> Result<reqwest::Client> {
    static CLIENT: OnceLock<std::result::Result<reqwest::Client, String>> = OnceLock::new();
    let res = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("Panfang/", env!("CARGO_PKG_VERSION"), " (watchface-installer)"))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
    });
    res.as_ref().cloned().map_err(|e| anyhow!("{e}"))
}

// ---------------------------------------------------------------- 登录态

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Session {
    pub license_token: String,
    pub expires_at: i64,
    pub mi_user_id: String,
    pub display: String,
    /// 爱发电 user_id（`a5d5fa40…`）：账号页要给买家截图用的唯一标识。
    /// `#[serde(default)]`：老登录态没有这两个字段，缺了不能让 session.enc 读不出来。
    #[serde(default)]
    pub afdian_user_id: String,
    /// 爱发电昵称（只有走 OAuth 绑定时拿得到；老绑定是空串，页面退回显示 user_id）。
    #[serde(default)]
    pub afdian_name: String,
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
        let fp = match std::fs::read_to_string(dir.join("android_id"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            // 跨卸载重装稳定：Kotlin 插件启动时把系统的 ANDROID_ID 写到 paid/android_id，
            // 原始值只在本机参与哈希，服务端只见到 sha256 结果。
            Some(id) => sha256_hex(format!("android:{id}").as_bytes()),
            None => sha256_hex(uuid.as_bytes()),
        };

        let base = std::fs::read_to_string(dir.join("base.txt"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .filter(|s| cfg!(debug_assertions) || s.starts_with("https://"))
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
        atomic_write(&self.dir.join("session.enc"), &out)?;
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
/// 安装时某个包的内容指纹。身份优先用**表盘 id**（跨版本稳定；重传/换包都不会变），
/// 无 id（空或全零）的老包退回文件名。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct InstalledPackage {
    #[serde(default)]
    pub watchface_id: String,
    #[serde(default)]
    pub file_name: String,
    /// 这个包在安装时的 `sha256_plain`
    #[serde(default)]
    pub sha: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InstallRecord {
    pub product_id: String,
    pub variant_id: String,
    pub version: String,
    /// 整行内容指纹（全量包体聚合）。老 App/老记录用它；新逻辑优先用 `packages`。
    pub sha: String,
    /// 这次装上车机（手环）的表盘 id：管理页标「可更新」时按它对上
    #[serde(default)]
    pub dials: Vec<String>,
    /// 安装时**最新版全部包**的快照（不只是这次勾选安装的几个）：
    /// 逐包判断「哪个包有更新」用。老记录没有这个字段 → 空，
    /// `paid_list` 会尝试用本机留档还原；还原不到再退回整行 `sha` 比较。
    #[serde(default)]
    pub packages: Vec<InstalledPackage>,
    /// 安装时间（unix 秒）
    pub at: i64,
}

/// 包的身份键：有有效表盘 id 用 id，否则用文件名。
fn package_key(watchface_id: Option<&str>, file_name: &str) -> String {
    let wid = watchface_id.unwrap_or("").trim();
    if wid.is_empty() || wid == "000000000000" {
        format!("name:{file_name}")
    } else {
        format!("id:{wid}")
    }
}

fn installed_package_key(p: &InstalledPackage) -> String {
    package_key(Some(&p.watchface_id), &p.file_name)
}

/// 老安装记录没有逐包快照：用本机留档（安装成功时按表盘 id 各存一份）还原
/// 「当时装进去的内容指纹」。找不到/解不开返回 None（这个包按「需要安装」处理，
/// 宁可多传一次也不漏更新）。
fn installed_sha_from_archive(
    app_data_dir: &Path,
    paid_dir: &Path,
    watchface_id: &str,
) -> Option<String> {
    let wid = watchface_id.trim();
    if wid.is_empty() || wid == "000000000000" {
        return None;
    }
    let root = app_data_dir.join("watchface_archive");
    // 同一块表盘可能装过两台设备（留档按 MAC 分目录）：取**最近一次**安装的那份
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for dev in std::fs::read_dir(root).ok()?.flatten() {
        let dir = dev.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            // `<id>.lua` 是集市 Lua 表盘的标记文件（空），不能当留档来算哈希
            if name.ends_with(".lua") {
                continue;
            }
            if name.split('.').next() != Some(wid) {
                continue;
            }
            let Ok(raw) = std::fs::read(&path) else {
                continue;
            };
            let Ok(plain) = open_archive(paid_dir, &raw) else {
                continue;
            };
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let sha = hex::encode(Sha256::digest(&plain));
            if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                best = Some((mtime, sha));
            }
        }
    }
    best.map(|(_, sha)| sha)
}

/// 逐包比较：返回与 `latest` 一一对应的 `(needs_update, is_new)`。
/// `installed` 为空时调用方应退回整行指纹比较（老记录且留档还原失败）。
fn package_flags(latest: &[Value], installed: &[InstalledPackage]) -> Vec<(bool, bool)> {
    latest
        .iter()
        .map(|p| {
            let key = package_key(
                p["watchface_id"].as_str(),
                p["file_name"].as_str().unwrap_or(""),
            );
            let latest_sha = p["sha256_plain"].as_str().unwrap_or("");
            match installed.iter().find(|x| installed_package_key(x) == key) {
                Some(old) => (old.sha != latest_sha, false),
                None => (true, true),
            }
        })
        .collect()
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
        let _ = atomic_write(&installs_path(dir), &b);
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

/// 设备身份（集市下载也用）：私钥、设备指纹、公钥（base64）。
pub(crate) fn device_identity(
    state: &AppState,
) -> Result<(StaticSecret, String, String)> {
    with_ctx(state, |c| Ok((c.secret.clone(), c.fp.clone(), c.pub_b64.clone())))
}

/// 当前授权服务地址（App 自更新也用它 —— 与付费链路同一个 base，联调时改一处即可）。
pub fn service_base(state: &AppState) -> Result<String, String> {
    with_ctx(state, |c| Ok(c.base.clone())).map_err(|e| e.to_string())
}

/// 视频表盘任务用：拿当前登录态（没绑定时返回 None，调用方给「先绑定爱发电」的提示）。
pub(crate) fn license_token(state: &AppState) -> Result<Option<String>> {
    with_ctx(state, |c| Ok(c.token().map(|t| t.to_string())))
}

// ---------------------------------------------------------------- 服务端交互

/// 服务端错误体是 `{ error, message }`：`message` 是给买家的中文说明，`error` 是稳定的机器码
/// （前端 `humanize` 靠它选文案/分类）。只取 message 会让 `device_limit`、`license_revoked`
/// 这类已知失败退化成「操作失败，请重试」——2026-10-17 真机上踩过。
pub(crate) fn api_error_text(v: &Value, fallback: &str) -> String {
    let msg = v["message"].as_str().unwrap_or(fallback);
    match v["error"].as_str() {
        Some(code) if !code.is_empty() => format!("{msg}（{code}）"),
        _ => msg.to_string(),
    }
}

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
        bail!("绑定失败：{}", api_error_text(&v, "服务端未开放联调绑定"));
    }
    Ok(Session {
        license_token: v["license_token"].as_str().unwrap_or_default().to_string(),
        expires_at: v["expires_at"].as_i64().unwrap_or(0),
        mi_user_id: v["subject"]["mi_user_id"].as_str().unwrap_or_default().to_string(),
        display: v["subject"]["display"].as_str().unwrap_or_default().to_string(),
        ..Default::default()
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
        bail!("{}", api_error_text(&v, "请求失败"));
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
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
        bail!("{}", api_error_text(&d, "获取下载凭证失败"));
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

pub(crate) fn emit_progress(app: &tauri::AppHandle, phase: &str, detail: &str, sent: Option<u64>, total: Option<u64>) {
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
pub async fn paid_device_screen(
    model: Option<String>,
    addr: Option<String>,
    device: Option<String>,
) -> Result<Value, String> {
    let mut scr = model
        .as_deref()
        .and_then(crate::watchface_edit::device_screen);
    // 小米型号表认不出时，vivo 设备的**名字**里通常带型号（vivo WATCH GT2 BF5），
    // 先按名字认；还认不出再向手表要 device_info.model。
    if scr.is_none() {
        if let Some(d) = device.as_deref() {
            scr = crate::watchface_edit::vivo_device_screen(d);
            if scr.is_some() {
                log::info!("[paid] 按设备名认出 vivo 屏幕 {d:?} → {scr:?}");
            }
        }
    }
    if scr.is_none() {
        if let Some(a) = addr.as_deref() {
            if crate::vivo::is_vivo(a).await {
                match crate::vivo::device_info(a).await {
                    Ok(info) => {
                        let m = info.get("model").and_then(Value::as_str).unwrap_or("");
                        log::info!("[paid] vivo 设备型号 model={m}");
                        scr = crate::watchface_edit::vivo_device_screen(m);
                    }
                    Err(e) => log::warn!("[paid] 读 vivo 设备信息失败：{e:#}"),
                }
            }
        }
    }
    Ok(json!({ "screen": scr.map(|(w, h)| vec![w, h]) }))
}

#[tauri::command]
pub async fn paid_status(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    // 小米账号 ID 取自**登录态**（与爱发电绑定无关）：账号页要显示它，买家用截图报障时
    // 卖家才能在订单表里按 miUserId 对上人。登录态里没有才退回登录会话里存的那份。
    let mi_uid_live = current_mi_uid(&state);
    with_ctx(&state, |c| {
        let mi_uid = if mi_uid_live.is_empty() {
            c.session
                .as_ref()
                .map(|s| s.mi_user_id.clone())
                .filter(|s| !s.starts_with("device:"))
        } else {
            Some(mi_uid_live.clone())
        };
        Ok(json!({
            "base": c.base,
            "device_fp": c.fp,
            "bound": c.token().is_some(),
            "mi_user_id": mi_uid,
            "display": c.session.as_ref().map(|s| s.display.clone()),
            "afdian_user_id": c.session.as_ref().map(|s| s.afdian_user_id.clone()),
            "afdian_name": c.session.as_ref().map(|s| s.afdian_name.clone()),
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

/// 绑定（仅开发联调模式可用；正式包禁用）
#[tauri::command]
pub async fn paid_bind(
    state: tauri::State<'_, AppState>,
    mi_user_id: String,
    device_name: Option<String>,
) -> Result<Value, String> {
    crate::dev_only("联调绑定")?;
    // 没显式给 UID 就用当前登录的小米账号；vivo 用户没有小米账号时用本机稳定主体键
    let mi_user_id = if mi_user_id.trim().is_empty() {
        current_subject_id(&state)?
    } else {
        mi_user_id.trim().to_string()
    };
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

/// 付费主体键：有小米登录就用小米 UID；vivo 用户没有小米账号时用本机稳定指纹。
///
/// 服务端只把它当字符串主键。`device:` 前缀让后台与账号页能区分「这不是小米 ID」，
/// 不会冒充小米账号；同一台手机重装后指纹稳定（Android ID 派生），权益还能找回。
fn current_subject_id(state: &tauri::State<'_, AppState>) -> Result<String, String> {
    // 已有会话主体优先：买过一次之后不因为「后来登录小米账号」而换主体，
    // 否则旧授权还挂在原主体上，新主体看不到已购（vivo 先买、后加小米手环就会踩到）。
    let existing = with_ctx(state, |c| {
        Ok(c.session
            .as_ref()
            .map(|s| s.mi_user_id.trim().to_string())
            .filter(|v| !v.is_empty()))
    })
    .unwrap_or(None);
    if let Some(v) = existing {
        return Ok(v);
    }
    let mi = current_mi_uid(state);
    if !mi.is_empty() {
        return Ok(mi);
    }
    with_ctx(state, |c| {
        let tail: String = c.fp.chars().take(16).collect();
        Ok(format!("device:{tail}"))
    })
    .map_err(|e| e.to_string())
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
        bail!("{}", api_error_text(&v, "暂时无法发起爱发电授权"));
    }
    Ok(v)
}

/// GET /v1/afdian/oauth/poll?state=… —— 用户在浏览器里授权完没有
pub async fn bind_oauth_poll(base: &str, state: &str) -> Result<Value> {
    let v = get_json(
        base,
        &format!("/v1/afdian/oauth/poll?state={}", urlencode(state)),
        None,
    )
    .await?;
    Ok(v)
}

/// 轮询到授权完成 → 把服务端备好的登录态存到本机（与联调绑定同一套存储）
pub fn save_oauth_session(state: &tauri::State<'_, AppState>, v: &Value) -> Result<Session> {
    let s = Session {
        license_token: v["license_token"].as_str().unwrap_or_default().to_string(),
        expires_at: v["expires_at"].as_i64().unwrap_or(0),
        mi_user_id: v["mi_user_id"].as_str().unwrap_or_default().to_string(),
        display: v["display"].as_str().unwrap_or_default().to_string(),
        afdian_user_id: v["afdian_user_id"].as_str().unwrap_or_default().to_string(),
        afdian_name: v["afdian_name"].as_str().unwrap_or_default().to_string(),
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
    let mi_uid = current_subject_id(&state)?;
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
            "afdian_user_id": s.afdian_user_id,
            "afdian_name": s.afdian_name,
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

/// 从 `/v1/me` 的 `afdian` 段补齐本机登录态里的爱发电身份（老服务端没有这一段就跳过）。
/// 只在值有变化时重写 session.enc，免得每次拉列表都落一次盘。
fn sync_identity_from_me(state: &tauri::State<'_, AppState>, v: &Value) {
    let afdian = &v["afdian"];
    if !afdian.is_object() {
        return;
    }
    let uid = afdian["user_id"].as_str().unwrap_or("").trim().to_string();
    let name = afdian["name"].as_str().unwrap_or("").trim().to_string();
    if uid.is_empty() && name.is_empty() {
        return;
    }
    let _ = with_ctx(state, |c| {
        let Some(mut s) = c.session.clone() else {
            return Ok(());
        };
        let mut changed = false;
        if !uid.is_empty() && s.afdian_user_id != uid {
            s.afdian_user_id = uid.clone();
            changed = true;
        }
        if !name.is_empty() && s.afdian_name != name {
            s.afdian_name = name.clone();
            changed = true;
        }
        if changed {
            c.save_session(s)?;
        }
        Ok(())
    });
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
    // 顺手把「我是谁」补进本机登录态：`/v1/me` 现在带 `afdian { user_id, name }`，
    // 这样 0.3.47 之前绑定的老登录态（里面只有尾号 4 位）也能在账号页显示爱发电 ID。
    sync_identity_from_me(&state, &v);
    let records = with_ctx(&state, |c| Ok(load_installs(&c.dir))).map_err(|e| e.to_string())?;
    let paid_dir = dir_of(&state);
    let app_data_dir = paid_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    let mut upgraded: Vec<InstallRecord> = Vec::new();
    if let Some(products) = v["products"].as_array_mut() {
        for p in products.iter_mut() {
            let pid = p["product_id"].as_str().unwrap_or("").to_string();
            let Some(vars) = p["variants"].as_array_mut() else { continue };
            for var in vars.iter_mut() {
                let vid = var["variant_id"].as_str().unwrap_or("").to_string();
                let rec = records
                    .iter()
                    .find(|r| r.product_id == pid && r.variant_id == vid)
                    .cloned();
                // 服务端下发的逐包指纹（老服务端没有这个字段）
                let latest_pkgs = var["packages"].as_array().cloned().unwrap_or_default();
                let (installed, installed_version, has_update) = match rec {
                    None => (false, Value::Null, false),
                    Some(mut r) => {
                        let latest_sha = var["latest_sha"].as_str().unwrap_or("");
                        let latest_ver = var["latest_version"].as_str().unwrap_or("");

                        // 老记录没有逐包快照：先用本机留档还原「每个包当时装的内容」。
                        // 这样升级 App 后第一次检查就能精确到包，不用把没变的包再传一遍。
                        if r.packages.is_empty() && !latest_pkgs.is_empty() {
                            let mut rebuilt = Vec::new();
                            for lp in &latest_pkgs {
                                let wid = lp["watchface_id"].as_str().unwrap_or("");
                                let fname = lp["file_name"].as_str().unwrap_or("");
                                if let Some(sha) =
                                    installed_sha_from_archive(&app_data_dir, &paid_dir, wid)
                                {
                                    rebuilt.push(InstalledPackage {
                                        watchface_id: wid.to_string(),
                                        file_name: fname.to_string(),
                                        sha,
                                    });
                                }
                            }
                            if !rebuilt.is_empty() {
                                r.packages = rebuilt;
                                upgraded.push(r.clone());
                            }
                        }

                        // 老口径：整行内容指纹（或版本号）比较
                        let fallback_up = || {
                            if !latest_sha.is_empty() && !r.sha.is_empty() {
                                latest_sha != r.sha
                            } else {
                                !latest_ver.is_empty() && latest_ver != r.version
                            }
                        };

                        let up = if !latest_pkgs.is_empty() && !r.packages.is_empty() {
                            // 精确到包：只有 sha 变了的包才需要更新；新包也算需要安装
                            let flags = package_flags(&latest_pkgs, &r.packages);
                            let has = flags.iter().any(|(needs, _)| *needs);
                            let pkgs: Vec<Value> = latest_pkgs
                                .iter()
                                .zip(flags.iter())
                                .map(|(lp, (needs, is_new))| {
                                    json!({
                                        "file_id": lp["file_id"],
                                        "file_name": lp["file_name"],
                                        "watchface_id": lp["watchface_id"],
                                        "version": lp["version"],
                                        "needs_update": needs,
                                        "new": is_new,
                                    })
                                })
                                .collect();
                            var["packages"] = json!(pkgs);
                            var["update_file_ids"] = json!(latest_pkgs
                                .iter()
                                .zip(flags.iter())
                                .filter(|(_, (needs, _))| *needs)
                                .map(|(lp, _)| lp["file_id"].clone())
                                .collect::<Vec<_>>());
                            has
                        } else if !latest_pkgs.is_empty() {
                            // 连一个包都没还原出来：退回整行指纹（老行为），所有包都算要更新
                            let up = fallback_up();
                            let pkgs: Vec<Value> = latest_pkgs
                                .iter()
                                .map(|lp| {
                                    json!({
                                        "file_id": lp["file_id"],
                                        "file_name": lp["file_name"],
                                        "watchface_id": lp["watchface_id"],
                                        "version": lp["version"],
                                        "needs_update": up,
                                        "new": false,
                                    })
                                })
                                .collect();
                            var["packages"] = json!(pkgs);
                            var["update_file_ids"] = json!(latest_pkgs
                                .iter()
                                .map(|lp| lp["file_id"].clone())
                                .collect::<Vec<_>>());
                            up
                        } else {
                            // 服务端还是老版本（没有逐包信息）：维持整行比较
                            fallback_up()
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
    // 老记录还原出的逐包快照写回一次（失败不影响本次判断）
    for r in upgraded {
        save_install(&paid_dir, r);
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

/// 「装完当前就停」的中断标记：付费安装一次只允许一个（前端有 busy 锁），
/// 用全局标记就够。进入 `paid_install` 时清零，循环里**每个包开始前**检查 ——
/// 点了停止也不会打断正在传输的那个包（用户要求：当前这个装完再停）。
static PAID_STOP_AFTER_CURRENT: AtomicBool = AtomicBool::new(false);

/// 请求「装完当前这个表盘就停」（多包体安装时的中断出口）。
#[tauri::command]
pub async fn paid_install_stop_after_current() -> Result<(), String> {
    PAID_STOP_AFTER_CURRENT.store(true, Ordering::SeqCst);
    Ok(())
}

#[derive(Clone, Debug)]
struct VivoDialInfo {
    id: String,
    version: Option<u64>,
    is_current: bool,
}

/// vivo 表盘列表快照。查询失败返回 None，不阻断安装。
async fn vivo_dial_snapshot(addr: &str) -> Option<Vec<VivoDialInfo>> {
    // 列表查询也走蓝牙；给它 8 秒上限，别让一次卡住的查询把整轮安装拖到看门狗超时。
    let value = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        crate::vivo::dial_list(addr),
    )
    .await
    .ok()?
    .ok()?;
    let items = value.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                let id = item.get("id")?.as_str()?.to_string();
                let version = item.get("versionCode").and_then(|v| v.as_u64());
                let is_current = item.get("isCurrent").and_then(|v| v.as_bool()).unwrap_or(false);
                Some(VivoDialInfo {
                    id,
                    version,
                    is_current,
                })
            })
            .collect(),
    )
}

/// 表盘剩余空间原始值的单位兜底：手表通常回字节；个别固件回 KB。
/// 小于 1MB 的值按 KB 估算（避免把 150000 当成 146KB）。
fn vivo_free_bytes_estimate(raw: u64) -> u64 {
    if raw >= 1_048_576 {
        raw
    } else {
        raw.saturating_mul(1024)
    }
}

/// 把 bridge 已经带上的「安装失败：」前缀剥掉，避免界面出现「安装失败：安装失败：…」。
fn strip_install_prefix(msg: &str) -> &str {
    msg.strip_prefix("安装失败：").unwrap_or(msg)
}

/// 查一次表盘分区剩余空间（8 秒上限）。查不到返回 None（老固件不支持/链路忙），
/// 调用方按「未知」处理 —— 不能因为查不到就不让装。
async fn vivo_dial_free_bytes(addr: &str) -> Option<u64> {
    let raw = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        crate::vivo::dial_free_storage(addr),
    )
    .await
    .ok()
    .and_then(|r| r.ok())?;
    // raw 太小说明不是字节单位（或者手表真的只剩几百字节），按未知处理，不做硬判断。
    if raw < 10_000 {
        log::info!("[paid] vivo dial free raw={raw} 单位不确定，按未知处理");
        return None;
    }
    Some(vivo_free_bytes_estimate(raw))
}

/// 单包安装需要的空间：手表要先落一份 rpk、再解包出一份表盘，按 2.5 倍算，另留 8MB 余量。
///
/// 为什么不是整批求和：买家经常只勾一个包，而且每装完一个空间都会掉；
/// 按单包算才跟「这一包能不能装上」对得上。
fn vivo_install_need_bytes(package_size: u64) -> u64 {
    package_size
        .saturating_mul(5)
        .saturating_div(2)
        .saturating_add(8 * 1_048_576)
}

/// 把空间快照写进诊断：买家报「装不上」时，先看这一组数就知道是不是空间问题。
fn diag_vivo_space(
    free: Option<u64>,
    need: u64,
    package_size: u64,
    dial_count: Option<usize>,
) {
    crate::diag::set_context(
        "vivo",
        serde_json::json!({
            "free_bytes": free,
            "free_mb": free.map(|v| v / 1_048_576),
            "package_bytes": package_size,
            "package_mb": package_size / 1_048_576,
            "need_mb": need / 1_048_576,
            "space_known": free.is_some(),
            "dial_count": dial_count,
        }),
    );
}

/// 同 ID 旧表盘先删后装（小米那边就是这条规矩）。
/// vivo 不允许删除当前表盘：如果目标正是当前表盘，先切到列表里另一张，再删。
async fn vivo_remove_existing_dial<F>(
    app: &tauri::AppHandle,
    addr: &str,
    phase: &F,
    target_id: &str,
    dials: &[VivoDialInfo],
) -> Result<Vec<VivoDialInfo>, String>
where
    F: Fn(&str) -> String,
{
    let Some(target) = dials.iter().find(|d| d.id == target_id) else {
        return Ok(dials.to_vec());
    };
    log::info!(
        "[paid] vivo dial {} 已在手表上（version {:?}，current={}），先卸载再重装",
        target.id,
        target.version,
        target.is_current
    );

    let mut dials_now = dials.to_vec();
    if target.is_current {
        let Some(other) = dials
            .iter()
            .find(|d| d.id != target_id && d.id.parse::<i64>().is_ok())
        else {
            return Err(
                "手表上只有这张表盘，无法删除当前表盘；请先在手表上切到别的表盘再回来重试"
                    .to_string(),
            );
        };
        emit_progress(app, "install", &phase("正在切换到另一张表盘…"), None, None);
        crate::vivo::set_current(addr, &other.id)
            .await
            .map_err(|e| format!("切换手表当前表盘失败：{e}"))?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        emit_progress(app, "install", &phase("正在刷新表盘列表…"), None, None);
        let Some(fresh) = vivo_dial_snapshot(addr).await else {
            return Err("切换表盘后没能刷新手表表盘列表，请重试".to_string());
        };
        dials_now = fresh;
    }

    emit_progress(app, "install", &phase("正在删除手表上的旧表盘…"), None, None);
    crate::vivo::uninstall(addr, target_id)
        .await
        .map_err(|e| format!("删除手表上的旧表盘失败：{e}"))?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    emit_progress(app, "install", &phase("正在刷新表盘列表…"), None, None);
    if let Some(fresh) = vivo_dial_snapshot(addr).await {
        dials_now = fresh;
    }
    Ok(dials_now)
}

/// 安装超时后回头查表盘列表：如果这张表盘已经在手表上，
/// 说明手表只是没回包（大包、忙、BLE 掉线），不能骗用户再装一遍。
async fn vivo_recover_installed_dial(
    addr: &str,
    expected: &Option<(String, Option<u64>)>,
) -> Option<(String, Vec<VivoDialInfo>)> {
    let (dial_id, expected_version) = expected.as_ref()?;
    let dials = vivo_dial_snapshot(addr).await?;
    let found = dials.iter().find(|d| d.id == *dial_id)?;
    // 版本对不上说明手表上还是旧表盘（很可能卸载没生效），不能按成功糊弄过去。
    if let (Some(expected), Some(installed)) =
        (expected_version.as_ref(), found.version.as_ref())
    {
        if expected != installed {
            log::warn!(
                "[paid] vivo dial {dial_id} 超时后仍在手表上，但版本 {installed} != 期望 {expected}，不按成功处理"
            );
            return None;
        }
    }
    Some((dial_id.clone(), dials))
}

/// 下载 → 解密（内存）→ 直装到设备；全程发 `wf-paid-progress`，安装阶段另有 `wf-install-progress`。
/// **一批多个包时按顺序依次安装**；用户点「装完当前就停」时，在包与包之间停下。
/// 返回 `{ lastId, installed, total, doneIds, cancelled }`：前端据此设当前表盘与显示「装了 N/M 个」。
/// 表盘 id 是设备往返解析出来的，个别情况下解析不出来 —— 那种情况**安装本身已经成功**，
/// `lastId` 给 None 让界面照常走成功页（不能反过来报「安装失败」骗用户重装）。
/// `file_ids` = 只装选中的这些包（多包体「勾选安装」）；None = 全装。
/// 记账：整行 `sha` 仍按全量包体算（老逻辑兼容）；逐包快照只更新这次**真正装了的**包，
/// 这次没勾（或中断没装）的包保留上次指纹（下次仍会提示它有更新）。
#[tauri::command]
pub async fn paid_install(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    addr: String,
    product_id: String,
    variant_id: String,
    device_name: Option<String>,
    file_ids: Option<Vec<String>>,
) -> Result<Value, String> {
    // 新一轮安装开始：清掉上一轮可能残留的中断请求与空间快照
    PAID_STOP_AFTER_CURRENT.store(false, Ordering::SeqCst);
    crate::diag::clear_context("vivo");
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

    // vivo：先看一眼当前表盘列表（同 ID 先删后装要用）。
    // 剩余空间不在这里按整批算：买家经常一次只勾一个，而且每装完一个空间都会掉，
    // 整批预检算不准 —— 改成进循环后逐包检查（见下面的「逐包空间预检」）。
    let is_vivo = crate::vivo::is_vivo(&addr).await;
    if is_vivo {
        // 这些查询也是蓝牙往返，先喂一口进度，别让前端 35 秒停滞看门狗先开枪。
        emit_progress(&app, "auth", "检查手表表盘列表…", None, None);
    }
    let mut vivo_dials = if is_vivo {
        vivo_dial_snapshot(&addr).await
    } else {
        None
    };

    let (secret, fp, pub_b64) = with_ctx(&state, |c| {
        Ok((c.secret.clone(), c.fp.clone(), c.pub_b64.clone()))
    })
    .map_err(|e| e.to_string())?;

    let mut last_id: Option<String> = None;
    // 这一批装上去的表盘 id（记账用：管理页据此标「可更新」）
    let mut dial_ids: Vec<String> = Vec::new();
    // 这次**真正装完**的包（中断时用来记账与回报「装了 N/M 个」）
    let mut completed: Vec<String> = Vec::new();
    let mut cancelled = false;
    for (idx, file) in install_files.iter().enumerate() {
        // 用户点了「装完当前就停」：上一个包已经完整装完，这里不再开始下一个。
        // 放在取包之前检查，保证不会出现「下到一半掐断」的半成品。
        if idx > 0 && PAID_STOP_AFTER_CURRENT.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
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

        // vivo 逐包空间预检：每装一个都会再吃掉一份 rpk + 一份表盘，整批只查一次算不准。
        // 查不到（老固件/链路忙）就照常装，但要把「未知」写进诊断，别再静默跳过。
        if is_vivo {
            // 进批前那次列表查询可能失败（链路忙）：没有列表就没法做「同 ID 先删后装」，
            // 半成品表盘会留在手表上。装第一个包之前再补一次。
            if vivo_dials.is_none() {
                emit_progress(&app, "auth", &phase("正在读取手表表盘列表…"), None, None);
                vivo_dials = vivo_dial_snapshot(&addr).await;
            }
            let package_size = file["size"].as_u64().unwrap_or(0);
            let need = vivo_install_need_bytes(package_size);
            emit_progress(&app, "auth", &phase("检查手表剩余空间…"), None, None);
            let free = vivo_dial_free_bytes(&addr).await;
            diag_vivo_space(
                free,
                need,
                package_size,
                vivo_dials.as_ref().map(|d| d.len()),
            );
            if let Some(free) = free {
                log::info!(
                    "[paid] vivo space precheck free={}MB need={}MB package={}MB",
                    free / 1_048_576,
                    need / 1_048_576,
                    package_size / 1_048_576
                );
                if package_size > 0 && free < need {
                    let msg = phase(&format!(
                        "安装失败：手表剩余空间不足（约 {} MB，这个表盘需要约 {} MB）。请先在手表上删掉几个不用的表盘，重启手表后再试",
                        (free / 1_048_576).max(1),
                        (need / 1_048_576).max(1),
                    ));
                    emit_progress(&app, "error", &msg, None, None);
                    return Err(msg);
                }
            }
        }

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
        // vivo：先看这张包的 dial_id/version，用于「同 ID 先删后装」和「超时后核实」。
        let mut vivo_expected: Option<(String, Option<u64>)> = None;
        if is_vivo {
            if let Ok((dial_id, _package, _version_name, version_code)) =
                crate::vivo::inspect_dial(&plain)
            {
                let version = u64::try_from(version_code).ok().filter(|v| *v > 0);
                vivo_expected = Some((dial_id.to_string(), version));
            }
        }

        // 手表上已经有同 ID 的表盘：先删掉再装（与小米一致），不再跳过。
        // 版本不同也删：买家要的是「干净重装」，旧版本留着只会让手表继续拒绝。
        if let Some((dial_id, _)) = &vivo_expected {
            let has_existing = vivo_dials
                .as_ref()
                .map(|d| d.iter().any(|x| x.id == *dial_id))
                .unwrap_or(false);
            if has_existing {
                let dials = vivo_dials.clone().unwrap_or_default();
                match vivo_remove_existing_dial(&app, &addr, &phase, dial_id, &dials).await {
                    Ok(fresh) => vivo_dials = Some(fresh),
                    Err(e) => {
                        let msg = phase(&format!("安装失败：{e}"));
                        emit_progress(&app, "error", &msg, None, None);
                        return Err(msg);
                    }
                }
            }
        }

        emit_progress(&app, "decrypt", &phase("解密完成，校验通过"), None, None);

        // 交给现有安装通道（它会发 wf-install-progress，并按表盘 id 各自留档 → 两个包可分别编辑）
        emit_progress(&app, "transfer", &phase("正在传输到手环…"), None, None);
        // 付费资源不允许复写 ID（最后一个参数恒为 None）。
        // 扩展名从包体文件名推导：vivo 的 .rpk 与小米 .bin 走同一个安装入口，按内容分派。
        let ext = file["file_name"]
            .as_str()
            .and_then(|n| n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()))
            .unwrap_or_else(|| "bin".to_string());
        let install_result =
            crate::bridge::install_file_bytes_id(&app, &addr, plain, ext, None).await;
        let installed_id: Option<String> = match install_result {
            Ok(id) => id,
            Err(e) => {
                // vivo 的 DialInstall 回包在「大包/手表忙/BLE 掉线」时可能丢；
                // 超时后先查一次列表，看手表上到底有没有这张表盘。
                let raw_msg = e.to_string();
                // 只对「等不到回包」做核实；code=5 是手表明确拒绝，不去猜。
                // 这里必须用 bridge 的分类函数：错误在返回前已经被翻成中文，
                // 按 `vivo_install:result_timeout` 原始前缀匹配永远匹配不到。
                let kind = crate::bridge::vivo_install_error_kind(&raw_msg);
                // 只核实 result_timeout：文件已经推完、DialInstall 也发了，手表可能只是回包丢了。
                // ack_timeout 是传输没完成、根本没走到安装，查列表没有意义，还白等 30 秒。
                let may_have_installed =
                    is_vivo && matches!(kind, Some("result_timeout"));
                let mut found_unconfirmed = None;
                if may_have_installed {
                    emit_progress(&app, "install", &phase("正在确认手表状态…"), None, None);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    // 手表装 10MB 大包时可能只是暂时忙，列表要过一会儿才更新。
                    // 单次查询容易误判「没装上」，最多查 3 次；每次查询前都喂一口进度，
                    // 别让前端 35 秒停滞看门狗把「未确认」盖成「安装停滞」。
                    for attempt in 0..3u8 {
                        if attempt > 0 {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                        emit_progress(
                            &app,
                            "install",
                            &phase(if attempt == 0 {
                                "正在读取手表表盘列表…"
                            } else {
                                "正在再次确认手表表盘列表…"
                            }),
                            None,
                            None,
                        );
                        if let Some(found) =
                            vivo_recover_installed_dial(&addr, &vivo_expected).await
                        {
                            found_unconfirmed = Some(found);
                            break;
                        }
                    }
                }
                if let Some((dial_id, _dials)) = found_unconfirmed {
                    // 列表里有，但没收到安装结果：不能当成功（它可能是半成品），
                    // 也不能让买家去点它 —— 半成品表盘会让手表在切换时直接崩掉。
                    log::warn!(
                        "[paid] vivo 安装没回结果，但手表上已有表盘 {dial_id}，按未确认处理：{raw_msg}"
                    );
                    let msg = phase(
                        "安装未确认：手表上已出现这张表盘，但没收到安装完成的结果。请先重启手表，暂时不要切换表盘；重新安装会先删除它再装",
                    );
                    emit_progress(&app, "error", &msg, None, None);
                    return Err(msg);
                }
                let msg = phase(&format!("安装失败：{}", strip_install_prefix(&e)));
                emit_progress(&app, "error", &msg, None, None);
                return Err(msg);
            }
        };
        if let Some(id) = installed_id {
            dial_ids.push(id.clone());
            last_id = Some(id);
        }
        // 走到这里 = 这个包完整装完了（下载/解密/传输都成功），记账与「装了 N/M 个」只算它
        completed.push(file_id.clone());

        // vivo：给手表一点落盘时间，再刷新列表给下一个包用。
        if is_vivo {
            emit_progress(&app, "install", &phase("手表正在保存表盘…"), None, None);
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            emit_progress(&app, "install", &phase("正在刷新手表表盘列表…"), None, None);
            if let Some(dials) = vivo_dial_snapshot(&addr).await {
                vivo_dials = Some(dials);
            }
        }
    }

    // 记账：这个设备行现在装着的是哪个版本（更新提醒靠它）。
    // 写失败不影响安装结果（用户已经装好了），所以忽略错误。
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // 上一次的逐包快照：这次没勾选的包要保留它原来的指纹（否则会被误记成「已最新」）
    let prev = with_ctx(&state, |c| Ok(load_installs(&c.dir)))
        .ok()
        .and_then(|all| {
            all.into_iter()
                .find(|r| r.product_id == product_id && r.variant_id == variant_id)
        });
    // 只把**真正装完**的包记成最新（中断时没开始的包要保留旧指纹，下次仍提示更新）
    let installed_ids: Vec<&str> = completed.iter().map(|s| s.as_str()).collect();
    let mut packages: Vec<InstalledPackage> = Vec::with_capacity(files.len());
    for f in files.iter() {
        let wid = f["watchface_id"].as_str().unwrap_or("");
        let fname = f["file_name"].as_str().unwrap_or("");
        let latest_sha = f["sha256_plain"].as_str().unwrap_or("");
        let was_installed = installed_ids.contains(&f["file_id"].as_str().unwrap_or(""));
        if was_installed {
            packages.push(InstalledPackage {
                watchface_id: wid.to_string(),
                file_name: fname.to_string(),
                sha: latest_sha.to_string(),
            });
            continue;
        }
        // 这次没装它：有上次记录就沿用上次指纹（仍会提示更新）；
        // 上次也没有（新包）且这次没勾 → 不写进快照，下次仍当「新增」提示。
        // 只有「首次安装 / 老记录还没还原」时退回老口径（记最新，不反复提示）。
        match &prev {
            Some(prev) if !prev.packages.is_empty() => {
                let key = package_key(Some(wid), fname);
                if let Some(old) = prev
                    .packages
                    .iter()
                    .find(|p| installed_package_key(p) == key)
                {
                    packages.push(InstalledPackage {
                        watchface_id: wid.to_string(),
                        file_name: fname.to_string(),
                        sha: old.sha.clone(),
                    });
                }
            }
            _ => packages.push(InstalledPackage {
                watchface_id: wid.to_string(),
                file_name: fname.to_string(),
                sha: latest_sha.to_string(),
            }),
        }
    }
    let rec = InstallRecord {
        product_id: product_id.clone(),
        variant_id: variant_id.clone(),
        version: version.clone(),
        sha: manifest_sha(&files),
        dials: dial_ids,
        packages,
        at,
    };
    let _ = with_ctx(&state, |c| {
        save_install(&c.dir, rec);
        Ok(())
    });

    let done = if cancelled {
        // 用户主动中断：说清「已经装了几个」，别让他以为全装完了
        format!(
            "已安装 {} 个表盘，其余已按你的选择停止",
            completed.len()
        )
    } else if total > 1 {
        format!("已安装 {} 个表盘（{version}）", completed.len())
    } else {
        format!("已安装 {version}")
    };
    emit_progress(&app, "done", &done, None, None);
    Ok(json!({
        "lastId": last_id,
        "installed": completed.len(),
        "total": install_files.len(),
        "doneIds": completed,
        "cancelled": cancelled,
    }))
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

    #[test]
    fn vivo_free_bytes_handles_bytes_and_kb() {
        // 字节单位原样用；KB 单位放大 1024；0 仍是 0。
        assert_eq!(vivo_free_bytes_estimate(150_000_000), 150_000_000);
        assert_eq!(vivo_free_bytes_estimate(150_000), 150_000 * 1024);
        assert_eq!(vivo_free_bytes_estimate(0), 0);
    }

    #[test]
    fn vivo_install_need_is_2_5x_plus_8mb() {
        // 空包也要留 8MB 底；10MB 的包按 2.5 倍 + 8MB = 33MB。
        assert_eq!(vivo_install_need_bytes(0), 8 * 1_048_576);
        assert_eq!(vivo_install_need_bytes(10 * 1_048_576), 33 * 1_048_576);
        // 超大值走饱和运算，不 panic、不溢出成小数。
        assert!(vivo_install_need_bytes(u64::MAX) > 1_000_000_000_000);
    }

    #[test]
    fn package_key_falls_back_to_name() {
        assert_eq!(package_key(Some("123"), "x.bin"), "id:123");
        assert_eq!(package_key(Some("000000000000"), "x.bin"), "name:x.bin");
        assert_eq!(package_key(Some("  "), "x.bin"), "name:x.bin");
    }

    /// 只更新第二个包时，只有它标「需要更新」；没变的不标，新包标 new
    #[test]
    fn package_flags_is_per_package() {
        let latest = vec![
            json!({"watchface_id":"111","file_name":"a.bin","sha256_plain":"sha-a"}),
            json!({"watchface_id":"222","file_name":"b.bin","sha256_plain":"sha-b2"}),
            json!({"watchface_id":"333","file_name":"c.bin","sha256_plain":"sha-c"}),
        ];
        let installed = vec![
            InstalledPackage {
                watchface_id: "111".into(),
                file_name: "a.bin".into(),
                sha: "sha-a".into(),
            },
            InstalledPackage {
                watchface_id: "222".into(),
                file_name: "b.bin".into(),
                sha: "sha-b1".into(),
            },
        ];
        assert_eq!(
            package_flags(&latest, &installed),
            vec![(false, false), (true, false), (true, true)]
        );
    }

    /// 老版本写的 installs.json 没有 packages 字段，必须还能读出来
    #[test]
    fn old_install_record_deserializes() {
        let old = r#"{"product_id":"p","variant_id":"v","version":"1.0.0","sha":"abc","dials":["111"],"at":1}"#;
        let rec: InstallRecord = serde_json::from_str(old).unwrap();
        assert!(rec.packages.is_empty());
        assert_eq!(rec.dials, vec!["111".to_string()]);
    }
}
