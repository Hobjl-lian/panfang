//! 爱发电私信助手：读会话列表 / 读对话 / 发消息。
//!
//! 为什么必须自己实现：爱发电开放平台（`/api/open/*`）只有订单能力，没有私信；
//! 私信只有网页版的内部接口，需要创作者本人的登录态（Cookie `auth_token`）。
//! 因此本模块：
//!   · 登录态由管理员填入，**用 KEK 加密后落库**，任何接口都不回显明文；
//!   · 所有出站请求只允许 `https://afdian.com` / `https://ifdian.net`（写死域名 + 禁重定向），
//!     不接受任何客户端传入的 URL（防 SSRF）；
//!   · 只做「读 + 手动发」，不做自动回复（自动回复要等通道稳定后再单独接）。
//!
//! 接口形状（2026-10 实测，来自网页版）：
//!   GET  /api/my/account?auth_token=…                    探活 + 昵称
//!   GET  /api/message/dialogs?page=1&unread=0            会话列表（data.list）
//!   GET  /api/message/messages?user_id=…&type=new        最近一页对话（data.list）
//!   POST /api/message/send { user_id, message_type:"1", content, auth_token }

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use reqwest::Url;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

use crate::crypto;
use crate::db::now;
use crate::state::{require_admin, ApiError, ApiResult, AppState};

const AFDIAN_BASE: &str = "https://afdian.com";
const IFDIAN_BASE: &str = "https://ifdian.net";
/// 出站只允许这两个域名：网页版主站 + 发消息接口所在域名（2026-10 实测）。
const ALLOWED_HOSTS: [&str; 2] = ["afdian.com", "ifdian.net"];
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
/// 密文用途绑定：同一把 KEK 下，这段密文不能当别的秘密用。
const TOKEN_AAD: &[u8] = b"wf-afdian-chat-token-v1";
const MAX_TOKEN_LEN: usize = 512;
const MAX_MESSAGE_CHARS: usize = 1500;

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            // 禁重定向：否则一个 302 就能把带 Cookie 的请求带出允许的域名
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("构建 HTTP 客户端")
    })
}

fn session_expired(msg: impl Into<String>) -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "afdian_session_expired",
        msg.into(),
    )
}

fn upstream(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "afdian_upstream", msg.into())
}

/// 只允许写死的两个域名、只允许 https、不允许 URL 里带账号密码。
/// 请求地址全部由本模块拼接，不接受任何客户端输入；这里是最后一道兜底。
fn validate_url(raw: &str) -> ApiResult<Url> {
    let url = Url::parse(raw).map_err(|_| upstream("爱发电地址无效"))?;
    if url.scheme() != "https" {
        return Err(upstream("爱发电地址必须是 HTTPS"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(upstream("爱发电地址不能包含账号密码"));
    }
    let host = url.host_str().ok_or_else(|| upstream("爱发电地址缺少域名"))?;
    if !ALLOWED_HOSTS.iter().any(|allowed| allowed == &host) {
        return Err(upstream(format!("拒绝访问未授权的域名：{host}")));
    }
    // 域名白名单已经排除了 IP 直连；这里再明确挡一次环回/私网/保留地址，
    // 防止将来有人把白名单改成可配置时漏掉这条。
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if ip.is_loopback() || ip.is_unspecified() || matches!(ip, std::net::IpAddr::V4(v4) if v4.is_private() || v4.is_link_local()) {
            return Err(upstream("拒绝访问内网地址"));
        }
    }
    Ok(url)
}

fn api_code(value: &Value) -> Option<i64> {
    value.get("ec").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

fn api_message(value: &Value) -> Option<String> {
    value
        .get("em")
        .or_else(|| value.get("msg"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn is_login_failure(message: &str) -> bool {
    const HINTS: [&str; 7] = [
        "请先登录",
        "尚未登录",
        "未登录",
        "登录已过期",
        "登录失效",
        "登录已失效",
        "请重新登录",
    ];
    HINTS.iter().any(|hint| message.contains(hint))
        || message.contains("HTTP 401")
        || message.contains("HTTP 403")
}

async fn parse_json(response: reqwest::Response, fallback: &str) -> ApiResult<Value> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|_| upstream("读取爱发电响应失败"))?;
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(session_expired("爱发电登录态已失效，请重新获取 Token"));
    }
    if !status.is_success() {
        return Err(upstream(format!("爱发电请求失败（HTTP {status}）")));
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|_| upstream("爱发电返回了无法解析的内容"))?;
    match api_code(&value) {
        Some(200) => Ok(value),
        Some(_) => {
            let message = api_message(&value).unwrap_or_else(|| fallback.to_string());
            if is_login_failure(&message) {
                Err(session_expired("爱发电登录态已失效，请重新获取 Token"))
            } else {
                Err(upstream(message))
            }
        }
        None => Err(upstream(fallback)),
    }
}

async fn get_json(
    path: &str,
    token: &str,
    query: &[(&str, &str)],
    fallback: &str,
) -> ApiResult<Value> {
    let url = validate_url(&format!("{AFDIAN_BASE}{path}"))?;
    let mut request = http()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", AFDIAN_BASE)
        .header("Cookie", format!("auth_token={token}"))
        .query(&[("auth_token", token)]);
    if !query.is_empty() {
        request = request.query(query);
    }
    let response = request
        .send()
        .await
        .map_err(|e| upstream(format!("爱发电网络请求失败：{e}")))?;
    parse_json(response, fallback).await
}

async fn post_json(path: &str, token: &str, body: &Value, fallback: &str) -> ApiResult<Value> {
    let url = validate_url(&format!("{IFDIAN_BASE}{path}"))?;
    let response = http()
        .post(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", AFDIAN_BASE)
        .header("Origin", AFDIAN_BASE)
        .header("Cookie", format!("auth_token={token}"))
        .json(body)
        .send()
        .await
        .map_err(|e| upstream(format!("爱发电网络请求失败：{e}")))?;
    parse_json(response, fallback).await
}

// ---------------------------------------------------------------- 登录态存取

fn validate_token(token: &str) -> ApiResult<String> {
    let token = token.trim();
    if token.is_empty() {
        return Err(ApiError::bad_request("请填写爱发电 auth_token"));
    }
    if token.chars().count() > MAX_TOKEN_LEN {
        return Err(ApiError::bad_request("auth_token 过长，请检查是否粘贴了整段 Cookie"));
    }
    if token.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ApiError::bad_request("auth_token 含空白或控制字符，请只粘贴值本身"));
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '=' | '+' | '/'))
    {
        return Err(ApiError::bad_request("auth_token 格式不正确，请只粘贴 auth_token 的值"));
    }
    Ok(token.to_string())
}

fn save_session(
    state: &AppState,
    token: &str,
    display_name: Option<&str>,
    afdian_user_id: Option<&str>,
) -> ApiResult<()> {
    let (nonce, cipher) = crypto::seal_secret(&state.kek, token.as_bytes(), TOKEN_AAD)
        .map_err(|e| ApiError::internal(format!("保存登录态失败：{e:#}")))?;
    let conn = state.conn();
    conn.execute(
        "INSERT INTO afdian_chat_session(id, token_nonce, token_cipher, display_name, afdian_user_id,
                                         checked_at, check_error, updated_at)
         VALUES(1, ?1, ?2, ?3, ?4, ?5, NULL, ?5)
         ON CONFLICT(id) DO UPDATE SET token_nonce = excluded.token_nonce,
             token_cipher = excluded.token_cipher, display_name = excluded.display_name,
             afdian_user_id = excluded.afdian_user_id, checked_at = excluded.checked_at,
             check_error = NULL, updated_at = excluded.updated_at",
        params![nonce, cipher, display_name, afdian_user_id, now()],
    )?;
    Ok(())
}

fn load_token(state: &AppState) -> ApiResult<String> {
    let row: Option<(Vec<u8>, Vec<u8>)> = {
        let conn = state.conn();
        conn.query_row(
            "SELECT token_nonce, token_cipher FROM afdian_chat_session WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
    };
    let (nonce, cipher) =
        row.ok_or_else(|| ApiError::bad_request("还没有配置爱发电私信登录态"))?;
    let plain = crypto::open_secret(&state.kek, &nonce, &cipher, TOKEN_AAD)
        .map_err(|_| ApiError::internal("登录态解密失败（KEK 不匹配或数据损坏）"))?;
    String::from_utf8(plain).map_err(|_| ApiError::internal("登录态内容损坏"))
}

struct SessionMeta {
    display_name: Option<String>,
    afdian_user_id: Option<String>,
    checked_at: Option<i64>,
    check_error: Option<String>,
}

fn load_meta(state: &AppState) -> SessionMeta {
    let conn = state.conn();
    conn.query_row(
        "SELECT display_name, afdian_user_id, checked_at, check_error
         FROM afdian_chat_session WHERE id = 1",
        [],
        |r| {
            Ok(SessionMeta {
                display_name: r.get(0)?,
                afdian_user_id: r.get(1)?,
                checked_at: r.get(2)?,
                check_error: r.get(3)?,
            })
        },
    )
    .unwrap_or(SessionMeta {
        display_name: None,
        afdian_user_id: None,
        checked_at: None,
        check_error: None,
    })
}

fn record_probe(state: &AppState, ok: bool, error: Option<&str>) {
    let conn = state.conn();
    if ok {
        let _ = conn.execute(
            "UPDATE afdian_chat_session SET checked_at = ?1, check_error = NULL WHERE id = 1",
            params![now()],
        );
    } else {
        let _ = conn.execute(
            "UPDATE afdian_chat_session SET check_error = ?1, updated_at = ?2 WHERE id = 1",
            params![error, now()],
        );
    }
}

fn configured(state: &AppState) -> bool {
    let conn = state.conn();
    conn.query_row(
        "SELECT 1 FROM afdian_chat_session WHERE id = 1",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

fn status_json(state: &AppState, meta: &SessionMeta, live_error: Option<String>) -> Value {
    json!({
        "configured": configured(state),
        "displayName": meta.display_name,
        "afdianUserId": meta.afdian_user_id,
        "checkedAt": meta.checked_at,
        "lastError": live_error.or_else(|| meta.check_error.clone()),
    })
}

async fn fetch_account(token: &str) -> ApiResult<Value> {
    get_json("/api/my/account", token, &[], "账号信息加载失败").await
}

fn account_name(data: &Value) -> Option<String> {
    [
        data.pointer("/data/oauth/wect/nickname"),
        data.pointer("/data/login/nickname"),
        data.pointer("/data/login/email"),
        data.pointer("/data/login/phone"),
        data.pointer("/data/user/name"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::trim)
    .find(|s| !s.is_empty())
    .map(str::to_string)
}

fn account_user_id(data: &Value) -> Option<String> {
    [
        data.pointer("/data/user_id"),
        data.pointer("/data/user/user_id"),
        data.pointer("/data/login/user_id"),
        data.pointer("/data/oauth/wect/user_id"),
    ]
    .into_iter()
    .flatten()
    .map(|v| match v {
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    })
    .find(|s| !s.is_empty())
}

// ---------------------------------------------------------------- 管理端接口

/// POST /admin/afdian/chat/token —— 保存登录态（先探活，验证不过不落库）
#[derive(Deserialize)]
pub struct TokenBody {
    pub token: String,
}

pub async fn chat_token_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TokenBody>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let token = validate_token(&body.token)?;
    let account = fetch_account(&token).await?;
    let name = account_name(&account);
    let uid = account_user_id(&account);
    save_session(&state, &token, name.as_deref(), uid.as_deref())?;
    record_probe(&state, true, None);
    let meta = load_meta(&state);
    Ok(Json(json!({
        "ok": true,
        "status": status_json(&state, &meta, None)
    })))
}

/// DELETE /admin/afdian/chat/token —— 清除登录态
pub async fn chat_token_clear(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    state
        .conn()
        .execute("DELETE FROM afdian_chat_session WHERE id = 1", [])?;
    Ok(Json(json!({ "ok": true })))
}

/// GET /admin/afdian/chat/status —— 状态；有登录态时顺手探活（失败只记录，不抛错）
pub async fn chat_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let meta = load_meta(&state);
    let mut live_error = None;
    if configured(&state) {
        match load_token(&state) {
            Ok(token) => match fetch_account(&token).await {
                Ok(_) => record_probe(&state, true, None),
                Err(e) => {
                    live_error = Some(e.message.clone());
                    record_probe(&state, false, Some(&e.message));
                }
            },
            Err(e) => live_error = Some(e.message),
        }
    }
    let meta = load_meta(&state);
    Ok(Json(status_json(&state, &meta, live_error)))
}

#[derive(Deserialize)]
pub struct DialogsQuery {
    #[serde(default)]
    pub page: Option<u32>,
}

/// GET /admin/afdian/chat/dialogs?page=1
pub async fn chat_dialogs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DialogsQuery>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let token = load_token(&state)?;
    let page = query.page.unwrap_or(1).clamp(1, 200).to_string();
    let value = get_json(
        "/api/message/dialogs",
        &token,
        &[("page", page.as_str()), ("unread", "0")],
        "私信列表加载失败",
    )
    .await
    .map_err(|e| {
        if e.error == "afdian_session_expired" {
            record_probe(&state, false, Some(&e.message));
        }
        e
    })?;

    let items = value
        .pointer("/data/list")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(parse_dialog).collect::<Vec<_>>())
        .unwrap_or_default();
    let total_page = value.pointer("/data/total_page").and_then(Value::as_i64);
    let has_more = value.pointer("/data/has_more").and_then(Value::as_i64) == Some(1)
        || total_page.map(|t| (query.page.unwrap_or(1) as i64) < t).unwrap_or(false);

    Ok(Json(json!({
        "items": items,
        "page": query.page.unwrap_or(1),
        "totalCount": value.pointer("/data/total_count").cloned().unwrap_or(Value::Null),
        "totalPage": total_page,
        "hasMore": has_more,
    })))
}

/// 把爱发电的 message.content 统一成可显示的文本。
///
/// 实测 `content` 不只有字符串：文本消息是字符串，而「发货成功」这类系统消息
/// （messageType 2/4/5）常常是嵌套对象或数组。这里优先取常见文本字段，
/// 取不到就递归找第一个非空字符串，避免系统消息在管理页变成「[非文本消息]」。
fn text_of(value: &Value) -> String {
    text_of_depth(value, 0)
}

/// 爱发电的图片消息 content 是 `<p><img src="…"></p>` 这种 HTML。
/// 页面里显示原始标签没有意义：含图片的标成「[图片]」，其余去掉标签、还原常用实体。
fn clean_text(raw: &str) -> String {
    let text = raw.trim();
    if !text.contains('<') {
        return text.to_string();
    }
    if text.contains("<img") {
        return "[图片]".to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn text_of_depth(value: &Value, depth: usize) -> String {
    const MAX_DEPTH: usize = 6;
    if depth > MAX_DEPTH {
        return String::new();
    }
    match value {
        Value::String(s) => clean_text(s),
        Value::Number(_) | Value::Bool(_) => value.to_string(),
        Value::Null => String::new(),
        Value::Array(items) => items
            .iter()
            .map(|item| text_of_depth(item, depth + 1))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => {
            const PREFERRED: [&str; 7] = [
                "text", "content", "msg", "message", "title", "url", "link",
            ];
            for key in PREFERRED {
                if let Some(inner) = map.get(key) {
                    let text = text_of_depth(inner, depth + 1);
                    if !text.trim().is_empty() {
                        return text;
                    }
                }
            }
            let mut best = String::new();
            for inner in map.values() {
                let text = text_of_depth(inner, depth + 1);
                // 结构未知时取最长的一段：短的往往是金额/编号，长的才是正文
                if text.trim().chars().count() > best.trim().chars().count() {
                    best = text;
                }
            }
            best
        }
    }
}

fn parse_dialog(record: &Value) -> Option<Value> {
    let user = record.get("user")?;
    let user_id = user.get("user_id").map(text_of).filter(|s| !s.is_empty())?;
    let name = user
        .get("name")
        .map(text_of)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "爱发电用户".to_string());
    Some(json!({
        "userId": user_id,
        "name": name,
        "avatar": user.get("avatar").cloned().unwrap_or(Value::Null),
        "preview": record.get("desc").map(text_of),
        "unreadCount": record.get("unread_count").and_then(Value::as_i64).unwrap_or(0),
        "totalCount": record.get("total_count").and_then(Value::as_i64).unwrap_or(0),
        "latestMessageId": record.get("latest_msg_id").map(text_of),
        "sentAt": record.get("send_time").cloned().unwrap_or(Value::Null),
    }))
}

/// 按消息类型决定显示内容：
///   · 1 = 普通文本，直接取正文；
///   · 4/5 等系统消息，尽量取正文（发货通知、兑换码）；
///   · 其余（卡片/图片）内部是订单 JSON 或 HTML，**不要把内部数据当正文**，
///     给一个稳定的标签，避免管理页出现 `[{"sku_id":…}]` 这种噪音。
fn display_content(message: &Value, kind: Option<i64>) -> String {
    let raw = message.get("content").cloned().unwrap_or(Value::Null);
    let text = text_of(&raw);
    if kind == Some(1) {
        return text;
    }
    let raw_string = raw.as_str().unwrap_or("");
    if raw_string.contains("<img") || text.contains("[图片]") {
        return "[图片]".to_string();
    }
    let trimmed = text.trim_start();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        return "[卡片消息]".to_string();
    }
    text
}

fn parse_message(record: &Value) -> Option<Value> {
    let message = record.get("message")?;
    let id = message
        .get("id")
        .or_else(|| message.get("msg_id"))
        .map(text_of)
        .filter(|s| !s.is_empty())?;
    let kind = message.get("type").and_then(Value::as_i64);
    Some(json!({
        "id": id,
        "direction": record.get("type").and_then(Value::as_str).unwrap_or("receive"),
        "sender": message.get("sender").cloned().unwrap_or(Value::Null),
        "messageType": kind,
        "content": display_content(message, kind),
        "sentAt": message.get("send_time").cloned().unwrap_or(Value::Null),
        "readStatus": message.get("r_status").and_then(Value::as_i64),
    }))
}

#[derive(Deserialize)]
pub struct MessagesQuery {
    pub user_id: String,
    #[serde(default)]
    pub message_id: Option<String>,
    /// new = 最新一页；old = 更早的历史（配合 message_id 翻页）
    #[serde(default, rename = "type", alias = "message_type")]
    pub kind: Option<String>,
}

/// GET /admin/afdian/chat/messages?user_id=…&type=new
pub async fn chat_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MessagesQuery>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let user_id = query.user_id.trim();
    if user_id.is_empty() || user_id.chars().count() > 128 {
        return Err(ApiError::bad_request("缺少对话用户 ID"));
    }
    let token = load_token(&state)?;
    let kind = if query.kind.as_deref() == Some("old") {
        "old"
    } else {
        "new"
    };
    let mut pairs = vec![("user_id", user_id), ("type", kind)];
    if let Some(mid) = query
        .message_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        pairs.push(("message_id", mid));
    }
    let value = get_json("/api/message/messages", &token, &pairs, "私信内容加载失败")
        .await
        .map_err(|e| {
            if e.error == "afdian_session_expired" {
                record_probe(&state, false, Some(&e.message));
            }
            e
        })?;

    let items = value
        .pointer("/data/list")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(parse_message).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut result = json!({
        "items": items,
        "hasMore": value.pointer("/data/has_more").and_then(Value::as_i64) == Some(1),
    });
    if let Some(list) = value.pointer("/data/list").and_then(Value::as_array) {
        if let (Some(first), Some(last)) = (list.first(), list.last()) {
            result["oldestMessageId"] = first
                .pointer("/message/id")
                .or_else(|| first.pointer("/message/msg_id"))
                .cloned()
                .unwrap_or(Value::Null);
            result["latestMessageId"] = last
                .pointer("/message/id")
                .or_else(|| last.pointer("/message/msg_id"))
                .cloned()
                .unwrap_or(Value::Null);
        }
    }
    Ok(Json(result))
}

#[derive(Deserialize)]
pub struct SendBody {
    pub user_id: String,
    pub content: String,
}

/// 发消息的请求体（以网页版 2026-10 的实际调用为准）：
/// `{ user_id, type:"1", content }`。`type` 是消息类型（1=文本、3=图片），
/// **不是** `message_type`——用错字段服务端会回「不支持的消息类型」。
/// 登录态走 Cookie，body 里不放 token。
fn send_payload(user_id: &str, content: &str) -> Value {
    json!({
        "user_id": user_id,
        "type": "1",
        "content": content,
    })
}

/// POST /admin/afdian/chat/send —— 手动发一条私信
pub async fn chat_send(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SendBody>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let user_id = body.user_id.trim();
    let content = body.content.trim();
    if user_id.is_empty() || user_id.chars().count() > 128 {
        return Err(ApiError::bad_request("缺少对话用户 ID"));
    }
    if content.is_empty() {
        return Err(ApiError::bad_request("消息内容不能为空"));
    }
    if content.chars().count() > MAX_MESSAGE_CHARS {
        return Err(ApiError::bad_request(format!(
            "消息太长（上限 {MAX_MESSAGE_CHARS} 字）"
        )));
    }
    let token = load_token(&state)?;
    let payload = send_payload(user_id, content);
    let value = post_json("/api/message/send", &token, &payload, "消息发送失败")
        .await
        .map_err(|e| {
            if e.error == "afdian_session_expired" {
                record_probe(&state, false, Some(&e.message));
            }
            e
        })?;
    let message_id = value
        .pointer("/data/id")
        .or_else(|| value.pointer("/data/msg_id"))
        .or_else(|| value.pointer("/data/message/id"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(Json(json!({ "ok": true, "messageId": message_id })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allows_the_two_hardcoded_https_hosts() {
        assert!(validate_url("https://afdian.com/api/message/dialogs").is_ok());
        assert!(validate_url("https://ifdian.net/api/message/send").is_ok());
        // 非 https
        assert!(validate_url("http://afdian.com/api/message/dialogs").is_err());
        // 白名单外（含环回/本机名/私网直连）
        assert!(validate_url("https://evil.example/api").is_err());
        assert!(validate_url("https://127.0.0.1/api").is_err());
        assert!(validate_url("https://localhost/api").is_err());
        assert!(validate_url("https://192.168.1.1/api").is_err());
        // 地址里塞账号密码
        assert!(validate_url("https://user:pass@afdian.com/api").is_err());
    }

    #[test]
    fn rejects_bad_tokens_before_calling_afdian() {
        assert!(validate_token("  ").is_err());
        assert!(validate_token("abc def").is_err());
        assert!(validate_token("a\nb").is_err());
        assert!(validate_token(&"a".repeat(MAX_TOKEN_LEN + 1)).is_err());
        assert_eq!(
            validate_token(" abcd-1234_ok.x ").unwrap(),
            "abcd-1234_ok.x"
        );
    }

    #[test]
    fn parses_dialog_items() {
        let raw = json!({
            "user": { "user_id": "u-1", "name": "买家", "avatar": "https://x/a.png" },
            "desc": "怎么安装",
            "unread_count": 2,
            "total_count": 9,
            "latest_msg_id": "m-9",
            "send_time": 1729000000
        });
        let parsed = parse_dialog(&raw).unwrap();
        assert_eq!(parsed["userId"], "u-1");
        assert_eq!(parsed["name"], "买家");
        assert_eq!(parsed["preview"], "怎么安装");
        assert_eq!(parsed["unreadCount"], 2);
        assert_eq!(parsed["latestMessageId"], "m-9");
    }

    #[test]
    fn parses_message_items_and_flattens_content() {
        let raw = json!({
            "type": "send",
            "message": {
                "id": "m-1",
                "sender": "creator",
                "type": 1,
                "content": "已发你邮箱",
                "send_time": 1729000001,
                "r_status": 1
            }
        });
        let parsed = parse_message(&raw).unwrap();
        assert_eq!(parsed["id"], "m-1");
        assert_eq!(parsed["direction"], "send");
        assert_eq!(parsed["content"], "已发你邮箱");

        let image = json!({ "url": "https://x/a.png" });
        assert_eq!(text_of(&image), "https://x/a.png");
        assert_eq!(text_of(&json!([1, 2])), "1\n2");
        // 系统消息（发货通知）常见嵌套形状：必须取到正文，不能变成占位符
        let notice = json!({
            "type": 4,
            "payload": { "title": "发货成功", "body": { "text": "感谢你的支持" } }
        });
        assert_eq!(text_of(&notice), "发货成功");
        assert_eq!(text_of(&json!({})), "");
        // 图片消息是 HTML，页面上应显示为「[图片]」而不是原始标签
        assert_eq!(text_of(&json!("<p><img src=\"https://pic1.afdiancdn.com/a.png\"></p>")), "[图片]");
        assert_eq!(text_of(&json!("<p>你好&nbsp;世界</p>")), "你好 世界");
    }

    #[test]
    fn card_messages_do_not_leak_internal_json() {
        let image = json!({ "type": 2, "content": "<p><img src=\"https://x/a.png\"></p>" });
        assert_eq!(display_content(&image, Some(2)), "[图片]");
        let card = json!({ "type": 2, "content": "[{\"sku_id\":\"4ef51b\",\"amount\":\"9.90\"}]" });
        assert_eq!(display_content(&card, Some(2)), "[卡片消息]");
        let notice = json!({ "type": 4, "content": { "text": "表盘已经发货成功" } });
        assert_eq!(display_content(&notice, Some(4)), "表盘已经发货成功");
        let normal = json!({ "type": 1, "content": "[这不是卡片]" });
        assert_eq!(display_content(&normal, Some(1)), "[这不是卡片]");
    }

    #[test]
    fn send_payload_matches_web_shape() {
        let payload = send_payload("u-1", "你好");
        assert_eq!(payload["user_id"], "u-1");
        assert_eq!(payload["type"], "1");
        assert_eq!(payload["content"], "你好");
        // 线上踩过：发 message_type 会回「不支持的消息类型」
        assert!(payload.get("message_type").is_none());
    }

    #[test]
    fn detects_login_failures() {
        assert!(is_login_failure("请先登录"));
        assert!(is_login_failure("请求失败 HTTP 401"));
        assert!(!is_login_failure("消息发送失败"));
    }
}
