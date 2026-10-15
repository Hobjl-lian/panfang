//! 付费链路自测 CLI（不需要设备、不需要界面）：
//! 绑定 → 我的表盘 → 取最新包体 → 下载解密 → 写出明文供比对。
//!
//! 用法：
//!   paidtest <数据目录> <服务端地址> <小米UID> <输出文件>
//! 例：
//!   paidtest ./paid-dev http://127.0.0.1:8787 test-uid out.bin
//!
//! 说明：解密出的明文只在这一步用于**校验**（与原始 .bin 逐字节比对），
//! App 正式流程里不会落盘（`paid_install` 直接在内存里转给安装通道）。

use wf_installer_lib::paid::{self, Ctx};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("用法: paidtest <数据目录> <服务端地址> <小米UID> <输出文件>");
        std::process::exit(2);
    }
    let dir = std::path::PathBuf::from(&args[1]);
    let base = args[2].clone();
    let uid = args[3].clone();
    let out = std::path::PathBuf::from(&args[4]);

    let mut ctx = Ctx::load_or_create(dir);
    ctx.base = base.clone();
    println!("设备指纹 {}", &ctx.fp[..16]);
    println!("设备公钥 {}", &ctx.pub_b64[..16]);

    // 1) 绑定方式
    let mode = paid::bind_mode(&base).await?;
    println!("[1] 绑定模式 {:?}", mode["mode"].as_str().unwrap_or("?"));

    // 2) 绑定（联调模式）
    if ctx.token().is_none() {
        let s = paid::bind_dev(&base, &uid, &ctx.pub_b64, "paidtest").await?;
        println!("[2] 绑定成功，账号尾号 {}", s.display);
        ctx.save_session(s)?;
    } else {
        println!("[2] 复用已有登录态");
    }
    let token = ctx.token().unwrap().to_string();

    // 3) 我的表盘
    let me = paid::me(&base, &token).await?;
    let products = me["products"].as_array().cloned().unwrap_or_default();
    println!("[3] 已购资源 {} 个", products.len());
    for p in &products {
        let variants = p["variants"].as_array().cloned().unwrap_or_default();
        println!(
            "    · {}（{} 个可装设备行）",
            p["name"].as_str().unwrap_or("?"),
            variants.len()
        );
    }
    let product = products
        .first()
        .ok_or_else(|| anyhow::anyhow!("该账号没有已购资源"))?;
    let pid = product["product_id"].as_str().unwrap_or_default().to_string();
    let variant = product["variants"]
        .as_array()
        .and_then(|v| v.first())
        .ok_or_else(|| anyhow::anyhow!("该资源没有可安装的设备行"))?;
    let vid = variant["variant_id"].as_str().unwrap_or_default().to_string();
    println!(
        "[4] 选设备行 {}（屏幕 {:?}）",
        variant["name"].as_str().unwrap_or("?"),
        variant["screen"]
    );

    // 5) 清单 → 最新包体
    let m = paid::manifest(&base, &token, &pid, &vid).await?;
    let latest = m["versions"]
        .as_array()
        .and_then(|a| a.iter().find(|v| v["is_latest"] == serde_json::json!(true)))
        .ok_or_else(|| anyhow::anyhow!("没有最新版本"))?;
    let file = latest["files"]
        .as_array()
        .and_then(|f| f.first())
        .ok_or_else(|| anyhow::anyhow!("最新版本没有文件"))?;
    let file_id = file["file_id"].as_str().unwrap_or_default();
    println!(
        "[5] 最新版本 {} · 文件 {}（{} 字节）",
        latest["version"].as_str().unwrap_or("?"),
        file["file_name"].as_str().unwrap_or("?"),
        file["size"].as_u64().unwrap_or(0)
    );

    // 6) 下载 + 解密（内存）+ 校验
    let plain = paid::fetch_decrypt(
        &base,
        &token,
        &ctx.secret,
        &ctx.fp,
        &ctx.pub_b64,
        "paidtest",
        file_id,
        |sent, total| {
            if sent >= total || sent % (512 * 1024) < 8192 {
                print!("\r    下载 {sent}/{total} 字节");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        },
    )
    .await?;
    println!();
    let sha = paid::sha256_hex(&plain);
    println!("[6] 解密成功 {} 字节 · sha256 {}", plain.len(), &sha[..16]);
    println!("    服务端记录的明文哈希 {}", &file["sha256_plain"].as_str().unwrap_or("")[..16]);

    // 7) 写出供人工比对（仅测试用）
    std::fs::write(&out, &plain)?;
    println!("[7] 已写出 {}", out.display());
    Ok(())
}
