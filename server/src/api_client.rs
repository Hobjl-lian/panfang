//! 客户端接口（契约 §1）。App 用：绑定 / 我的授权 / 换设备 / 版本清单 / 下载。

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::now;
use crate::state::{bearer, client_ip, require_client, user_agent, ApiError, ApiResult, AppState};

/// 买家下载页（`/get`）：静态模板 + 当前发版信息占位符。
const GET_PAGE_HTML: &str = include_str!("../static/get.html");

// ---------------------------------------------------------------- 兑换码（已下线）
// 兑换码这条路已废弃：App 与管理页都不再用它。接口一并删除（曾经的 /v1/redeem 没有任何开关
// 保护，留着就是白拿资源的口子）。数据库里的 codes 表保留（不做破坏性迁移），但不再读写。

fn display_of(mi_uid: &str) -> String {
    // 只回显后 4 位，避免完整账号出现在日志/响应里
    let s: String = mi_uid.chars().rev().take(4).collect();
    s.chars().rev().collect()
}

/// 有效授权状态：`subjects.all_access=1` 的主体对**任何**产品都视作 `active`
/// （永久全资源，含以后新增的产品，不用逐个 grant）；否则读 licenses 表的逐产品授权。
/// 返回 `None` = 从未授权过。调用方据此区分「没有授权」与「授权被回收」。
fn subject_all_access(conn: &rusqlite::Connection, subject_id: &str) -> ApiResult<bool> {
    let v: i64 = conn
        .query_row(
            "SELECT COALESCE(all_access,0) FROM subjects WHERE id=?1",
            [subject_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(v == 1)
}

fn license_status(
    conn: &rusqlite::Connection,
    subject_id: &str,
    product_id: &str,
) -> ApiResult<Option<String>> {
    if subject_all_access(conn, subject_id)? {
        return Ok(Some("active".to_string()));
    }
    Ok(conn
        .query_row(
            "SELECT status FROM licenses WHERE subject_id=?1 AND product_id=?2",
            params![subject_id, product_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// 该 subject 名下所有「有效授权」的产品 + 变体 + 最新版本。
fn subject_products(state: &AppState, subject_id: &str) -> ApiResult<Vec<Value>> {
    let conn = state.conn();
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.cover_url,
                (SELECT COUNT(*) FROM activations a
                  WHERE a.subject_id=?1 AND a.product_id=p.id AND a.status='active')
         FROM products p
         WHERE p.status='on'
           AND (EXISTS(SELECT 1 FROM subjects s WHERE s.id=?1 AND s.all_access=1)
                OR EXISTS(SELECT 1 FROM licenses l
                          WHERE l.subject_id=?1 AND l.product_id=p.id AND l.status='active'))",
    )?;
    let products: Vec<(String, String, Option<String>, i64)> = stmt
        .query_map([subject_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut out = Vec::new();
    for (pid, name, cover, used) in products {
        // 只返回「已有包体」的设备行（App 列表里点进去必须能装）
        let rows: Vec<(String, String, String, Option<i64>, Option<i64>, Option<String>)> = {
            let mut vstmt = conn.prepare(
                "SELECT v.id, v.name, v.device_models, v.screen_w, v.screen_h,
                        (SELECT version FROM versions x WHERE x.variant_id=v.id AND x.is_latest=1 LIMIT 1)
                 FROM variants v WHERE v.product_id=?1
                   AND EXISTS (SELECT 1 FROM versions x JOIN files f ON f.version_id=x.id WHERE x.variant_id=v.id)
                 ORDER BY v.created_at",
            )?;
            let it = vstmt.query_map([&pid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?;
            it.filter_map(|r| r.ok()).collect()
        };

        let mut variants: Vec<Value> = Vec::new();
        for (vid, vname, models, sw, sh, latest_ver) in rows {
            // 最新版本的每个包：App 据此精确判断「哪个包有更新」。
            // `latest_sha` 仍保留（老 App 按整行指纹判断），新 App 按 packages 逐包比较。
            let pkgs: Vec<(String, String, Option<String>, Option<String>, String, i64)> = {
                let mut sstmt = conn.prepare(
                    "SELECT f.id, f.file_name, f.watchface_id, f.bin_version,
                            f.sha256_plain, f.size
                     FROM files f JOIN versions x ON f.version_id=x.id
                     WHERE x.variant_id=?1 AND x.is_latest=1
                     ORDER BY f.sort_order, f.id",
                )?;
                let it = sstmt.query_map([&vid], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?;
                it.filter_map(|r| r.ok()).collect()
            };
            // 整行内容指纹 = 按文件顺序把各包 sha256_plain 连起来再 sha256（老 App 用）
            let shas: Vec<&str> = pkgs.iter().map(|p| p.4.as_str()).collect();
            let latest_sha = if shas.is_empty() {
                Value::Null
            } else {
                json!(crate::crypto::sha256_hex(shas.join("|").as_bytes()))
            };
            let packages: Vec<Value> = pkgs
                .iter()
                .map(|(file_id, file_name, wfid, bin_ver, sha, size)| {
                    json!({
                        "file_id": file_id,
                        "file_name": file_name,
                        "watchface_id": wfid,
                        "version": bin_ver.clone().or_else(|| latest_ver.clone()),
                        "sha256_plain": sha,
                        "size": size,
                    })
                })
                .collect();
            variants.push(json!({
                "variant_id": vid,
                "name": vname,
                "device_models": serde_json::from_str::<Value>(&models).unwrap_or(json!([])),
                "screen": [sw, sh],
                "latest_version": latest_ver,
                "latest_sha": latest_sha,
                // 逐包指纹：App 用「已装快照」按表盘 id 对每个包分别判断有没有更新
                "packages": packages,
            }));
        }
        out.push(json!({
            "product_id": pid,
            "name": name,
            "cover_url": cover,
            "variants": variants,
            "activations_used": used,
            "activation_limit": state.device_limit,
        }));
    }
    Ok(out)
}

// ---------------------------------------------------------------- 绑定（爱发电 / 开发模式）

/// GET /v1/bind/mode —— App 决定绑定页怎么显示：oauth（已配置爱发电）/ dev（联调用）/ none
pub async fn bind_mode() -> ApiResult<Json<Value>> {
    let oauth = crate::afdian::oauth_cfg().is_some();
    // 走统一的到期判断（只设 WF_DEV_BIND=1 没有到期时间＝关闭），否则这里会绕过 fail-closed
    let dev = crate::afdian::dev_mode();
    let mode = if oauth { "oauth" } else if dev { "dev" } else { "none" };
    Ok(Json(json!({ "mode": mode, "shop_url": std::env::var("WF_SHOP_URL").ok() })))
}

/// 找（或建）某个小米账号的授权主体
fn subject_ensure(conn: &rusqlite::Connection, mi_uid: &str) -> ApiResult<String> {
    match conn
        .query_row("SELECT id FROM subjects WHERE mi_user_id = ?1", [mi_uid], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    {
        Some(id) => Ok(id),
        None => {
            let id = crate::db::new_id("s_");
            conn.execute(
                "INSERT INTO subjects(id, mi_user_id, created_at) VALUES(?1, ?2, ?3)",
                params![id, mi_uid, now()],
            )?;
            Ok(id)
        }
    }
}

/// 「永久全资源」主体的登录态有效期：10 年（实际等同长期有效）。
///
/// 为什么要区别对待：普通买家走爱发电 OAuth 换登录态（30 天后可重绑，clientID 就绪后自助）。
/// 而开发者本人是 all_access，本来就不依赖爱发电；若也用 30 天 TTL，token 一过期 App 就
/// 卡在绑定页进不去「我的表盘」。注意 token 泄露＝该账号被冒用，但该账号本来就全资源，
/// 风险面不比「联调开关大开」更大。
const OWNER_TOKEN_TTL: i64 = 10 * 365 * 24 * 3600;

/// 给某个主体签发登录态（App 侧用它调 /v1/me、/v1/manifest、/v1/download）
fn issue_token(state: &AppState, conn: &rusqlite::Connection, subject_id: &str) -> ApiResult<(String, i64)> {
    let token = crate::crypto::new_token();
    let ttl = if subject_all_access(conn, subject_id)? {
        OWNER_TOKEN_TTL
    } else {
        state.token_ttl
    };
    let expires_at = now() + ttl;
    conn.execute(
        "INSERT INTO tokens(token_hash, subject_id, expires_at, created_at) VALUES(?1, ?2, ?3, ?4)",
        params![crate::crypto::token_hash(&token), subject_id, expires_at, now()],
    )?;
    Ok((token, expires_at))
}

// ---------------------------------------------------------------- 爱发电 OAuth 关联授权

#[derive(Deserialize)]
pub struct OauthStart {
    pub mi_user_id: String,
    pub device_fp: Option<String>,
    pub device_pubkey: Option<String>,
    pub device_name: Option<String>,
}

/// POST /v1/afdian/oauth/start —— App 发起绑定：登记一次会话，返回让用户去授权的地址。
/// 用户授权后爱发电把浏览器带回 `/v1/afdian/callback`，App 用 `state` 轮询 `/v1/afdian/oauth/poll` 取结果。
pub async fn afdian_oauth_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<OauthStart>,
) -> ApiResult<Json<Value>> {
    let Some(cfg) = crate::afdian::oauth_cfg() else {
        return Err(ApiError::not_found("绑定功能暂不可用"));
    };
    let mi_uid = body.mi_user_id.trim().to_string();
    if mi_uid.is_empty() {
        return Err(ApiError::bad_request("请先用小米账号登录"));
    }
    if let Some(pk) = body.device_pubkey.as_deref().filter(|s| !s.is_empty()) {
        let raw = crate::crypto::b64d(pk).map_err(|_| ApiError::bad_request("device_pubkey 不是合法 base64"))?;
        if raw.len() != 32 {
            return Err(ApiError::bad_request("device_pubkey 必须是 32 字节"));
        }
    }
    let st = crate::crypto::new_token();
    let expires_at = now() + crate::afdian::OAUTH_TTL;
    {
        let conn = state.conn();
        let subject_id = subject_ensure(&conn, &mi_uid)?;
        conn.execute(
            "INSERT INTO afdian_oauth(state, subject_id, mi_user_id, device_fp, device_pubkey, device_name,
                                      status, expires_at, created_at, start_ip)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9)",
            params![
                st,
                subject_id,
                mi_uid,
                body.device_fp.unwrap_or_default(),
                body.device_pubkey.unwrap_or_default(),
                body.device_name.unwrap_or_default(),
                expires_at,
                now(),
                client_ip(&headers)
            ],
        )?;
    }
    Ok(Json(json!({
        "state": st,
        "url": crate::afdian::authorize_url(&cfg, &st),
        "expires_at": expires_at,
    })))
}

/// GET /v1/afdian/dev-authorize —— **仅联调**（WF_DEV_BIND=1）：模拟爱发电的授权页。
/// 真的授权页在爱发电域名下、要等审批；这一页让「打开授权页 → 回调 → App 轮询」整条链路现在就能点着测。
pub async fn afdian_dev_authorize(Query(q): Query<OauthPoll>) -> Response {
    if !crate::afdian::dev_mode() {
        return (StatusCode::NOT_FOUND, "该接口未启用").into_response();
    }
    let state = q.state;
    let tpl = r##"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>模拟爱发电授权</title></head>
<body style="margin:0;font-family:-apple-system,'PingFang SC',sans-serif;background:#F5F6F9;
display:flex;align-items:center;justify-content:center;min-height:100vh">
<form method="get" action="/v1/afdian/callback" style="background:#fff;border-radius:24px;padding:32px 26px;
max-width:340px;width:100%;box-shadow:0 1px 3px rgba(0,0,0,.04)">
  <h1 style="font-size:20px;margin:0 0 6px;color:#000">模拟爱发电授权</h1>
  <p style="font-size:13px;color:rgba(0,0,0,.45);margin:0 0 18px;line-height:1.6">
    联调专用页（不是爱发电官方页）。随便填一个「爱发电用户 ID」，点下面按钮就当作你已授权。</p>
  <input name="uid" value="devtest" style="width:100%;box-sizing:border-box;height:44px;border-radius:14px;
    border:1px solid rgba(0,0,0,.1);padding:0 14px;font-size:15px"/>
  <input type="hidden" name="state" value="%STATE%"/>
  <input type="hidden" name="code" id="code" value=""/>
  <button type="submit" style="margin-top:16px;width:100%;height:48px;border:0;border-radius:999px;
    background:#4361EE;color:#fff;font-size:16px;font-weight:700" onclick="
      document.getElementById('code').value='dev:'+document.querySelector('[name=uid]').value">授权并返回 App</button>
</form>
<script>
  // 提交前把 uid 拼成 code=dev:<uid>（服务端联调模式下认这个前缀）
  document.querySelector('form').addEventListener('submit', function(){
    document.getElementById('code').value='dev:'+document.querySelector('[name=uid]').value;
  });
</script>
</body></html>"##;
    axum::response::Html(tpl.replace("%STATE%", &state)).into_response()
}

#[derive(Deserialize)]
pub struct OauthCb {
    pub code: Option<String>,
    pub state: Option<String>,
}

/// GET /v1/afdian/callback —— 用户在浏览器里授权完，爱发电跳到这里。
/// 拿 code 换身份 → 建立绑定 → 把这个买家的历史订单一次性补上 → 给 App 备好登录态。
pub async fn afdian_oauth_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<OauthCb>,
) -> ApiResult<Response> {
    let page = |title: &str, body: &str| {
        axum::response::Html(format!(
            r##"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title></head>
<body style="margin:0;font-family:-apple-system,'PingFang SC',sans-serif;background:#F5F6F9;
display:flex;align-items:center;justify-content:center;min-height:100vh">
<div style="background:#fff;border-radius:24px;padding:36px 28px;max-width:340px;text-align:center;
box-shadow:0 1px 3px rgba(0,0,0,.04)">
<div style="width:56px;height:56px;border-radius:16px;background:#4361EE;margin:0 auto 20px;
display:flex;align-items:center;justify-content:center">
<svg viewBox="0 0 24 24" width="26" height="26" fill="none" stroke="#fff" stroke-width="2"
stroke-linecap="round" stroke-linejoin="round"><path d="M12 20s-7-4.35-7-9a4 4 0 0 1 7-2.65A4 4 0 0 1 19 11c0 4.65-7 9-7 9z"/></svg>
</div>
<h1 style="font-size:20px;margin:0 0 8px;color:#000">{title}</h1>
<p style="font-size:14px;line-height:1.6;color:rgba(0,0,0,.45);margin:0">{body}</p>
</div></body></html>"##
        ))
    };
    let Some(st) = q.state.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(page("授权没走完", "请回到盘坊 App 重新点一次「用爱发电账号登录」。").into_response());
    };
    let Some(code) = q.code.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(page("授权没走完", "请回到盘坊 App 重新点一次「用爱发电账号登录」。").into_response());
    };

    // 1) 校验会话（读操作单独一段，别持锁去调会抢锁的辅助函数）
    let row: Option<(String, String, i64, String, Option<String>)> = {
        let conn = state.conn();
        conn.query_row(
            "SELECT subject_id, mi_user_id, expires_at, status, start_ip FROM afdian_oauth WHERE state = ?1",
            [st],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(ApiError::from)?
    };
    let Some((subject_id, mi_uid, expires_at, status, start_ip)) = row else {
        return Ok(page("授权链接已失效", "请回到盘坊 App 重新点一次「用爱发电账号登录」。").into_response());
    };
    if status != "pending" {
        return Ok(page("已授权过了", "可以直接回到盘坊 App；没反应就重新发起一次绑定。").into_response());
    }
    if now() > expires_at {
        mark_oauth_error(&state, st, "授权超时，请回到 App 重新发起");
        return Ok(page("授权超时", "请回到盘坊 App 重新点一次「用爱发电账号登录」。").into_response());
    }
    // 发起绑定（App）与用户授权（浏览器）正常都在同一台手机上，出口 IP 应当一致。
    // 不一致 = 这条授权链接可能是别人发给用户点的（OAuth 登录 CSRF）：
    // 只打告警日志、不阻断（手机中途切网会误报），出事时可据此定位哪一次绑定被借用。
    let cb_ip = client_ip(&headers);
    if let Some(sip) = start_ip.as_deref().filter(|s| !s.is_empty()) {
        if !cb_ip.is_empty() && sip != cb_ip {
            let short: String = st.chars().take(8).collect(); // state 未校验字符集，按字符截断防 panic
            eprintln!(
                "[afdian] ⚠ 绑定会话 {} 的发起 IP 与授权 IP 不一致：start={} callback={}（核对这一单是否是买家本人操作）",
                short, sip, cb_ip
            );
        }
    }
    {
        let conn = state.conn();
        let _ = conn.execute(
            "UPDATE afdian_oauth SET callback_ip=?2 WHERE state=?1",
            params![st, cb_ip],
        );
    }

    // 2) code → 爱发电 user_id（联调模式支持 code=dev:<uid> 直接模拟）
    let afdian_uid = match crate::afdian::dev_code_uid(code) {
        Some(uid) => uid,
        None => {
            let Some(cfg) = crate::afdian::oauth_cfg() else {
                return Ok(page("绑定功能暂不可用", "请稍后再试。").into_response());
            };
            match crate::afdian::exchange_code(&cfg, code).await {
                Ok(uid) => uid,
                Err(e) => {
                    eprintln!("[afdian] OAuth 换身份失败：{e:?}");
                    mark_oauth_error(&state, st, &format!("{e}"));
                    return Ok(page("授权没成功", "请回到盘坊 App 重试；一直在失败就联系我们。").into_response());
                }
            }
        }
    };

    // 3) 绑定 + 补发历史订单（各自内部抢锁，调用时上面不能持有 guard）
    let bound_subject = match crate::afdian::bind_afdian(&state, &afdian_uid, &mi_uid) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("[afdian] 建绑定失败：{e:?}");
            mark_oauth_error(&state, st, "绑定失败，请回到 App 重试");
            return Ok(page("授权没成功", "请回到盘坊 App 重试；一直在失败就联系我们。").into_response());
        }
    };
    let resent = crate::afdian::sync_after_bind(&state, &afdian_uid).unwrap_or(0);
    println!(
        "[afdian] OAuth 绑定成功：爱发电 {} ↔ 小米账号 {}（补发 {resent} 笔订单）",
        afdian_uid, mi_uid
    );

    // 4) 备好登录态给 App 取走
    let minted = {
        let conn = state.conn();
        let t = issue_token(&state, &conn, &bound_subject).ok();
        if t.is_none() {
            return Ok(page("授权没成功", "请回到盘坊 App 重试；一直在失败就联系我们。").into_response());
        }
        t.unwrap()
    };
    {
        let conn = state.conn();
        let _ = conn.execute(
            "UPDATE afdian_oauth SET status='ok', afdian_user_id=?2, license_token=?3, done_at=?4 WHERE state=?1",
            params![st, afdian_uid, minted.0, now()],
        );
        // subject_id 只是防止并发换绑时错位的兜底，这里再确认一次它没变
        let _ = bound_subject;
    }
    Ok(page("授权成功", "回到盘坊 App，你的已购表盘会自动同步过来。").into_response())
}

fn mark_oauth_error(state: &AppState, st: &str, msg: &str) {
    let conn = state.conn();
    let _ = conn.execute(
        "UPDATE afdian_oauth SET status='error', message=?2, done_at=?3 WHERE state=?1",
        params![st, msg, now()],
    );
}

#[derive(Deserialize)]
pub struct OauthPoll {
    pub state: String,
}

/// 登录令牌在 `tokens` 表里的真实过期时间。
///
/// `afdian_oauth.expires_at` 是授权会话（15 分钟）的过期时间，**不能**当令牌有效期；
/// 查不到令牌时退回 `fallback`（正常不会发生，只做兜底）。
fn license_token_expires_at(
    conn: &rusqlite::Connection,
    token: &str,
    fallback: i64,
) -> ApiResult<i64> {
    if token.is_empty() {
        return Ok(fallback);
    }
    Ok(conn
        .query_row(
            "SELECT expires_at FROM tokens WHERE token_hash=?1",
            [crate::crypto::token_hash(token)],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(fallback))
}

/// GET /v1/afdian/oauth/poll?state=… —— App 轮询：pending / ok（带登录态）/ error
pub async fn afdian_oauth_poll(
    State(state): State<AppState>,
    Query(q): Query<OauthPoll>,
) -> ApiResult<Json<Value>> {
    let row: Option<(String, Option<String>, Option<String>, i64, String)> = {
        let conn = state.conn();
        conn.query_row(
            "SELECT status, afdian_user_id, license_token, expires_at, mi_user_id
             FROM afdian_oauth WHERE state = ?1",
            [&q.state],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
    };
    let Some((status, afdian_uid, token, expires_at, mi_uid)) = row else {
        return Err(ApiError::not_found("这次绑定已失效，请重新发起"));
    };
    if status == "pending" && now() > expires_at {
        mark_oauth_error(&state, &q.state, "授权超时，请回到 App 重新发起");
        return Err(ApiError::bad_request("授权超时，请重新发起"));
    }
    match status.as_str() {
        "ok" => {
            let uid = afdian_uid.unwrap_or_default();
            let license_token = token.unwrap_or_default();
            // ⚠️ afdian_oauth.expires_at 是「这次授权会话」的 15 分钟有效期，
            // 不是登录令牌的有效期。App 会把这里返回的 expires_at 原样存进
            // session.enc 当作令牌过期时间 —— 返回授权会话时间会让用户在 15 分钟后
            // 被判成「未绑定」（2026-10-17 真机报障）。这里必须返回 tokens 表的真实值。
            let license_expires_at = {
                let conn = state.conn();
                license_token_expires_at(&conn, &license_token, expires_at)?
            };
            Ok(Json(json!({
                "status": "ok",
                "bound": true,
                "afdian_user_id": uid,
                "display": display_of(&uid),
                "mi_user_id": mi_uid,
                "license_token": license_token,
                "expires_at": license_expires_at,
                // 授权会话本身的过期时间（排障用；App 不读）
                "oauth_expires_at": expires_at,
            })))
        }
        "error" => Ok(Json(json!({
            "status": "error",
            "message": "授权没成功，请重试",
        }))),
        _ => Ok(Json(json!({ "status": "pending" }))),
    }
}

/// POST /v1/bind/dev —— **仅供联调**：不做爱发电校验，直接给这个小米账号签发登录态。
/// 仅在服务端设置 `WF_DEV_BIND=1` 时可用；上线前必须关闭。
pub async fn bind_dev(
    State(state): State<AppState>,
    Json(body): Json<DevBind>,
) -> ApiResult<Json<Value>> {
    if !crate::afdian::dev_mode() {
        return Err(ApiError::not_found("该接口未启用"));
    }
    let mi_uid = body.mi_user_id.trim().to_string();
    if mi_uid.is_empty() {
        return Err(ApiError::bad_request("缺少 mi_user_id"));
    }
    if let Some(pk) = body.device_pubkey.as_deref().filter(|s| !s.is_empty()) {
        let raw = crate::crypto::b64d(pk).map_err(|_| ApiError::bad_request("device_pubkey 不是合法 base64"))?;
        if raw.len() != 32 {
            return Err(ApiError::bad_request("device_pubkey 必须是 32 字节"));
        }
    }
    let conn = state.conn();
    let subject_id = subject_ensure(&conn, &mi_uid)?;
    // 联调模式：顺手把「所有资源」都开通给这个账号（方便端到端试装；上线前关掉 WF_DEV_BIND 即失效）
    let granted: i64 = conn.execute(
        "INSERT INTO licenses(subject_id, product_id, source, status, created_at)
         SELECT ?1, id, 'dev', 'active', ?2 FROM products WHERE status = 'on'
         ON CONFLICT(subject_id, product_id) DO NOTHING",
        params![subject_id, now()],
    )? as i64;
    if granted > 0 {
        println!("[wf-auth] 联调绑定：给 {mi_uid} 开通了 {granted} 个资源");
    }

    let (token, expires_at) = issue_token(&state, &conn, &subject_id)?;
    drop(conn);
    let products = subject_products(&state, &subject_id)?;
    Ok(Json(json!({
        "license_token": token,
        "expires_at": expires_at,
        "subject": { "mi_user_id": mi_uid, "display": display_of(&mi_uid) },
        "products": products,
        "dev": true,
    })))
}

#[derive(Deserialize)]
pub struct DevBind {
    pub mi_user_id: String,
    pub device_fp: Option<String>,
    pub device_pubkey: Option<String>,
    pub device_name: Option<String>,
}

// ---------------------------------------------------------------- 我的授权 / 换设备

pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let auth = require_client(&state, &headers)?;
    let products = subject_products(&state, &auth.subject_id)?;
    let mi_uid: String = {
        let conn = state.conn();
        conn.query_row("SELECT mi_user_id FROM subjects WHERE id=?1", [&auth.subject_id], |r| {
            r.get(0)
        })
        .unwrap_or_default()
    };
    let activations: Vec<Value> = {
        let conn = state.conn();
        let mut stmt = conn.prepare(
            "SELECT product_id, device_fp, device_name, status, created_at, last_seen_at
             FROM activations WHERE subject_id=?1 ORDER BY id DESC",
        )?;
        let v: Vec<Value> = stmt
            .query_map([&auth.subject_id], |r| {
                Ok(json!({
                    "product_id": r.get::<_, String>(0)?,
                    "device_fp": r.get::<_, String>(1)?,
                    "device_name": r.get::<_, Option<String>>(2)?,
                    "status": r.get::<_, String>(3)?,
                    "activated_at": r.get::<_, i64>(4)?,
                    "last_seen_at": r.get::<_, Option<i64>>(5)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();
        v
    };
    Ok(Json(json!({
        "subject": { "mi_user_id": mi_uid, "display": display_of(&mi_uid) },
        "products": products,
        "activations": activations,
    })))
}

#[derive(Deserialize)]
pub struct Activate {
    pub product_id: String,
    pub device_fp: String,
    pub device_pubkey: String,
    pub device_name: Option<String>,
}

/// 名额满时的宽限：把「最久没出现、且已超过宽限期」的那条激活置为 revoked，腾一个位置。
///
/// 为什么需要：`device_fp` 是 App 内随机生成的设备 uuid，卸载重装/清数据就会变成新指纹，
/// 老记录仍留在库里占名额 → 买家重装两次就被锁死（2026-10-17 真机踩到）。
/// 只回收确实很久没出现的设备；两台都在用时不回收，仍按原样报 device_limit。
fn reclaim_stale_activation(
    conn: &rusqlite::Connection,
    subject_id: &str,
    product_id: &str,
    stale_secs: i64,
) -> ApiResult<bool> {
    if stale_secs <= 0 {
        return Ok(false);
    }
    let oldest: Option<(String, i64)> = conn
        .query_row(
            "SELECT device_fp, COALESCE(last_seen_at, created_at, 0) AS seen FROM activations
             WHERE subject_id=?1 AND product_id=?2 AND status='active'
             ORDER BY seen ASC LIMIT 1",
            params![subject_id, product_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((fp, seen)) = oldest else {
        return Ok(false);
    };
    if seen > now() - stale_secs {
        return Ok(false);
    }
    conn.execute(
        "UPDATE activations SET status='revoked'
         WHERE subject_id=?1 AND product_id=?2 AND device_fp=?3",
        params![subject_id, product_id, fp],
    )?;
    eprintln!(
        "[device_limit] 回收久未使用的激活 subject={subject_id} product={product_id} fp={} last_seen={seen}（宽限 {stale_secs} 秒）",
        &fp[..fp.len().min(12)]
    );
    Ok(true)
}

pub async fn activate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Activate>,
) -> ApiResult<Json<Value>> {
    let auth = require_client(&state, &headers)?;
    let raw = crate::crypto::b64d(&body.device_pubkey)
        .map_err(|_| ApiError::bad_request("device_pubkey 不是合法 base64"))?;
    if raw.len() != 32 {
        return Err(ApiError::bad_request("device_pubkey 必须是 32 字节"));
    }
    let conn = state.conn();
    let lic = license_status(&conn, &auth.subject_id, &body.product_id)?;
    match lic.as_deref() {
        None => return Err(ApiError::forbidden("license_revoked", "没有该产品的授权")),
        Some("revoked") => {
            return Err(ApiError::forbidden("license_revoked", "该授权已被回收"))
        }
        _ => {}
    }
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM activations WHERE subject_id=?1 AND product_id=?2 AND device_fp=?3",
            params![auth.subject_id, body.product_id, body.device_fp],
            |r| r.get(0),
        )
        .optional()?;
    let used: i64 = conn.query_row(
        "SELECT COUNT(*) FROM activations WHERE subject_id=?1 AND product_id=?2 AND status='active'",
        params![auth.subject_id, body.product_id],
        |r| r.get(0),
    )?;
    if exists.is_none()
        && used >= state.device_limit
        && !reclaim_stale_activation(
            &conn,
            &auth.subject_id,
            &body.product_id,
            state.device_stale_secs,
        )?
    {
        return Err(ApiError::forbidden(
            "device_limit",
            format!("已有 {} 台设备装了这份表盘（上限 {} 台）。换手机或重装后需要清掉旧设备记录，请联系开发者处理", state.device_limit, state.device_limit),
        ));
    }
    conn.execute(
        "INSERT INTO activations(subject_id, product_id, device_fp, device_pubkey, device_name,
             status, created_at, last_seen_at)
         VALUES(?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)
         ON CONFLICT(subject_id, product_id, device_fp)
         DO UPDATE SET device_pubkey=excluded.device_pubkey, device_name=excluded.device_name,
                       status='active', last_seen_at=excluded.last_seen_at",
        params![
            auth.subject_id,
            body.product_id,
            body.device_fp,
            body.device_pubkey,
            body.device_name,
            now()
        ],
    )?;
    let used_after: i64 = conn.query_row(
        "SELECT COUNT(*) FROM activations WHERE subject_id=?1 AND product_id=?2 AND status='active'",
        params![auth.subject_id, body.product_id],
        |r| r.get(0),
    )?;
    Ok(Json(json!({
        "ok": true,
        "activations_used": used_after,
        "activation_limit": state.device_limit,
    })))
}

// ---------------------------------------------------------------- 版本清单

#[derive(Deserialize)]
pub struct ManifestQuery {
    pub product_id: String,
    pub variant_id: String,
}

pub async fn manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ManifestQuery>,
) -> ApiResult<Json<Value>> {
    let auth = require_client(&state, &headers)?;
    let conn = state.conn();
    let lic = license_status(&conn, &auth.subject_id, &q.product_id)?;
    match lic.as_deref() {
        None => return Err(ApiError::forbidden("license_revoked", "没有该产品的授权")),
        Some("revoked") => {
            return Err(ApiError::forbidden("license_revoked", "该授权已被回收"))
        }
        _ => {}
    }
    let pstatus: Option<String> = conn
        .query_row("SELECT status FROM products WHERE id=?1", [&q.product_id], |r| r.get(0))
        .optional()?;
    if pstatus.as_deref() != Some("on") {
        return Err(ApiError::forbidden("product_offline", "该表盘已下架"));
    }

    let mut vstmt = conn.prepare(
        "SELECT id, version, notes, is_latest, created_at FROM versions
         WHERE variant_id=?1 ORDER BY id DESC",
    )?;
    let versions: Vec<Value> = vstmt
        .query_map([&q.variant_id], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "version": r.get::<_, String>(1)?,
                "notes": r.get::<_, Option<String>>(2)?,
                "is_latest": r.get::<_, i64>(3)? != 0,
                "released_at": r.get::<_, i64>(4)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut out = Vec::new();
    for v in versions {
        let vid = v["id"].as_i64().unwrap_or(0);
        let mut fstmt = conn.prepare(
            // 一个版本可以有多个包（同一设备因内存限制拆成两个独立表盘）：按 sort_order 定序，App 依次安装
            "SELECT id, file_name, size, sha256_plain, sha256_cipher, watchface_id, bin_version
             FROM files WHERE version_id=?1 ORDER BY sort_order, id",
        )?;
        let files: Vec<Value> = fstmt
            .query_map([vid], |r| {
                Ok(json!({
                    "file_id": r.get::<_, String>(0)?,
                    "file_name": r.get::<_, String>(1)?,
                    "size": r.get::<_, i64>(2)?,
                    "sha256_plain": r.get::<_, String>(3)?,
                    "sha256_cipher": r.get::<_, String>(4)?,
                    // 逐包更新判断用：App 拿它跟本机安装快照按表盘 id 对齐
                    "watchface_id": r.get::<_, Option<String>>(5)?,
                    "version": r.get::<_, Option<String>>(6)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();
        out.push(json!({
            "version": v["version"],
            "notes": v["notes"],
            "is_latest": v["is_latest"],
            "released_at": v["released_at"],
            "files": files,
        }));
    }
    let latest = out
        .iter()
        .find(|v| v["is_latest"] == json!(true))
        .map(|v| v["version"].clone());
    Ok(Json(json!({
        "product_id": q.product_id,
        "variant_id": q.variant_id,
        "latest_version": latest,
        "versions": out,
    })))
}

// ---------------------------------------------------------------- 下载

#[derive(Deserialize)]
pub struct DownloadReq {
    pub file_id: String,
    /// 目标设备指纹（多设备时必填；单设备可省略）
    pub device_fp: Option<String>,
    /// 设备公钥（首次下载时用于自动激活）
    pub device_pubkey: Option<String>,
    pub device_name: Option<String>,
}

pub async fn download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DownloadReq>,
) -> ApiResult<Json<Value>> {
    let auth = require_client(&state, &headers)?;
    let conn = state.conn();

    // 文件 → 版本 → 变体 → 产品
    let row: Option<(String, String, i64, String, String, Vec<u8>, Vec<u8>, String)> = conn
        .query_row(
            "SELECT f.file_name, f.sha256_plain, f.size, v.version, va.product_id,
                    f.file_nonce, f.cek_wrapped, f.storage_path
             FROM files f
             JOIN versions v ON v.id = f.version_id
             JOIN variants va ON va.id = v.variant_id
             WHERE f.id = ?1",
            [&body.file_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?;
    let (file_name, sha_plain, size, version, product_id, _file_nonce, cek_wrapped, storage_path) =
        row.ok_or_else(|| ApiError::not_found("文件不存在"))?;

    // 授权
    let lic = license_status(&conn, &auth.subject_id, &product_id)?;
    match lic.as_deref() {
        None => return Err(ApiError::forbidden("license_revoked", "没有该产品的授权")),
        Some("revoked") => {
            return Err(ApiError::forbidden("license_revoked", "该授权已被回收"))
        }
        _ => {}
    }

    // 目标设备的公钥
    let dev: Option<(String, String)> = match body.device_fp.as_deref().filter(|s| !s.is_empty()) {
        Some(fp) => conn
            .query_row(
                "SELECT device_fp, device_pubkey FROM activations
                 WHERE subject_id=?1 AND product_id=?2 AND device_fp=?3 AND status='active'",
                params![auth.subject_id, product_id, fp],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?,
        None => conn
            .query_row(
                "SELECT device_fp, device_pubkey FROM activations
                 WHERE subject_id=?1 AND product_id=?2 AND status='active'
                 ORDER BY last_seen_at DESC LIMIT 1",
                params![auth.subject_id, product_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?,
    };
    // 没有该产品的激活记录 → 限额内自动激活（App 无需单独的「激活」步骤）
    let (device_fp, device_pubkey) = match dev {
        Some(v) => v,
        None => {
            let fp = body
                .device_fp
                .clone()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ApiError::bad_request("缺少 device_fp"))?;
            let pk = body
                .device_pubkey
                .clone()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ApiError::bad_request("首次下载需要 device_pubkey"))?;
            let raw = crate::crypto::b64d(&pk)?;
            if raw.len() != 32 {
                return Err(ApiError::bad_request("device_pubkey 必须是 32 字节"));
            }
            let used: i64 = conn.query_row(
                "SELECT COUNT(*) FROM activations WHERE subject_id=?1 AND product_id=?2 AND status='active'",
                params![auth.subject_id, product_id],
                |r| r.get(0),
            )?;
            if used >= state.device_limit
                && !reclaim_stale_activation(
                    &conn,
                    &auth.subject_id,
                    &product_id,
                    state.device_stale_secs,
                )?
            {
                return Err(ApiError::forbidden(
                    "device_limit",
                    format!("已有 {} 台设备装了这份表盘（上限 {} 台）。换手机或重装后需要清掉旧设备记录，请联系开发者处理", state.device_limit, state.device_limit),
                ));
            }
            conn.execute(
                // 必须 UPSERT：这台设备以前装过（后来被解绑/吊销过）时，表里还留着同一
                // (subject, product, device_fp) 的行，裸 INSERT 会撞 UNIQUE 约束报 500，
                // 用户就卡死在「重装也装不上」——解绑后重新安装正是要走到这里。
                "INSERT INTO activations(subject_id, product_id, device_fp, device_pubkey, device_name,
                     status, created_at, last_seen_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)
                 ON CONFLICT(subject_id, product_id, device_fp)
                 DO UPDATE SET device_pubkey=excluded.device_pubkey, device_name=excluded.device_name,
                               status='active', last_seen_at=excluded.last_seen_at",
                params![auth.subject_id, product_id, fp, pk, body.device_name, now()],
            )?;
            (fp, pk)
        }
    };

    let dev_pub_raw = crate::crypto::b64d(&device_pubkey)?;
    if dev_pub_raw.len() != 32 {
        return Err(ApiError::internal("设备公钥损坏"));
    }
    let mut dev_pub = [0u8; 32];
    dev_pub.copy_from_slice(&dev_pub_raw);

    // KEK 解出 CEK → 用设备公钥重新包裹（每次下载新临时密钥对）
    let cek_wrap_nonce: Vec<u8> = conn.query_row(
        "SELECT cek_wrap_nonce FROM files WHERE id=?1",
        [&body.file_id],
        |r| r.get(0),
    )?;
    let cek = crate::crypto::unwrap_cek(&state.kek, &cek_wrapped, &cek_wrap_nonce)?;
    let wrap = crate::crypto::wrap_cek_for_device(&cek, &dev_pub)?;

    // 直链（5 分钟签名）
    let exp = now() + 300;
    let uid = crate::db::new_id("u_");
    let sig = crate::crypto::sign_download(&state.url_secret, &body.file_id, exp, &uid);
    let url = format!(
        "{}/dl/{}?exp={}&uid={}&sig={}",
        state.public_base.trim_end_matches('/'),
        body.file_id,
        exp,
        uid,
        sig
    );

    let file_nonce: Vec<u8> = conn.query_row(
        "SELECT file_nonce FROM files WHERE id=?1",
        [&body.file_id],
        |r| r.get(0),
    )?;

    conn.execute(
        "UPDATE activations SET last_seen_at=?1 WHERE subject_id=?2 AND product_id=?3 AND device_fp=?4",
        params![now(), auth.subject_id, product_id, device_fp],
    )?;
    conn.execute(
        "INSERT INTO download_logs(file_id, subject_id, device_fp, ts, ip, ua)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            body.file_id,
            auth.subject_id,
            device_fp,
            now(),
            client_ip(&headers),
            user_agent(&headers)
        ],
    )?;

    Ok(Json(json!({
        "url": url,
        "expires_at": exp,
        "file": {
            "file_id": body.file_id,
            "file_name": file_name,
            "version": version,
            "size": size,
            "sha256_plain": sha_plain,
        },
        "alg": "AES-256-GCM",
        "file_nonce": crate::crypto::b64(&file_nonce),
        "key_wrap": {
            "alg": "X25519-HKDF-SHA256-A256GCM",
            "epk": crate::crypto::b64(&wrap.epk),
            "salt": crate::crypto::b64(&wrap.salt),
            "wrap_nonce": crate::crypto::b64(&wrap.wrap_nonce),
            "cipher": crate::crypto::b64(&wrap.cipher),
        }
    })))
}

// ---------------------------------------------------------------- 直链下载（签名）

#[derive(Deserialize)]
pub struct DlQuery {
    pub exp: i64,
    pub uid: String,
    pub sig: String,
}

pub async fn download_link(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
    Query(q): Query<DlQuery>,
) -> Response {
    if q.exp < now() {
        return ApiError::new(StatusCode::GONE, "bad_request", "下载链接已过期").into_response();
    }
    if !crate::crypto::verify_download(&state.url_secret, &file_id, q.exp, &q.uid, &q.sig) {
        return ApiError::forbidden("bad_request", "下载签名无效").into_response();
    }
    let path: Option<String> = {
        let conn = state.conn();
        conn.query_row("SELECT storage_path FROM files WHERE id=?1", [&file_id], |r| r.get(0))
            .optional()
            .unwrap_or(None)
    };
    let Some(path) = path else {
        return ApiError::not_found("文件不存在").into_response();
    };
    match std::fs::read(&path) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=0, no-store".to_string(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => ApiError::internal(format!("读取密文失败: {e}")).into_response(),
    }
}

// ---------------------------------------------------------------- App 自更新（客户端本体）

/// GET /v1/app/latest —— App 启动/手动「检查更新」时问一句。
///
/// 不需要登录：装什么东西都还没影子的时候也可能要更新（老版本登录态失效就得先升级）。
/// 客户端本体免费分发，这条和下面的 `/app/<file>` 都是公开接口。
pub async fn app_latest(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let row: Option<(i64, String, Option<String>, String, String, i64, i64, i64)> = {
        let conn = state.conn();
        conn.query_row(
            "SELECT version_code, version_name, notes, file_name, sha256, size, min_version_code, created_at
             FROM app_releases WHERE is_current=1 ORDER BY id DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
                    r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
                ))
            },
        )
        .optional()?
    };
    let Some((code, name, notes, file, sha, size, min_code, created)) = row else {
        return Err(ApiError::not_found("还没有发布过 App 版本"));
    };
    Ok(Json(json!({
        "version_code": code,
        "version_name": name,
        "notes": notes.unwrap_or_default(),
        "url": format!("{}/app/{}", state.public_base.trim_end_matches('/'), file),
        "sha256": sha,
        "size": size,
        "min_version_code": min_code,
        "published_at": created,
    })))
}

/// GET /get（与 `/download` 同一个页面）——买家下载页。
///
/// 只保留下载所需的最小信息：版本 + 微信/QQ 打开提示 + 三条下载相关问题。
/// 安装步骤放在 App 内的「使用指引」里，页面不再重复。
pub async fn get_page(State(state): State<AppState>) -> Response {
    let row: Option<(String, i64)> = {
        let conn = state.conn();
        conn.query_row(
            "SELECT version_name, size
             FROM app_releases WHERE is_current=1 ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .unwrap_or(None)
    };
    let (version, size) = row.unwrap_or_else(|| ("—".to_string(), 0));
    let size_mb = if size > 0 {
        // 与浏览器下载时显示的口径一致（十进制 MB），避免出现「24 MB 的包写成 23.1 MB」
        format!("{:.1}", size as f64 / 1_000_000.0)
    } else {
        "—".to_string()
    };
    let html = GET_PAGE_HTML
        .replace("{{VERSION}}", &escape_html(&version))
        .replace("{{SIZE_MB}}", &size_mb);
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        html,
    )
        .into_response()
}

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// 对外文件名只允许这些字符：它同时是 URL 段与库内查询键，不给路径穿越留缝。
fn safe_app_file(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// GET /app/latest.apk —— 永久链接：302 到当前版本。
/// 爱发电商品详情、自动回复里贴这一条就永不过期。
pub async fn app_latest_apk(State(state): State<AppState>) -> Response {
    let file: Option<String> = {
        let conn = state.conn();
        conn.query_row(
            "SELECT file_name FROM app_releases WHERE is_current=1 ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None)
    };
    match file {
        Some(f) if safe_app_file(&f) => (
            StatusCode::FOUND,
            [
                (header::LOCATION, format!("/app/{f}")),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ],
        )
            .into_response(),
        _ => ApiError::not_found("还没有发布过 App 版本").into_response(),
    }
}

/// GET /app/<file> —— 直发 APK 文件（按库里的文件名精确查，不拼路径）。
pub async fn app_file(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    if !safe_app_file(&file) {
        return ApiError::not_found("文件不存在").into_response();
    }
    let path: Option<String> = {
        let conn = state.conn();
        conn.query_row(
            "SELECT storage_path FROM app_releases WHERE file_name=?1 ORDER BY id DESC LIMIT 1",
            [&file],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None)
    };
    let Some(path) = path else {
        return ApiError::not_found("文件不存在").into_response();
    };
    match std::fs::read(&path) {
        Ok(bytes) => (
            [
                (
                    header::CONTENT_TYPE,
                    "application/vnd.android.package-archive".to_string(),
                ),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file}\""),
                ),
                // 文件名里带版本号 → 同一个 URL 的内容永远不变，可以放心长缓存
                (header::CACHE_CONTROL, "public, max-age=86400".to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => ApiError::internal(format!("读取 APK 失败: {e}")).into_response(),
    }
}

// ---------------------------------------------------------------- 壁纸方案（照片表盘的壁纸生成器）

/// GET /v1/editor-template?watchface_id= —— App 拉这个表盘的壁纸方案。
///
/// **不需要登录**：配置与素材不是付费内容（定位是"不发 App 版本就能上新方案"）。
/// 没有方案/被停用时返回 `{enabled:false}` —— App 见到它就退回普通编辑页，**不报错**。
/// 素材只回**相对路径**（`/wp/<watchface_id>/<file>`），App 自己拼到 base 上；配置里不许有外链。
pub async fn editor_template(
    State(state): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let wfid = crate::editor::safe_watchface_id(
        q.get("watchface_id").map(|s| s.as_str()).unwrap_or(""),
    )?;
    let conn = state.conn();
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT config_json, enabled FROM editor_templates WHERE watchface_id = ?1",
            [&wfid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(ApiError::from)?;
    let Some((cfg, enabled)) = row else {
        return Ok(Json(json!({ "enabled": false, "watchfaceId": wfid })));
    };
    if enabled == 0 {
        return Ok(Json(json!({ "enabled": false, "watchfaceId": wfid })));
    }
    let mut stmt = conn
        .prepare("SELECT path, size, sha256 FROM editor_assets WHERE watchface_id = ?1 ORDER BY path")
        .map_err(ApiError::from)?;
    let assets: Vec<Value> = stmt
        .query_map([&wfid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(ApiError::from)?
        .filter_map(|r| r.ok())
        .map(|(path, size, sha)| {
            json!({ "path": path, "size": size, "sha256": sha,
                    "url": format!("/wp/{wfid}/{path}") })
        })
        .collect();
    Ok(Json(json!({
        "enabled": true,
        "watchfaceId": wfid,
        "config": serde_json::from_str::<Value>(&cfg).unwrap_or(Value::Null),
        "assets": assets,
    })))
}

/// GET /wp/<watchface_id>/<file> —— 直发方案素材（png/jpg/webp）。
///
/// 只按**库里的登记记录**精确查（不拼路径），文件名再走一次白名单 —— 双重保险，防目录穿越。
pub async fn editor_asset(
    State(state): State<AppState>,
    Path((wfid, file)): Path<(String, String)>,
) -> Response {
    let (Ok(wfid), Ok(file)) = (
        crate::editor::safe_watchface_id(&wfid),
        crate::editor::safe_asset_name(&file),
    ) else {
        return ApiError::not_found("素材不存在").into_response();
    };
    let ok: Option<String> = {
        let conn = state.conn();
        conn.query_row(
            "SELECT path FROM editor_assets WHERE watchface_id = ?1 AND path = ?2",
            params![&wfid, &file],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None)
    };
    if ok.is_none() {
        return ApiError::not_found("素材不存在").into_response();
    }
    let path = crate::editor::assets_dir(&state, &wfid).join(&file);
    let ctype = match file
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    };
    match std::fs::read(&path) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, ctype.to_string()),
                // 素材名按色号固定、内容有 sha256 兜底 → 可以长缓存
                (header::CACHE_CONTROL, "public, max-age=86400".to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => ApiError::internal(format!("读取素材失败: {e}")).into_response(),
    }
}

// ---------------------------------------------------------------- 客户端诊断上报（失败打点）

/// 一次上报最多收几笔（App 正常只发 1~3 笔；这是防灌垃圾的上限）
const EVENT_BATCH_MAX: usize = 50;

#[derive(Deserialize)]
pub struct EventItem {
    #[serde(default)]
    pub kind: Option<String>,
    pub step: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub device_model: Option<String>,
    /// App 安装时本地随机生成的设备指纹（同一台手机重装会变）
    #[serde(default)]
    pub device_fp: Option<String>,
    /// 手机厂商 + 型号（如 OnePlus PJZ110）——第一次连不上时手环型号为空，靠它区分是谁
    #[serde(default)]
    pub phone_model: Option<String>,
    /// 手机系统（如 Android 17 (SDK 37)）
    #[serde(default)]
    pub phone_os: Option<String>,
    #[serde(default)]
    pub app_version: Option<String>,
}

#[derive(Deserialize)]
pub struct EventBatch {
    pub events: Vec<EventItem>,
}

/// 字段一律截断后入库：这些是给排障看的线索，不是数据仓库，也防有人往里灌长文本。
fn clip(s: Option<&str>, max: usize) -> Option<String> {
    let t = s?.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.chars().take(max).collect())
}

/// 上报文本里**可能是密钥**的高熵长串先洗掉：我们的令牌/密钥都是 base64url 或十六进制、
/// 长度 ≥32 的连续串（43 位令牌、64 位 KEK/sha256）→ 一律替换成 [已隐藏]。
/// 订单号是 27 位纯数字、设备指纹是 64 位十六进制（这个会被洗掉，可接受）。
/// 2026-10-15 加：App 会把服务端返回的原始错误文本一起上报，万一哪条错误里带了令牌就落到库里了。
fn push_run(out: &mut String, run: &mut String) {
    if run.len() >= 32 {
        out.push_str("[已隐藏]");
    } else {
        out.push_str(run);
    }
    run.clear();
}

fn scrub_secretish(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            run.push(ch);
        } else {
            push_run(&mut out, &mut run);
            out.push(ch);
        }
    }
    push_run(&mut out, &mut run);
    out
}

/// POST /v1/unbind —— App「退出登录」时调用：把当前登录态在**服务端**立即作废。
///
/// 令牌是 bearer（默认 30 天），以前只在本地清掉 → 泄露的令牌能一直用到过期。
/// 这里做成**幂等**：令牌无效/已过期也返回 ok（退出登录不该因为令牌过期而报错）。
pub async fn unbind(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let Some(token) = bearer(&headers) else {
        return Ok(Json(json!({ "ok": true, "revoked": 0 })));
    };
    let th = crate::crypto::token_hash(&token);
    let n = {
        let conn = state.conn();
        conn.execute("DELETE FROM tokens WHERE token_hash = ?1", [&th])?
    };
    if n > 0 {
        println!("[unbind] 客户端主动退出登录：已作废 1 个登录态");
    }
    Ok(Json(json!({ "ok": true, "revoked": n })))
}

/// POST /v1/events —— App 上报「哪一步失败了」。
///
/// 为什么**不**强制登录：最早的一批失败（扫不到设备、蓝牙权限没给、绑定失败）都发生在拿到
/// `license_token` 之前，要求令牌等于把这些信号全丢掉。所以：能认到人就记 subject_id，
/// 认不到也照收；字段截断、超量丢弃，且**永远返回 200**——上报本身绝不能变成 App 里新的失败点。
pub async fn report_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EventBatch>,
) -> Json<Value> {
    let subject = require_client(&state, &headers).ok().map(|a| a.subject_id);
    let ip = client_ip(&headers);
    let ua = user_agent(&headers);
    let ts = now();
    let mut stored = 0usize;
    {
        let conn = state.conn();
        for e in body.events.iter().take(EVENT_BATCH_MAX) {
            let Some(step) = clip(Some(e.step.as_str()), 40) else {
                continue;
            };
            // 上报文本先洗掉可能混进来的密钥长串（服务端这道对所有版本都生效）
            let code = e.code.as_deref().map(scrub_secretish);
            let msg = e.message.as_deref().map(scrub_secretish);
            let ok = conn
                .execute(
                    "INSERT INTO client_events(ts, kind, step, code, message, device_model,
                         device_fp, phone_model, phone_os, app_version, subject_id, ip, ua)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                    params![
                        ts,
                        clip(e.kind.as_deref(), 16).unwrap_or_else(|| "fail".to_string()),
                        step,
                        clip(code.as_deref(), 80),
                        clip(msg.as_deref(), 500),
                        clip(e.device_model.as_deref(), 60),
                        clip(e.device_fp.as_deref(), 80),
                        clip(e.phone_model.as_deref(), 60),
                        clip(e.phone_os.as_deref(), 40),
                        clip(e.app_version.as_deref(), 20),
                        subject.as_deref(),
                        clip(Some(ip.as_str()), 60),
                        clip(Some(ua.as_str()), 120),
                    ],
                )
                .is_ok();
            if ok {
                stored += 1;
            }
        }
    }
    Json(json!({ "ok": true, "stored": stored }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_page_helpers_escape_html() {
        assert_eq!(
            escape_html("<b>\"x\"&'y'</b>"),
            "&lt;b&gt;&quot;x&quot;&amp;&#39;y&#39;&lt;/b&gt;"
        );
    }

    #[test]
    fn scrub_masks_long_secretish_runs_only() {
        // 43 位令牌（crypto::new_token 的形态）必须被洗掉
        let tok = "SB2o_1ZhbiGnqaBpeTg2uxmhLHt-cwc2RNxVl1Q_lUU";
        assert_eq!(
            scrub_secretish(&format!("授权失败 token={tok} 请联系我们")),
            "授权失败 token=[已隐藏] 请联系我们"
        );
        // 40 位 hex（≥32 → 应当被洗掉。这里刻意不用 64 位，免得公开仓库的密钥体检把测试夹具当真值）
        assert!(scrub_secretish("k=5f3d1c9b0e6a48d7b2c5e9a10f4d8c3b7e6a1f0d").contains("[已隐藏]"));
        // 订单号（27 位纯数字）与中文说明不能被动
        assert_eq!(
            scrub_secretish("订单 202610070148365297524823631 还没绑定账号"),
            "订单 202610070148365297524823631 还没绑定账号"
        );
        assert_eq!(scrub_secretish("蓝牙未开启，请先打开蓝牙"), "蓝牙未开启，请先打开蓝牙");
    }

    #[test]
    fn reclaims_only_stale_activation() {
        // 回归（2026-10-17）：随机 device_uuid 在重装后会变，名额满时必须能回收久未使用的旧记录，
        // 但两台都还在用时不能乱踢。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE activations(
                 subject_id TEXT NOT NULL, product_id TEXT NOT NULL, device_fp TEXT NOT NULL,
                 device_pubkey TEXT NOT NULL, device_name TEXT,
                 status TEXT NOT NULL, created_at INTEGER NOT NULL, last_seen_at INTEGER,
                 PRIMARY KEY(subject_id, product_id, device_fp));",
        )
        .unwrap();
        let now = crate::db::now();
        let ins = |fp: &str, seen: i64| {
            conn.execute(
                "INSERT INTO activations(subject_id,product_id,device_fp,device_pubkey,device_name,
                     status,created_at,last_seen_at)
                 VALUES('s','p',?1,'pk',NULL,'active',?2,?2)",
                rusqlite::params![fp, seen],
            )
            .unwrap();
        };
        ins("old", now - 40 * 86400); // 40 天没出现
        ins("live", now - 3600); // 1 小时前还在用

        assert!(reclaim_stale_activation(&conn, "s", "p", 7 * 86400).unwrap());
        let mut st = conn
            .prepare("SELECT device_fp || ':' || status FROM activations ORDER BY device_fp")
            .unwrap();
        let rows: Vec<String> = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, vec!["live:active", "old:revoked"]);

        // 剩下那台在宽限期内 → 不许回收
        assert!(!reclaim_stale_activation(&conn, "s", "p", 7 * 86400).unwrap());
        // 宽限期设为 0 = 关闭回收
        conn.execute(
            "UPDATE activations SET last_seen_at=?1 WHERE device_fp='live'",
            [now - 40 * 86400],
        )
        .unwrap();
        assert!(!reclaim_stale_activation(&conn, "s", "p", 0).unwrap());
    }

    #[test]
    fn oauth_poll_returns_token_expiry_not_oauth_window() {
        // 回归（2026-10-17）：App 把 poll 返回的 expires_at 当作令牌过期时间存盘。
        // 曾经这里返回 afdian_oauth 的 15 分钟授权窗口，导致绑定 15 分钟后被判成「未绑定」。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE tokens(token_hash TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);")
            .unwrap();
        let tok = "SB2o_1ZhbiGnqaBpeTg2uxmhLHt-cwc2RNxVl1Q_lUU";
        let token_expiry = 1_792_210_026i64; // 令牌自己的有效期
        conn.execute(
            "INSERT INTO tokens(token_hash, expires_at) VALUES(?1, ?2)",
            rusqlite::params![crate::crypto::token_hash(tok), token_expiry],
        )
        .unwrap();

        // 查得到 → 必须返回令牌有效期，而不是传入的 15 分钟授权窗口
        assert_eq!(
            license_token_expires_at(&conn, tok, 111).unwrap(),
            token_expiry
        );
        // 查不到 / 空令牌 → 兜底（正常不会走到）
        assert_eq!(license_token_expires_at(&conn, "unknown", 111).unwrap(), 111);
        assert_eq!(license_token_expires_at(&conn, "", 111).unwrap(), 111);
    }
}
