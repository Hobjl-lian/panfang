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
use crate::db::{new_id, now};
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

// ---------------------------------------------------------------- 关键词自动回复

/// 轮询间隔：60 秒。服务端是固定 IP，保守一点比被风控划算；
/// 只拉会话列表，只有「最新消息变了」的会话才再拉一次内容。
const AUTO_REPLY_INTERVAL_SECS: u64 = 60;
/// 每轮最多回复几个会话：防止一次性给很多人发消息。
const AUTO_REPLY_MAX_PER_TICK: usize = 10;
/// 同一会话的冷却时间：10 分钟内不再自动回复，避免买卖双方来回触发。
const AUTO_REPLY_COOLDOWN_SECS: i64 = 600;
const MAX_RULES: usize = 50;
const MAX_KEYWORDS_PER_RULE: usize = 10;
const MAX_KEYWORD_CHARS: usize = 40;

#[derive(Debug, Clone)]
struct Rule {
    id: String,
    keywords: Vec<String>,
    match_mode: String,
    reply: String,
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub struct RuleIn {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default, rename = "matchMode", alias = "match_mode")]
    pub match_mode: Option<String>,
    #[serde(default)]
    pub reply: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct RulesBody {
    #[serde(default)]
    pub rules: Vec<RuleIn>,
}

#[derive(Deserialize)]
pub struct AutoReplyBody {
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct RuleTestBody {
    pub text: String,
}

fn load_rules(state: &AppState) -> Vec<Rule> {
    let conn = state.conn();
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, keywords, match_mode, reply, enabled FROM afdian_chat_rules
         ORDER BY sort, created_at",
    ) else {
        return Vec::new();
    };
    let rows: Vec<Rule> = stmt
        .query_map([], |r| {
            let keywords: String = r.get(1)?;
            Ok(Rule {
                id: r.get(0)?,
                keywords: serde_json::from_str::<Vec<String>>(&keywords).unwrap_or_default(),
                match_mode: r.get(2)?,
                reply: r.get(3)?,
                enabled: r.get::<_, i64>(4)? != 0,
            })
        })
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    rows
}

fn normalize_rules(input: &[RuleIn]) -> ApiResult<Vec<Rule>> {
    if input.len() > MAX_RULES {
        return Err(ApiError::bad_request(format!(
            "规则最多 {MAX_RULES} 条"
        )));
    }
    let mut rules = Vec::new();
    for item in input {
        let keywords: Vec<String> = item
            .keywords
            .iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        let reply = item.reply.trim().to_string();
        // 没填关键词或没填回复的行直接忽略（前端新增但没填完的行不该报错）
        if keywords.is_empty() || reply.is_empty() {
            continue;
        }
        if keywords.len() > MAX_KEYWORDS_PER_RULE {
            return Err(ApiError::bad_request(format!(
                "每条规则最多 {MAX_KEYWORDS_PER_RULE} 个关键词"
            )));
        }
        if keywords.iter().any(|k| k.chars().count() > MAX_KEYWORD_CHARS) {
            return Err(ApiError::bad_request(format!(
                "单个关键词最长 {MAX_KEYWORD_CHARS} 个字"
            )));
        }
        if reply.chars().count() > MAX_MESSAGE_CHARS {
            return Err(ApiError::bad_request(format!(
                "回复内容太长（上限 {MAX_MESSAGE_CHARS} 字）"
            )));
        }
        let match_mode = match item.match_mode.as_deref().unwrap_or("any") {
            "all" => "all",
            "exact" => "exact",
            _ => "any",
        }
        .to_string();
        let id = item
            .id
            .clone()
            .map(|v| v.trim().to_string())
            .filter(|v| {
                !v.is_empty()
                    && v.chars().count() <= 64
                    && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
            .unwrap_or_else(|| new_id("r_"));
        rules.push(Rule {
            id,
            keywords,
            match_mode,
            reply,
            enabled: item.enabled,
        });
    }
    Ok(rules)
}

fn replace_rules(state: &AppState, rules: &[Rule]) -> ApiResult<()> {
    let mut conn = state.conn();
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM afdian_chat_rules", [])?;
    for (index, rule) in rules.iter().enumerate() {
        let keywords = serde_json::to_string(&rule.keywords).unwrap_or_else(|_| "[]".into());
        tx.execute(
            "INSERT OR REPLACE INTO afdian_chat_rules(id, keywords, match_mode, reply, enabled, sort, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                rule.id,
                keywords,
                rule.match_mode,
                rule.reply,
                if rule.enabled { 1i64 } else { 0i64 },
                index as i64,
                now()
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[derive(Default)]
struct AutoSettings {
    enabled: bool,
    initialized: bool,
    last_poll_at: Option<i64>,
    last_poll_error: Option<String>,
}

fn load_settings(state: &AppState) -> AutoSettings {
    let conn = state.conn();
    conn.query_row(
        "SELECT auto_reply_enabled, initialized, last_poll_at, last_poll_error
         FROM afdian_chat_settings WHERE id = 1",
        [],
        |r| {
            Ok(AutoSettings {
                enabled: r.get::<_, i64>(0)? != 0,
                initialized: r.get::<_, i64>(1)? != 0,
                last_poll_at: r.get(2)?,
                last_poll_error: r.get(3)?,
            })
        },
    )
    .unwrap_or_default()
}

fn set_auto_reply_enabled(state: &AppState, enabled: bool) -> ApiResult<()> {
    // 先读旧状态再取连接锁：持锁再调用 load_settings 会自己等自己（死锁）
    let was_enabled = load_settings(state).enabled;
    let initialized = if enabled && !was_enabled { 0i64 } else { 1i64 };
    let conn = state.conn();
    // 从关到开：清掉基线，下一次轮询只记录当前进度、不回复历史消息
    conn.execute(
        "INSERT INTO afdian_chat_settings(id, auto_reply_enabled, initialized, updated_at)
         VALUES(1, ?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET auto_reply_enabled = excluded.auto_reply_enabled,
             initialized = excluded.initialized, updated_at = excluded.updated_at",
        params![if enabled { 1i64 } else { 0i64 }, initialized, now()],
    )?;
    Ok(())
}

fn mark_initialized(state: &AppState) {
    let conn = state.conn();
    let _ = conn.execute(
        "UPDATE afdian_chat_settings SET initialized = 1, updated_at = ?1 WHERE id = 1",
        params![now()],
    );
}

fn mark_poll(state: &AppState, error: Option<&str>) {
    let conn = state.conn();
    let _ = conn.execute(
        "UPDATE afdian_chat_settings SET last_poll_at = ?1, last_poll_error = ?2, updated_at = ?1
         WHERE id = 1",
        params![now(), error],
    );
}

#[derive(Default, Clone)]
struct UserState {
    last_seen: Option<String>,
    last_replied: Option<String>,
    last_reply_at: Option<i64>,
}

fn load_user_state(state: &AppState, user_id: &str) -> UserState {
    let conn = state.conn();
    conn.query_row(
        "SELECT last_seen_msg_id, last_replied_msg_id, last_reply_at
         FROM afdian_chat_autoreply_state WHERE user_id = ?1",
        [user_id],
        |r| {
            Ok(UserState {
                last_seen: r.get(0)?,
                last_replied: r.get(1)?,
                last_reply_at: r.get(2)?,
            })
        },
    )
    .unwrap_or_default()
}

fn touch_seen(state: &AppState, user_id: &str, message_id: Option<&str>) {
    let conn = state.conn();
    let _ = conn.execute(
        "INSERT INTO afdian_chat_autoreply_state(user_id, last_seen_msg_id, updated_at)
         VALUES(?1, ?2, ?3)
         ON CONFLICT(user_id) DO UPDATE SET last_seen_msg_id = excluded.last_seen_msg_id,
             updated_at = excluded.updated_at, last_error = NULL",
        params![user_id, message_id, now()],
    );
}

fn mark_replied(state: &AppState, user_id: &str, seen_id: &str, reply_at: i64) {
    let conn = state.conn();
    let _ = conn.execute(
        "INSERT INTO afdian_chat_autoreply_state(user_id, last_seen_msg_id, last_replied_msg_id, last_reply_at, updated_at)
         VALUES(?1, ?2, ?2, ?3, ?3)
         ON CONFLICT(user_id) DO UPDATE SET last_seen_msg_id = excluded.last_seen_msg_id,
             last_replied_msg_id = excluded.last_replied_msg_id,
             last_reply_at = excluded.last_reply_at, updated_at = excluded.updated_at,
             last_error = NULL",
        params![user_id, seen_id, reply_at],
    );
}

fn set_user_error(state: &AppState, user_id: &str, error: &str) {
    let conn = state.conn();
    let _ = conn.execute(
        "INSERT INTO afdian_chat_autoreply_state(user_id, last_error, updated_at)
         VALUES(?1, ?2, ?3)
         ON CONFLICT(user_id) DO UPDATE SET last_error = excluded.last_error,
             updated_at = excluded.updated_at",
        params![user_id, error, now()],
    );
}

fn log_autoreply(
    state: &AppState,
    user_id: &str,
    user_name: &str,
    inbound_id: &str,
    outbound_id: &str,
    rule_id: &str,
    keyword: &str,
    content: &str,
) {
    let conn = state.conn();
    let _ = conn.execute(
        "INSERT INTO afdian_chat_autoreply_log
             (user_id, user_name, inbound_msg_id, outbound_msg_id, rule_id, matched_keyword, content, ts)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![user_id, user_name, inbound_id, outbound_id, rule_id, keyword, content, now()],
    );
    // 只保留最近 200 条，避免无限增长（管理页只显示 20 条）
    let _ = conn.execute(
        "DELETE FROM afdian_chat_autoreply_log WHERE id NOT IN
             (SELECT id FROM afdian_chat_autoreply_log ORDER BY id DESC LIMIT 200)",
        [],
    );
}

fn normalize_match(text: &str) -> String {
    text.trim().to_lowercase()
}

/// 关键词命中判定：any=包含任意一个，all=全部包含，exact=完全等于。
/// 返回命中的关键词（all 返回全部，用「 + 」连接），用于审计日志。
fn rule_hit(text: &str, rule: &Rule) -> Option<String> {
    if !rule.enabled || rule.reply.trim().is_empty() {
        return None;
    }
    let normalized = normalize_match(text);
    if normalized.is_empty() {
        return None;
    }
    let keywords: Vec<&String> = rule
        .keywords
        .iter()
        .filter(|k| !normalize_match(k).is_empty())
        .collect();
    if keywords.is_empty() {
        return None;
    }
    match rule.match_mode.as_str() {
        "exact" => keywords
            .iter()
            .find(|k| normalize_match(k) == normalized)
            .map(|k| k.trim().to_string()),
        "all" => {
            if keywords
                .iter()
                .all(|k| normalized.contains(&normalize_match(k)))
            {
                Some(
                    keywords
                        .iter()
                        .map(|k| k.trim().to_string())
                        .collect::<Vec<_>>()
                        .join(" + "),
                )
            } else {
                None
            }
        }
        _ => keywords
            .iter()
            .find(|k| normalized.contains(&normalize_match(k)))
            .map(|k| k.trim().to_string()),
    }
}

fn find_rule<'a>(text: &str, rules: &'a [Rule]) -> Option<(&'a Rule, String)> {
    rules
        .iter()
        .find_map(|rule| rule_hit(text, rule).map(|keyword| (rule, keyword)))
}

struct DialogRow {
    user_id: String,
    name: String,
    latest_id: Option<String>,
}

fn parse_dialog_rows(value: &Value) -> Vec<DialogRow> {
    value
        .pointer("/data/list")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|record| {
                    let user = record.get("user")?;
                    let user_id = user.get("user_id").map(text_of).filter(|s| !s.is_empty())?;
                    let name = user
                        .get("name")
                        .map(text_of)
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| "爱发电用户".to_string());
                    let latest_id = record
                        .get("latest_msg_id")
                        .map(text_of)
                        .filter(|s| !s.is_empty());
                    Some(DialogRow {
                        user_id,
                        name,
                        latest_id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_message_list(value: &Value) -> Vec<Value> {
    value
        .pointer("/data/list")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(parse_message).collect())
        .unwrap_or_default()
}

/// 对方发来的最后一段连续文字（遇到我们自己的消息或非文本消息就停）。
fn trailing_inbound_text(items: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for item in items.iter().rev() {
        if item["direction"] != "receive" || item["messageType"] != json!(1) {
            break;
        }
        let content = item["content"].as_str().unwrap_or("").trim();
        if !content.is_empty() {
            parts.push(content.to_string());
        }
    }
    parts.reverse();
    parts.join("\n")
}

fn response_message_id(value: &Value) -> String {
    value
        .pointer("/data/id")
        .or_else(|| value.pointer("/data/msg_id"))
        .or_else(|| value.pointer("/data/message/id"))
        .map(text_of)
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

/// 启动后台轮询（服务启动时调用一次）。默认关闭；管理页打开开关后才真正发消息。
pub fn spawn_auto_reply_worker(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(AUTO_REPLY_INTERVAL_SECS));
        tick.tick().await; // interval 的第一次是立即触发的，先吃掉
        loop {
            tick.tick().await;
            if !load_settings(&state).enabled {
                continue;
            }
            match auto_reply_tick(&state).await {
                Ok(()) => {}
                Err(error) => {
                    println!("[afdian-chat] 自动回复失败：{}", error.message);
                    mark_poll(&state, Some(&error.message));
                }
            }
        }
    });
}

async fn auto_reply_tick(state: &AppState) -> ApiResult<()> {
    let settings = load_settings(state);
    let rules: Vec<Rule> = load_rules(state)
        .into_iter()
        .filter(|rule| rule.enabled && !rule.keywords.is_empty() && !rule.reply.trim().is_empty())
        .collect();
    if rules.is_empty() {
        mark_poll(state, Some("还没有可用的关键词规则"));
        return Ok(());
    }
    let token = load_token(state)?;
    let value = get_json(
        "/api/message/dialogs",
        &token,
        &[("page", "1"), ("unread", "0")],
        "私信列表加载失败",
    )
    .await
    .map_err(|error| {
        if error.error == "afdian_session_expired" {
            record_probe(state, false, Some(&error.message));
        }
        error
    })?;
    let dialogs = parse_dialog_rows(&value);

    // 刚开启：只记录当前每个会话的最新消息，历史未读一律不回
    if !settings.initialized {
        for dialog in &dialogs {
            touch_seen(state, &dialog.user_id, dialog.latest_id.as_deref());
        }
        mark_initialized(state);
        mark_poll(state, None);
        return Ok(());
    }

    let mut budget = AUTO_REPLY_MAX_PER_TICK;
    let mut failed = 0usize;
    for dialog in dialogs {
        if budget == 0 {
            break;
        }
        let Some(latest) = dialog.latest_id.as_deref() else {
            continue;
        };
        let user_state = load_user_state(state, &dialog.user_id);
        if user_state.last_seen.as_deref() == Some(latest) {
            continue;
        }
        // 第一次见到这个会话：只记基线，不回
        if user_state.last_seen.is_none() {
            touch_seen(state, &dialog.user_id, Some(latest));
            continue;
        }
        if user_state.last_replied.as_deref() == Some(latest) {
            touch_seen(state, &dialog.user_id, Some(latest));
            continue;
        }
        if let Some(ts) = user_state.last_reply_at {
            if ts > now() - AUTO_REPLY_COOLDOWN_SECS {
                touch_seen(state, &dialog.user_id, Some(latest));
                continue;
            }
        }

        let page = match get_json(
            "/api/message/messages",
            &token,
            &[("user_id", dialog.user_id.as_str()), ("type", "new")],
            "私信内容加载失败",
        )
        .await
        {
            Ok(page) => page,
            Err(error) => {
                if error.error == "afdian_session_expired" {
                    return Err(error);
                }
                set_user_error(state, &dialog.user_id, &error.message);
                failed += 1;
                continue;
            }
        };
        let items = parse_message_list(&page);
        let Some(last) = items.last() else {
            touch_seen(state, &dialog.user_id, Some(latest));
            continue;
        };
        // 最后一条必须还是对方发来的文字（我们手动回过、或只是系统消息，都不回）
        if last["direction"] != json!("receive") || last["messageType"] != json!(1) {
            touch_seen(state, &dialog.user_id, Some(latest));
            continue;
        }
        let inbound = trailing_inbound_text(&items);
        if inbound.trim().is_empty() {
            touch_seen(state, &dialog.user_id, Some(latest));
            continue;
        }
        let Some((rule, keyword)) = find_rule(&inbound, &rules) else {
            touch_seen(state, &dialog.user_id, Some(latest));
            continue;
        };

        match post_json(
            "/api/message/send",
            &token,
            &send_payload(&dialog.user_id, &rule.reply),
            "消息发送失败",
        )
        .await
        {
            Ok(sent) => {
                let outbound_id = response_message_id(&sent);
                mark_replied(state, &dialog.user_id, latest, now());
                log_autoreply(
                    state,
                    &dialog.user_id,
                    &dialog.name,
                    latest,
                    &outbound_id,
                    &rule.id,
                    &keyword,
                    &rule.reply,
                );
                println!(
                    "[afdian-chat] 自动回复 {}（命中「{}」）",
                    dialog.name, keyword
                );
                budget -= 1;
            }
            Err(error) => {
                if error.error == "afdian_session_expired" {
                    return Err(error);
                }
                set_user_error(state, &dialog.user_id, &error.message);
                failed += 1;
            }
        }
    }
    mark_poll(
        state,
        if failed > 0 {
            Some("部分会话自动回复失败，详见下方最近记录")
        } else {
            None
        },
    );
    Ok(())
}

// ---------------------------------------------------------------- 自动回复管理接口

fn rules_json(rules: &[Rule]) -> Value {
    Value::Array(
        rules
            .iter()
            .map(|rule| {
                json!({
                    "id": rule.id,
                    "keywords": rule.keywords,
                    "matchMode": rule.match_mode,
                    "reply": rule.reply,
                    "enabled": rule.enabled,
                })
            })
            .collect(),
    )
}

/// GET /admin/afdian/chat/rules —— 规则 + 开关状态 + 最近自动回复
pub async fn chat_rules_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let settings = load_settings(&state);
    let rules = load_rules(&state);
    let logs: Vec<Value> = {
        let conn = state.conn();
        let mut stmt = conn.prepare(
            "SELECT user_name, content, matched_keyword, ts FROM afdian_chat_autoreply_log
             ORDER BY id DESC LIMIT 20",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(json!({
                    "userName": r.get::<_, Option<String>>(0)?,
                    "content": r.get::<_, String>(1)?,
                    "keyword": r.get::<_, Option<String>>(2)?,
                    "ts": r.get::<_, i64>(3)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    Ok(Json(json!({
        "enabled": settings.enabled,
        "initialized": settings.initialized,
        "lastPollAt": settings.last_poll_at,
        "lastPollError": settings.last_poll_error,
        "rules": rules_json(&rules),
        "logs": logs,
    })))
}

/// POST /admin/afdian/chat/rules —— 整表保存（前端一次提交全部规则）
pub async fn chat_rules_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RulesBody>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let rules = normalize_rules(&body.rules)?;
    replace_rules(&state, &rules)?;
    Ok(Json(json!({
        "ok": true,
        "rules": rules_json(&rules),
    })))
}

/// POST /admin/afdian/chat/auto-reply —— 开关（默认关闭；开启时重置基线）
pub async fn chat_auto_reply_set(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AutoReplyBody>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    set_auto_reply_enabled(&state, body.enabled)?;
    let settings = load_settings(&state);
    Ok(Json(json!({
        "ok": true,
        "enabled": settings.enabled,
        "initialized": settings.initialized,
    })))
}

/// POST /admin/afdian/chat/rules/test —— 拿一句话试规则（只预览，不发送）
pub async fn chat_rules_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RuleTestBody>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let text = body.text.trim();
    if text.is_empty() {
        return Err(ApiError::bad_request("请输入一句要测试的内容"));
    }
    let rules: Vec<Rule> = load_rules(&state)
        .into_iter()
        .filter(|rule| rule.enabled)
        .collect();
    match find_rule(text, &rules) {
        Some((rule, keyword)) => Ok(Json(json!({
            "matched": true,
            "ruleId": rule.id,
            "keyword": keyword,
            "reply": rule.reply,
        }))),
        None => Ok(Json(json!({ "matched": false }))),
    }
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

    fn rule(keywords: &[&str], mode: &str, enabled: bool) -> Rule {
        Rule {
            id: "r1".into(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            match_mode: mode.into(),
            reply: "固定回复".into(),
            enabled,
        }
    }

    #[test]
    fn keyword_matching_modes() {
        let any = rule(&["安装", "连不上"], "any", true);
        assert_eq!(rule_hit("怎么安装表盘", &any).as_deref(), Some("安装"));
        assert_eq!(rule_hit("手表连不上了", &any).as_deref(), Some("连不上"));
        assert!(rule_hit("照片怎么换", &any).is_none());

        let all = rule(&["安装", "照片"], "all", true);
        assert_eq!(
            rule_hit("照片表盘怎么安装", &all).as_deref(),
            Some("安装 + 照片")
        );
        assert!(rule_hit("怎么安装", &all).is_none());

        let exact = rule(&["安装", "连不上"], "exact", true);
        assert!(rule_hit("安装", &exact).is_some());
        assert!(rule_hit("怎么安装", &exact).is_none());

        // 大小写不敏感；空文本、停用规则不命中
        let english = rule(&["Install"], "any", true);
        assert!(rule_hit("install failed", &english).is_some());
        assert!(rule_hit("", &any).is_none());
        assert!(rule_hit("怎么安装", &rule(&["安装"], "any", false)).is_none());
    }

    #[test]
    fn inbound_text_collects_only_trailing_text_messages() {
        let items = vec![
            json!({"direction":"receive","messageType":1,"content":"第一句"}),
            json!({"direction":"send","messageType":1,"content":"我们的回复"}),
            json!({"direction":"receive","messageType":1,"content":"第二句"}),
            json!({"direction":"receive","messageType":1,"content":"第三句"}),
        ];
        assert_eq!(trailing_inbound_text(&items), "第二句\n第三句");
        // 最后一条是我们发的：不算待回复内容
        let items2 = vec![
            json!({"direction":"receive","messageType":1,"content":"问题"}),
            json!({"direction":"send","messageType":1,"content":"回答"}),
        ];
        assert_eq!(trailing_inbound_text(&items2), "");
    }

    #[test]
    fn rule_validation_skips_empty_rows() {
        let input = vec![
            RuleIn {
                id: None,
                keywords: vec!["安装".into(), " ".into()],
                match_mode: Some("exact".into()),
                reply: "看这里".into(),
                enabled: true,
            },
            // 没填完的行直接跳过，不报错
            RuleIn {
                id: None,
                keywords: vec![],
                match_mode: None,
                reply: "".into(),
                enabled: true,
            },
        ];
        let rules = normalize_rules(&input).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].keywords, vec!["安装"]);
        assert_eq!(rules[0].match_mode, "exact");
        assert!(rules[0].id.starts_with("r_"));

        // 非法 id 会被换掉（避免前端传路径字符）
        let bad_id = RuleIn {
            id: Some("../x".into()),
            keywords: vec!["安装".into()],
            match_mode: None,
            reply: "ok".into(),
            enabled: true,
        };
        let rules = normalize_rules(&[bad_id]).unwrap();
        assert!(rules[0].id.starts_with("r_"));
        assert_eq!(rules[0].match_mode, "any");
    }
}
