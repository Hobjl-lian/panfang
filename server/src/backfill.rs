//! 启动时回填 `files.bin_version`（每个包自己的 .bin 版本号）。
//!
//! 背景：一个设备行可能有多个包（同一设备拆成两个独立表盘），版本各自独立；但
//! `files` 表以前只存整行一个版本（取排序第一个包的头部）。按包替换需要知道
//! 「这个包自己上次的版本」，所以加列后在启动时把存量数据解密识别一次。
//!
//! 明文只在内存里、不落盘；单条失败只跳过那一条（上传时退回整行版本比较），
//! 不影响服务启动。判据见 `bin_inspect::version_label`（头部 0x04）。

use crate::state::AppState;
use crate::{bin_inspect, crypto};
use rusqlite::params;

pub fn backfill_bin_versions(state: &AppState) {
    // 先把待处理行一次性取出再释放锁：解密是纯内存/磁盘操作，不占着数据库互斥锁。
    let rows: Vec<(String, String, String, Vec<u8>, Vec<u8>, Vec<u8>, String)> = {
        let conn = state.conn();
        let mut stmt = match conn.prepare(
            "SELECT id, file_name, storage_path, file_nonce, cek_wrapped, cek_wrap_nonce, sha256_plain
             FROM files WHERE bin_version IS NULL OR bin_version = '' ORDER BY id",
        ) {
            Ok(s) => s,
            Err(e) => {
                println!("[backfill] 查询待回填包体失败：{e}");
                return;
            }
        };
        let out: Vec<_> = match stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        }) {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                println!("[backfill] 读取待回填包体失败：{e}");
                return;
            }
        };
        out
    };
    if rows.is_empty() {
        return;
    }

    println!("[backfill] 包体版本回填：{} 个包需要解密识别", rows.len());
    let (mut ok, mut failed) = (0usize, 0usize);
    for (id, file_name, storage_path, nonce, cek_wrapped, cek_wrap_nonce, sha_plain) in &rows {
        let found = (|| -> anyhow::Result<String> {
            let cipher = std::fs::read(storage_path)?;
            let cek = crypto::unwrap_cek(&state.kek, cek_wrapped, cek_wrap_nonce)?;
            let plain = crypto::decrypt_file(&cek, nonce, &cipher)?;
            // 明文指纹必须与入库时一致，否则说明密文/元数据错位，宁可不动
            let sha_now = crypto::sha256_hex(&plain);
            if !sha_plain.is_empty() && &sha_now != sha_plain {
                anyhow::bail!("指纹与库不一致（密文/元数据错位）");
            }
            Ok(bin_inspect::inspect(&plain)?.version_label)
        })();
        match found {
            Ok(version) => {
                let conn = state.conn();
                match conn.execute(
                    "UPDATE files SET bin_version = ?2 WHERE id = ?1",
                    params![id, version],
                ) {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        println!("[backfill] ✗ 写库失败 {file_name}：{e}");
                        failed += 1;
                    }
                }
            }
            Err(e) => {
                println!("[backfill] ✗ {file_name}：{e:#}");
                failed += 1;
            }
        }
    }
    println!("[backfill] 包体版本回填完成：成功 {ok}，失败 {failed}");
}
