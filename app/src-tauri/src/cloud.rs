//! 小米云端补丁：登录（含 2FA）+ 拉取设备列表（含 authkey）+ 凭证持久化。
//! 登录与加密调用框架来自 AstroBox-NG account 模块（AGPL-3.0，署名要求见 README）；
//! get_source_list 调用形态参照环表工具箱的实现。

use account::xiaomi::{DeviceListRespone, MiAccountToken};
use anyhow::Context;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use tauri::State;

pub const LOGIN_UA: &str =
    "Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";

#[derive(Debug, Clone, Serialize)]
pub struct CloudDevice {
    pub name: String,
    pub model: String,
    pub mac: String,
    pub authkey: String,
    #[serde(rename = "beaconkey")]
    pub beacon_key: String,
    #[serde(rename = "fwVer")]
    pub fw_ver: String,
    pub identifier: String,
    pub status: u64,
}

impl CloudDevice {
    fn from_info(info: account::xiaomi::DeviceInfo) -> Option<Self> {
        let mac = info.detail.mac.trim().to_string();
        if mac.is_empty() {
            return None;
        }
        let authkey = info.detail.token.trim().to_string();
        if authkey.is_empty() {
            return None;
        }
        Some(Self {
            name: info.name,
            model: info.model,
            mac,
            authkey,
            beacon_key: info.detail.beaconkey,
            fw_ver: info.detail.fw_ver,
            identifier: info.identifier,
            status: info.status,
        })
    }
}

// ---------- 小米云端加密原语（与 account 模块实现一致，AGPL） ----------

mod mi_crypto {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use cipher::{KeyInit, StreamCipher};
    use rand::rngs::OsRng;
    use rand::RngCore;
    use rc4::{Rc4, consts::U32};
    use sha1::{Digest as _, Sha1};
    use sha2::Sha256;
    use std::collections::HashMap;

    pub fn calc_signed_nonce(ssecurity: &str, nonce: &str) -> anyhow::Result<String> {
        let mut hasher = Sha256::default();
        hasher.update(B64.decode(ssecurity)?);
        hasher.update(B64.decode(nonce)?);
        Ok(B64.encode(hasher.finalize()))
    }

    pub fn generate_nonce(millis: u64) -> String {
        let mut rand_part = [0u8; 8];
        OsRng.fill_bytes(&mut rand_part);
        let mut buf = Vec::with_capacity(12);
        buf.extend_from_slice(&rand_part);
        buf.extend_from_slice(&((millis / 60_000) as u32).to_be_bytes());
        B64.encode(buf)
    }

    pub fn generate_enc_signature(
        url_path: &str,
        method: &str,
        signed_nonce: &str,
        params: &HashMap<String, String>,
    ) -> String {
        let mut keys: Vec<&String> = params.keys().collect();
        keys.sort();
        let mut pieces = Vec::with_capacity(2 + keys.len() + 1);
        pieces.push(method.to_uppercase());
        pieces.push(url_path.to_owned());
        for k in keys {
            pieces.push(format!("{k}={}", params.get(k).unwrap()));
        }
        pieces.push(signed_nonce.to_owned());
        let raw = pieces.join("&");
        let mut sha1 = Sha1::default();
        sha1.update(raw.as_bytes());
        B64.encode(sha1.finalize())
    }

    /// Xiaomi 风格 RC4-DROP-1024（对称：加解密同一函数）。
    pub fn rc4_drop(signed_nonce: &str, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let key_bytes = B64.decode(signed_nonce)?;
        let mut cipher = Rc4::<U32>::new(rc4::Key::<U32>::from_slice(&key_bytes));
        let mut drop_buf = [0u8; 1024];
        cipher.apply_keystream(&mut drop_buf);
        let mut out = data.to_vec();
        cipher.apply_keystream(&mut out);
        Ok(out)
    }

    pub fn rc4_encrypt_params(
        signed_nonce: &str,
        params_plain: &HashMap<String, String>,
    ) -> anyhow::Result<HashMap<String, String>> {
        let mut keys: Vec<&String> = params_plain.keys().collect();
        keys.sort();
        let mut encrypted = HashMap::new();
        for k in keys {
            let data = params_plain.get(k).unwrap().as_bytes().to_vec();
            encrypted.insert(k.to_string(), B64.encode(rc4_drop(signed_nonce, &data)?));
        }
        Ok(encrypted)
    }
}

/// get_source_list 调用——逐字节对齐环表工具箱的可行实现：
/// 只带 cUserId / serviceToken / locale 三个 cookie，不附加任何多余头。
async fn get_source_list_call(token: &MiAccountToken) -> anyhow::Result<DeviceListRespone> {
    use base64::Engine as _;
    const PATH: &str = "/app/v1/source/get_source_list";

    let mut plain: HashMap<String, String> = HashMap::new();
    plain.insert("data".to_string(), r#"{"page_size":50,"status":1}"#.to_string());

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let nonce = mi_crypto::generate_nonce(millis);
    let signed_nonce = mi_crypto::calc_signed_nonce(&token.ssecurity, &nonce)?;

    let mut form = plain.clone();
    form.insert(
        "rc4_hash__".to_string(),
        mi_crypto::generate_enc_signature(PATH, "POST", &signed_nonce, &plain),
    );
    let mut encrypted = mi_crypto::rc4_encrypt_params(&signed_nonce, &form)?;
    encrypted.insert(
        "signature".to_string(),
        mi_crypto::generate_enc_signature(PATH, "POST", &signed_nonce, &encrypted),
    );
    encrypted.insert("_nonce".to_string(), nonce);

    let cookie_header = format!(
        "cUserId={}; serviceToken={}; locale=en_us;",
        token.c_user_id, token.service_token
    );

    let client = reqwest::Client::builder()
        .user_agent(LOGIN_UA)
        .build()?;
    let resp = client
        .post(format!("https://hlth.io.mi.com{PATH}"))
        .header("Cookie", cookie_header)
        .form(&encrypted)
        .send()
        .await
        .context("get_source_list 网络请求失败")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success(),
        "Mi API call failed: {status}, body: {body}"
    );

    let decrypted = mi_crypto::rc4_drop(
        &signed_nonce,
        &base64::engine::general_purpose::STANDARD.decode(body.trim_matches('"'))?,
    )?;
    let json = String::from_utf8_lossy(&decrypted).into_owned();

    let parsed: DeviceListRespone = serde_json::from_str(&json)
        .with_context(|| format!("解析设备列表失败: {json}"))?;
    Ok(parsed)
}

/// 拉取账号下已绑定设备列表（含密钥）。
pub async fn fetch_device_list(token: MiAccountToken) -> anyhow::Result<Vec<CloudDevice>> {
    log::info!(
        "[get_source_list] token shape: ssecurity_len={} service_token_len={} user_id_set={} c_user_id_set={} device_id_set={}",
        token.ssecurity.len(),
        token.service_token.len(),
        !token.user_id.is_empty(),
        !token.c_user_id.is_empty(),
        !token.device_id.is_empty()
    );

    let resp = get_source_list_call(&token).await?;
    anyhow::ensure!(
        resp.code == 0,
        "设备列表接口返回错误: code={}, msg={}",
        resp.code,
        resp.message
    );

    Ok(resp
        .result
        .list
        .into_iter()
        .filter_map(CloudDevice::from_info)
        .collect())
}

/// 小米账号密码登录；返回 None 表示需要 2FA（状态已存入应用状态，等待完成验证）。
pub async fn login(
    state: &State<'_, crate::bridge::AppState>,
    username: String,
    password: String,
) -> anyhow::Result<Option<Vec<CloudDevice>>> {
    // 持久化 deviceId：同一"设备"重复登录可显著降低触发短信验证的概率
    let device_id = load_or_create_device_id(state);
    let token = match account::xiaomi::login_mi_account_with_options(
        username,
        password,
        LOGIN_UA.to_string(),
        None,
        Some(device_id),
    )
    .await
    {
        Ok(token) => token,
        Err(err) => {
            // account 模块把 2FA 中间态打包进错误消息（"2-f-a=..." 多行载荷）
            let text = format!("{err:#}");
            if let Some(twofa) = TwoFaState::parse(&text) {
                *state.mi_2fa.lock() = Some(twofa.clone());
                return Ok(None);
            }
            return Err(err);
        }
    };
    let devices = finish_with_token(state, token).await?;
    Ok(Some(devices))
}

/// 读取或创建持久化的小米设备标识（6 位小写字母，与 account 模块随机格式一致）。
fn load_or_create_device_id(state: &State<'_, crate::bridge::AppState>) -> String {
    use rand::Rng;
    let generate = || -> String {
        let mut rng = rand::thread_rng();
        (0..6)
            .map(|_| (b'a' + rng.gen_range(0..26u8)) as char)
            .collect()
    };
    if let Some(dir) = &state.app_data_dir {
        let path = dir.join("mi_device_id");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
        let id = generate();
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&path, &id);
        return id;
    }
    generate()
}

/// 用 token 拉设备列表并持久化凭证。
pub async fn finish_with_token(
    state: &State<'_, crate::bridge::AppState>,
    token: MiAccountToken,
) -> anyhow::Result<Vec<CloudDevice>> {
    let devices = fetch_device_list(token.clone()).await?;
    *state.mi_token.lock() = Some(token.clone());
    save_token_file(state, &token);
    Ok(devices)
}

// ---------- 2FA：应用内 webview 完成流程（两种验证类型通用） ----------

const XIAOMI_SDK_VERSION: &str = "accountsdk-18.8.15";

fn insert_cookie_header_replica(
    store: &mut reqwest_cookie_store::CookieStore,
    url: &url::Url,
    cookie_header: &str,
) -> anyhow::Result<()> {
    for raw_pair in cookie_header.split(';') {
        let pair = raw_pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            continue;
        }
        store.insert_raw(
            &reqwest_cookie_store::RawCookie::new(name.to_string(), value.to_string()),
            url,
        )?;
    }
    Ok(())
}

/// 打开应用内验证窗口/受控浏览器（验证会话的 cookie 会留在里面，供完成后读取）。
/// 注意：必须是 async 命令——同步命令在主线程执行，创建窗口会死锁（窗口黑屏、应用未响应）。
///
/// 返回受控浏览器 id（仅移动端；桌面端为 None）——前端用它过滤**过期**的 `wf-2fa` 事件
/// （用户点「重新验证」会开新窗口，旧窗口关闭时会推一条「未完成」，不该盖掉新流程）。
#[tauri::command]
pub async fn xiaomi_2fa_open(
    app: tauri::AppHandle,
    state: State<'_, crate::bridge::AppState>,
) -> Result<Option<u32>, String> {
    let Some(twofa) = state.mi_2fa.lock().clone() else {
        return Err("没有进行中的二次验证".to_string());
    };

    #[cfg(mobile)]
    {
        use inappbrowser::InappbrowserExt;
        let browser = app.inappbrowser();
        let request = inappbrowser::OpenControlledRequest {
            url: twofa.url.clone(),
            title: Some("小米账号验证".to_string()),
            user_agent: None,
            // 验证完成后页面会跳转到 sts 链接，命中即上报并自动关窗（给用户明确的完成信号）
            intercept_prefixes: vec!["https://sts-hlth.io.mi.com/healthapp/sts".to_string()],
            close_on_intercept: true,
            ephemeral: true,
            width: None,
            height: None,
        };
        let id = browser
            .open_controlled(request)
            .await
            .map_err(|e| format!("打开验证窗口失败: {e}"))?;
        *state.mi_2fa_browser.lock() = Some(id);
        // ★ 自动收尾：用户不需要点任何「我已完成验证」（2026-10-06 用户要求）
        spawn_2fa_auto_complete(app.clone(), id);
        Ok(Some(id))
    }

    #[cfg(desktop)]
    {
        use tauri::Manager;
        if let Some(existing) = app.get_webview_window("2fa") {
            let _ = existing.set_focus();
            return Ok(None);
        }
        let url: url::Url = twofa
            .url
            .parse()
            .map_err(|e| format!("notificationUrl 无效: {e}"))?;
        tauri::webview::WebviewWindowBuilder::new(&app, "2fa", tauri::WebviewUrl::External(url))
            .title("小米账号验证")
            .inner_size(430.0, 740.0)
            .build()
            .map_err(|e| format!("打开验证窗口失败: {e}"))?;
        // 桌面端同理：轮询验证窗口里的会话 cookie，拿到就自动完成（不用点按钮）
        spawn_2fa_auto_complete_desktop(app.clone());
        Ok(None)
    }
}

/// 2FA 进度事件（`wf-2fa`）：`phase = verifying | done | error`。
///
/// `browserId` 用来让前端丢弃过期事件（用户可能「重新验证」开了新窗口）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TwoFaProgress {
    pub phase: String,
    pub message: String,
    pub browser_id: Option<u32>,
    /// 仅 `phase = done` 时带：已登录账号下的云端设备
    pub devices: Option<Vec<CloudDevice>>,
}

/// 2FA 自动收尾（移动端）：等受控浏览器命中拦截 → 换凭证 → 推事件给前端。
///
/// 插件语义（`inappbrowser/src/controlled.rs`）：命中拦截时 `wait_for_intercept` 返回
/// `Ok(命中 URL)`（**即使窗口已 close_on_intercept 自动关闭**）；用户提前关窗且没命中
/// 则返回 `Err("browser was closed")` —— 正好对应「成功」与「验证未完成」两条路径。
#[cfg(mobile)]
fn spawn_2fa_auto_complete(app: tauri::AppHandle, browser_id: u32) {
    use tauri::{Emitter, Manager};

    let bid = browser_id;
    let emit = move |app: &tauri::AppHandle, phase: &str, message: &str, devices: Option<Vec<CloudDevice>>| {
        let _ = app.emit(
            "wf-2fa",
            TwoFaProgress {
                phase: phase.to_string(),
                message: message.to_string(),
                browser_id: Some(bid),
                devices,
            },
        );
    };

    tauri::async_runtime::spawn(async move {
        use inappbrowser::InappbrowserExt;
        let hit = app.inappbrowser().wait_for_intercept(browser_id, None).await;
        let url = match hit {
            Ok(url) => url,
            Err(e) => {
                log::warn!("[2fa] 未命中拦截（{e}）→ 提示用户重新验证");
                emit(&app, "error", "验证未完成，请重新验证", None);
                return;
            }
        };
        log::info!("[2fa] 拦截命中，自动收尾：{url}");
        emit(&app, "verifying", "验证通过，正在完成登录…", None);
        // close_on_intercept 会立刻关窗；给原生层一点时间把会话 cookie 落盘
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let state = app.state::<crate::bridge::AppState>();
        match xiaomi_complete_2fa(app.clone(), state).await {
            Ok(devices) => {
                log::info!("[2fa] 自动收尾成功，云端设备 {} 台", devices.len());
                let msg = format!("已登录，找到 {} 台设备", devices.len());
                emit(&app, "done", &msg, Some(devices));
            }
            Err(e) => {
                log::warn!("[2fa] 自动收尾失败：{e}");
                emit(&app, "error", &format!("验证未完成，请重新验证（{e}）"), None);
            }
        }
    });
}

/// 2FA 自动收尾（桌面端，仅自测用）：轮询验证窗口的 cookie，能换到凭证就自动完成并关窗。
#[cfg(desktop)]
fn spawn_2fa_auto_complete_desktop(app: tauri::AppHandle) {
    use tauri::{Emitter, Manager};

    tauri::async_runtime::spawn(async move {
        let emit = |app: &tauri::AppHandle, phase: &str, message: &str, devices: Option<Vec<CloudDevice>>| {
            let _ = app.emit(
                "wf-2fa",
                TwoFaProgress {
                    phase: phase.to_string(),
                    message: message.to_string(),
                    browser_id: None,
                    devices,
                },
            );
        };
        // 最多轮询 5 分钟（2s 一次）；窗口被关掉就停
        for _ in 0..150 {
            tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
            let Some(win) = app.get_webview_window("2fa") else {
                return; // 用户自己关了窗口：不再自动收尾（前端会提示重新验证）
            };
            let state = app.state::<crate::bridge::AppState>();
            if let Ok(devices) = xiaomi_complete_2fa(app.clone(), state).await {
                log::info!("[2fa/desktop] 自动收尾成功，云端设备 {} 台", devices.len());
                let msg = format!("已登录，找到 {} 台设备", devices.len());
                let _ = win.close();
                emit(&app, "done", &msg, Some(devices));
                return;
            }
        }
        emit(&app, "error", "验证未完成，请重新验证", None);
    });
}

#[cfg(desktop)]
fn collect_webview_cookies(
    window: &tauri::WebviewWindow,
) -> Result<String, String> {
    use tauri::Manager;
    let mut pairs: Vec<(String, String)> = Vec::new();
    for url_text in [
        "https://account.xiaomi.com/pass/serviceLoginAuth2",
        "https://account.xiaomi.com/",
        "https://xiaomi.com/",
    ] {
        let Ok(url) = url::Url::parse(url_text) else { continue };
        let Ok(cookies) = window.cookies_for_url(url) else {
            continue;
        };
        for c in cookies {
            if !pairs.iter().any(|(n, _)| *n == c.name()) {
                pairs.push((c.name().to_string(), c.value().to_string()));
            }
        }
    }
    if pairs.is_empty() {
        return Err("验证窗口内没有读到任何会话 cookie（请确认验证已在弹出的窗口中完成）".to_string());
    }
    Ok(pairs
        .into_iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; "))
}

/// 从 cookie 头里取单个值（复刻 account::extract_cookie_value 语义）。
fn cookie_value(cookie_header: &str, name: &str) -> Option<String> {
    cookie_header.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_string())
    })
}

/// 用户在验证窗口完成验证后调用：读取 webview cookie → 重跑 serviceLogin → 换取完整凭证。
#[tauri::command]
pub async fn xiaomi_complete_2fa(
    app: tauri::AppHandle,
    state: State<'_, crate::bridge::AppState>,
) -> Result<Vec<CloudDevice>, String> {
    let Some(twofa) = state.mi_2fa.lock().clone() else {
        return Err("没有进行中的二次验证".to_string());
    };

    let cookie_header =
        collect_verified_cookies(&app, &state).await.map_err(|e| e.to_string())?;

    // 验证通过的会话属于验证窗口自己的 deviceId，后续请求必须沿用同一个，
    // 否则 serviceToken 与请求头 deviceId 不一致会报 401 auth err
    let browser_device_id = cookie_value(&cookie_header, "deviceId")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| twofa.device_id.clone());

    // 用验证后的 cookie 重跑 serviceLogin，拿完整（含 ssecurity）的会话响应
    let auth_resp = account::xiaomi::fetch_2fa_session_with_cookie(
        LOGIN_UA.to_string(),
        cookie_header.clone(),
        Some(browser_device_id.clone()),
    )
    .await
    .map_err(|e| format!("{e:#}"))?;

    // 复刻 finish_service_login_with_auth_response：跟随 location 到 STS 取 serviceToken
    let token = finalize_2fa_session(auth_resp, cookie_header, browser_device_id)
        .await
        .map_err(|e| format!("{e:#}"))?;

    finish_with_token(&state, token)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// 按平台收集验证会话 cookie。
#[cfg(desktop)]
async fn collect_verified_cookies(
    app: &tauri::AppHandle,
    _state: &State<'_, crate::bridge::AppState>,
) -> anyhow::Result<String> {
    use tauri::Manager;
    let window = app
        .get_webview_window("2fa")
        .ok_or_else(|| anyhow::anyhow!("验证窗口未打开，请先点「打开验证页面」"))?;
    let header = collect_webview_cookies(&window).map_err(|e| anyhow::anyhow!("{e}"))?;
    let _ = window.close();
    Ok(header)
}

#[cfg(mobile)]
async fn collect_verified_cookies(
    app: &tauri::AppHandle,
    state: &State<'_, crate::bridge::AppState>,
) -> anyhow::Result<String> {
    use inappbrowser::InappbrowserExt;
    let browser = app.inappbrowser();
    let id = state
        .mi_2fa_browser
        .lock()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("验证窗口未打开，请先点「打开验证页面」"))?;

    // 等待验证完成：页面跳转到 sts 链接会被拦截上报（最多等 5 分钟）
    let _intercepted = browser
        .wait_for_intercept(id, Some(300_000))
        .await
        .map_err(|e| anyhow::anyhow!("等待验证完成失败: {e}"))?;

    // 收集小米账号域的会话 cookie
    let mut pairs: Vec<(String, String)> = Vec::new();
    for url in [
        "https://account.xiaomi.com/",
        "https://account.xiaomi.com/pass/serviceLoginAuth2",
        "https://xiaomi.com/",
    ] {
        let cookies = browser
            .get_cookies(id, url.to_string())
            .await
            .map_err(|e| anyhow::anyhow!("读取验证 cookie 失败: {e}"))?;
        for c in cookies {
            if !pairs.iter().any(|(n, _)| *n == c.name) {
                pairs.push((c.name, c.value));
            }
        }
    }
    let _ = browser.close_controlled(id).await;
    *state.mi_2fa_browser.lock() = None;

    anyhow::ensure!(!pairs.is_empty(), "验证会话里没有读到 cookie");
    Ok(pairs
        .into_iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; "))
}

async fn finalize_2fa_session(
    auth_resp: account::xiaomi::ServiceLoginAuthRespone,
    cookie_header: String,
    device_id: String,
) -> anyhow::Result<account::xiaomi::MiAccountToken> {
    // userId/cUserId 兜底：部分响应不回填时，用验证会话 cookie 里的值
    let user_id_from_cookie = cookie_value(&cookie_header, "userId").unwrap_or_default();
    let c_user_id_from_cookie = cookie_value(&cookie_header, "cUserId").unwrap_or_default();
    anyhow::ensure!(
        !auth_resp.ssecurity.trim().is_empty(),
        "验证会话仍未返回 ssecurity（{}）",
        auth_resp.description
    );
    anyhow::ensure!(
        !auth_resp.location.trim().is_empty(),
        "验证会话缺少 location 跳转"
    );

    let cookie_store = std::sync::Arc::new(reqwest_cookie_store::CookieStoreMutex::new(
        reqwest_cookie_store::CookieStore::default(),
    ));
    let client = reqwest::Client::builder()
        .user_agent(LOGIN_UA)
        .cookie_provider(cookie_store.clone())
        .build()?;

    for domain in [
        "https://mi.com",
        "https://xiaomi.com",
        "https://account.xiaomi.com",
        "https://sts-hlth.io.mi.com",
    ] {
        let url = url::Url::parse(domain)?;
        let mut store = cookie_store.lock().unwrap();
        store.insert_raw(
            &reqwest_cookie_store::RawCookie::new("sdkVersion", XIAOMI_SDK_VERSION),
            &url,
        )?;
        store.insert_raw(
            &reqwest_cookie_store::RawCookie::new("deviceId", device_id.clone()),
            &url,
        )?;
        if domain.contains("xiaomi.com") {
            insert_cookie_header_replica(&mut store, &url, &cookie_header)?;
        }
    }

    let step3 = client
        .get(&auth_resp.location)
        .header("User-Agent", LOGIN_UA)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await?;
    let status = step3.status();
    if !status.is_success() {
        let body = step3.text().await.unwrap_or_default();
        anyhow::bail!("STS 跳转失败: {status}, body: {body}");
    }

    let store = cookie_store.lock().unwrap();
    let service_token = store
        .get("sts-hlth.io.mi.com", "/", "serviceToken")
        .map(|ck| ck.value().to_string())
        .ok_or_else(|| anyhow::anyhow!("STS 未返回 serviceToken cookie"))?;

    Ok(account::xiaomi::MiAccountToken {
        user_id: if auth_resp.user_id != 0 {
            auth_resp.user_id.to_string()
        } else {
            user_id_from_cookie
        },
        device_id,
        ssecurity: auth_resp.ssecurity,
        service_token,
        c_user_id: if !auth_resp.c_user_id.trim().is_empty() {
            auth_resp.c_user_id
        } else {
            c_user_id_from_cookie
        },
        pass_token: auth_resp.pass_token,
        psecurity: auth_resp.psecurity,
    })
}

/// 备用路径（推送确认型 2FA）：粘贴验证后浏览器地址栏的 sts 链接完成登录。
#[tauri::command]
pub async fn xiaomi_complete_2fa_sts(
    state: State<'_, crate::bridge::AppState>,
    sts_url: String,
) -> Result<Vec<CloudDevice>, String> {
    let Some(twofa) = state.mi_2fa.lock().clone() else {
        return Err("没有进行中的二次验证".to_string());
    };
    let token = account::xiaomi::complete_2fa_login_with_sts_url(
        twofa.ssecurity,
        twofa.c_user_id,
        sts_url.trim().to_string(),
        twofa.nonce,
        LOGIN_UA.to_string(),
        None,
    )
    .await
    .map_err(|e| format!("{e:#}"))?;
    finish_with_token(&state, token)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Clone, Serialize)]
pub struct TwoFaState {
    pub url: String,
    pub user_id: String,
    pub device_id: String,
    pub ssecurity: String,
    pub c_user_id: String,
    pub location: String,
    pub nonce: u64,
    pub psecurity: String,
    pub pass_token: String,
}

impl TwoFaState {
    /// 解析 account 模块打包在错误消息里的 2FA 载荷：
    /// `2-f-a={url}\nuser-id=...\ndevice-id=...\nssecurity=...\nc-user-id=...\nlocation=...\nnonce=...\npsecurity=...\npass-token=...`
    fn parse(text: &str) -> Option<Self> {
        if !text.contains("2-f-a=") {
            return None;
        }
        let mut map: HashMap<String, String> = HashMap::new();
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                map.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        Some(Self {
            url: map.get("2-f-a")?.clone(),
            user_id: map.get("user-id").cloned().unwrap_or_default(),
            device_id: map.get("device-id").cloned().unwrap_or_default(),
            ssecurity: map.get("ssecurity").cloned().unwrap_or_default(),
            c_user_id: map.get("c-user-id").cloned().unwrap_or_default(),
            location: map.get("location").cloned().unwrap_or_default(),
            nonce: map.get("nonce").and_then(|v| v.parse().ok()).unwrap_or(0),
            psecurity: map.get("psecurity").cloned().unwrap_or_default(),
            pass_token: map.get("pass-token").cloned().unwrap_or_default(),
        })
    }
}

// ---------- 凭证持久化 ----------

fn token_file_path(state: &State<'_, crate::bridge::AppState>) -> Option<PathBuf> {
    let dir = state.app_data_dir.as_ref()?;
    Some(dir.join("mi_token.json"))
}

fn save_token_file(state: &State<'_, crate::bridge::AppState>, token: &MiAccountToken) {
    if let Some(path) = token_file_path(state) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(token) {
            let _ = std::fs::write(&path, json);
        }
    }
}

fn load_token_file(state: &State<'_, crate::bridge::AppState>) -> Option<MiAccountToken> {
    let path = token_file_path(state)?;
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

// ---------- Tauri 命令 ----------

#[tauri::command]
pub async fn xiaomi_login(
    state: State<'_, crate::bridge::AppState>,
    username: String,
    password: String,
) -> Result<serde_json::Value, String> {
    match login(&state, username, password).await {
        Ok(Some(devices)) => Ok(serde_json::json!({ "requires2fa": false, "devices": devices })),
        Ok(None) => {
            let s = state.mi_2fa.lock().clone().expect("2fa state");
            Ok(serde_json::json!({ "requires2fa": true, "url": s.url }))
        }
        Err(err) => Err(format!("{err:#}")),
    }
}

#[tauri::command]
pub fn xiaomi_2fa_url(state: State<'_, crate::bridge::AppState>) -> Result<String, String> {
    state
        .mi_2fa
        .lock()
        .clone()
        .map(|s| s.url)
        .ok_or_else(|| "没有进行中的二次验证".to_string())
}

/// 启动时尝试用本地保存的凭证续期（passToken → serviceToken），成功则免登录。
#[tauri::command]
pub async fn xiaomi_restore(
    state: State<'_, crate::bridge::AppState>,
) -> Result<Vec<CloudDevice>, String> {
    let Some(token) = load_token_file(&state) else {
        return Err("无本地凭证".to_string());
    };
    let refreshed = account::xiaomi::refresh_mi_account_token(token.clone(), LOGIN_UA.to_string())
        .await
        .map_err(|e| format!("{e:#}"))?;
    let devices = finish_with_token(&state, refreshed)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(devices)
}

#[tauri::command]
pub fn xiaomi_logout(state: State<'_, crate::bridge::AppState>) -> () {
    *state.mi_token.lock() = None;
    *state.mi_2fa.lock() = None;
    if let Some(path) = token_file_path(&state) {
        let _ = std::fs::remove_file(path);
    }
}
