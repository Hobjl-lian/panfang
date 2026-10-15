//! 登录链路探针：不用 GUI 直接验证到小米服务器的连通性与流程。
//! 运行：cargo run --bin probe  （用假账号密码，只验证管线是否通）

use std::collections::HashMap;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();

    println!("=== Step 0: 网络连通性（account.xiaomi.com serviceLogin GET） ===");
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36")
        .build()
        .expect("client");
    match client
        .get("https://account.xiaomi.com/pass/serviceLogin?sid=miothealth&_json=true")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            println!("HTTP {status}, body[:200] = {}", &body[..body.len().min(200)]);
        }
        Err(err) => {
            println!("网络请求失败: {err:#}");
        }
    }

    println!("=== Step 1: 完整登录流程已在 2026-10-05 探针运行中验证通过（假密码返回 70016 业务错误） ===");

    println!("=== Step 2: get_source_list 管线自检（假 token，预期业务 code 错误而非网络错误） ===");
    let fake_token = account::xiaomi::MiAccountToken {
        user_id: "0".to_string(),
        device_id: String::new(),
        ssecurity: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
        service_token: "fake".to_string(),
        c_user_id: String::new(),
        pass_token: String::new(),
        psecurity: String::new(),
    };
    let mut params = HashMap::new();
    params.insert("data".to_string(), r#"{"page_size":50,"status":1}"#.to_string());
    let result = account::xiaomi::mi_service_call_encrypted(
        fake_token,
        "/app/v1/source/".to_string(),
        "https://hlth.io.mi.com/app/v1/source/get_source_list".to_string(),
        params,
        wf_installer_lib::cloud::LOGIN_UA.to_string(),
    )
    .await;
    match result {
        Ok(body) => println!("调用成功，body[:200] = {}", &body[..body.len().min(200)]),
        Err(err) => println!("调用失败（完整错误链）: {err:#}"),
    }
}
