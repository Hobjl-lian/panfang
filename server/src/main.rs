//! 盘坊授权服务（wf-auth-server）
//!
//! 付费表盘「加密分发 + 兑换码 + 授权」服务端，实现 `docs/接口契约-授权服务-v1.md`。
//! 本机跑通：`cargo run`（默认 127.0.0.1:8787，管理页 http://127.0.0.1:8787/）

use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, Method};
use axum::routing::{get, post};
use axum::Router;
use wf_auth_server::{afdian, api_admin, api_client, crypto, db, state::AppState};
use std::sync::{Arc, Mutex};
use tower_http::cors::CorsLayer;

const ADMIN_HTML: &str = include_str!("../static/admin.html");
const PRIVACY_HTML: &str = include_str!("../static/privacy.html");
const TERMS_HTML: &str = include_str!("../static/terms.html");

/// 定期清理过期数据：未完成的绑定会话（成功的保留作审计留痕）与早已过期的登录态。
/// 2026-10-15 加：此前这两张表只增不减，是「刷库」隐患（未登录接口也能建行）。
fn cleanup_expired(state: &AppState) {
    let now = db::now();
    let conn = state.conn();
    if let Ok(n) = conn.execute(
        "DELETE FROM afdian_oauth WHERE status <> 'ok' AND expires_at < ?1",
        [now - 86_400],
    ) {
        if n > 0 {
            println!("[cleanup] 清掉 {n} 条过期绑定会话");
        }
    }
    if let Ok(n) = conn.execute("DELETE FROM tokens WHERE expires_at < ?1", [now - 90 * 86_400]) {
        if n > 0 {
            println!("[cleanup] 清掉 {n} 条过期登录态");
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind = std::env::var("WF_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let data_dir = std::path::PathBuf::from(
        std::env::var("WF_DATA_DIR").unwrap_or_else(|_| "./data".into()),
    );
    let cipher_dir = data_dir.join("cipher");
    std::fs::create_dir_all(&cipher_dir)?;

    let kek = crypto::load_or_create_kek(&data_dir)?;
    let url_secret = crypto::load_or_create_url_secret(&data_dir)?;

    let admin_token = match std::env::var("WF_ADMIN_TOKEN") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            // 默认令牌等于没有鉴权：线上绝不允许。本机调试要显式开 WF_ALLOW_DEV_ADMIN=1。
            if std::env::var("WF_ALLOW_DEV_ADMIN").ok().as_deref() == Some("1") {
                println!("[wf-auth] 警告：WF_ADMIN_TOKEN 未设置 → 使用开发令牌（因显式设了 WF_ALLOW_DEV_ADMIN=1）");
                "dev-admin-token".to_string()
            } else {
                anyhow::bail!(
                    "未设置 WF_ADMIN_TOKEN 且未开 WF_ALLOW_DEV_ADMIN=1：管理接口会变成无鉴权，拒绝启动。\
                     （本机调试：WF_ALLOW_DEV_ADMIN=1 cargo run）"
                );
            }
        }
    };
    let public_base = std::env::var("WF_PUBLIC_BASE")
        .unwrap_or_else(|_| format!("http://{}", if bind.starts_with("0.0.0.0") {
            bind.replace("0.0.0.0", "127.0.0.1")
        } else {
            bind.clone()
        }));
    let device_limit = std::env::var("WF_DEVICE_LIMIT")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(2);

    let conn = db::open(&data_dir.join("wf.db"))?;
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        data_dir: data_dir.clone(),
        cipher_dir,
        kek,
        url_secret,
        admin_token: admin_token.clone(),
        public_base: public_base.clone(),
        device_limit,
        token_ttl: 30 * 24 * 3600,
    };
    let bg_state = state.clone();

    // 首次启动：若无任何产品，打印一条指引
    {
        let conn = state.conn();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM products", [], |r| r.get(0))
            .unwrap_or(0);
        if n == 0 {
            println!("[wf-auth] 空库。管理页打开后先建产品/设备变体，再上传 .bin。");
        }
    }

    let admin_routes = Router::new()
        // 设备组（型号 ↔ 组 对照表）
        .route(
            "/device-groups",
            get(api_admin::device_groups_list).post(api_admin::device_group_upsert),
        )
        // 资源（表盘）
        .route(
            "/products",
            post(api_admin::create_product).get(api_admin::list_products),
        )
        .route("/products/reorder", post(api_admin::reorder_products))
        .route(
            "/products/:id",
            get(api_admin::product_detail).delete(api_admin::delete_product),
        )
        // 设备行 + 付费映射
        .route("/variants", post(api_admin::create_variant))
        .route("/variants/:id", axum::routing::delete(api_admin::delete_variant))
        .route("/variants/afdian", post(api_admin::set_variant_afdian))
        .route("/variants/afdian/remove", post(api_admin::remove_variant_afdian))
        // 包体
        .route("/inspect", post(api_admin::inspect_upload))
        .route("/versions/upload", post(api_admin::upload_version))
        // 手动开通（售后/联调兜底）
        .route("/grant", post(api_admin::grant))
        // 某账号永久全资源（含以后新增的产品）
        .route("/subject-all-access", post(api_admin::subject_all_access))
        // 爱发电：订单同步 / 订单列表 / 手动绑定 / 模拟订单（联调开关下）
        .route("/afdian/sync", post(api_admin::afdian_sync))
        .route("/afdian/orders", get(api_admin::afdian_orders))
        .route("/afdian/sync-status", get(api_admin::afdian_sync_status))
        .route("/afdian/spec-report", get(api_admin::afdian_spec_report))
        .route("/afdian/bind", post(api_admin::afdian_bind))
        .route("/afdian/simulate", post(api_admin::afdian_simulate))
        // 授权与统计
        .route("/activations", get(api_admin::activations_list))
        .route("/revoke", post(api_admin::revoke))
        .route("/stats", get(api_admin::stats))
        // 客户端诊断（App 上报的失败打点）
        .route("/events", get(api_admin::events_summary))
        // App 发版（客户端本体的更新通道）
        .route(
            "/app/releases",
            get(api_admin::app_releases_list).post(api_admin::app_release_upload),
        )
        .route(
            "/app/releases/:id/current",
            post(api_admin::app_release_set_current),
        )
        .route(
            "/app/releases/:id",
            axum::routing::delete(api_admin::app_release_delete),
        );

    // 兑换码已下线（界面与 App 都不再用；接口一并移除，避免留无保护的攻击面）
    let client_routes = Router::new()
        .route("/bind/mode", get(api_client::bind_mode))
        // App 检查更新（公开：还没登录也可能要先升级，比如旧版的登录态失效）
        .route("/app/latest", get(api_client::app_latest))
        .route("/bind/dev", post(api_client::bind_dev))
        .route("/afdian/oauth/start", post(api_client::afdian_oauth_start))
        .route("/afdian/oauth/poll", get(api_client::afdian_oauth_poll))
        .route("/afdian/callback", get(api_client::afdian_oauth_callback))
        .route("/afdian/dev-authorize", get(api_client::afdian_dev_authorize))
        .route("/me", get(api_client::me))
        .route("/unbind", post(api_client::unbind))
        .route("/activate", post(api_client::activate))
        .route("/manifest", get(api_client::manifest))
        .route("/download", post(api_client::download))
        // 失败打点：不需要登录（最早那批失败发生在拿到令牌之前）
        .route("/events", post(api_client::report_events));

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(|| async { axum::response::Html(ADMIN_HTML) }))
        // 隐私政策与用户协议：App 内「关于」与登录/购买页都指向这里（已备案域名下的静态页）
        .route("/privacy", get(|| async { axum::response::Html(PRIVACY_HTML) }))
        .route("/terms", get(|| async { axum::response::Html(TERMS_HTML) }))
        .route("/dl/:file_id", get(api_client::download_link))
        // App 自更新：latest 给 App 检查更新；latest.apk 是永久链接（302 到当前版）；
        // app/<file> 直发安装包（客户端本体免费，公开可下）
        .route("/app/latest.apk", get(api_client::app_latest_apk))
        .route("/app/:file", get(api_client::app_file))
        .nest("/v1", client_routes)
        .nest("/admin", admin_routes)
        // 一次请求可能带多个包（同一设备拆成两个独立表盘）→ 上限按「单包 64MB × 3」留余量
        .layer(DefaultBodyLimit::max(192 * 1024 * 1024))
        // CORS：只允许**我们自己的**站点跨域（2026-10-15 从「任意来源」收紧）。
        // 目前没有任何跨域调用方（管理页与 App 都是同源/原生请求），这里是给将来的网页端留的口子。
        .layer(
            CorsLayer::new()
                .allow_origin([
                    HeaderValue::from_static("https://api.hobjl-shop.top"),
                    HeaderValue::from_static("https://hobjl-shop.top"),
                    HeaderValue::from_static("https://www.hobjl-shop.top"),
                ])
                .allow_methods([Method::GET, Method::POST, Method::DELETE])
                .allow_headers(tower_http::cors::Any),
        )
        .layer(axum::middleware::from_fn(log_requests))
        .with_state(state);

    // 后台维护（每 10 分钟一轮）：
    //   1) 清过期数据（未完成的绑定会话、早已过期的登录态）—— 以前从不清理，是「刷库」隐患；
    //   2) 配了爱发电令牌就顺手做一轮订单对账（幂等）。
    {
        let st = bg_state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
            loop {
                tick.tick().await;
                cleanup_expired(&st);
                let configured = std::env::var("WF_AFDIAN_TOKEN")
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false);
                if !configured {
                    continue;
                }
                match afdian::sync_once(&st, 5).await {
                    Ok((sum, n)) => println!(
                        "[afdian] 定时同步：拉取 {} 单，新开通 {}，收回 {}，未绑定 {}",
                        n,
                        sum.new_grants.len(),
                        sum.revoked.len(),
                        sum.unbound
                    ),
                    Err(e) => println!("[afdian] 定时同步失败：{e:#}"),
                }
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("[wf-auth] 监听 http://{bind}（管理页 /，数据目录 {}）", data_dir.display());
    println!("[wf-auth] 管理员令牌：{}", mask(&admin_token));
    println!("[wf-auth] 公开地址（直链用）：{public_base}");
    // 联调开关会「免费放开全部资源」，启动时大声说清楚当前状态（含自动到期）
    {
        let st = afdian::dev_status();
        if st["on"] == serde_json::json!(true) {
            println!(
                "[wf-auth] ⚠️ 联调模式【开启中】—— /v1/bind/dev 会给任何人开通全部资源；到期时间 {}\n\
                 [wf-auth]    发布前请确认它已自动到期（或删掉 WF_DEV_BIND / WF_DEV_BIND_UNTIL）",
                st["until_text"].as_str().unwrap_or("未设置")
            );
        } else {
            println!("[wf-auth] 联调模式：关闭（{}）", st["why"].as_str().unwrap_or(""));
        }
    }
    axum::serve(listener, app).await?;
    Ok(())
}

/// 极简请求日志（排障用）：方法 路径 → 状态 耗时
async fn log_requests(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let t0 = std::time::Instant::now();
    let resp = next.run(req).await;
    println!(
        "[req] {} {} → {} ({}ms)",
        method,
        path,
        resp.status().as_u16(),
        t0.elapsed().as_millis()
    );
    resp
}

fn mask(s: &str) -> String {
    if s.len() <= 4 {
        return "****".into();
    }
    format!("{}****{}", &s[..2], &s[s.len() - 2..])
}
