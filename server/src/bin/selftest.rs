//! 端到端自测：把真实 .bin 走完整链路，并在「设备侧」解开密钥、解密、比对哈希。
//!
//! ```bash
//! # 终端 1
//! WF_ADMIN_TOKEN=dev cargo run
//! # 终端 2（默认读取 D:/设计相关/AWOS-照片表盘/表盘文件）
//! WF_BIN_DIR="D:/设计相关/AWOS-照片表盘/表盘文件" cargo run --bin selftest
//! ```
//!
//! 覆盖：建产品/变体 → 上传即加密（自动识别变体+版本）→ 发码 → 兑换（含重复码/设备上限
//! 负例）→ 清单 → 下载（ECIES 包裹）→ **设备侧解密并与原文件逐字节比对**。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;
use x25519_dalek::{PublicKey, StaticSecret};

const WRAP_INFO: &[u8] = b"wf-cek-wrap-v1";

// ---------------------------------------------------------------- 极简 HTTP 客户端

struct Http {
    base: String, // http://127.0.0.1:8787
}

struct Resp {
    status: u16,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::json!({}))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

impl Http {
    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: Option<Vec<u8>>,
    ) -> Resp {
        let addr = self.base.trim_start_matches("http://");
        let mut stream = TcpStream::connect(addr).expect("连接服务失败（先启动 wf-auth-server）");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(8)))
            .unwrap();
        let body = body.unwrap_or_default();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        let mut raw = Vec::new();
        // 带超时读取：超时后就用已收到的数据（诊断用）
        let _ = stream.read_to_end(&mut raw);
        if raw.is_empty() {
            eprintln!("[debug] {method} {path} 无响应字节");
        }

        // 解析状态行 + 头 + 体（服务端一律带 Content-Length）
        let head_end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("响应不完整");
        let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
        let status: u16 = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = raw[head_end + 4..].to_vec();
        Resp { status, body }
    }

    fn post_json(&self, path: &str, token: Option<&str>, json: serde_json::Value) -> Resp {
        let mut headers = vec![("Content-Type", "application/json".to_string())];
        if let Some(t) = token {
            headers.push(("Authorization", format!("Bearer {t}")));
        }
        self.request("POST", path, &headers, Some(json.to_string().into_bytes()))
    }

    fn get(&self, path: &str, token: Option<&str>) -> Resp {
        let mut headers = vec![];
        if let Some(t) = token {
            headers.push(("Authorization", format!("Bearer {t}")));
        }
        self.request("GET", path, &headers, None)
    }

    /// multipart/form-data 上传
    fn upload(
        &self,
        path: &str,
        token: &str,
        fields: &[(&str, &str)],
        file_name: &str,
        file_bytes: &[u8],
    ) -> Resp {
        let boundary = "----wfauthselftest7f3a";
        let mut body: Vec<u8> = Vec::new();
        for (k, v) in fields {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes(),
            );
        }
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let headers = vec![
            (
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            ),
            ("Authorization", format!("Bearer {token}")),
        ];
        self.request("POST", path, &headers, Some(body))
    }

    /// 一批多个包（同一设备拆成的两个独立表盘）用一次请求上传
    fn upload_many(
        &self,
        path: &str,
        token: &str,
        fields: &[(&str, &str)],
        files: &[(String, Vec<u8>)],
    ) -> Resp {
        let boundary = "----wfauthselftest7f3a";
        let mut body: Vec<u8> = Vec::new();
        for (k, v) in fields {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes(),
            );
        }
        for (name, bytes) in files {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
                     Content-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        let headers = vec![
            (
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            ),
            ("Authorization", format!("Bearer {token}")),
        ];
        self.request("POST", path, &headers, Some(body))
    }
}

// ---------------------------------------------------------------- 小工具

fn b64d(s: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).unwrap()
}
fn b64(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}
fn sha256_hex(d: &[u8]) -> String {
    hex::encode(Sha256::digest(d))
}

fn ok(cond: bool, label: &str) {
    println!("  {} {}", if cond { "✅" } else { "❌" }, label);
    if !cond {
        FAILED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
static FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 设备侧：用私钥解开 CEK，再解出明文（与 App 端 `secure_dl` 同一套步骤）。
fn device_decrypt(
    device_priv: &StaticSecret,
    wrap: &serde_json::Value,
    file_nonce_b64: &str,
    cipher: &[u8],
) -> Vec<u8> {
    let epk: [u8; 32] = b64d(wrap["epk"].as_str().unwrap()).try_into().unwrap();
    let salt = b64d(wrap["salt"].as_str().unwrap());
    let wrap_nonce = b64d(wrap["wrap_nonce"].as_str().unwrap());
    let wrapped = b64d(wrap["cipher"].as_str().unwrap());

    let shared = device_priv.diffie_hellman(&PublicKey::from(epk));
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut kek = [0u8; 32];
    hk.expand(WRAP_INFO, &mut kek).unwrap();

    let cek = Aes256Gcm::new_from_slice(&kek)
        .unwrap()
        .decrypt(Nonce::from_slice(&wrap_nonce), Payload { msg: &wrapped, aad: b"" })
        .expect("设备侧解包 CEK 失败");

    Aes256Gcm::new_from_slice(&cek)
        .unwrap()
        .decrypt(
            Nonce::from_slice(&b64d(file_nonce_b64)),
            Payload { msg: cipher, aad: b"wf-file-v1" },
        )
        .expect("文件解密失败")
}

// ---------------------------------------------------------------- 主流程

#[derive(Clone)]
struct Family {
    label: &'static str,
    models: &'static [&'static str],
    screen: (i64, i64),
    files: [&'static str; 3], // v1(根目录)、v2(2.0/)、v3(3.0/)
}

fn main() {
    let base = std::env::var("WF_BASE").unwrap_or_else(|_| "http://127.0.0.1:8787".into());
    let admin = std::env::var("WF_ADMIN_TOKEN").unwrap_or_else(|_| "dev-admin-token".into());
    let bin_dir = std::path::PathBuf::from(
        std::env::var("WF_BIN_DIR")
            .unwrap_or_else(|_| "D:/设计相关/AWOS-照片表盘/表盘文件".into()),
    );
    let http = Http { base: base.clone() };

    println!("== 盘坊授权服务 端到端自测 ==\n   服务 {base}\n   .bin 目录 {}\n", bin_dir.display());

    // 0. 健康检查
    let h = http.get("/healthz", None);
    ok(h.status == 200, "服务在线 (/healthz)");
    if h.status != 200 {
        std::process::exit(1);
    }

    // 1. 建资源（名称 + 批量勾选设备组）
    println!("
[1] 建资源与设备行");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() % 100000;
    let product_id = format!("awos-st{stamp}");
    // 按新流程：只给名字、**不预选设备**（行由上传时自动建立）
    let r = http.post_json(
        "/admin/products",
        Some(&admin),
        serde_json::json!({"id": product_id, "name": "AWOS-照片表盘（联调）", "device_group_ids": []}),
    );
    ok(r.status == 200, &format!("资源已创建（{}）", r.status));
    let expect_groups = ["band9", "band9pro_10pro", "band10_11", "rw5"];
    let variant_ids: Vec<String> = expect_groups.iter().map(|g| format!("{product_id}@{g}")).collect();
    let r = http.get("/admin/products", Some(&admin));
    let found = r.json()["products"].as_array().map(|a| a.iter().any(|p| p["id"] == serde_json::json!(product_id))).unwrap_or(false);
    ok(found, "资源列表里能看到它");

    // 四个设备族（顺序与上面对应的设备组一致）
    let families = [
        Family { label: "手环9", models: &["小米手环9"], screen: (192, 490),
                 files: ["AWOS-照片表盘-9.bin", "2.0/AWOS-9-v2.bin", "3.0/AWOS-9-v3.bin"] },
        Family { label: "9Pro/10Pro", models: &["小米手环9 Pro", "小米手环10 Pro"], screen: (336, 480),
                 files: ["AWOS-照片表盘-9P10P.bin", "2.0/AWOS-9P10P-v2.bin", "3.0/AWOS-9P10P-v3.bin"] },
        Family { label: "手环10/11", models: &["小米手环10", "小米手环11"], screen: (212, 520),
                 files: ["AWOS-照片表盘-10.bin", "2.0/AWOS-10-v2.bin", "3.0/AWOS-1011-v3.bin"] },
        Family { label: "RW5/RW6", models: &["Redmi Watch 5", "Redmi Watch 6"], screen: (432, 514),
                 files: ["AWOS-照片表盘-RW5RW6.bin", "2.0/AWOS-RW5RW6-v2.bin", "3.0/AWOS-RW5RW6-v3.bin"] },
    ];

    // 2. 上传 12 个真实 .bin（3 个版本 × 4 个变体）
    println!("\n[2] 上传 .bin（自动识别变体与版本）");
    for (vi, f) in families.iter().enumerate() {
        for (stage, rel) in f.files.iter().enumerate() {
            let path = bin_dir.join(rel);
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    ok(false, &format!("读取 {} 失败: {e}", path.display()));
                    continue;
                }
            };
            let fname = path.file_name().unwrap().to_string_lossy().to_string();
            let r = http.upload(
                "/admin/versions/upload",
                &admin,
                &[("product_id", product_id.as_str()), ("path_hint", rel)],
                &fname,
                &bytes,
            );
            let j = r.json();
            let det = &j["detection"];
            let ver = j["version"].as_str().unwrap_or("?");
            // 期望版本号 = 各设备族头部版本号（实测值）
            let expect_ver = match (vi, stage) {
                (0, 0) => "1.0.0", (0, 1) => "1.0.1", (0, 2) => "1.0.2",   // 手环9
                (1, 0) => "1.0.1", (1, 1) => "1.0.2", (1, 2) => "1.0.3",   // 9Pro/10Pro
                (2, 0) => "1.0.2", (2, 1) => "1.0.3", (2, 2) => "1.0.4",   // 手环10/11
                (3, 0) => "1.0.6", (3, 1) => "1.0.7", (3, 2) => "1.0.8",   // RW5/RW6
                _ => "?",
            };
            // 走真实识别：应自动归位到该设备族对应的行
            let variant_ok = j["variantId"].as_str() == Some(variant_ids[vi].as_str());
            ok(
                r.status == 200 && ver == expect_ver && variant_ok,
                &format!(
                    "{:<28} → 变体 {} · 版本 {ver}（期望 {expect_ver}）· 头部 {} · 画布 {} · 识别依据 {}/{}",
                    rel,
                    if variant_ok { "✓" } else { "✗" },
                    j["bin"]["fmtVersion"].as_str().unwrap_or("?"),
                    j["bin"]["canvas"].as_str().unwrap_or("?"),
                    det["variantMatch"].as_str().unwrap_or("-"),
                    det["versionReason"].as_str().unwrap_or("-"),
                ),
            );
        }
    }

    // 2b. 一批两个包（同一设备拆成两个独立表盘）：一次请求上传，按文件名自然排序
    println!("\n[2b] 一批两个包（同设备两个独立表盘）");
    let batch_pid = format!("awos-batch-{stamp}");
    let r = http.post_json(
        "/admin/products",
        Some(&admin),
        serde_json::json!({"id": batch_pid, "name": "拆包联调（可删）", "device_group_ids": []}),
    );
    ok(r.status == 200, &format!("建拆包测试资源（{}）", r.status));
    // 两个 336×480 的真实文件、表盘 id 各不相同（惊鸿-10P 与 AWOS-9P10P）
    let pair = [
        ("b10-AWOS9P.bin", bin_dir.join("3.0/AWOS-9P10P-v3.bin")),
        ("b2-惊鸿10P.bin", std::path::PathBuf::from("D:/设计相关/惊鸿/表盘文件/2/惊鸿-10P.bin")),
    ];
    let mut payload: Vec<(String, Vec<u8>)> = Vec::new();
    for (n, p) in pair.iter() {
        match std::fs::read(p) {
            Ok(b) => payload.push((n.to_string(), b)),
            Err(e) => ok(false, &format!("读取 {} 失败: {e}", p.display())),
        }
    }
    if payload.len() == 2 {
        let r = http.upload_many(
            "/admin/versions/upload",
            &admin,
            &[("product_id", batch_pid.as_str())],
            &payload,
        );
        let j = r.json();
        let fs = j["files"].as_array().cloned().unwrap_or_default();
        let order_ok = fs.len() == 2
            && fs[0]["fileName"] == serde_json::json!("b2-惊鸿10P.bin")  // 自然排序：2 < 10
            && fs[1]["fileName"] == serde_json::json!("b10-AWOS9P.bin")
            && fs[0]["sort"] == serde_json::json!(0)
            && fs[1]["sort"] == serde_json::json!(1);
        ok(
            r.status == 200 && j["count"] == serde_json::json!(2) && order_ok,
            &format!(
                "一次传两个包 → 同版本 2 个文件，按文件名自然排序（状态 {}，{} 个，顺序 {}{}）",
                r.status,
                fs.len(),
                fs.first().and_then(|f| f["fileName"].as_str()).unwrap_or("?"),
                if order_ok { " ✓" } else { " ✗（期望 b2 在前、b10 在后）" },
            ),
        );
        ok(
            j["variantId"].as_str() == Some(format!("{batch_pid}@band9pro_10pro").as_str()),
            "两个包归到同一设备行（band9pro_10pro）",
        );
        // 管理页详情里应能看到「2 个包」，且顺序一致
        let d = http.get(&format!("/admin/products/{batch_pid}"), Some(&admin));
        let pk = d.json()["variants"][0]["packages"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        ok(
            pk.len() == 2 && pk[0]["sort"] == serde_json::json!(0) && pk[1]["sort"] == serde_json::json!(1),
            &format!("资源详情返回 2 个包（实际 {}）", pk.len()),
        );
        // 重复上传整批 → 拒
        let r2 = http.upload_many(
            "/admin/versions/upload",
            &admin,
            &[("product_id", batch_pid.as_str()), ("force", "true")],
            &payload,
        );
        ok(
            r2.status == 409 && r2.json()["error"] == serde_json::json!("duplicate"),
            &format!("整批重复上传被拒（409 duplicate，实际 {}）", r2.status),
        );
        // 同表盘 id 的两个包 → 必须拒绝（设备上会互相覆盖、留档会丢一个）
        let same_id = [
            ("v1.bin", std::path::PathBuf::from("D:/设计相关/惊鸿/表盘文件/惊鸿-9-V1.bin")),
            ("v1.2.bin", std::path::PathBuf::from("D:/设计相关/惊鸿/表盘文件/惊鸿-9-V1.2.bin")),
        ];
        let mut p2: Vec<(String, Vec<u8>)> = Vec::new();
        for (n, p) in same_id.iter() {
            if let Ok(b) = std::fs::read(p) {
                p2.push((n.to_string(), b));
            }
        }
        if p2.len() == 2 {
            let r3 = http.upload_many(
                "/admin/versions/upload",
                &admin,
                &[("product_id", batch_pid.as_str())],
                &p2,
            );
            ok(
                r3.status == 400,
                &format!("同一批出现相同表盘 id 被拒（实际 {}）", r3.status),
            );
        }
        // [2c] 按表盘 id 只替换其中一个包：另一个包原样保留（老版本是整行替换，会丢包）
        if let Some((_, bytes)) = payload.iter().find(|(n, _)| n == "b2-惊鸿10P.bin") {
            let mut changed = bytes.clone();
            if changed.len() >= 8 {
                // 头部 0x04 = 版本号（u32 LE）；末位 +1 造一个「更高版本」的新内容
                let v = u32::from_le_bytes([changed[4], changed[5], changed[6], changed[7]]);
                changed[4..8].copy_from_slice(&(v + 1).to_le_bytes());
                let r = http.upload(
                    "/admin/versions/upload",
                    &admin,
                    &[("product_id", batch_pid.as_str())],
                    "b2-惊鸿10P.bin",
                    &changed,
                );
                let j = r.json();
                let fs = j["files"].as_array().cloned().unwrap_or_default();
                let acts: Vec<&str> = fs.iter().filter_map(|f| f["action"].as_str()).collect();
                ok(
                    r.status == 200
                        && fs.len() == 2
                        && acts == vec!["replaced", "kept"]
                        && fs[1]["fileName"] == serde_json::json!("b10-AWOS9P.bin"),
                    &format!(
                        "只传第二个包 → 只替换它、另一个原样保留（状态 {}，{} 个包，动作 {:?}）",
                        r.status,
                        fs.len(),
                        acts
                    ),
                );
                let d = http.get(&format!("/admin/products/{batch_pid}"), Some(&admin));
                let pk = d.json()["variants"][0]["packages"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let v0 = pk.first().and_then(|x| x["version"].as_str()).unwrap_or("?");
                let v1 = pk.get(1).and_then(|x| x["version"].as_str()).unwrap_or("?");
                ok(
                    pk.len() == 2 && v0 != v1,
                    &format!("详情页两个包各自显示自己的版本（{v0} / {v1}）"),
                );
            }
        }
        // 清理：删掉拆包测试资源（连带设备行与包体）
        let _ = http.request(
            "DELETE",
            &format!("/admin/products/{batch_pid}"),
            &[("Authorization", format!("Bearer {admin}"))],
            None,
        );
    }

    // 3. 版本覆盖规则（旧包被拒 / 只留最新）
    println!("
[3] 版本规则与付费链接");
    let v1 = bin_dir.join(families[2].files[0]);
    if let Ok(bytes) = std::fs::read(&v1) {
        let r = http.upload(
            "/admin/versions/upload",
            &admin,
            &[("product_id", product_id.as_str()), ("variant_id", variant_ids[2].as_str())],
            "AWOS-照片表盘-10.bin",
            &bytes,
        );
        ok(r.status == 409 && r.json()["error"] == "version_not_newer",
           &format!("重传旧包（1.0.2）被拒（409 version_not_newer，实际 {}）", r.status));
    }
    let r = http.post_json(
        "/admin/variants/afdian",
        Some(&admin),
        serde_json::json!({"variant_id": variant_ids[2], "url": "https://afdian.com/item/aacfe164985411f1aef45254001e7c00"}),
    );
    ok(r.status == 200 && r.json()["planId"] == serde_json::json!("aacfe164985411f1aef45254001e7c00"),
       "购买链接解析出商品编号");

    // 4. 开通 + 绑定（App 的真实入口：管理员手动开通 → App 联调绑定）
    println!("
[4] 开通与绑定");
    // 每次运行用唯一的测试账号，避免上一轮的授权残留影响断言
    let mi_uid = format!("test-uid-{stamp}");
    let mi_uid = mi_uid.as_str();
    let r = http.post_json(
        "/admin/grant",
        Some(&admin),
        serde_json::json!({"mi_user_id": mi_uid, "product_id": product_id}),
    );
    ok(r.status == 200 && r.json()["ok"] == serde_json::json!(true), "管理员手动开通该资源");

    let mut dev_priv_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut dev_priv_bytes);
    let dev = StaticSecret::from(dev_priv_bytes);
    let dev_pub = PublicKey::from(&dev).to_bytes();

    let r = http.post_json(
        "/v1/bind/dev",
        None,
        serde_json::json!({
            "mi_user_id": mi_uid,
            "device_fp": "fp-selftest-A",
            "device_pubkey": b64(&dev_pub),
            "device_name": "自测设备A"
        }),
    );
    let token = r.json()["license_token"].as_str().unwrap_or("").to_string();
    ok(r.status == 200 && !token.is_empty(), &format!("联调绑定拿到登录态（{}）", r.status));

    // 5. 我的表盘（App 列表页的数据源）
    println!("
[5] 我的表盘");
    let r = http.get("/v1/me", Some(&token));
    let me = r.json();
    let prods = me["products"].as_array().cloned().unwrap_or_default();
    ok(prods.len() == 1, &format!("列表里有 1 个已购资源（实际 {}）", prods.len()));
    let variants = prods.first().map(|p| p["variants"].as_array().cloned().unwrap_or_default()).unwrap_or_default();
    let band10 = variants.iter().find(|v| v["variant_id"] == serde_json::json!(variant_ids[2].as_str()));
    ok(
        variants.len() == 4 && band10.is_some(),
        &format!("列表只列出「有包体」的 4 个设备行（实际 {}）", variants.len()),
    );
    ok(
        band10.map(|v| v["latest_version"] == serde_json::json!("1.0.4")).unwrap_or(false),
        "每个设备行带最新版本号（手环10/11 → 1.0.4）",
    );
    let r = http.get("/v1/me", None);
    ok(r.status == 401, "无登录态拿不到列表（401）");

    // 6. 版本清单
    println!("
[6] 版本清单");
    let r = http.get(
        &format!("/v1/manifest?product_id={product_id}&variant_id={}", variant_ids[2]),
        Some(&token),
    );
    let m = r.json();
    ok(
        m["latest_version"] == serde_json::json!("1.0.4"),
        &format!("最新版 = {}（期望 1.0.4）", m["latest_version"]),
    );

    // 7. 下载（首次自动激活）→ 设备侧解包 → 解密 → 比对原文件
    println!("
[7] 下载与解密");
    let latest_files = m["versions"]
        .as_array()
        .and_then(|a| a.iter().find(|v| v["is_latest"] == serde_json::json!(true)))
        .and_then(|v| v["files"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut downloaded = false;
    for f in latest_files {
        let file_id = f["file_id"].as_str().unwrap_or("").to_string();
        let sha_plain = f["sha256_plain"].as_str().unwrap_or("").to_string();
        // 注意：不传 device_fp/device_pubkey 之外的任何激活信息——服务器应自动激活这台设备
        let r = http.post_json(
            "/v1/download",
            Some(&token),
            serde_json::json!({
                "file_id": file_id, "device_fp": "fp-selftest-A",
                "device_pubkey": b64(&dev_pub), "device_name": "自测设备A"
            }),
        );
        if r.status != 200 {
            ok(false, &format!("下载失败 {}：{}", r.status, r.text()));
            continue;
        }
        let d = r.json();
        let url = d["url"].as_str().unwrap();
        let path = url.split_once(&base).map(|(_, p)| p).unwrap_or(url);
        let cipher_resp = http.get(path, None);
        let plain = device_decrypt(&dev, &d["key_wrap"], d["file_nonce"].as_str().unwrap(), &cipher_resp.body);
        let got = sha256_hex(&plain);
        let null = std::fs::read(bin_dir.join("3.0/AWOS-1011-v3.bin")).unwrap();
        ok(
            got == sha_plain && plain.len() == null.len(),
            &format!("首次下载自动激活 + 解密校验通过（明文 {} 字节）", plain.len()),
        );
        downloaded = true;

        // 负例：篡改签名 / 过期链接
        let bad = match path.split_once("&sig=") {
            Some((head, _)) => format!("{head}&sig=deadbeef"),
            None => path.to_string(),
        };
        let r2 = http.get(&bad, None);
        ok(r2.status == 403, &format!("篡改签名的直链被拒（403，实际 {}）", r2.status));
        let expired = match path.split_once("exp=") {
            Some((head, rest)) => {
                let tail = rest.split_once("&").map(|(_, t)| t).unwrap_or("");
                format!("{head}exp=1000000000&{tail}")
            }
            None => path.to_string(),
        };
        let r3 = http.get(&expired, None);
        ok(r3.status == 410, &format!("过期直链被拒（410，实际 {}）", r3.status));
        break;
    }
    if !downloaded {
        ok(false, "没有可下载的文件");
    }

    // 7.5 负例：同一账号第 3 台设备下载应被拒（前两台已自动激活）
    println!("
[7.5] 设备上限");
    let file_id = {
        let r = http.get(
            &format!("/v1/manifest?product_id={product_id}&variant_id={}", variant_ids[2]),
            Some(&token),
        );
        r.json()["versions"]
            .as_array()
            .and_then(|a| a.iter().find(|v| v["is_latest"] == serde_json::json!(true)))
            .and_then(|v| v["files"].as_array())
            .and_then(|f| f.first())
            .and_then(|f| f["file_id"].as_str())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    // 第二台：应当成功（自动激活第 2 台）
    let mut k2 = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut k2);
    let pk2 = b64(&PublicKey::from(&StaticSecret::from(k2)).to_bytes());
    let r = http.post_json(
        "/v1/download",
        Some(&token),
        serde_json::json!({"file_id": file_id, "device_fp": "fp-selftest-B", "device_pubkey": pk2}),
    );
    ok(r.status == 200, &format!("第二台设备可下载（自动激活，实际 {}）", r.status));
    // 第三台：应被拒
    let mut k3 = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut k3);
    let pk3 = b64(&PublicKey::from(&StaticSecret::from(k3)).to_bytes());
    let r = http.post_json(
        "/v1/download",
        Some(&token),
        serde_json::json!({"file_id": file_id, "device_fp": "fp-selftest-C", "device_pubkey": pk3}),
    );
    ok(
        r.status == 403 && r.json()["error"] == serde_json::json!("device_limit"),
        &format!("第三台设备被拒（403 device_limit，实际 {} {}）", r.status, r.json()["error"]),
    );

    // 7.8 爱发电订单链路（模拟）：订单 → 自动开通；退款 → 自动收回
    println!("
[7.8] 爱发电订单同步（模拟）");
    let plan_id = "aacfe164985411f1aef45254001e7c00"; // 第 3 步给手环10/11 行配的测试链接
    let afdian_uid = format!("afdian-{stamp}");
    // 先绑定「爱发电账号 ↔ 这个小米账号」
    let r = http.post_json(
        "/admin/afdian/bind",
        Some(&admin),
        serde_json::json!({"afdian_user_id": afdian_uid, "mi_user_id": mi_uid}),
    );
    ok(r.status == 200, "爱发电账号与小米账号建立绑定");

    // 先收回 [4] 步的手动授权：这样"开通"只可能来自订单（隔离变量）
    let r = http.post_json(
        "/admin/revoke",
        Some(&admin),
        serde_json::json!({"product_id": product_id, "mi_user_id": mi_uid}),
    );
    ok(r.status == 200, "先收回手动授权（隔离变量）");

    // 模拟一笔「手环10/11 那条链接」的成功订单（固定单号，后面演示退款）
    let sim_order = format!("SIMORDER-{stamp}");
    let r = http.post_json(
        "/admin/afdian/simulate",
        Some(&admin),
        serde_json::json!({"afdian_user_id": afdian_uid, "plan_id": plan_id, "status": 2, "out_trade_no": sim_order}),
    );
    let sum = r.json()["summary"].clone();
    ok(
        sum["new_grants"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
        &format!("模拟成功订单 → 自动开通 {} 个资源", sum["new_grants"].as_array().map(|a| a.len()).unwrap_or(0)),
    );

    // 同一笔订单重复同步：不应重复开通（幂等）
    let r = http.post_json(
        "/admin/afdian/simulate",
        Some(&admin),
        serde_json::json!({"afdian_user_id": afdian_uid, "plan_id": plan_id, "status": 2}),
    );
    ok(
        r.json()["summary"]["unchanged"].as_i64().unwrap_or(0) >= 1,
        "重复订单不会重复开通（幂等）",
    );

    // 同一单号改成「退款」状态：应自动收回
    let r = http.post_json(
        "/admin/afdian/simulate",
        Some(&admin),
        serde_json::json!({"afdian_user_id": afdian_uid, "plan_id": plan_id, "status": 3, "out_trade_no": sim_order}),
    );
    ok(
        r.json()["summary"]["revoked"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "退款订单 → 自动收回授权",
    );

    // 订单列表能查到刚才那几笔
    let r = http.get("/admin/afdian/orders", Some(&admin));
    ok(
        r.json()["orders"].as_array().map(|a| a.len()).unwrap_or(0) >= 2,
        "订单列表可查",
    );

    // 7.9 爱发电 OAuth 绑定链路（联调模拟）：发起 → 回调授权 → App 轮询取登录态 → 历史订单自动补发
    println!("\n[7.9] 爱发电 OAuth 绑定（模拟授权）");
    let oauth_uid = format!("afdian-oauth-{stamp}");
    let oauth_mi = format!("mi-oauth-{stamp}");
    let oauth_order = format!("OAuthOrder-{stamp}");

    // 先造一笔「买家的成功订单」——此时他还没绑定，只能先记下来（unbound）
    let r = http.post_json(
        "/admin/afdian/simulate",
        Some(&admin),
        serde_json::json!({"afdian_user_id": oauth_uid, "plan_id": plan_id, "status": 2,
                           "out_trade_no": oauth_order}),
    );
    ok(
        r.json()["summary"]["unbound"].as_i64().unwrap_or(0) >= 1,
        "买家未绑定时订单先攒着（unbound）",
    );

    // App 发起绑定：拿到授权页地址 + 会话号
    let r = http.post_json(
        "/v1/afdian/oauth/start",
        None,
        serde_json::json!({"mi_user_id": oauth_mi, "device_fp": "fp-oauth", "device_pubkey": pk3}),
    );
    let oauth_state = r.json()["state"].as_str().unwrap_or_default().to_string();
    let oauth_url = r.json()["url"].as_str().unwrap_or_default().to_string();
    ok(
        !oauth_state.is_empty() && oauth_url.contains("state="),
        &format!("发起绑定拿到授权地址（{}…）", oauth_url.chars().take(52).collect::<String>()),
    );

    // 轮询：还没授权 → pending
    let r = http.get(&format!("/v1/afdian/oauth/poll?state={oauth_state}"), None);
    ok(r.json()["status"] == serde_json::json!("pending"), "未授权时轮询为 pending");

    // 用户在浏览器里授权完 → 爱发电把我们带回回调（联调：code=dev:<爱发电UID>）
    let r = http.get(
        &format!("/v1/afdian/callback?state={oauth_state}&code=dev:{oauth_uid}"),
        None,
    );
    ok(
        r.status == 200 && r.text().contains("授权成功"),
        &format!("回调完成授权（HTTP {}）", r.status),
    );

    // App 再轮询：拿到登录态
    let r = http.get(&format!("/v1/afdian/oauth/poll?state={oauth_state}"), None);
    let oauth_token = r.json()["license_token"].as_str().unwrap_or_default().to_string();
    ok(
        r.json()["status"] == serde_json::json!("ok") && !oauth_token.is_empty(),
        "轮询拿到登录态",
    );
    let r = http.get("/v1/me", Some(&oauth_token));
    ok(
        r.json()["products"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "绑定后的登录态能查到已购资源",
    );
    // 关键：绑定之前的订单要自动补发（不然老买家绑定后一片空白）
    let r = http.get("/admin/afdian/orders", Some(&admin));
    let resent_ok = r.json()["orders"]
        .as_array()
        .map(|a| {
            a.iter().any(|o| {
                o["outTradeNo"] == serde_json::json!(oauth_order)
                    && o["grantedAt"].as_i64().unwrap_or(0) > 0
            })
        })
        .unwrap_or(false);
    ok(resent_ok, "绑定后历史订单自动补发（已开通）");

    // 7.10 按规格路由：一个商品多规格时，只能开通买家买的那个规格对应的设备行
    println!("
[7.10] 规格名路由（买哪个规格开哪行）");
    let r = http.post_json(
        "/admin/afdian/simulate",
        Some(&admin),
        serde_json::json!({"afdian_user_id": format!("sku-redmi-{stamp}"),
                           "mi_user_id": format!("mi-sku-redmi-{stamp}"),
                           "plan_id": plan_id, "sku_name": "红米手表5/6", "status": 2,
                           "out_trade_no": format!("SkuRedmi-{stamp}")}),
    );
    ok(
        r.json()["summary"]["new_grants"].as_array().map(|a| a.is_empty()).unwrap_or(false),
        "买「红米手表5/6」规格 → 不开通手环那行",
    );
    let r = http.post_json(
        "/admin/afdian/simulate",
        Some(&admin),
        serde_json::json!({"afdian_user_id": format!("sku-band-{stamp}"),
                           "mi_user_id": format!("mi-sku-band-{stamp}"),
                           "plan_id": plan_id, "sku_name": "小米手环10/小米手环11", "status": 2,
                           "out_trade_no": format!("SkuBand-{stamp}")}),
    );
    ok(
        r.json()["summary"]["new_grants"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "买「小米手环10/小米手环11」规格 → 正常开通",
    );

    // 7.11 一行多链接：同一个包体可以对应多个爱发电商品（手环9 Pro / 10 Pro 分开卖的情况）
    println!("
[7.11] 一行多链接");
    let extra_plan = "secondplan0000000000000000000000";
    let r = http.post_json(
        "/admin/variants/afdian",
        Some(&admin),
        serde_json::json!({"variant_id": variant_ids[2], "url": format!("https://afdian.com/item/{extra_plan}")}),
    );
    ok(
        r.json()["links"].as_array().map(|a| a.len()).unwrap_or(0) >= 2,
        "同一设备行挂上第二条购买链接",
    );
    let r = http.post_json(
        "/admin/afdian/simulate",
        Some(&admin),
        serde_json::json!({"afdian_user_id": format!("multi-{stamp}"),
                           "mi_user_id": format!("mi-multi-{stamp}"),
                           "plan_id": extra_plan, "sku_name": "小米手环10/小米手环11", "status": 2,
                           "out_trade_no": format!("Multi-{stamp}")}),
    );
    ok(
        r.json()["summary"]["new_grants"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "第二条链接的订单同样开通",
    );
    let r = http.post_json(
        "/admin/variants/afdian/remove",
        Some(&admin),
        serde_json::json!({"variant_id": variant_ids[2], "plan_id": extra_plan}),
    );
    ok(
        r.json()["links"].as_array().map(|a| a.len()).unwrap_or(9) == 1,
        "删掉第二条链接后只剩一条",
    );
    let r = http.post_json(
        "/admin/afdian/simulate",
        Some(&admin),
        serde_json::json!({"afdian_user_id": format!("multi2-{stamp}"),
                           "mi_user_id": format!("mi-multi2-{stamp}"),
                           "plan_id": extra_plan, "sku_name": "小米手环10/小米手环11", "status": 2,
                           "out_trade_no": format!("Multi2-{stamp}")}),
    );
    ok(
        r.json()["summary"]["new_grants"].as_array().map(|a| a.is_empty()).unwrap_or(false),
        "删掉的链接不再开通任何东西",
    );

    // 8. 统计
    println!("\n[8] 统计");
    let r = http.get("/admin/stats", Some(&admin));
    println!("  {}", serde_json::to_string_pretty(&r.json()).unwrap().replace('\n', "\n  "));

    let failed = FAILED.load(std::sync::atomic::Ordering::SeqCst);
    println!(
        "\n== {} ==",
        if failed { "❌ 有失败项，见上文" } else { "✅ 全链路自测通过" }
    );
    if failed {
        std::process::exit(1);
    }
}
