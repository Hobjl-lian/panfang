//! 服务端状态、统一错误类型与鉴权小工具。

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::Connection;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub data_dir: PathBuf,
    pub cipher_dir: PathBuf,
    pub kek: [u8; 32],
    pub url_secret: Vec<u8>,
    pub admin_token: String,
    pub public_base: String,
    /// 每 subject 可激活的设备数上限（契约：默认 2）
    pub device_limit: i64,
    /// 名额满时，多久没出现过的激活可以自动回收（秒；0 = 不回收）
    pub device_stale_secs: i64,
    /// license_token 有效期（秒）
    pub token_ttl: i64,
}

impl AppState {
    pub fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().expect("db mutex poisoned")
    }
}

// ---------------------------------------------------------------- 错误

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub error: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, error: &'static str, message: impl Into<String>) -> Self {
        Self { status, error, message: message.into() }
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", msg)
    }
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "token_invalid", msg)
    }
    pub fn forbidden(error: &'static str, msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, error, msg)
    }
    pub fn conflict(error: &'static str, msg: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, error, msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "server_error", msg)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": self.error, "message": self.message })),
        )
            .into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::internal(format!("{e:#}"))
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError::internal(format!("数据库错误: {e}"))
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

// ---------------------------------------------------------------- 鉴权

pub fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string())
}

/// 管理端鉴权（独立 admin_token）。
pub fn require_admin(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    match bearer(headers) {
        Some(t) if t == state.admin_token => Ok(()),
        _ => Err(ApiError::unauthorized("管理令牌无效")),
    }
}

pub struct ClientAuth {
    pub subject_id: String,
}

/// 客户端鉴权：license_token → subject（校验存在、未过期、授权未吊销）。
pub fn require_client(state: &AppState, headers: &HeaderMap) -> ApiResult<ClientAuth> {
    let token = bearer(headers).ok_or_else(|| ApiError::unauthorized("缺少登录令牌"))?;
    let th = crate::crypto::token_hash(&token);
    let conn = state.conn();
    let now = crate::db::now();
    let row = conn
        .query_row(
            "SELECT subject_id, expires_at FROM tokens WHERE token_hash = ?1",
            [&th],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .map_err(|_| ApiError::unauthorized("登录状态已失效，请重新兑换"))?;
    if row.1 < now {
        return Err(ApiError::unauthorized("登录状态已过期，请重新兑换"));
    }
    conn.execute(
        "UPDATE tokens SET last_used_at = ?1 WHERE token_hash = ?2",
        rusqlite::params![now, th],
    )
    .ok();
    Ok(ClientAuth { subject_id: row.0 })
}

pub fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

pub fn user_agent(headers: &HeaderMap) -> String {
    headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}
