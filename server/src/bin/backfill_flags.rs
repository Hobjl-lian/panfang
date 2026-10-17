//! 回填每个包体的「表盘 id」与「是不是照片表盘」。
//!
//! 为什么需要它：这两个字段是 2026-10-15 才加到 `files` 表上的，之前上架的包都没存。
//! 服务端只存密文，所以要**解密后重新识别**（用数据目录里的 kek.bin；明文只在内存里，不落盘）。
//!
//! 判据是内容、不是名字：亮屏样式的 `backImageId` 高字节 `0x82`（见 `bin_inspect::photo_album`）。
//!
//! 用法：
//!   wf-backfill-flags             # 只补还没识别的（watchface_id 或 photo_album 为 NULL）
//!   wf-backfill-flags --all       # 全部重新识别一遍
//!   wf-backfill-flags --dry       # 只打印结果，不写库
//!
//! 数据目录取 `WF_DATA_DIR`（线上 = /var/lib/wf-auth），与主服务同一套规则。
//! ⚠️ 会写库（UPDATE files）；线上跑之前确认当天备份已生成（systemd timer 每天 04:30）。

use std::path::PathBuf;
use wf_auth_server::{bin_inspect, crypto, db};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let all = args.iter().any(|a| a == "--all");
    let dry = args.iter().any(|a| a == "--dry");

    let data_dir = PathBuf::from(std::env::var("WF_DATA_DIR").unwrap_or_else(|_| "./data".into()));
    // 数据目录来自环境变量：先在文件系统层面**落实成真目录**再往下走 ——
    // 规范化会把 `..`、软链之类resolve掉，后面 join 出来的库/Kek 路径就不会跑到别处去。
    let data_dir = data_dir.canonicalize().map_err(|e| {
        anyhow::anyhow!("WF_DATA_DIR 指不到目录（{}）: {e}", data_dir.display())
    })?;
    if !data_dir.is_dir() {
        anyhow::bail!("WF_DATA_DIR 不是目录: {}", data_dir.display());
    }
    let db_path = data_dir.join("wf.db");
    if !db_path.exists() {
        anyhow::bail!("{} 不存在", db_path.display());
    }
    // **只读**主密钥：这个工具绝不允许现场生成（生成了也解不开任何密文，只会误导）。
    // 路径先用「必须就落在数据目录正下方」挡一道（`..`/软链越界一律拒绝），再交给 crypto 读。
    let kek_path = data_dir.join("kek.bin");
    if kek_path.parent() != Some(data_dir.as_path())
        || kek_path.components().any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("主密钥路径越界: {}", kek_path.display());
    }
    let kek = crypto::read_kek_at(&kek_path)?;
    let conn = db::open(&db_path)?;

    let filter = if all {
        String::new()
    } else {
        "WHERE f.watchface_id IS NULL OR f.photo_album IS NULL".to_string()
    };
    let sql = format!(
        "SELECT f.id, f.file_name, f.storage_path, f.file_nonce, f.cek_wrapped,
                f.cek_wrap_nonce, f.sha256_plain
         FROM files f {filter} ORDER BY f.id"
    );
    let rows: Vec<(String, String, String, Vec<u8>, Vec<u8>, Vec<u8>, String)> = {
        let mut stmt = conn.prepare(&sql)?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();
        mapped
    };

    println!(
        "[回填] 待处理 {} 个包（{}），数据目录 {}",
        rows.len(),
        if all { "--all 全部重识别" } else { "只补未识别的" },
        data_dir.display()
    );

    let (mut ok, mut photo, mut skipped, mut failed) = (0usize, 0usize, 0usize, 0usize);
    for (id, file_name, storage_path, nonce, cek_wrapped, cek_wrap_nonce, sha_plain) in &rows {
        let cipher = match std::fs::read(storage_path) {
            Ok(c) => c,
            Err(e) => {
                println!("  ✗ 密文读不到 {file_name}（{storage_path}）：{e}");
                failed += 1;
                continue;
            }
        };
        let plain = (|| -> anyhow::Result<Vec<u8>> {
            let cek = crypto::unwrap_cek(&kek, cek_wrapped, cek_wrap_nonce)?;
            crypto::decrypt_file(&cek, nonce, &cipher)
        })();
        let plain = match plain {
            Ok(p) => p,
            Err(e) => {
                println!("  ✗ 解密失败 {file_name}：{e}");
                failed += 1;
                continue;
            }
        };
        // 明文指纹必须与入库时一致，否则说明密文/元数据错位，宁可不动
        let sha_now = crypto::sha256_hex(&plain);
        if !sha_plain.is_empty() && &sha_now != sha_plain {
            println!("  ✗ 指纹不符，跳过 {file_name}（库 {sha_plain:.12}… vs 现 {sha_now:.12}…）");
            failed += 1;
            continue;
        }
        let info = match bin_inspect::inspect(&plain) {
            Ok(i) => i,
            Err(e) => {
                println!("  ✗ 解析失败 {file_name}：{e}");
                failed += 1;
                continue;
            }
        };
        let flag = info.photo_album as i64;
        if info.photo_album {
            photo += 1;
            println!(
                "  ★ 照片表盘  {:<24} id={:<14} {}",
                info.name, info.watchface_id, file_name
            );
        }
        if dry {
            ok += 1;
            continue;
        }
        match conn.execute(
            "UPDATE files SET watchface_id = ?2, photo_album = ?3 WHERE id = ?1",
            rusqlite::params![id, info.watchface_id, flag],
        ) {
            Ok(0) => {
                println!("  ✗ 库里没找到这一行，跳过 {file_name}");
                skipped += 1;
            }
            Ok(_) => ok += 1,
            Err(e) => {
                println!("  ✗ 写库失败 {file_name}：{e}");
                failed += 1;
            }
        }
    }

    println!(
        "[回填] 完成：处理 {ok}，其中照片表盘 {photo}；跳过 {skipped}；失败 {failed}{}",
        if dry { "（--dry，没有写库）" } else { "" }
    );
    Ok(())
}
