//! 订单路由排障工具：喂一份 **wf.db 副本**，逐步看清某买家的订单怎么落位、为什么没开通。
//! 用法：diag_orders <wf.db 路径> [买家 afdian_user_id] [小米账号（可选，看它的授权列表）]
//!
//! ⚠️ **它会写库**（跑真实 sync_after_bind，会把能开的开出来）——**只对副本跑**，别指向线上库。
//! 典型用法（本地排查「这单为什么没到账」）：
//!   1) python deploy/deploy-from-local.py --cmd 'python3 -c "import sqlite3; s=sqlite3.connect(\"/var/lib/wf-auth/wf.db\"); d=sqlite3.connect(\"/tmp/copy.db\"); s.backup(d)"'
//!   2) 把 /tmp/copy.db 拉回本地 → cargo run --bin diag_orders -- <副本> <买家 uid>

use rusqlite::OptionalExtension;
use std::sync::{Arc, Mutex};
use wf_auth_server::afdian;
use wf_auth_server::db;
use wf_auth_server::state::AppState;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: diag_orders <wf.db 路径> [买家 afdian_user_id]");
        std::process::exit(2);
    }
    let db_path = std::path::PathBuf::from(&args[1]);
    let buyer = args.get(2).cloned().unwrap_or_else(|| "a5d5fa40f35111ee8efd5254001e7c00".into());
    let dir = db_path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
    let conn = db::open(&db_path)?;
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        data_dir: dir.clone(),
        cipher_dir: dir,
        kek: [0u8; 32],
        url_secret: vec![0u8; 32],
        admin_token: "t".into(),
        public_base: "http://x".into(),
        device_limit: 2,
        token_ttl: 100,
    };

    // 1) 先把 sync_after_bind 的选单条件原样跑一遍，看看会捞出哪些单
    {
        let conn = state.conn();
        let mut stmt = conn.prepare(
            "SELECT out_trade_no, plan_id, sku, sku_name, status, (granted_at is not null)
             FROM afdian_orders WHERE afdian_user_id = ?1 AND status = 2 AND granted_at IS NULL",
        )?;
        let rows: Vec<(String, String, Option<String>, Option<String>, i64, i64)> = stmt
            .query_map([&buyer], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        println!("sync_after_bind 会捞到 {} 单：", rows.len());
        for (no, plan, sku, name, st, granted) in &rows {
            println!("  {no} plan={} sku={:?} name={:?} status={st} granted={granted}", &plan[..12.min(plan.len())], sku, name);
        }
        // 该买家的绑定
        let b: Option<(String, String)> = conn
            .query_row(
                "SELECT b.subject_id, s.mi_user_id FROM afdian_bindings b JOIN subjects s ON s.id=b.subject_id WHERE b.afdian_user_id=?1",
                [&buyer],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        println!("绑定: {b:?}");
    }

    // 2) 跑真实的 sync_after_bind（会把能开的开出来）
    let n = afdian::sync_after_bind(&state, &buyer).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    println!("sync_after_bind 处理了 {n} 单");

    // 3) 结果：该买家的授权与订单 granted 状态
    {
        let conn = state.conn();
        if let Some(mi_uid) = args.get(3) {
            let sid: Option<String> = conn
                .query_row("SELECT id FROM subjects WHERE mi_user_id=?1", [mi_uid], |r| r.get(0))
                .optional()?;
            if let Some(sid) = sid {
                let mut stmt = conn.prepare("SELECT product_id, source, status FROM licenses WHERE subject_id=?1")?;
                let rows: Vec<(String, String, String)> = stmt
                    .query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .filter_map(|r| r.ok())
                    .collect();
                println!("授权: {rows:?}");
            }
        }
        let mut stmt = conn.prepare(
            "SELECT out_trade_no, (granted_at is not null) FROM afdian_orders WHERE afdian_user_id=?1",
        )?;
        let rows: Vec<(String, i64)> = stmt
            .query_map([&buyer], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        for (no, granted) in rows {
            println!("  订单 {no} {}", if granted == 1 { "granted" } else { "-" });
        }
    }
    Ok(())
}
