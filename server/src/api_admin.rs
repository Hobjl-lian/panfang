//! 管理端接口。鉴权：独立 admin_token。
//!
//! 模型（见 `docs/管理页设计-方案.md`）：
//!   product（资源/表盘）→ variant（设备行，一行 = 一个设备组）→ version（当前包体）+ 爱发电链接
//! 上传即加密；版本号来自 .bin 头部（如 1.0.2），更高才允许替换；默认只保留最新一版。

use axum::extract::{Multipart, Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::bin_inspect::version_tuple;
use crate::db::{new_id, now};
use crate::state::{require_admin, ApiError, ApiResult, AppState};

const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------- 设备组

/// GET /admin/device-groups
pub async fn device_groups_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let mut stmt = conn.prepare(
        "SELECT id, name, models, screen_w, screen_h, sort FROM device_groups ORDER BY sort, created_at",
    )?;
    let rows: Vec<Value> = stmt
        .query_map([], |r| {
            let models: String = r.get(2)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "models": serde_json::from_str::<Value>(&models).unwrap_or(json!([])),
                "screenW": r.get::<_, Option<i64>>(3)?,
                "screenH": r.get::<_, Option<i64>>(4)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(Json(json!({ "deviceGroups": rows })))
}

#[derive(Deserialize)]
pub struct DeviceGroupUpsert {
    pub id: Option<String>,
    pub name: String,
    pub models: Vec<String>,
    pub screen_w: Option<i64>,
    pub screen_h: Option<i64>,
}

/// POST /admin/device-groups —— 新增或更新（型号 ↔ 设备组 对照表）
pub async fn device_group_upsert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DeviceGroupUpsert>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    if body.name.trim().is_empty() || body.models.is_empty() {
        return Err(ApiError::bad_request("设备组需要名称与至少一个型号"));
    }
    let models = serde_json::to_string(&body.models).unwrap_or_else(|_| "[]".into());
    let conn = state.conn();
    let id = body
        .id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| new_id("g_"));
    conn.execute(
        "INSERT INTO device_groups(id, name, models, screen_w, screen_h, sort, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5,
                COALESCE((SELECT sort FROM device_groups WHERE id = ?1), 999), ?6)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, models = excluded.models,
             screen_w = excluded.screen_w, screen_h = excluded.screen_h",
        params![id, body.name.trim(), models, body.screen_w, body.screen_h, now()],
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

// ---------------------------------------------------------------- 资源（产品）

#[derive(Deserialize)]
pub struct NewProduct {
    pub id: Option<String>,
    pub name: String,
    /// 勾选的设备组（会为每个组建一行）
    #[serde(default)]
    pub device_group_ids: Vec<String>,
}

/// POST /admin/products —— 新建资源：名称 + 批量勾选的设备组
pub async fn create_product(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<NewProduct>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request("资源名称不能为空"));
    }
    let id = body
        .id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| new_id("p_"))
        .to_lowercase();
    let conn = state.conn();

    let mut groups: Vec<(String, String, String, Option<i64>, Option<i64>)> = Vec::new();
    for gid in &body.device_group_ids {
        let row = conn
            .query_row(
                "SELECT id, name, models, screen_w, screen_h FROM device_groups WHERE id = ?1",
                [gid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        if let Some(g) = row {
            groups.push(g);
        }
    }

    let tx = conn.unchecked_transaction().map_err(ApiError::from)?;
    let next_sort: i64 = tx
        .query_row("SELECT COALESCE(MAX(sort_order), -1) + 1 FROM products", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO products(id, name, status, sort_order, created_at) VALUES(?1, ?2, 'on', ?3, ?4)",
        params![id, body.name.trim(), next_sort, now()],
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            ApiError::conflict("bad_request", format!("资源 {id} 已存在"))
        } else {
            ApiError::from(e)
        }
    })?;
    let mut variant_ids = Vec::new();
    for (gid, gname, models, w, h) in &groups {
        let vid = format!("{id}@{gid}");
        tx.execute(
            "INSERT INTO variants(id, product_id, name, device_models, screen_w, screen_h, device_group_id, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![vid, id, gname, models, w, h, gid, now()],
        )
        .map_err(ApiError::from)?;
        variant_ids.push(vid);
    }
    tx.commit().map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true, "productId": id, "variantIds": variant_ids })))
}

/// 组装资源的完整视图（卡片墙与详情共用）。
fn product_view(state: &AppState, product_id: Option<&str>) -> ApiResult<Vec<Value>> {
    let conn = state.conn();
    let products: Vec<(String, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT id, name, sort_order FROM products WHERE (?1 IS NULL OR id = ?1)
             ORDER BY sort_order, created_at",
        )?;
        let v: Vec<(String, String, i64)> = stmt
            .query_map(params![product_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        v
    };

    // 链接 → 使用处数（跨资源统计一次）
    let mut link_usage: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT plan_id, sku, COUNT(*) FROM variant_links GROUP BY plan_id, sku",
        )?;
        let rows: Vec<(String, Option<String>, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        for (plan, sku, n) in rows {
            link_usage.insert(format!("{plan}|{}", sku.unwrap_or_default()), n);
        }
    }

    let mut out = Vec::new();
    for (pid, pname, sort) in products {
        let vrows: Vec<(
            String,
            String,
            Option<String>,
            String,
            Option<i64>,
            Option<i64>,
        )> = {
            let mut stmt = conn.prepare(
                "SELECT v.id, v.name, v.device_group_id, v.device_models, v.screen_w, v.screen_h
                 FROM variants v WHERE v.product_id = ?1 ORDER BY v.created_at",
            )?;
            let v: Vec<(
                String,
                String,
                Option<String>,
                String,
                Option<i64>,
                Option<i64>,
            )> = stmt
                .query_map([&pid], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();
            v
        };

        let mut variants = Vec::new();
        let mut latest_overall: Option<String> = None;
        let mut with_pkg = 0i64;
        for (vid, vname, gid, models, w, h) in vrows {
            let pkg: Option<(String, i64, String, String, String, Option<String>)> = conn
                .query_row(
                    "SELECT f.file_name, f.size, vr.version, f.sha256_plain, vr.bin_canvas, vr.bin_name
                     FROM files f JOIN versions vr ON vr.id = f.version_id
                     WHERE vr.variant_id = ?1 ORDER BY vr.id DESC LIMIT 1",
                    [&vid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?;
            // 这一行的全部包（一个版本可能有多个：同一设备拆成两个独立表盘，按 sort_order 依次安装）
            let packages: Vec<Value> = {
                let mut stmt = conn.prepare(
                    "SELECT f.file_name, f.size, vr.version, f.sort_order
                     FROM files f JOIN versions vr ON vr.id = f.version_id
                     WHERE vr.variant_id = ?1 AND vr.is_latest = 1
                     ORDER BY f.sort_order, f.id",
                )?;
                let rows: Vec<(String, i64, String, i64)> = stmt
                    .query_map([&vid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .filter_map(|r| r.ok())
                    .collect();
                rows.into_iter()
                    .map(|(file_name, size, version, sort)| {
                        json!({ "fileName": file_name, "size": size, "version": version, "sort": sort })
                    })
                    .collect()
            };
            if let Some(p) = &pkg {
                with_pkg += 1;
                if latest_overall
                    .as_deref()
                    .map(|cur| version_tuple(&p.2) > version_tuple(cur))
                    .unwrap_or(true)
                {
                    latest_overall = Some(p.2.clone());
                }
            }
            // 这个设备行挂着的所有购买链接（可能多条：同一个包体对应多个爱发电商品）
            let links: Vec<Value> = {
                let mut stmt = conn.prepare(
                    "SELECT url, plan_id, sku FROM variant_links WHERE variant_id = ?1 ORDER BY id",
                )?;
                let rows: Vec<(Option<String>, String, Option<String>)> = stmt
                    .query_map([&vid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .filter_map(|r| r.ok())
                    .collect();
                rows.into_iter()
                    .map(|(url, plan, sku)| {
                        let shared = *link_usage
                            .get(&format!("{plan}|{}", sku.clone().unwrap_or_default()))
                            .unwrap_or(&1);
                        json!({ "url": url, "planId": plan, "sku": sku, "shared": shared })
                    })
                    .collect()
            };
            let first = links.first();
            let url = first.and_then(|l| l["url"].as_str()).map(|s| s.to_string());
            let plan = first.and_then(|l| l["planId"].as_str()).map(|s| s.to_string());
            let sku = first.and_then(|l| l["sku"].as_str()).map(|s| s.to_string());
            let usage = first.and_then(|l| l["shared"].as_i64()).unwrap_or(0);
            variants.push(json!({
                "id": vid,
                "name": vname,
                "deviceGroupId": gid,
                "deviceModels": serde_json::from_str::<Value>(&models).unwrap_or(json!([])),
                "screen": [w, h],
                "package": pkg.map(|(file_name, size, version, sha, canvas, bin_name)| json!({
                    "fileName": file_name, "size": size, "version": version,
                    "sha256": sha, "canvas": canvas, "binName": bin_name,
                })),
                "packages": packages,
                "afdian": { "url": url, "planId": plan, "sku": sku, "shared": usage, "links": links },
            }));
        }
        let buyers: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM licenses WHERE product_id = ?1 AND status='active'",
                [&pid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        out.push(json!({
            "id": pid,
            "name": pname,
            "sort": sort,
            "variantCount": variants.len(),
            "deviceCount": with_pkg,
            "latestVersion": latest_overall,
            "buyers": buyers,
            "variants": variants,
        }));
    }
    Ok(out)
}

/// GET /admin/products —— 卡片墙（含每行详情）
pub async fn list_products(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    Ok(Json(json!({ "products": product_view(&state, None)? })))
}

/// GET /admin/products/:id —— 资源详情
pub async fn product_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    product_view(&state, Some(&id))?
        .into_iter()
        .next()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("资源不存在"))
}

#[derive(Deserialize)]
pub struct Reorder {
    pub ids: Vec<String>,
}

/// POST /admin/products/reorder —— 卡片排序
pub async fn reorder_products(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Reorder>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let tx = conn.unchecked_transaction().map_err(ApiError::from)?;
    for (i, id) in body.ids.iter().enumerate() {
        tx.execute(
            "UPDATE products SET sort_order = ?2 WHERE id = ?1",
            params![id, i as i64],
        )
        .map_err(ApiError::from)?;
    }
    tx.commit().map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

/// DELETE /admin/products/:id —— 删除资源（含各设备行包体；已购用户将无法再下载）
pub async fn delete_product(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let paths: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT f.storage_path FROM files f
             JOIN versions vr ON vr.id = f.version_id
             JOIN variants v ON v.id = vr.variant_id
             WHERE v.product_id = ?1",
        )?;
        let v: Vec<String> = stmt
            .query_map([&id], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        v
    };
    let tx = conn.unchecked_transaction().map_err(ApiError::from)?;
    tx.execute(
        "DELETE FROM files WHERE version_id IN (
           SELECT vr.id FROM versions vr JOIN variants v ON v.id = vr.variant_id WHERE v.product_id = ?1)",
        [&id],
    )
    .map_err(ApiError::from)?;
    tx.execute(
        "DELETE FROM versions WHERE variant_id IN (SELECT id FROM variants WHERE product_id = ?1)",
        [&id],
    )
    .map_err(ApiError::from)?;
    tx.execute("DELETE FROM variants WHERE product_id = ?1", [&id])
        .map_err(ApiError::from)?;
    let n = tx
        .execute("DELETE FROM products WHERE id = ?1", [&id])
        .map_err(ApiError::from)?;
    tx.commit().map_err(ApiError::from)?;
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
    if n == 0 {
        return Err(ApiError::not_found("资源不存在"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- 设备行（variant）

#[derive(Deserialize)]
pub struct AddVariant {
    pub product_id: String,
    pub device_group_id: String,
}

/// 按设备组在资源下建一行（幂等：已存在则返回现有 id）。
fn ensure_variant_for_group(
    conn: &rusqlite::Connection,
    product_id: &str,
    group_id: &str,
) -> ApiResult<String> {
    let vid = format!("{product_id}@{group_id}");
    let exists: Option<String> = conn
        .query_row("SELECT id FROM variants WHERE id = ?1", [&vid], |r| r.get(0))
        .optional()?;
    if exists.is_some() {
        return Ok(vid);
    }
    let g = conn
        .query_row(
            "SELECT name, models, screen_w, screen_h FROM device_groups WHERE id = ?1",
            [group_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| ApiError::bad_request(format!("设备组 {group_id} 不存在")))?;
    conn.execute(
        "INSERT INTO variants(id, product_id, name, device_models, screen_w, screen_h, device_group_id, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![vid, product_id, g.0, g.1, g.2, g.3, group_id, now()],
    )?;
    Ok(vid)
}

/// POST /admin/variants —— 给已有资源「添加设备」
pub async fn create_variant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AddVariant>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let g = conn
        .query_row(
            "SELECT name, models, screen_w, screen_h FROM device_groups WHERE id = ?1",
            [&body.device_group_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| ApiError::not_found("设备组不存在"))?;
    let _ = g; // 型号信息由 ensure_variant_for_group 内部读取
    let vid = ensure_variant_for_group(&conn, &body.product_id, &body.device_group_id)?;
    Ok(Json(json!({ "ok": true, "variantId": vid })))
}

/// DELETE /admin/variants/:id —— 删除某个设备行（含其包体）
pub async fn delete_variant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let paths: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT f.storage_path FROM files f JOIN versions vr ON vr.id = f.version_id WHERE vr.variant_id = ?1",
        )?;
        let v: Vec<String> = stmt
            .query_map([&id], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        v
    };
    let tx = conn.unchecked_transaction().map_err(ApiError::from)?;
    tx.execute(
        "DELETE FROM files WHERE version_id IN (SELECT id FROM versions WHERE variant_id = ?1)",
        [&id],
    )
    .map_err(ApiError::from)?;
    tx.execute("DELETE FROM versions WHERE variant_id = ?1", [&id])
        .map_err(ApiError::from)?;
    let n = tx
        .execute("DELETE FROM variants WHERE id = ?1", [&id])
        .map_err(ApiError::from)?;
    tx.commit().map_err(ApiError::from)?;
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
    if n == 0 {
        return Err(ApiError::not_found("设备行不存在"));
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct AfdianMap {
    pub variant_id: String,
    /// 爱发电商品/规格链接；空字符串 = 清除
    pub url: String,
}

/// 从爱发电链接解析（商品编号, 规格）。容错：商品页 / 下单页 / 带 sku 参数都能认。
/// 单个 %XX 解码（够用：sku 参数里只有 JSON 与常见符号）
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if b[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 递归收集 sku_id（数组 / 对象 / 字符串都认）
fn push_sku_ids(v: &serde_json::Value, ids: &mut Vec<String>) {
    match v {
        serde_json::Value::Array(a) => {
            for e in a {
                push_sku_ids(e, ids);
            }
        }
        serde_json::Value::Object(o) => {
            if let Some(id) = o.get("sku_id").and_then(|x| x.as_str()) {
                ids.push(id.to_string());
            }
            if let Some(inner) = o.get("sku") {
                push_sku_ids(inner, ids);
            }
        }
        serde_json::Value::String(x) if x.len() >= 8 => ids.push(x.clone()),
        _ => {}
    }
}

/// 从任意形状的 JSON 里捞出 sku_id（字符串 / 数组 / 对象），逗号分隔
fn extract_sku_ids(v: &serde_json::Value) -> String {
    let mut ids: Vec<String> = Vec::new();
    push_sku_ids(v, &mut ids);
    ids.dedup();
    ids.join(",")
}

/// 从爱发电商品链接里解析 (商品编号, 规格 id)
pub fn parse_afdian_url(url: &str) -> (Option<String>, Option<String>) {
    let u = url.trim();
    let mut plan = None;
    let mut sku = None;
    if let Some(idx) = u.find("/item/") {
        let rest = &u[idx + 6..];
        let seg: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        if !seg.is_empty() {
            plan = Some(seg);
        }
    }
    if plan.is_none() {
        for key in ["plan_id=", "planId="] {
            if let Some(idx) = u.find(key) {
                let rest = &u[idx + key.len()..];
                let seg: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                if !seg.is_empty() {
                    plan = Some(seg);
                    break;
                }
            }
        }
    }
    // sku：链接里可能带 `sku=<url 编码的 JSON>`（从爱发电页面复制时常见），
    // 也可能是朴素的 `sku_id=xxxx`。这里统一解成逗号分隔的 sku_id，别把整串 JSON 存进库
    // （存错会让这行永远匹配不上订单——2026-10-07 线上踩过）。
    for key in ["sku=", "sku_id=", "skuId="] {
        if let Some(idx) = u.find(key) {
            let rest = &u[idx + key.len()..];
            let raw: String = rest
                .chars()
                .take_while(|c| *c != '&' && *c != '#' && *c != ' ')
                .collect();
            let decoded = percent_decode(&raw);
            let cleaned = decoded
                .trim()
                .trim_matches(|c: char| c == '[' || c == ']' || c == '"' || c == '\\' || c == '\'')
                .to_string();
            // 形如 [{"sku_id":"…","count":1}] → 抽出所有 sku_id
            if cleaned.starts_with('{') || cleaned.starts_with('[') {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&format!("[{cleaned}]")) {
                    let ids = extract_sku_ids(&v);
                    if !ids.is_empty() {
                        sku = Some(ids);
                    }
                } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned) {
                    let ids = extract_sku_ids(&v);
                    if !ids.is_empty() {
                        sku = Some(ids);
                    }
                }
            } else if !cleaned.is_empty() {
                sku = Some(cleaned);
            }
            break;
        }
    }
    (plan, sku)
}

/// POST /admin/variants/afdian —— 给设备行**添加/更新**一条购买链接（url 传空 = 清空该行全部链接）。
/// 一行可以挂多条：同一个包体可能对应多个爱发电商品（如手环9 Pro / 10 Pro 分成两个商品卖）。
pub async fn set_variant_afdian(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AfdianMap>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let url = body.url.trim().to_string();
    let conn = state.conn();
    if url.is_empty() {
        conn.execute("DELETE FROM variant_links WHERE variant_id = ?1", params![body.variant_id])?;
        return Ok(Json(json!({ "ok": true, "cleared": true, "links": [] })));
    }
    let (plan, sku) = parse_afdian_url(&url);
    let Some(plan) = plan else {
        return Err(ApiError::bad_request(
            "没能从链接里认出商品编号：请贴爱发电商品页链接（形如 https://afdian.com/item/xxxxxxxx）",
        ));
    };
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM variants WHERE id = ?1",
        [&body.variant_id],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Err(ApiError::not_found("设备行不存在"));
    }
    conn.execute(
        "INSERT INTO variant_links(variant_id, url, plan_id, sku, created_at) VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(variant_id, plan_id, sku) DO UPDATE SET url = excluded.url",
        params![body.variant_id, url, plan, sku, now()],
    )?;
    let links = variant_links_of(&conn, &body.variant_id)?;
    Ok(Json(json!({ "ok": true, "planId": plan, "sku": sku, "links": links })))
}

#[derive(Deserialize)]
pub struct AfdianLinkDel {
    pub variant_id: String,
    pub plan_id: String,
    pub sku: Option<String>,
}

/// POST /admin/variants/afdian/remove —— 删掉设备行上的某一条购买链接
pub async fn remove_variant_afdian(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AfdianLinkDel>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    conn.execute(
        "DELETE FROM variant_links WHERE variant_id = ?1 AND plan_id = ?2
           AND COALESCE(sku, '') = COALESCE(?3, '')",
        params![body.variant_id, body.plan_id, body.sku],
    )?;
    let links = variant_links_of(&conn, &body.variant_id)?;
    Ok(Json(json!({ "ok": true, "links": links })))
}

/// 某设备行上已配的链接（给管理页显示用）
fn variant_links_of(conn: &rusqlite::Connection, variant_id: &str) -> ApiResult<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT url, plan_id, sku FROM variant_links WHERE variant_id = ?1 ORDER BY id",
    )?;
    let rows: Vec<(Option<String>, String, Option<String>)> = stmt
        .query_map([variant_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows
        .into_iter()
        .map(|(url, plan, sku)| json!({ "url": url, "planId": plan, "sku": sku }))
        .collect())
}

// ---------------------------------------------------------------- 上传（识别 + 加密）

/// 一次上传里的一个文件（一个「包」）。
struct UploadPart {
    bytes: Vec<u8>,
    fname: String,
    path_hint: String,
}

/// 读 multipart：**支持一次带多个 `file` 字段**（同一设备拆成两个独立表盘的情形）。
/// `path_hint` 可以按同样的顺序重复给，也可以只给一个（都当第一个用）。
async fn read_upload(
    multipart: &mut Multipart,
) -> ApiResult<(Vec<UploadPart>, HashMap<String, String>)> {
    let mut files: Vec<(Vec<u8>, String)> = Vec::new();
    let mut hints: Vec<String> = Vec::new();
    let mut fields: HashMap<String, String> = HashMap::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("表单解析失败: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            let fname = field.file_name().unwrap_or("upload.bin").to_string();
            let bytes = field
                .bytes()
                .await
                .map_err(|e| ApiError::bad_request(format!("读取文件失败: {e}")))?;
            if bytes.len() > MAX_UPLOAD_BYTES {
                return Err(ApiError::bad_request(format!(
                    "文件 {} 超过 64MB 上限",
                    fname
                )));
            }
            files.push((bytes.to_vec(), fname));
        } else if name == "path_hint" {
            hints.push(field.text().await.unwrap_or_default());
        } else {
            let v = field.text().await.unwrap_or_default();
            fields.insert(name, v);
        }
    }
    if files.is_empty() {
        return Err(ApiError::bad_request("缺少 file 字段"));
    }
    let fallback = hints.first().cloned().unwrap_or_default();
    let parts: Vec<UploadPart> = files
        .into_iter()
        .enumerate()
        .map(|(i, (bytes, fname))| UploadPart {
            bytes,
            fname,
            path_hint: hints.get(i).cloned().unwrap_or_else(|| fallback.clone()),
        })
        .collect();
    Ok((parts, fields))
}

/// 文件名自然排序键：让 `包1` < `包2` < `包10`（按数字段比较，而不是字典序）。
fn natural_key(name: &str) -> Vec<(u8, u64, String)> {
    let mut out = Vec::new();
    let mut num = String::new();
    let mut txt = String::new();
    for ch in name.chars() {
        if ch.is_ascii_digit() {
            if !txt.is_empty() {
                out.push((1u8, 0u64, std::mem::take(&mut txt)));
            }
            num.push(ch);
        } else {
            if !num.is_empty() {
                out.push((0u8, num.parse::<u64>().unwrap_or(0), String::new()));
                num.clear();
            }
            txt.push(ch.to_ascii_lowercase());
        }
    }
    if !num.is_empty() {
        out.push((0u8, num.parse::<u64>().unwrap_or(0), String::new()));
    }
    if !txt.is_empty() {
        out.push((1u8, 0u64, txt));
    }
    out
}

/// POST /admin/inspect —— 只识别不写库
pub async fn inspect_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let (parts, fields) = read_upload(&mut multipart).await?;
    let (bytes, fname, path_hint) = {
        let p = &parts[0];
        (p.bytes.clone(), p.fname.clone(), p.path_hint.clone())
    };
    let info =
        crate::bin_inspect::inspect(&bytes).map_err(|e| ApiError::bad_request(format!("{e}")))?;
    let mut detection = None;
    if let Some(product_id) = fields.get("product_id").filter(|s| !s.is_empty()) {
        detection = Some(crate::derive::detect(&state, product_id, &fname, &path_hint, &info)?);
    }
    Ok(Json(json!({ "info": info, "detection": detection })))
}

/// POST /admin/versions/upload —— 上传即加密。
/// **一次请求可以带多个 `file`**：同一设备因内存限制拆成两个独立表盘时，
/// 它们同属一个版本（一个购买链接），按**文件名自然排序**依次安装。
/// 版本号取第一个包的 .bin 头部（1.0.2 这种）；比当前更高才替换（`force=true` 可强制）；
/// 替换成功后只保留这一版（旧包密文删除）。
pub async fn upload_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let (mut parts, fields) = read_upload(&mut multipart).await?;
    // 安装顺序 = 文件名自然排序（用户约定：文件名决定顺序）
    parts.sort_by(|a, b| natural_key(&a.fname).cmp(&natural_key(&b.fname)));

    let mut infos = Vec::with_capacity(parts.len());
    for p in &parts {
        let info = crate::bin_inspect::inspect(&p.bytes)
            .map_err(|e| ApiError::bad_request(format!("{}: {e}", p.fname)))?;
        infos.push(info);
    }
    let info = infos[0].clone();
    let fname = parts[0].fname.clone();
    let version = info.version_label.clone();
    let force = fields
        .get("force")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    let product_id = fields
        .get("product_id")
        .cloned()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("缺少 product_id"))?;

    // 所有包的识别都在**取数据库连接之前**做完：
    // derive::detect 内部也会拿同一把连接锁，持锁时再调用会直接死锁。
    let mut dets = Vec::with_capacity(parts.len());
    for (i, p) in parts.iter().enumerate() {
        dets.push(crate::derive::detect(
            &state,
            &product_id,
            &p.fname,
            &p.path_hint,
            &infos[i],
        )?);
    }
    let detection = dets[0].clone();

    let conn = state.conn();
    // 设备行：显式指定 > 已有行自动匹配 > 按设备组自动建行
    let mut created_row = false;
    let variant_id = if let Some(v) = fields.get("variant_id").cloned().filter(|s| !s.is_empty()) {
        v
    } else if let Some(v) = detection.variant_id.clone() {
        v
    } else if let Some(g) = detection.device_group_id.clone() {
        let v = ensure_variant_for_group(&conn, &product_id, &g)?;
        created_row = true;
        v
    } else {
        return Err(ApiError::bad_request(format!(
            "认不出这个包体属于哪个设备（表盘 id={}，画布={:?}，文件名={}）。
             可在「设置 → 设备组」里补一个该屏幕尺寸的设备组，或手动添加设备行后指定。",
            info.watchface_id, info.canvas, fname
        )));
    };
    let vrow: Option<(String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT product_id, name, device_group_id, watchface_id FROM variants WHERE id = ?1",
            [&variant_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (vprod, vname, vgroup, vwfid) = vrow.ok_or_else(|| ApiError::bad_request("设备行不存在"))?;
    if vprod != product_id {
        return Err(ApiError::bad_request("设备行不属于该资源"));
    }

    // 同一批的每个包都必须归到这一行（否则买家装了不配套的两个包）
    let want_wfid = vwfid.clone().unwrap_or_else(|| info.watchface_id.trim().to_string());
    for (i, (p, det)) in parts.iter().zip(dets.iter()).enumerate().skip(1) {
        let same_row = det.variant_id.as_deref() == Some(variant_id.as_str());
        let same_group_new = det.variant_id.is_none()
            && det.device_group_id.as_deref() == Some(vgroup.as_str());
        if !same_row && !same_group_new {
            return Err(ApiError::bad_request(format!(
                "同一批的包必须属于同一个设备行：{} 归到了 {:?}，而第一个包 {} 属于「{}」",
                p.fname,
                det.variant_name.clone().or(det.device_group_name.clone()),
                fname,
                vname
            )));
        }
        // 两个包是「两个独立表盘」——表盘 id 不能相同，否则设备上会互相顶掉、留档也会覆盖
        let wid = infos[i].watchface_id.trim();
        if !wid.is_empty() && wid != "000000000000" && wid == want_wfid.trim() {
            return Err(ApiError::bad_request(format!(
                "同一批的两个包表盘 id 相同（{wid}）：设备上会互相覆盖、编辑留档也会丢一个。
                 拆包时每个包要生成各自的表盘 id。"
            )));
        }
    }

    let current: Option<(String, String)> = conn
        .query_row(
            "SELECT vr.version, f.sha256_plain FROM files f JOIN versions vr ON vr.id = f.version_id
             WHERE vr.variant_id = ?1 ORDER BY vr.id DESC LIMIT 1",
            [&variant_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    // 整批内容都已经在这个设备行里了 → 重复上传（只更新了其中一个包时照样允许）
    let existing: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT f.sha256_plain FROM files f
                 JOIN versions vr ON vr.id = f.version_id WHERE vr.variant_id = ?1",
            )
            .map_err(ApiError::from)?;
        let rows: Vec<String> = stmt
            .query_map([&variant_id], |r| r.get(0))
            .map_err(ApiError::from)?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    if !existing.is_empty() && infos.iter().all(|i| existing.iter().any(|s| s == &i.sha256)) {
        let cur = current
            .as_ref()
            .map(|(v, _)| v.clone())
            .unwrap_or_else(|| "当前版本".into());
        return Err(ApiError::conflict(
            "duplicate",
            format!("这批包和 {cur} 完全相同，无需重复上传"),
        ));
    }

    if let Some((cur_ver, _)) = &current {
        if version_tuple(&version) <= version_tuple(cur_ver) && !force {
            return Err(ApiError::conflict(
                "version_not_newer",
                format!("包体版本 {version} 不高于当前的 {cur_ver}；确认要替换请点「强制替换」"),
            ));
        }
    }

    // 逐个加密（明文只在内存里）
    struct EncPart {
        file_id: String,
        storage: std::path::PathBuf,
        enc: crate::crypto::EncryptedFile,
        sha_cipher: String,
    }
    let mut encs = Vec::with_capacity(parts.len());
    for p in parts.iter() {
        let enc = crate::crypto::encrypt_file(&state.kek, &p.bytes)?;
        let file_id = new_id("f_");
        let storage = state.cipher_dir.join(format!("{file_id}.enc"));
        std::fs::write(&storage, &enc.cipher)
            .map_err(|e| ApiError::internal(format!("写密文失败: {e}")))?;
        let sha_cipher = crate::crypto::sha256_hex(&enc.cipher);
        encs.push(EncPart { file_id, storage, enc, sha_cipher });
    }
    let canvas = info.canvas.map(|(w, h)| format!("{w}x{h}"));
    let batch_files: Vec<Value> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            json!({
                "fileId": encs[i].file_id,
                "fileName": p.fname,
                "size": p.bytes.len(),
                "sha256Plain": infos[i].sha256,
                "watchfaceId": infos[i].watchface_id,
                "name": infos[i].name,
                "styleCount": infos[i].style_count,
                "sort": i as i64,
            })
        })
        .collect();

    let tx = conn.unchecked_transaction().map_err(ApiError::from)?;
    let old_paths: Vec<String> = {
        let mut stmt = tx
            .prepare(
                "SELECT f.storage_path FROM files f
                 JOIN versions vr ON vr.id = f.version_id WHERE vr.variant_id = ?1",
            )
            .map_err(ApiError::from)?;
        let rows: Vec<String> = stmt
            .query_map([&variant_id], |r| r.get(0))
            .map_err(ApiError::from)?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    tx.execute(
        "DELETE FROM files WHERE version_id IN (SELECT id FROM versions WHERE variant_id = ?1)",
        [&variant_id],
    )
    .map_err(ApiError::from)?;
    tx.execute("DELETE FROM versions WHERE variant_id = ?1", [&variant_id])
        .map_err(ApiError::from)?;
    tx.execute(
        "INSERT INTO versions(variant_id, version, notes, is_latest, bin_fmt_version,
             bin_style_count, bin_canvas, bin_name, created_at)
         VALUES(?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8)",
        params![
            variant_id,
            version,
            fields.get("notes").cloned().filter(|s| !s.is_empty()),
            info.fmt_version,
            info.style_count as i64,
            canvas,
            info.name,
            now()
        ],
    )
    .map_err(ApiError::from)?;
    let version_id = tx.last_insert_rowid();
    for (i, p) in parts.iter().enumerate() {
        let e = &encs[i];
        tx.execute(
            "INSERT INTO files(id, version_id, file_name, size, sha256_plain, sha256_cipher,
                 storage_path, file_nonce, cek_wrapped, cek_wrap_nonce, created_at, sort_order)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                e.file_id,
                version_id,
                p.fname,
                p.bytes.len() as i64,
                infos[i].sha256,
                e.sha_cipher,
                e.storage.to_string_lossy(),
                &e.enc.file_nonce[..],
                &e.enc.cek_wrapped,
                &e.enc.cek_wrap_nonce[..],
                now(),
                i as i64
            ],
        )
        .map_err(ApiError::from)?;
    }
    if !info.watchface_id.trim().is_empty() && info.watchface_id.trim() != "000000000000" {
        tx.execute(
            "UPDATE variants SET watchface_id = ?2 WHERE id = ?1 AND (watchface_id IS NULL OR watchface_id = '')",
            params![variant_id, info.watchface_id.trim()],
        )
        .map_err(ApiError::from)?;
    }
    tx.commit().map_err(ApiError::from)?;
    for p in old_paths {
        let _ = std::fs::remove_file(p);
    }

    let replaced = current.as_ref().map(|(v, _)| v.clone());
    Ok(Json(json!({
        "ok": true,
        "variantId": variant_id,
        "variantName": vname,
        "createdRow": created_row,
        "version": version,
        "replacedVersion": replaced,
        "forced": force,
        "count": batch_files.len(),
        // 一批多个包时：files 是全部（按 sort 顺序），file 保留第一个做兼容
        "files": batch_files,
        "file": batch_files.first().cloned().unwrap_or(Value::Null),
        "bin": {
            "watchfaceId": info.watchface_id, "name": info.name,
            "versionLabel": info.version_label, "fmtVersion": info.fmt_version,
            "styleCount": info.style_count, "canvas": canvas,
        },
        "detection": detection,
    })))
}

// ---------------------------------------------------------------- 爱发电

#[derive(Deserialize)]
pub struct AfdianSyncReq {
    pub pages: Option<i64>,
}

/// 全量拉取的运行状态（后台任务写、管理页轮询读）
static BACKFILL_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static BACKFILL_LAST: std::sync::OnceLock<std::sync::Mutex<Option<Value>>> =
    std::sync::OnceLock::new();

fn backfill_last() -> &'static std::sync::Mutex<Option<Value>> {
    BACKFILL_LAST.get_or_init(|| std::sync::Mutex::new(None))
}

/// POST /admin/afdian/sync —— 拉一轮订单并应用。`pages: 0` = **全量拉取历史订单**
/// （迁移老用户用；上万单要上百次请求，所以放后台跑，用 /admin/afdian/sync-status 看进度）。
pub async fn afdian_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AfdianSyncReq>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let pages = body.pages.unwrap_or(2);
    if pages <= 0 {
        use std::sync::atomic::Ordering;
        if BACKFILL_RUNNING.swap(true, Ordering::SeqCst) {
            return Ok(Json(json!({ "ok": true, "backfill": "already_running" })));
        }
        let st = state.clone();
        tokio::spawn(async move {
            println!("[afdian] 开始全量拉取历史订单…");
            let r = crate::afdian::sync_all(&st, |page, n| {
                println!("[afdian] 全量拉取：已翻 {page} 页 / 累计 {n} 单");
            })
            .await;
            let done: Value = match r {
                Ok((sum, count)) => {
                    println!(
                        "[afdian] 全量拉取完成：{count} 单，新开通 {}，收回 {}，未绑定 {}",
                        sum.new_grants.len(),
                        sum.revoked.len(),
                        sum.unbound
                    );
                    json!({ "ok": true, "orders": count, "summary": sum, "finishedAt": crate::db::now() })
                }
                Err(e) => {
                    println!("[afdian] 全量拉取失败：{e:?}");
                    json!({ "ok": false, "error": format!("{e}"), "finishedAt": crate::db::now() })
                }
            };
            *backfill_last().lock().unwrap() = Some(done);
            BACKFILL_RUNNING.store(false, Ordering::SeqCst);
        });
        return Ok(Json(json!({ "ok": true, "backfill": "started" })));
    }
    match crate::afdian::sync_once(&state, pages).await {
        Ok((sum, count)) => Ok(Json(json!({ "ok": true, "orders": count, "summary": sum }))),
        Err(e) => Err(ApiError::bad_request(format!("同步失败：{e:#}"))),
    }
}

/// GET /admin/afdian/sync-status —— 全量拉取进度（管理页轮询）
pub async fn afdian_sync_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    use std::sync::atomic::Ordering;
    let last = backfill_last().lock().unwrap().clone();
    Ok(Json(json!({
        "running": BACKFILL_RUNNING.load(Ordering::SeqCst),
        "last": last,
    })))
}

/// GET /admin/afdian/spec-report —— 规格名体检：已配链接的行上，订单里出现过的规格名是否都认得
/// （老用户迁移前先看这个：认不出的规格名 → 那批买家绑定了也拿不到权限）
pub async fn afdian_spec_report(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let rows = crate::afdian::spec_report(&state)?;
    Ok(Json(json!({ "rows": rows })))
}

/// GET /admin/afdian/orders —— 已记录的订单（含是否已开通、买家绑定情况）
pub async fn afdian_orders(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let mut stmt = conn.prepare(
        "SELECT o.out_trade_no, o.afdian_user_id, o.plan_id, o.sku, o.status, o.granted_at, o.seen_at,
                s.mi_user_id, o.sku_name
         FROM afdian_orders o
         LEFT JOIN afdian_bindings b ON b.afdian_user_id = o.afdian_user_id
         LEFT JOIN subjects s ON s.id = b.subject_id
         ORDER BY o.seen_at DESC LIMIT 200",
    )?;
    let rows: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "outTradeNo": r.get::<_, String>(0)?,
                "afdianUserId": r.get::<_, String>(1)?,
                "planId": r.get::<_, Option<String>>(2)?,
                "sku": r.get::<_, Option<String>>(3)?,
                "status": r.get::<_, i64>(4)?,
                "grantedAt": r.get::<_, Option<i64>>(5)?,
                "seenAt": r.get::<_, i64>(6)?,
                "miUserId": r.get::<_, Option<String>>(7)?,
                "skuName": r
                    .get::<_, Option<String>>(8)?
                    .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
                    .map(|v| v.join(" / "))
                    .unwrap_or_default(),
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(Json(json!({ "orders": rows })))
}

#[derive(Deserialize)]
pub struct AfdianBindReq {
    pub afdian_user_id: String,
    pub mi_user_id: String,
}

/// POST /admin/afdian/bind —— 手动建立绑定（售后/联调用；OAuth 接好后由回调写入）
pub async fn afdian_bind(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AfdianBindReq>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    if body.afdian_user_id.trim().is_empty() || body.mi_user_id.trim().is_empty() {
        return Err(ApiError::bad_request("爱发电用户 ID 与小米账号都要填"));
    }
    crate::afdian::bind_afdian(&state, body.afdian_user_id.trim(), body.mi_user_id.trim())?;
    // 绑定后回头把该买家已有的成功订单补开通
    let synced = crate::afdian::sync_after_bind(&state, body.afdian_user_id.trim())?;
    Ok(Json(json!({ "ok": true, "resentOrders": synced })))
}

#[derive(Deserialize)]
pub struct AfdianSimReq {
    /// 买家爱发电账号（默认用绑定过的第一个）
    pub afdian_user_id: Option<String>,
    /// 商品编号（取管理页里某行已配的链接解析出的 plan_id）
    pub plan_id: String,
    pub sku: Option<String>,
    /// 规格名（如「红米手表5/6」）：模拟"买的是这个规格"，验证按规格路由到设备行
    pub sku_name: Option<String>,
    /// 状态：2 = 成功（默认）；其它值演示「退款收回」
    pub status: Option<i64>,
    /// 顺带把该买家绑定到这个小米账号（联调用）
    pub mi_user_id: Option<String>,
    /// 指定订单号（可选）：同一单号再传一次 status=3 就能演示「退款收回」
    pub out_trade_no: Option<String>,
}

/// POST /admin/afdian/simulate —— **联调用**：模拟一笔爱发电订单，验证「订单 → 自动开通/收回」链路。
/// 仅在联调开关开启且未到期时可用（与 /v1/bind/dev 同一开关）。
/// 传 `sku_name` 可模拟"买的是某个规格"（如「红米手表5/6」），用来验证按规格路由到设备行。
pub async fn afdian_simulate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AfdianSimReq>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    if !crate::afdian::dev_mode() {
        return Err(ApiError::not_found("模拟订单只在联调开关打开时可用"));
    }
    let afdian_user_id = body
        .afdian_user_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("sim-{}", new_id("")));
    if let Some(mi) = body.mi_user_id.clone().filter(|s| !s.is_empty()) {
        crate::afdian::bind_afdian(&state, &afdian_user_id, &mi)?;
    }
    let order = crate::afdian::OrderIn {
        out_trade_no: body
            .out_trade_no
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("SIM{}", new_id(""))),
        afdian_user_id: afdian_user_id.clone(),
        plan_id: body.plan_id.clone(),
        sku: body.sku.clone(),
        sku_titles: body
            .sku_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .map(|s| vec![s])
            .unwrap_or_default(),
        status: body.status.unwrap_or(2),
        amount: 6.0,
        month: 1,
    };
    let sum = crate::afdian::apply_orders(&state, std::slice::from_ref(&order))?;
    Ok(Json(json!({
        "ok": true,
        "order": order,
        "summary": sum,
        "hint": if sum.unbound > 0 { "该买家还没绑定小米账号：填 mi_user_id 再试一次（会顺带建立绑定）" } else { "" },
    })))
}

// ---------------------------------------------------------------- 手动开通

#[derive(Deserialize)]
pub struct Grant {
    pub mi_user_id: String,
    pub product_id: String,
}

/// POST /admin/grant —— 手动给某个小米账号开通某资源（联调/售后用）
pub async fn grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Grant>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let uid = body.mi_user_id.trim().to_string();
    if uid.is_empty() {
        return Err(ApiError::bad_request("请填小米账号 ID"));
    }
    let conn = state.conn();
    let exists: Option<String> = conn
        .query_row("SELECT id FROM products WHERE id = ?1", [&body.product_id], |r| r.get(0))
        .optional()?;
    if exists.is_none() {
        return Err(ApiError::not_found("资源不存在"));
    }
    let subject_id: String = match conn
        .query_row("SELECT id FROM subjects WHERE mi_user_id = ?1", [&uid], |r| r.get::<_, String>(0))
        .optional()?
    {
        Some(id) => id,
        None => {
            let id = new_id("s_");
            conn.execute(
                "INSERT INTO subjects(id, mi_user_id, created_at) VALUES(?1, ?2, ?3)",
                params![id, uid, now()],
            )?;
            id
        }
    };
    conn.execute(
        "INSERT INTO licenses(subject_id, product_id, source, status, created_at)
         VALUES(?1, ?2, 'manual', 'active', ?3)
         ON CONFLICT(subject_id, product_id) DO UPDATE SET status='active', revoked_at=NULL",
        params![subject_id, body.product_id, now()],
    )?;
    // 顺带列出该账号当前拥有的资源，便于核对
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name FROM licenses l JOIN products p ON p.id = l.product_id
         WHERE l.subject_id = ?1 AND l.status = 'active'",
    )?;
    let owned: Vec<Value> = stmt
        .query_map([&subject_id], |r| Ok(json!({ "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)? })))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(Json(json!({ "ok": true, "subjectId": subject_id, "owned": owned })))
}

#[derive(Deserialize)]
pub struct AllAccess {
    pub mi_user_id: String,
    pub all_access: bool,
}

/// POST /admin/subject-all-access —— 给某个小米账号设/取消「永久全资源」
///
/// `all_access=true` 时该账号对**所有在售资源与将来新增的资源**都可下载，
/// 不用逐个 grant（鉴权时按 `subjects.all_access` 判断，licenses 表不受影响）。
/// 这是关掉联调开关（WF_DEV_BIND）之后，给开发者本人保留全资源的正规做法。
pub async fn subject_all_access(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AllAccess>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let uid = body.mi_user_id.trim().to_string();
    if uid.is_empty() {
        return Err(ApiError::bad_request("请填小米账号 ID"));
    }
    let flag = if body.all_access { 1 } else { 0 };
    let conn = state.conn();
    let subject_id: String = match conn
        .query_row(
            "SELECT id FROM subjects WHERE mi_user_id = ?1",
            [&uid],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        Some(id) => id,
        None => {
            let id = new_id("s_");
            conn.execute(
                "INSERT INTO subjects(id, mi_user_id, all_access, created_at) VALUES(?1, ?2, ?3, ?4)",
                params![id, uid, flag, now()],
            )?;
            id
        }
    };
    conn.execute(
        "UPDATE subjects SET all_access = ?2 WHERE id = ?1",
        params![subject_id, flag],
    )?;
    // 只回显后 4 位，别把完整账号回显到响应/日志里
    let masked: String = uid
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    Ok(Json(json!({
        "ok": true,
        "subjectId": subject_id,
        "miUserIdMasked": masked,
        "allAccess": body.all_access,
    })))
}

// ---------------------------------------------------------------- 兑换码
// 已彻底下线（服务端接口、管理页入口都不再提供）。数据库里的 codes 表保留，不再读写。

// ---------------------------------------------------------------- 授权 / 统计

#[derive(Deserialize)]
pub struct ActivationQuery {
    /// 只看某个资源（不传＝全部资源，最多 500 条）
    pub product_id: Option<String>,
}

pub async fn activations_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ActivationQuery>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let mut stmt = conn.prepare(
        "SELECT a.product_id, s.mi_user_id, a.device_fp, a.device_name, a.status, a.created_at, a.last_seen_at
         FROM activations a JOIN subjects s ON s.id = a.subject_id
         WHERE (?1 IS NULL OR a.product_id = ?1) ORDER BY a.id DESC LIMIT 500",
    )?;
    let rows: Vec<Value> = stmt
        .query_map(params![q.product_id], |r| {
            Ok(json!({
                "productId": r.get::<_, String>(0)?,
                "miUserId": r.get::<_, String>(1)?,
                "deviceFp": r.get::<_, String>(2)?,
                "deviceName": r.get::<_, Option<String>>(3)?,
                "status": r.get::<_, String>(4)?,
                "activatedAt": r.get::<_, i64>(5)?,
                "lastSeenAt": r.get::<_, Option<i64>>(6)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(Json(json!({ "activations": rows })))
}

#[derive(Deserialize)]
pub struct Revoke {
    pub product_id: String,
    pub mi_user_id: Option<String>,
    pub device_fp: Option<String>,
}

pub async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Revoke>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let mut revoked = 0usize;
    if let Some(uid) = body.mi_user_id.clone() {
        let sid: Option<String> = conn
            .query_row("SELECT id FROM subjects WHERE mi_user_id = ?1", [&uid], |r| r.get(0))
            .optional()?;
        if let Some(sid) = sid {
            revoked += conn.execute(
                "UPDATE licenses SET status='revoked', revoked_at=?3
                 WHERE subject_id=?1 AND product_id=?2 AND status='active'",
                params![sid, body.product_id, now()],
            )?;
            conn.execute(
                "UPDATE activations SET status='revoked' WHERE subject_id=?1 AND product_id=?2",
                params![sid, body.product_id],
            )?;
            conn.execute("DELETE FROM tokens WHERE subject_id = ?1", [&sid])?;
        }
    }
    if let Some(fp) = body.device_fp.clone() {
        revoked += conn.execute(
            "UPDATE activations SET status='revoked' WHERE product_id=?1 AND device_fp=?2",
            params![body.product_id, fp],
        )?;
    }
    Ok(Json(json!({ "ok": true, "revoked": revoked })))
}

pub async fn stats(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let one = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    Ok(Json(json!({
        "resources": one("SELECT COUNT(*) FROM products"),
        "deviceRows": one("SELECT COUNT(*) FROM variants"),
        "packages": one("SELECT COUNT(*) FROM files"),
        "subjects": one("SELECT COUNT(*) FROM subjects"),
        "activeLicenses": one("SELECT COUNT(*) FROM licenses WHERE status='active'"),
        "activeActivations": one("SELECT COUNT(*) FROM activations WHERE status='active'"),
        "downloads": one("SELECT COUNT(*) FROM download_logs"),
        // 近 7 天客户端失败打点：>0 时首页会亮一条（以前这类信号在 App 里就没了）
        "failEvents7d": one("SELECT COUNT(*) FROM client_events WHERE kind='fail' AND ts >= strftime('%s','now') - 7*86400"),
        // 联调开关状态（管理页顶部据此挂警示条：开着时任何人调 /v1/bind/dev 都能白拿全部资源）
        "devBind": crate::afdian::dev_status(),
    })))
}

// ---------------------------------------------------------------- 客户端诊断（失败打点）

#[derive(Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// GET /admin/events —— App 上报的失败打点（近 N 天）。
///
/// 回答两个问题：① 该先修哪个（按「步骤 + 错误码」聚合的次数）② 到底报了什么（最近几条原文）。
/// 聚合在 Rust 里做：量小（个人项目），比一条花哨的 SQL 好读、好改。
pub async fn events_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let days = q.days.unwrap_or(7).clamp(1, 90);
    let limit = q.limit.unwrap_or(30).clamp(1, 200) as usize;
    let since = now() - days * 86_400;
    let conn = state.conn();

    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM client_events WHERE ts >= ?1",
        [since],
        |r| r.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT ts, step, COALESCE(code,''), COALESCE(message,''), COALESCE(device_model,''),
                COALESCE(app_version,'')
         FROM client_events WHERE ts >= ?1 ORDER BY ts DESC LIMIT 1000",
    )?;
    let rows: Vec<(i64, String, String, String, String, String)> = stmt
        .query_map([since], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // 按「步骤 + 错误码」聚合；rows 已按时间倒序，第一次遇到的就是最近一条
    let mut agg: HashMap<(String, String), (i64, i64, String, String, String)> = HashMap::new();
    for (ts, step, code, msg, model, ver) in &rows {
        let e = agg
            .entry((step.clone(), code.clone()))
            .or_insert((0, 0, String::new(), String::new(), String::new()));
        e.0 += 1;
        if *ts > e.1 {
            e.1 = *ts;
            e.2 = msg.clone();
            e.3 = model.clone();
            e.4 = ver.clone();
        }
    }
    let mut by_step: Vec<(String, String, i64, i64, String, String, String)> = agg
        .into_iter()
        .map(|((step, code), (n, last, msg, model, ver))| (step, code, n, last, msg, model, ver))
        .collect();
    by_step.sort_by(|a, b| b.2.cmp(&a.2));

    Ok(Json(json!({
        "days": days,
        "since": since,
        "total": total,
        "byStep": by_step.iter().take(40).map(|(step, code, n, last, msg, model, ver)| json!({
            "step": step, "code": code, "count": n, "lastTs": last,
            "message": msg, "deviceModel": model, "appVersion": ver,
        })).collect::<Vec<_>>(),
        "recent": rows.iter().take(limit).map(|(ts, step, code, msg, model, ver)| json!({
            "ts": ts, "step": step, "code": code, "message": msg,
            "deviceModel": model, "appVersion": ver,
        })).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------- App 发版（客户端本体）

const APP_RELEASE_COLS: &str = "id, version_code, version_name, notes, file_name, \
     sha256, size, min_version_code, is_current, created_at";

/// 一行 app_releases → 给管理页/客户端的 JSON。
fn app_release_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "versionCode": r.get::<_, i64>(1)?,
        "versionName": r.get::<_, String>(2)?,
        "notes": r.get::<_, Option<String>>(3)?,
        "fileName": r.get::<_, String>(4)?,
        "sha256": r.get::<_, String>(5)?,
        "size": r.get::<_, i64>(6)?,
        "minVersionCode": r.get::<_, i64>(7)?,
        "isCurrent": r.get::<_, i64>(8)? == 1,
        "createdAt": r.get::<_, i64>(9)?,
    }))
}

/// POST /admin/app/release —— 发布一个 App 版本（multipart：file=APK + 表单字段）。
///
/// 字段：`version_name`（0.3.7）、`version_code`（3007）、`notes`、`min_version_code`（可选）、
/// `force`（版本号不高于当前也允许发布，用于「内容变了但版本号没变」的补救）。
/// 落盘：`<data_dir>/app/panfang-<version_name>.apk`，并在库里把这一行设为当前版本。
pub async fn app_release_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let (parts, fields) = read_upload(&mut multipart).await?;
    let bytes = &parts[0].bytes;

    // 它得真是个 APK：ZIP 头 + ZIP 目录里必然出现的 AndroidManifest.xml
    let looks_apk = bytes.starts_with(b"PK\x03\x04")
        && bytes
            .windows(b"AndroidManifest.xml".len())
            .any(|w| w == b"AndroidManifest.xml");
    if !looks_apk {
        return Err(ApiError::bad_request(
            "这不像一个 APK（缺少 ZIP 头或 AndroidManifest.xml）—— 请上传签名后的安装包",
        ));
    }

    let version_name = fields
        .get("version_name")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("缺少 version_name（如 0.3.7）"))?;
    if !version_name.chars().all(|c| c.is_ascii_digit() || c == '.') || !version_name.contains('.') {
        return Err(ApiError::bad_request("version_name 格式应为 0.3.7 这样"));
    }
    let version_code: i64 = fields
        .get("version_code")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::bad_request("缺少 version_code（如 3007）"))?;
    let notes = fields.get("notes").cloned().unwrap_or_default();
    let min_version_code: i64 = fields
        .get("min_version_code")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let force = fields
        .get("force")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    // 版本号只许往前走：否则「检查更新」的比对会失效（或把用户带到旧包）
    {
        let conn = state.conn();
        let newest: Option<(i64, String)> = conn
            .query_row(
                "SELECT version_code, version_name FROM app_releases ORDER BY version_code DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((code, name)) = newest {
            if version_code <= code && !force {
                return Err(ApiError::bad_request(format!(
                    "已有更高或同号的版本 {name}（{code}）—— 要覆盖就带 force=1"
                )));
            }
        }
    }

    let app_dir = state.data_dir.join("app");
    std::fs::create_dir_all(&app_dir).map_err(|e| ApiError::internal(format!("建目录失败: {e}")))?;
    let sha256 = crate::crypto::sha256_hex(bytes);
    // 文件名带内容哈希前缀：同名强制覆盖时两份内容不会共用同一个 URL
    // （否则「删旧行连带把当前版本的文件删掉」；也让 `max-age=86400` 的长缓存真正安全）
    let file_name = format!("panfang-{version_name}-{}.apk", &sha256[..8]);
    let storage = app_dir.join(&file_name);
    std::fs::write(&storage, bytes).map_err(|e| ApiError::internal(format!("写 APK 失败: {e}")))?;
    let size = bytes.len() as i64;

    {
        let conn = state.conn();
        conn.execute("UPDATE app_releases SET is_current=0", [])?;
        conn.execute(
            "INSERT INTO app_releases(version_code, version_name, notes, file_name, storage_path,
                 sha256, size, min_version_code, is_current, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,?9)",
            params![
                version_code,
                version_name,
                notes,
                file_name,
                storage.to_string_lossy(),
                sha256,
                size,
                min_version_code,
                now()
            ],
        )?;
    }
    println!(
        "[wf-auth] App 发版：{version_name}（{version_code}，{:.1} MB）→ /app/{file_name}",
        size as f64 / 1048576.0
    );
    let conn = state.conn();
    let row = conn.query_row(
        &format!("SELECT {APP_RELEASE_COLS} FROM app_releases WHERE is_current=1 ORDER BY id DESC LIMIT 1"),
        [],
        app_release_json,
    )?;
    Ok(Json(row))
}

/// GET /admin/app/releases —— 发版列表（新的在前）。
pub async fn app_releases_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let mut stmt = conn.prepare(&format!(
        "SELECT {APP_RELEASE_COLS} FROM app_releases ORDER BY version_code DESC, id DESC LIMIT 100"
    ))?;
    let rows: Vec<Value> = stmt
        .query_map([], app_release_json)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(Json(json!({ "releases": rows })))
}

/// POST /admin/app/releases/:id/current —— 把某一版设为当前（旧包还在，可直接切回去当回滚）。
pub async fn app_release_set_current(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let exists: bool = conn
        .query_row("SELECT 1 FROM app_releases WHERE id=?1", [id], |_| Ok(()))
        .optional()?
        .is_some();
    if !exists {
        return Err(ApiError::not_found("没有这个版本"));
    }
    conn.execute("UPDATE app_releases SET is_current=0", [])?;
    conn.execute("UPDATE app_releases SET is_current=1 WHERE id=?1", [id])?;
    let row = conn.query_row(
        &format!("SELECT {APP_RELEASE_COLS} FROM app_releases WHERE id=?1"),
        [id],
        app_release_json,
    )?;
    println!("[wf-auth] App 当前版本切到 id={id}");
    Ok(Json(row))
}

/// DELETE /admin/app/releases/:id —— 删掉一个版本（连同服务器上的 APK）。
/// 用途：版本号传错/传错包时收拾。当前版本要先切走再删（避免「latest 指向已删的包」）。
pub async fn app_release_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let row: Option<(String, i64, String)> = {
        let conn = state.conn();
        conn.query_row(
            "SELECT version_name, is_current, storage_path FROM app_releases WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
    };
    let Some((name, is_current, path)) = row else {
        return Err(ApiError::not_found("没有这个版本"));
    };
    if is_current == 1 {
        return Err(ApiError::bad_request(
            "这是当前版本：先把别的版本设为当前，再删它",
        ));
    }
    {
        let conn = state.conn();
        conn.execute("DELETE FROM app_releases WHERE id=?1", [id])?;
    }
    // 只有没有任何一行还指向这个文件时才删它（历史上「同名强制覆盖」的两行会共用路径）
    let still_used: i64 = {
        let conn = state.conn();
        conn.query_row(
            "SELECT COUNT(*) FROM app_releases WHERE storage_path=?1",
            [&path],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    if still_used == 0 {
        let _ = std::fs::remove_file(&path);
    } else {
        println!("[wf-auth] 该文件还有 {still_used} 行在用，保留不删：{path}");
    }
    println!("[wf-auth] App 版本已删除：{name}");
    Ok(Json(json!({ "ok": true, "versionName": name })))
}

#[cfg(test)]
mod tests {
    use super::parse_afdian_url;

    #[test]
    fn afdian_url_parsing() {
        let (p, s) = parse_afdian_url("https://afdian.com/item/aacfe164985411f1aef45254001e7c00");
        assert_eq!(p.as_deref(), Some("aacfe164985411f1aef45254001e7c00"));
        assert!(s.is_none());

        let (p, s) = parse_afdian_url("https://afdian.com/item/abc123?sku=sku_789");
        assert_eq!(p.as_deref(), Some("abc123"));
        assert_eq!(s.as_deref(), Some("sku_789"));

        // 线上真实踩过的坑：链接里带 URL 编码的 sku JSON，必须解成纯 sku_id
        let (p, s) = parse_afdian_url(
            "https://afdian.com/item/d367300eb99311f191a95254001e7c00?sku=%5B%7B%22sku_id%22%3A%22d376fb42b99311f1be0e5254001e7c00%22%2C%22count%22%3A1%7D%5D",
        );
        assert_eq!(p.as_deref(), Some("d367300eb99311f191a95254001e7c00"));
        assert_eq!(s.as_deref(), Some("d376fb42b99311f1be0e5254001e7c00"));

        // 未编码的形式也要认
        let (_, s) = parse_afdian_url(
            "https://afdian.com/item/abc?sku=[{\"sku_id\":\"sku_a\",\"count\":1},{\"sku_id\":\"sku_b\",\"count\":2}]",
        );
        assert_eq!(s.as_deref(), Some("sku_a,sku_b"));

        let (p, _) = parse_afdian_url("https://afdian.com/order/create?plan_id=plan999&month=1");
        assert_eq!(p.as_deref(), Some("plan999"));

        let (p, _) = parse_afdian_url("随便一段文本");
        assert!(p.is_none());
    }
}
