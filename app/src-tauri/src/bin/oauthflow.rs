//! 爱发电 OAuth 绑定链路自测 CLI（不需要设备、不需要界面，走 App 的真实代码路径）：
//! （可选）先在店里"买过" → 发起绑定 → 模拟用户在浏览器里授权 → 轮询取登录态 → 已购表盘自动到账。
//!
//! 用法（服务端需开 WF_DEV_BIND=1 且配了 WF_AFDIAN_CLIENT_ID/SECRET）：
//!   oauthflow <数据目录> <服务端地址> <小米UID> <爱发电UID>
//! 想让「已购资源」非空（完整复现"买过再绑定"），再加上两个环境变量：
//!   WF_ADMIN_TOKEN=<管理令牌> WF_AFDIAN_PLAN=<购买链接里的商品编号>
//! 例：
//!   oauthflow ./paid-oauth http://127.0.0.1:8787 mi-test afdian-test

use wf_installer_lib::paid::{self, Ctx};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("用法: oauthflow <数据目录> <服务端地址> <小米UID> <爱发电UID>");
        std::process::exit(2);
    }
    let dir = std::path::PathBuf::from(&args[1]);
    let base = args[2].clone();
    let mi_uid = args[3].clone();
    let afdian_uid = args[4].clone();

    let ctx = Ctx::load_or_create(dir);
    println!("[1] 设备指纹 {}", &ctx.fp[..16]);

    // 0) 可选：先给这个爱发电账号造一笔成功订单（复现"他早就买过"）
    let admin = std::env::var("WF_ADMIN_TOKEN").unwrap_or_default();
    let plan = std::env::var("WF_AFDIAN_PLAN").unwrap_or_default();
    if !admin.is_empty() && !plan.is_empty() {
        let out_trade_no = format!("OAuthFlow-{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?.as_secs());
        let r = reqwest::Client::new()
            .post(format!("{}/admin/afdian/simulate", base.trim_end_matches('/')))
            .bearer_auth(&admin)
            .json(&serde_json::json!({
                "afdian_user_id": afdian_uid, "plan_id": plan, "status": 2,
                "out_trade_no": out_trade_no,
            }))
            .send()
            .await?;
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        println!(
            "[1.5] 先造一笔已付款订单 {out_trade_no}（未绑定时先攒着：unbound={}）",
            v["summary"]["unbound"].as_i64().unwrap_or(-1)
        );
    } else {
        println!("[1.5] 跳过造单（没给 WF_ADMIN_TOKEN / WF_AFDIAN_PLAN）");
    }

    // 1) 发起绑定：拿到授权页地址 + 会话号（真机上这一步之后会用系统浏览器打开 url）
    let started = paid::bind_oauth_start(&base, &mi_uid, &ctx.fp, &ctx.pub_b64, "oauthflow").await?;
    let state = started["state"].as_str().unwrap_or_default().to_string();
    let url = started["url"].as_str().unwrap_or_default().to_string();
    println!("[2] 授权页 {url}");
    anyhow::ensure!(!state.is_empty(), "没有拿到会话号");

    // 2) 还没授权 → 应该还是 pending
    let p = paid::bind_oauth_poll(&base, &state).await?;
    println!("[3] 未授权时轮询 → {}", p["status"].as_str().unwrap_or("?"));
    anyhow::ensure!(p["status"] == serde_json::json!("pending"), "预期 pending");

    // 3) 模拟浏览器完成授权（联调：code=dev:<爱发电UID>；真机上是爱发电跳回来）
    let cb = format!("{}/v1/afdian/callback?state={state}&code=dev:{afdian_uid}", base.trim_end_matches('/'));
    let html = reqwest::get(&cb).await?.text().await?;
    println!("[4] 回调完成授权：{}", if html.contains("授权成功") { "授权成功页 ✓" } else { "返回异常 ✗" });
    anyhow::ensure!(html.contains("授权成功"), "回调没有返回成功页");

    // 4) 再轮询 → ok + 登录态
    let p = paid::bind_oauth_poll(&base, &state).await?;
    let token = p["license_token"].as_str().unwrap_or_default().to_string();
    println!("[5] 授权后轮询 → {}（账号尾号 {}）", p["status"].as_str().unwrap_or("?"), p["display"].as_str().unwrap_or(""));
    anyhow::ensure!(!token.is_empty(), "没有拿到登录态");

    // 5) 用这个登录态查已购资源（真机上接着就是列表 → 安装）
    let me = paid::me(&base, &token).await?;
    let products = me["products"].as_array().cloned().unwrap_or_default();
    println!(
        "[6] 已购资源 {} 个（账号 {}）",
        products.len(),
        me["subject"]["display"].as_str().unwrap_or("?")
    );
    for p in &products {
        println!(
            "    · {}（{} 个可装设备行）",
            p["name"].as_str().unwrap_or("?"),
            p["variants"].as_array().map(|a| a.len()).unwrap_or(0)
        );
    }
    if !plan.is_empty() {
        anyhow::ensure!(
            !products.is_empty(),
            "买过却没到账：检查购买链接（plan_id={plan}）是否配在某个设备行上"
        );
        println!("[7] 买过 → 绑定后自动到账 ✓");
    }

    println!("== ✅ OAuth 绑定链路通过 ==");
    Ok(())
}
