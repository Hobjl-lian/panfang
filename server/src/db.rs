//! SQLite 库表与通用小工具（语义见 `docs/接口契约-授权服务-v1.md` §3）。

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}{}", crate::crypto::b64url(&crate::crypto::rand_bytes(9)))
}

pub fn open(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // WAL 下 NORMAL 是标准做法：崩溃最多丢最后几笔事务，但不会损坏库；
    // 默认 FULL 每条写语句都 fsync —— 全量补拉 2.5 万单（十万级写）会把全局锁占上分钟级。
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    seed_device_groups(&conn)?;
    merge_rw_groups(&conn)?;
    Ok(conn)
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = match conn.prepare(&format!("PRAGMA table_info({table})")) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    rows
}

/// 增量迁移：缺列就补（老库升级不丢数据）。
fn migrate(conn: &Connection) -> Result<()> {
    let add = |table: &str, col: &str, ddl: &str| -> Result<()> {
        if !columns(conn, table).iter().any(|c| c == col) {
            conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {ddl}"), [])?;
            println!("[wf-auth] 迁移：{table} 增加列 {col}");
        }
        Ok(())
    };
    add("products", "sort_order", "sort_order INTEGER NOT NULL DEFAULT 0")?;
    add("variants", "device_group_id", "device_group_id TEXT")?;
    add("variants", "afdian_url", "afdian_url TEXT")?;
    add("variants", "afdian_plan_id", "afdian_plan_id TEXT")?;
    add("variants", "afdian_sku", "afdian_sku TEXT")?;
    add("afdian_orders", "sku_name", "sku_name TEXT")?;  // 规格名（JSON 数组），用于按规格路由到设备行
    // 这笔订单的权益归属哪个主体：第一次同步处理时钉下，之后**换绑到别的小米账号也不再重放**。
    // 没有它时：买家把爱发电账号换绑到其他账号，下一轮同步会把同一订单再开通一遍，
    // 且旧账号授权不收回 → 一份订单可扩散到任意多个账号（2026-10-16 修）。
    add("afdian_orders", "granted_subject_id", "granted_subject_id TEXT")?;
    // 一个版本可以有多个包（同一设备因内存限制拆成两个独立表盘，依次安装）；
    // sort_order = 安装顺序（按文件名自然排序得出），从 0 起
    add("files", "sort_order", "sort_order INTEGER NOT NULL DEFAULT 0")?;
    // 每个包自己的表盘 id（一个设备行可能有 2 个包）与「是不是照片表盘」的自动识别结果。
    // 老数据由 `cargo run --bin wf-backfill-flags` 解密后回填；NULL = 未识别。
    add("files", "watchface_id", "watchface_id TEXT")?;
    add("files", "photo_album", "photo_album INTEGER")?;
    // 每个包自己的 .bin 版本号（同一设备行可能有 2 个包，版本各自独立）。
    // 上传时按**包**比较新旧（只替换 ID 命中的那个包）；老数据由服务启动时解密回填
    // （backfill.rs）。NULL = 还没回填，比较时退回整行版本。
    add("files", "bin_version", "bin_version TEXT")?;
    // 某些主体（例如 App 开发者本人）需要永久全资源：在售与将来新增的资源都自动可下载，
    // 不用每个产品手动 grant 一次。0/1 标记，生效与否在鉴权时判断（licenses 表保持不变）。
    add("subjects", "all_access", "all_access INTEGER NOT NULL DEFAULT 0")?;
    // 绑定会话的发起 IP 与回调 IP：正常绑定两步都来自同一台手机（同一出口 IP）；
    // 不一致说明「链接是别人发来的」（OAuth 登录 CSRF / 钓鱼），只记录告警、不阻断，
    // 出事时能据此定位是哪一次绑定被借用（2026-10-17 上线前审查加）。
    add("afdian_oauth", "start_ip", "start_ip TEXT")?;
    add("afdian_oauth", "callback_ip", "callback_ip TEXT")?;

    // 诊断信息保留 90 天 —— 《隐私政策》里写明了，就得真删，否则政策是假的
    let cutoff = now() - 90 * 86_400;
    match conn.execute("DELETE FROM client_events WHERE ts < ?1", [cutoff]) {
        Ok(0) => {}
        Ok(n) => println!("[wf-auth] 清理：删除 {n} 条超过 90 天的客户端诊断记录"),
        Err(_) => {} // 老库可能还没有这张表（首次建表在 SCHEMA 里，正常不会走到这）
    }

    // 一行多链接：把老的单条链接（variants.afdian_*）搬进 variant_links（幂等：搬完就把老列清空）
    {
        let rows: Vec<(String, Option<String>, String, Option<String>)> = conn
            .prepare(
                "SELECT id, afdian_url, afdian_plan_id, afdian_sku FROM variants
                 WHERE afdian_plan_id IS NOT NULL AND afdian_plan_id != ''",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .filter_map(|r| r.ok())
            .collect();
        for (vid, url, plan, sku) in rows {
            conn.execute(
                "INSERT OR IGNORE INTO variant_links(variant_id, url, plan_id, sku, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![vid, url, plan, sku, now()],
            )?;
            conn.execute(
                "UPDATE variants SET afdian_url = NULL, afdian_plan_id = NULL, afdian_sku = NULL WHERE id = ?1",
                rusqlite::params![vid],
            )?;
            println!("[wf-auth] 迁移：设备行 {vid} 的购买链接移入 variant_links（支持一行多链接）");
        }
    }
    Ok(())
}

/// 把「红米手表6」设备组并进「红米手表5 / 6」（同屏同代号 XMHD06）。
///
/// 处理已被资源引用的情形：
///   · 该资源没有 rw5 行 → 把 rw6 行**改名**为 rw5 行（包体原样保留）
///   · 该资源已有 rw5 行但没包体、rw6 行有 → 把包体**挪到** rw5 行，删掉 rw6 行
///   · 两边都有包体 → 保留 rw5 行的，删掉 rw6 行（并打印被丢弃的文件名）
fn merge_rw_groups(conn: &Connection) -> Result<()> {
    let has_rw6: bool = conn
        .query_row("SELECT 1 FROM device_groups WHERE id = 'rw6'", [], |_| Ok(()))
        .optional()
        .unwrap_or(None)
        .is_some();
    if !has_rw6 {
        return Ok(());
    }
    const MERGED_NAME: &str = "红米手表5 / 6";
    let merged_models = serde_json::to_string(&[
        "Redmi Watch 5",
        "Redmi Watch 5 eSIM",
        "Redmi Watch 6",
    ])
    .unwrap_or_else(|_| "[]".into());

    let rows: Vec<(String, String)> = {
        let mut stmt =
            conn.prepare("SELECT id, product_id FROM variants WHERE device_group_id = 'rw6'")?;
        let v: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        v
    };
    if rows.is_empty() {
        conn.execute(
            "UPDATE device_groups SET name = ?1, models = ?2 WHERE id = 'rw5'",
            rusqlite::params![MERGED_NAME, merged_models],
        )?;
        conn.execute("DELETE FROM device_groups WHERE id = 'rw6'", [])?;
        println!("[wf-auth] 设备组「红米手表6」已合并进「红米手表5 / 6」（无资源引用）");
        return Ok(());
    }

    for (vid, pid) in rows {
        let target = format!("{pid}@rw5");
        let target_exists: bool = conn
            .query_row("SELECT 1 FROM variants WHERE id = ?1", [&target], |_| Ok(()))
            .optional()?
            .is_some();
        let count_of = |v: &str| -> i64 {
            conn.query_row("SELECT COUNT(*) FROM versions WHERE variant_id = ?1", [v], |r| r.get(0))
                .unwrap_or(0)
        };
        let v6_pkgs = count_of(&vid);
        if !target_exists {
            conn.execute(
                "UPDATE variants SET id = ?2, name = ?3, device_models = ?4,
                        screen_w = 432, screen_h = 514, device_group_id = 'rw5' WHERE id = ?1",
                rusqlite::params![vid, target, MERGED_NAME, merged_models],
            )?;
            conn.execute(
                "UPDATE versions SET variant_id = ?2 WHERE variant_id = ?1",
                rusqlite::params![vid, target],
            )?;
            println!("[wf-auth] 资源 {pid}：红米手表6 行已并入 红米手表5 / 6（保留包体）");
        } else if count_of(&target) == 0 && v6_pkgs > 0 {
            conn.execute(
                "UPDATE versions SET variant_id = ?2 WHERE variant_id = ?1",
                rusqlite::params![vid, target],
            )?;
            conn.execute("DELETE FROM variants WHERE id = ?1", [&vid])?;
            println!("[wf-auth] 资源 {pid}：红米手表6 行的包体已挪到 红米手表5 / 6 行");
        } else {
            // 目标行已有包体（或两边都空）：清掉 rw6 行
            let paths: Vec<String> = {
                let mut stmt = conn.prepare(
                    "SELECT f.storage_path FROM files f JOIN versions vr ON vr.id = f.version_id WHERE vr.variant_id = ?1",
                )?;
                let v: Vec<String> = stmt
                    .query_map([&vid], |r| r.get::<_, String>(0))?
                    .filter_map(|r| r.ok())
                    .collect();
                v
            };
            conn.execute(
                "DELETE FROM files WHERE version_id IN (SELECT id FROM versions WHERE variant_id = ?1)",
                [&vid],
            )?;
            conn.execute("DELETE FROM versions WHERE variant_id = ?1", [&vid])?;
            conn.execute("DELETE FROM variants WHERE id = ?1", [&vid])?;
            for p in paths {
                let _ = std::fs::remove_file(p);
            }
            println!("[wf-auth] 资源 {pid}：红米手表6 行已删除（该行包体被红米手表5 / 6 行取代）");
        }
    }
    conn.execute(
        "UPDATE device_groups SET name = ?1, models = ?2 WHERE id = 'rw5'",
        rusqlite::params![MERGED_NAME, merged_models],
    )?;
    conn.execute("DELETE FROM device_groups WHERE id = 'rw6'", [])?;
    println!("[wf-auth] 设备组「红米手表6」已合并进「红米手表5 / 6」");
    Ok(())
}

/// 首次运行灌入预设设备组（权威表：设计流水线 devices.json）。
fn seed_device_groups(conn: &Connection) -> Result<()> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM device_groups", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    const PRESETS: &[(&str, &str, &[&str], i64, i64)] = &[
        ("band9", "小米手环9", &["小米手环9"], 192, 490),
        ("band10_11", "小米手环10 / 11", &["小米手环10", "小米手环11"], 212, 520),
        ("band9pro_10pro", "小米手环9 Pro / 10 Pro", &["小米手环9 Pro", "小米手环10 Pro"], 336, 480),
        // 红米手表5/6 同屏同代号（XMHD06，432×514）→ 合成一组、共用一份包体
        ("rw5", "红米手表5 / 6", &["Redmi Watch 5", "Redmi Watch 5 eSIM", "Redmi Watch 6"], 432, 514),
        ("rw4", "红米手表4", &["Redmi Watch 4"], 390, 450),
        ("watch_s34", "小米手表 S3 / S4", &["Xiaomi Watch S3", "Xiaomi Watch S4", "Xiaomi Watch S4 Sport", "Xiaomi Watch S4 eSIM", "Xiaomi Watch S4 41mm"], 464, 464),
        ("watch_s5", "小米手表 S5 / Watch 5", &["Xiaomi Watch S5", "Xiaomi Watch 5"], 480, 480),
    ];
    for (i, (id, name, models, w, h)) in PRESETS.iter().enumerate() {
        conn.execute(
            "INSERT INTO device_groups(id, name, models, screen_w, screen_h, sort, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                name,
                serde_json::to_string(models).unwrap_or_else(|_| "[]".into()),
                w,
                h,
                i as i64,
                now()
            ],
        )?;
    }
    println!("[wf-auth] 已灌入 {} 个预设设备组", PRESETS.len());
    Ok(())
}

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS device_groups(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,                    -- 如「红米手表5」
  models TEXT NOT NULL,                  -- JSON 数组：精确型号（含 eSIM 等）
  screen_w INTEGER, screen_h INTEGER,
  sort INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS products(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  cover_url TEXT,
  sort_order INTEGER NOT NULL DEFAULT 0,
  shop_wf_id TEXT,                         -- watchface-shop 的 wf-XX（可选关联）
  status TEXT NOT NULL DEFAULT 'on',       -- on / off
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS variants(
  id TEXT PRIMARY KEY,
  product_id TEXT NOT NULL,
  name TEXT NOT NULL,                      -- 如「手环10/11」
  device_models TEXT NOT NULL,             -- JSON 数组：["小米手环10","小米手环11"]
  screen_w INTEGER, screen_h INTEGER,
  watchface_id TEXT,                       -- .bin 头部 id（同产品同族稳定）
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_variants_product ON variants(product_id);

CREATE TABLE IF NOT EXISTS versions(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  variant_id TEXT NOT NULL,
  version TEXT NOT NULL,
  notes TEXT,
  is_latest INTEGER NOT NULL DEFAULT 0,
  bin_fmt_version TEXT, bin_style_count INTEGER, bin_canvas TEXT, bin_name TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE(variant_id, version)
);
CREATE INDEX IF NOT EXISTS idx_versions_variant ON versions(variant_id);

CREATE TABLE IF NOT EXISTS files(
  id TEXT PRIMARY KEY,
  version_id INTEGER NOT NULL,
  file_name TEXT NOT NULL,
  size INTEGER NOT NULL,
  sha256_plain TEXT NOT NULL,
  sha256_cipher TEXT NOT NULL,
  storage_path TEXT NOT NULL,
  file_nonce BLOB NOT NULL,
  cek_wrapped BLOB NOT NULL,
  cek_wrap_nonce BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  sort_order INTEGER NOT NULL DEFAULT 0,      -- 同一版本内的安装顺序（0 起）
  watchface_id TEXT,                          -- 这个包自己的表盘 id（一个设备行可能有 2 个包，各自独立）
  photo_album INTEGER                         -- 1/0 = 自动识别是不是照片表盘；NULL = 还没识别（老数据）
);
CREATE INDEX IF NOT EXISTS idx_files_version ON files(version_id);
CREATE INDEX IF NOT EXISTS idx_files_plain ON files(sha256_plain);

-- 照片表盘的**人工纠正**（自动判据见 bin_inspect::photo_album：亮屏样式 backImageId 高字节 0x82）。
-- 按 watchface_id 存而不是按 files 行存：重传包体会换掉 files 行，但表盘 id 是稳定的，
-- 人工纠正不会因为重传而丢失。生效值 = 有纠正就用纠正，否则用 files.photo_album。
CREATE TABLE IF NOT EXISTS photo_album_overrides(
  watchface_id TEXT PRIMARY KEY,
  is_photo_album INTEGER NOT NULL,            -- 1 = 是、0 = 不是
  note TEXT,
  updated_at INTEGER NOT NULL
);

-- 照片表盘的「壁纸方案」（一条 = 一个表盘包体，见 docs/方案-壁纸生成器.md）。
-- config_json = 版本化的配置：画布尺寸 / 每张照片的基准与可调范围 / 相框窗口与多色号素材。
-- 素材文件落 <data_dir>/editor-assets/<watchface_id>/，这里只登记文件名 + sha256（供 App 校验）。
-- **素材不加密**：定位是"不发 App 版本就能上新方案"，不是保密内容（用户明确）。
CREATE TABLE IF NOT EXISTS editor_templates(
  watchface_id TEXT PRIMARY KEY,
  config_json TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS editor_assets(
  watchface_id TEXT NOT NULL,
  path TEXT NOT NULL,                         -- 素材文件名，只允许相对名（无目录、无 ..）
  size INTEGER NOT NULL,
  sha256 TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(watchface_id, path)
);

CREATE TABLE IF NOT EXISTS codes(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  code_hash TEXT UNIQUE NOT NULL,
  product_id TEXT NOT NULL,
  batch_id TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'unused',   -- unused / used / disabled
  expires_at INTEGER,
  used_at INTEGER,
  used_by TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_codes_batch ON codes(batch_id);
CREATE INDEX IF NOT EXISTS idx_codes_product ON codes(product_id);

CREATE TABLE IF NOT EXISTS subjects(
  id TEXT PRIMARY KEY,
  mi_user_id TEXT UNIQUE NOT NULL,
  all_access INTEGER NOT NULL DEFAULT 0,   -- 1 = 永久全资源（含以后新增），绕过 licenses 逐个授权
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS licenses(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  subject_id TEXT NOT NULL,
  product_id TEXT NOT NULL,
  source TEXT,                             -- redeem:CODE / manual / afdian
  status TEXT NOT NULL DEFAULT 'active',   -- active / revoked
  created_at INTEGER NOT NULL,
  revoked_at INTEGER,
  UNIQUE(subject_id, product_id)
);

CREATE TABLE IF NOT EXISTS tokens(
  token_hash TEXT PRIMARY KEY,
  subject_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER
);

CREATE TABLE IF NOT EXISTS activations(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  subject_id TEXT NOT NULL,
  product_id TEXT NOT NULL,
  device_fp TEXT NOT NULL,
  device_pubkey TEXT,
  device_name TEXT,
  status TEXT NOT NULL DEFAULT 'active',
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER,
  UNIQUE(subject_id, product_id, device_fp)
);
CREATE INDEX IF NOT EXISTS idx_activations_product ON activations(product_id);

CREATE TABLE IF NOT EXISTS afdian_orders(
  out_trade_no TEXT PRIMARY KEY,
  afdian_user_id TEXT NOT NULL,
  plan_id TEXT,
  sku TEXT,
  status INTEGER NOT NULL,
  amount REAL NOT NULL DEFAULT 0,
  month INTEGER NOT NULL DEFAULT 0,
  granted_at INTEGER,                       -- 已据此订单开通过（订单失效时用来判断要不要收回）
  seen_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_afdian_orders_user ON afdian_orders(afdian_user_id);

-- 设备行 ↔ 爱发电购买链接（**一行可挂多条**：同一个包体、可能对应多个商品）
CREATE TABLE IF NOT EXISTS variant_links(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  variant_id TEXT NOT NULL,
  url TEXT,
  plan_id TEXT NOT NULL,
  sku TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE(variant_id, plan_id, sku)
);
CREATE INDEX IF NOT EXISTS idx_variant_links_plan ON variant_links(plan_id);
CREATE INDEX IF NOT EXISTS idx_variant_links_variant ON variant_links(variant_id);

CREATE TABLE IF NOT EXISTS afdian_bindings(
  afdian_user_id TEXT PRIMARY KEY,
  subject_id TEXT NOT NULL,
  bound_at INTEGER NOT NULL
);

-- OAuth 关联授权的一次性会话：App 发起 → 用户浏览器授权 → 回调落结果 → App 轮询取走
CREATE TABLE IF NOT EXISTS afdian_oauth(
  state TEXT PRIMARY KEY,                    -- 随机会话号（App 轮询凭据，一次性）
  subject_id TEXT NOT NULL,
  mi_user_id TEXT NOT NULL,
  device_fp TEXT,
  device_pubkey TEXT,
  device_name TEXT,
  status TEXT NOT NULL DEFAULT 'pending',    -- pending / ok / error
  afdian_user_id TEXT,
  license_token TEXT,                        -- 换好的登录态，等 App 取走
  expires_at INTEGER NOT NULL,               -- 取走前一直有效；过期后 App 重新发起
  message TEXT,
  created_at INTEGER NOT NULL,
  done_at INTEGER
);

CREATE TABLE IF NOT EXISTS download_logs(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  file_id TEXT,
  subject_id TEXT,
  device_fp TEXT,
  ts INTEGER,
  ip TEXT,
  ua TEXT
);

-- 客户端失败打点：App 上报「哪一步失败了、什么码、什么机型」
-- 存在的理由：download_logs / activations 只记成功——买家装不上时，卖家这边什么都看不到。
-- subject_id 可为 NULL（最早那批失败发生在拿到 license_token 之前），所以不加外键。
CREATE TABLE IF NOT EXISTS client_events(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  kind TEXT NOT NULL DEFAULT 'fail',   -- fail / info
  step TEXT NOT NULL,                  -- connect/scan/permission/login/bind/download/decrypt/install/archive/edit/apply
  code TEXT,                           -- 设备或接口返回码（如 code=3、prepare_downgrade、timeout）
  message TEXT,                        -- 技术细节：只给开发者看，买家那边仍是一句操作指引
  device_model TEXT,                   -- 手环/手表型号
  device_fp TEXT,
  app_version TEXT,
  subject_id TEXT,
  ip TEXT,
  ua TEXT
);
CREATE INDEX IF NOT EXISTS idx_client_events_ts ON client_events(ts);

-- App 自身发版（盘坊 APK）。存在的理由：安卓端不支持 Tauri 官方 updater，
-- 第一个公开版本必须自带「检查更新 → 下载 → 调系统安装器」这条通道，
-- 否则买家以后只能手动重装（而他当初多半是用浏览器装的，App 里没有下载入口）。
-- 永久链接 /app/latest.apk 302 到 is_current=1 这一行（爱发电商品详情/自动回复里贴一条就永不过期）。
CREATE TABLE IF NOT EXISTS app_releases(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  version_code INTEGER NOT NULL,          -- Android versionCode（0.3.7 → 3007）
  version_name TEXT NOT NULL,             -- "0.3.7"
  notes TEXT,                             -- 更新说明（给买家看的一两句话）
  file_name TEXT NOT NULL,                -- 对外文件名（/app/<file_name>）
  storage_path TEXT NOT NULL,             -- 服务器上的实际路径
  sha256 TEXT NOT NULL,                   -- 下载后校验（App 侧比对）
  size INTEGER NOT NULL,
  min_version_code INTEGER NOT NULL DEFAULT 0,  -- 低于它就必须更新（App 只提示，不锁死）
  is_current INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_app_releases_current ON app_releases(is_current);
"#;
