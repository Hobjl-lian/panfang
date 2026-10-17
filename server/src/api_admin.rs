//! 管理端接口。鉴权：独立 admin_token。
//!
//! 模型（见 `docs/管理页设计-方案.md`）：
//!   product（资源/表盘）→ variant（设备行，一行 = 一个设备组）→ version（当前包体）+ 爱发电链接
//! 上传即加密；按**表盘 id** 认包（同 id 替换、没上传的保留）；
//! 版本闸门按包比较（`files.bin_version`），整行版本取各包最大值。

use axum::extract::{Multipart, Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

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
                    "SELECT f.id, f.file_name, f.size,
                            COALESCE(NULLIF(f.bin_version, ''), vr.version) AS pkg_version,
                            f.sort_order, f.watchface_id, f.photo_album, o.is_photo_album
                     FROM files f
                     JOIN versions vr ON vr.id = f.version_id
                     LEFT JOIN photo_album_overrides o ON o.watchface_id = f.watchface_id
                     WHERE vr.variant_id = ?1 AND vr.is_latest = 1
                     ORDER BY f.sort_order, f.id",
                )?;
                // (文件 id, 文件名, 大小, 该包自己的版本, 顺序, 表盘 id, 自动判定, 人工纠正)
                type PkgRow = (
                    String,
                    String,
                    i64,
                    String,
                    i64,
                    Option<String>,
                    Option<i64>,
                    Option<i64>,
                );
                let rows: Vec<PkgRow> = stmt
                    .query_map([&vid], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                        ))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                rows.into_iter()
                    .map(|(file_id, file_name, size, version, sort, wfid, auto, manual)| {
                        // 生效值 = 有人工纠正就听人的，否则用自动识别（都缺 = 未知）
                        let effective = manual.or(auto);
                        json!({
                            "fileId": file_id,
                            "fileName": file_name,
                            "size": size,
                            "version": version,
                            "sort": sort,
                            "watchfaceId": wfid,
                            "photoAlbum": effective.map(|v| v != 0),
                            "photoAlbumAuto": auto.map(|v| v != 0),
                            "photoAlbumManual": manual.map(|v| v != 0),
                        })
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

// ---------------------------------------------------------------- 照片表盘

/// GET /admin/photo-albums —— 全部包体的「照片表盘」判定（自动识别 + 人工纠正）。
///
/// 判据是**内容**而不是名字（见 `bin_inspect::photo_album`：亮屏样式 `backImageId` 高字节
/// `0x82`），所以名字里没有「照片表盘」的（如《OS27模块》）照样能被标出来。
/// `photoAlbumAuto = null` 表示这个包还没被识别过（等回填工具跑一次）。
pub async fn photo_albums_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let mut stmt = conn
        .prepare(
            "SELECT f.watchface_id, f.file_name, vr.version, f.sort_order,
                    va.id, va.name, p.id, p.name, f.photo_album, o.is_photo_album,
                    va.screen_w, va.screen_h
             FROM files f
             JOIN versions vr ON vr.id = f.version_id
             JOIN variants va ON va.id = vr.variant_id
             JOIN products p ON p.id = va.product_id
             LEFT JOIN photo_album_overrides o ON o.watchface_id = f.watchface_id
             WHERE vr.is_latest = 1 AND f.watchface_id IS NOT NULL AND f.watchface_id != ''
             ORDER BY p.sort_order, p.name, va.name, f.sort_order, f.id",
        )
        .map_err(ApiError::from)?;
    type FlagRow = (
        String,         // watchface_id
        String,         // file_name
        String,         // version
        i64,            // sort_order
        String,         // variant_id
        String,         // variant_name
        String,         // product_id
        String,         // product_name
        Option<i64>,    // 自动判定
        Option<i64>,    // 人工纠正
        Option<i64>,    // 设备行屏宽
        Option<i64>,    // 设备行屏高
    );
    let rows: Vec<FlagRow> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
            ))
        })
        .map_err(ApiError::from)?
        .filter_map(|r| r.ok())
        .collect();

    let mut items = Vec::with_capacity(rows.len());
    let (mut packages, mut photos, mut unidentified) = (0i64, 0i64, 0i64);
    for (wfid, file_name, version, sort, vid, vname, pid, pname, auto, manual, sw, sh) in rows {
        let effective = manual.or(auto);
        packages += 1;
        match effective {
            Some(1) => photos += 1,
            None => unidentified += 1,
            _ => {}
        }
        items.push(json!({
            "watchfaceId": wfid,
            "fileName": file_name,
            "version": version,
            "sort": sort,
            "variantId": vid,
            "variantName": vname,
            "productId": pid,
            "productName": pname,
            "photoAlbum": effective.map(|v| v != 0),
            "photoAlbumAuto": auto.map(|v| v != 0),
            "photoAlbumManual": manual.map(|v| v != 0),
            "screen": sw.zip(sh).map(|(w, h)| json!([w, h])),
        }));
    }
    Ok(Json(json!({
        "summary": { "packages": packages, "photoAlbums": photos, "unidentified": unidentified },
        "items": items,
    })))
}

#[derive(Deserialize)]
pub struct PhotoAlbumSet {
    pub watchface_id: String,
    /// true/false = 人工判定；**null = 清除人工判定**（回到自动识别）
    pub is_photo_album: Option<bool>,
    #[serde(default)]
    pub note: Option<String>,
}

/// POST /admin/photo-album —— 人工纠正某个表盘「是不是照片表盘」。
///
/// 自动判据（读 .bin 内容）实测够用，这个开关是给例外和你手工指定用的。
/// 值按 `watchface_id` 存 → **重传包体不会丢**；传 `is_photo_album: null` 即清除。
pub async fn set_photo_album(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PhotoAlbumSet>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let wfid = body.watchface_id.trim().to_string();
    if wfid.is_empty() {
        return Err(ApiError::bad_request("缺少 watchface_id"));
    }
    let conn = state.conn();
    // 只允许改库里真实存在的表盘，避免手滑把垃圾 id 写进去
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM files WHERE watchface_id = ?1 LIMIT 1",
            [&wfid],
            |r| r.get(0),
        )
        .optional()
        .map_err(ApiError::from)?;
    if exists.is_none() {
        return Err(ApiError::not_found("库里没有这个表盘 id"));
    }
    match body.is_photo_album {
        Some(v) => {
            conn.execute(
                "INSERT INTO photo_album_overrides(watchface_id, is_photo_album, note, updated_at)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(watchface_id) DO UPDATE SET
                   is_photo_album = excluded.is_photo_album,
                   note = excluded.note,
                   updated_at = excluded.updated_at",
                params![&wfid, v as i64, body.note, now()],
            )
            .map_err(ApiError::from)?;
            Ok(Json(json!({ "ok": true, "watchfaceId": wfid, "isPhotoAlbum": v })))
        }
        None => {
            conn.execute(
                "DELETE FROM photo_album_overrides WHERE watchface_id = ?1",
                [&wfid],
            )
            .map_err(ApiError::from)?;
            Ok(Json(json!({ "ok": true, "watchfaceId": wfid, "cleared": true })))
        }
    }
}

// ---------------------------------------------------------------- 壁纸方案（照片表盘的壁纸生成器）

/// GET /admin/editor-templates —— 全部「壁纸方案」清单
pub async fn editor_list(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let mut stmt = conn
        .prepare(
            "SELECT t.watchface_id, t.enabled, t.updated_at, length(t.config_json),
                    (SELECT COUNT(*) FROM editor_assets a WHERE a.watchface_id = t.watchface_id)
             FROM editor_templates t ORDER BY t.updated_at DESC",
        )
        .map_err(ApiError::from)?;
    let rows: Vec<(String, i64, i64, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(ApiError::from)?
        .filter_map(|r| r.ok())
        .collect();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(wf, enabled, at, bytes, assets)| {
            json!({ "watchfaceId": wf, "enabled": enabled != 0,
                    "updatedAt": at, "configBytes": bytes, "assetCount": assets })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

/// GET /admin/editor-template/:watchface_id —— 取一条方案（含素材清单）
pub async fn editor_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(wfid): Path<String>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let wfid = crate::editor::safe_watchface_id(&wfid)?;
    let conn = state.conn();
    let row: Option<(String, i64, i64)> = conn
        .query_row(
            "SELECT config_json, enabled, updated_at FROM editor_templates WHERE watchface_id = ?1",
            [&wfid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(ApiError::from)?;
    let Some((cfg, enabled, at)) = row else {
        return Err(ApiError::not_found("这个表盘还没有壁纸方案"));
    };
    let mut stmt = conn
        .prepare("SELECT path, size, sha256 FROM editor_assets WHERE watchface_id = ?1 ORDER BY path")
        .map_err(ApiError::from)?;
    let assets: Vec<Value> = stmt
        .query_map([&wfid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(ApiError::from)?
        .filter_map(|r| r.ok())
        .map(|(path, size, sha)| json!({ "path": path, "size": size, "sha256": sha }))
        .collect();
    Ok(Json(json!({
        "watchfaceId": wfid,
        "exists": true,
        "enabled": enabled != 0,
        "updatedAt": at,
        "config": serde_json::from_str::<Value>(&cfg).unwrap_or(Value::Null),
        "configRaw": cfg,
        "assets": assets,
    })))
}

/// POST /admin/editor-template —— 保存方案（multipart：`watchface_id` + `config`(JSON 文本) + 可选 `file[]`）
///
/// 素材文件名就是配置里引用的相对名（`1.png`…）。保存后会**如实回报配置引用了但还没传的图**，
/// 免得 App 那边拉到一份引用不存在素材的配置。
pub async fn editor_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let (parts, fields) = read_upload(&mut multipart, false).await?;
    let wfid = crate::editor::safe_watchface_id(
        fields.get("watchface_id").map(|s| s.as_str()).unwrap_or(""),
    )?;
    let cfg_raw = fields
        .get("config")
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("缺少 config"))?;
    let cfg: Value = serde_json::from_str(&cfg_raw)
        .map_err(|e| ApiError::bad_request(format!("config 不是合法 JSON：{e}")))?;
    let refs = crate::editor::validate_config(&cfg)?;

    // 写素材（文件名先过白名单；只落在这个表盘自己的目录里）
    let dir = crate::editor::assets_dir(&state, &wfid);
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(format!("建素材目录失败: {e}")))?;
    let conn = state.conn();
    let mut written: Vec<String> = Vec::new();
    for p in &parts {
        let name = crate::editor::safe_asset_name(&p.fname)?;
        let path = dir.join(&name);
        std::fs::write(&path, &p.bytes).map_err(|e| ApiError::internal(format!("写素材失败: {e}")))?;
        conn.execute(
            "INSERT INTO editor_assets(watchface_id, path, size, sha256, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(watchface_id, path) DO UPDATE SET
               size = excluded.size, sha256 = excluded.sha256, created_at = excluded.created_at",
            params![
                &wfid,
                &name,
                p.bytes.len() as i64,
                crate::crypto::sha256_hex(&p.bytes),
                now()
            ],
        )
        .map_err(ApiError::from)?;
        written.push(name);
    }

    conn.execute(
        "INSERT INTO editor_templates(watchface_id, config_json, enabled, updated_at)
         VALUES(?1, ?2, 1, ?3)
         ON CONFLICT(watchface_id) DO UPDATE SET
           config_json = excluded.config_json, updated_at = excluded.updated_at",
        params![&wfid, &cfg_raw, now()],
    )
    .map_err(ApiError::from)?;

    // 配置引用了、但库里/这一批都没传的图 → 如实报出来
    let have: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT path FROM editor_assets WHERE watchface_id = ?1")
            .map_err(ApiError::from)?;
        let v: Vec<String> = stmt
            .query_map([&wfid], |r| r.get(0))
            .map_err(ApiError::from)?
            .filter_map(|r| r.ok())
            .collect();
        v
    };
    let missing: Vec<&String> = refs.iter().filter(|r| !have.contains(r)).collect();
    Ok(Json(json!({
        "ok": true,
        "watchfaceId": wfid,
        "uploaded": written,
        "assets": have.len(),
        "missing": missing,
    })))
}

/// DELETE /admin/editor-template/:watchface_id —— 删方案（连带素材登记与文件）
pub async fn editor_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(wfid): Path<String>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let wfid = crate::editor::safe_watchface_id(&wfid)?;
    let conn = state.conn();
    let n = conn
        .execute("DELETE FROM editor_templates WHERE watchface_id = ?1", [&wfid])
        .map_err(ApiError::from)?;
    conn.execute("DELETE FROM editor_assets WHERE watchface_id = ?1", [&wfid])
        .map_err(ApiError::from)?;
    let dir = crate::editor::assets_dir(&state, &wfid);
    if dir.starts_with(crate::editor::assets_root(&state)) {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(Json(json!({ "ok": true, "removed": n })))
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

/// DELETE /admin/files/:id —— 从设备行里删掉**一个包**（其余包原样保留）。
///
/// 用途：按表盘 id 合并后，如果重新导出的包换了 id（会被当成新包追加）、
/// 或者误传了多余的包，可以在这里收拾。删掉最后一个包时连同版本行一起删，
/// 设备行本身保留（购买链接/别名不丢），之后还能重新上传。
pub async fn delete_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let row: Option<(i64, String, Option<String>)> = conn
        .query_row(
            "SELECT f.version_id, f.storage_path, f.watchface_id
             FROM files f WHERE f.id = ?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (version_id, storage, wfid) =
        row.ok_or_else(|| ApiError::not_found("包体不存在"))?;
    let current_ver: String = conn
        .query_row("SELECT version FROM versions WHERE id = ?1", [version_id], |r| {
            r.get(0)
        })
        .unwrap_or_default();

    // 剩余包（按安装顺序）：删完要重新编号，并把整行版本落到各包最大值
    let remaining: Vec<(String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, bin_version FROM files WHERE version_id = ?1 AND id <> ?2
             ORDER BY sort_order, id",
        )?;
        let out: Vec<(String, Option<String>)> = stmt
            .query_map(params![version_id, &id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        out
    };

    let tx = conn.unchecked_transaction().map_err(ApiError::from)?;
    tx.execute("DELETE FROM files WHERE id = ?1", [&id])
        .map_err(ApiError::from)?;
    let emptied = remaining.is_empty();
    if emptied {
        tx.execute("DELETE FROM versions WHERE id = ?1", [version_id])
            .map_err(ApiError::from)?;
    } else {
        for (i, (fid, _)) in remaining.iter().enumerate() {
            tx.execute(
                "UPDATE files SET sort_order = ?2 WHERE id = ?1",
                params![fid, i as i64],
            )
            .map_err(ApiError::from)?;
        }
        let max_ver = remaining
            .iter()
            .filter_map(|(_, v)| v.as_deref())
            .filter(|s| !s.trim().is_empty())
            .max_by_key(|s| version_tuple(s))
            .map(|s| s.to_string())
            .unwrap_or(current_ver);
        tx.execute(
            "UPDATE versions SET version = ?2 WHERE id = ?1",
            params![version_id, max_ver],
        )
        .map_err(ApiError::from)?;
    }
    tx.commit().map_err(ApiError::from)?;
    let _ = std::fs::remove_file(&storage);
    println!(
        "[wf-auth] 已从设备行删除包 {id}（表盘 id={}）",
        wfid.as_deref().unwrap_or("-")
    );
    Ok(Json(json!({ "ok": true, "variantEmptied": emptied })))
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
/// 解析 multipart：`require_file` = 这类请求必须带文件（上传 .bin 是；壁纸方案的通用表盘没有模板图）。
async fn read_upload(
    multipart: &mut Multipart,
    require_file: bool,
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
    if files.is_empty() && require_file {
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
    let (parts, fields) = read_upload(&mut multipart, true).await?;
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

/// 上传合并计划：一个设备行里按**表盘 id** 认包，只替换上传上来的那一个，
/// 没上传的原样保留。纯内存计算，便于单测。
#[derive(Debug, Clone)]
struct ExistingFile {
    id: String,
    file_name: String,
    sha: String,
    storage_path: String,
    sort_order: i64,
    watchface_id: Option<String>,
    bin_version: Option<String>,
}

#[derive(Debug, Clone)]
struct IncomingFile {
    file_name: String,
    sha: String,
    watchface_id: String,
    version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PkgAction {
    /// 同 id、同内容：不重新加密，只保留（并补齐缺的版本号/文件名）
    Unchanged,
    /// 同 id、内容变了：替换这一个包，槽位不变
    Replace,
    /// 没在现有包里找到这个 id：作为新包追加
    Add,
    /// 这次没上传：原样保留
    Kept,
}

#[derive(Debug, Clone)]
struct SlotPlan {
    action: PkgAction,
    existing_idx: Option<usize>,
    incoming_idx: Option<usize>,
    sort_order: i64,
}

#[derive(Debug)]
struct MergePlan {
    slots: Vec<SlotPlan>,
    /// 整行版本 = 各包版本的最大值
    version: String,
}

#[derive(Debug)]
struct MergeError {
    kind: &'static str,
    message: String,
}

fn valid_watchface_id(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && s != "000000000000"
}

/// 严格校验（上传入口用）：表盘 id 必须是 **9 或 12 位字母数字**，且不能是全零。
///
/// 为什么不能放宽：App 端安装前用 corelib 的 `get_watchface_id` 解析，它只认 9/12 位；
/// 10/11 位（以及全零）会被判成 `invalid watchface id`，包传到手环前就失败。
/// 2026-10-17 线上有 6 个这种包（设计师导出时 ID 生成错了），所以在上传入口直接拦住，
/// 并把「哪个包、什么 id、多少位」写清楚，避免再让买家当试错的人。
fn valid_watchface_id_strict(s: &str) -> bool {
    let s = s.trim();
    matches!(s.chars().count(), 9 | 12)
        && s.bytes().all(|b| b.is_ascii_alphanumeric())
        && s != "000000000000"
}

/// `incoming` 已按文件名自然排序。
fn plan_merge(
    existing: &[ExistingFile],
    incoming: &[IncomingFile],
    current_version: Option<&str>,
    force: bool,
) -> Result<MergePlan, MergeError> {
    let mut claimed = vec![false; existing.len()];
    // (incoming index, action, 命中的现有包)
    let mut decided: Vec<(usize, PkgAction, Option<usize>)> = Vec::with_capacity(incoming.len());

    for (ii, inc) in incoming.iter().enumerate() {
        let wid = inc.watchface_id.trim();
        let target = if valid_watchface_id(wid) {
            existing
                .iter()
                .position(|e| e.watchface_id.as_deref().map(str::trim) == Some(wid))
        } else if existing.is_empty() {
            // 首次上传：没有可识别 id 也允许（老包/特殊工程的 id 可能是全零）
            None
        } else if existing.len() == 1 && !claimed[0] {
            // 老流程/老数据：单包行允许没有可识别 id 时直接替换
            Some(0)
        } else {
            return Err(MergeError {
                kind: "no_id",
                message: format!(
                    "包 {} 没有可识别的表盘 id，而这一行有 {} 个包，认不出要替换哪一包。\
                     请重新导出带表盘 id 的 .bin，或先删掉旧包再上传。",
                    inc.file_name,
                    existing.len()
                ),
            });
        };
        match target {
            Some(ei) => {
                if claimed[ei] {
                    return Err(MergeError {
                        kind: "duplicate_id",
                        message: format!("同一批里有两个包的表盘 id 相同（{wid}）"),
                    });
                }
                claimed[ei] = true;
                if existing[ei].sha == inc.sha {
                    decided.push((ii, PkgAction::Unchanged, Some(ei)));
                } else {
                    // 版本闸门按**这个包自己**上次的版本比；没有按包版本的老数据退回整行版本
                    let old_ver = existing[ei]
                        .bin_version
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .or(current_version)
                        .unwrap_or("");
                    if !force
                        && !old_ver.is_empty()
                        && version_tuple(&inc.version) <= version_tuple(old_ver)
                    {
                        return Err(MergeError {
                            kind: "version_not_newer",
                            message: format!(
                                "包 {} 的版本 {} 不高于它当前的 {}；确认要替换请点「强制替换」",
                                inc.file_name, inc.version, old_ver
                            ),
                        });
                    }
                    decided.push((ii, PkgAction::Replace, Some(ei)));
                }
            }
            None => decided.push((ii, PkgAction::Add, None)),
        }
    }

    // 最终顺序：已有的包保持原槽位（被替换的也留在原位），新增包按本批文件名顺序追加
    let mut slots: Vec<SlotPlan> = Vec::with_capacity(existing.len() + incoming.len());
    let mut order: Vec<usize> = (0..existing.len()).collect();
    order.sort_by(|&a, &b| {
        existing[a]
            .sort_order
            .cmp(&existing[b].sort_order)
            .then_with(|| existing[a].id.cmp(&existing[b].id))
    });
    for ei in order {
        match decided.iter().find(|(_, _, e)| *e == Some(ei)) {
            Some((ii, action, _)) => slots.push(SlotPlan {
                action: *action,
                existing_idx: Some(ei),
                incoming_idx: Some(*ii),
                sort_order: 0,
            }),
            None => slots.push(SlotPlan {
                action: PkgAction::Kept,
                existing_idx: Some(ei),
                incoming_idx: None,
                sort_order: 0,
            }),
        }
    }
    for (ii, action, ei) in &decided {
        if ei.is_none() {
            slots.push(SlotPlan {
                action: *action,
                existing_idx: None,
                incoming_idx: Some(*ii),
                sort_order: 0,
            });
        }
    }
    for (i, s) in slots.iter_mut().enumerate() {
        s.sort_order = i as i64;
    }

    // 整批内容都已经在这一行里了 → 重复上传（没有替换、也没有新增）
    if slots
        .iter()
        .all(|s| matches!(s.action, PkgAction::Kept | PkgAction::Unchanged))
    {
        return Err(MergeError {
            kind: "duplicate",
            message: format!(
                "这批包和 {} 完全相同，无需重复上传",
                current_version.unwrap_or("当前版本")
            ),
        });
    }

    // 整行版本 = 各包版本的最大值（买家端「可更新」另按内容指纹判断，不受影响）
    let mut max_ver = String::new();
    for s in &slots {
        let mut v = match (s.existing_idx, s.incoming_idx) {
            (Some(ei), Some(ii)) => match s.action {
                PkgAction::Replace | PkgAction::Unchanged => incoming[ii].version.clone(),
                _ => existing[ei].bin_version.clone().unwrap_or_default(),
            },
            (Some(ei), None) => existing[ei].bin_version.clone().unwrap_or_default(),
            (None, Some(ii)) => incoming[ii].version.clone(),
            _ => String::new(),
        };
        if v.trim().is_empty() {
            v = current_version.unwrap_or("").to_string();
        }
        if !v.trim().is_empty()
            && (max_ver.is_empty() || version_tuple(&v) > version_tuple(&max_ver))
        {
            max_ver = v;
        }
    }
    if max_ver.is_empty() {
        max_ver = current_version.unwrap_or("1.0.0").to_string();
    }

    Ok(MergePlan { slots, version: max_ver })
}

/// 已加密、待落库的一个包。
struct EncPart {
    file_id: String,
    storage: std::path::PathBuf,
    enc: crate::crypto::EncryptedFile,
    sha_cipher: String,
}

fn insert_file_row(
    tx: &rusqlite::Transaction<'_>,
    version_id: i64,
    enc: &EncPart,
    part: &UploadPart,
    info: &crate::bin_inspect::BinInfo,
    sort_order: i64,
) -> ApiResult<()> {
    tx.execute(
        "INSERT INTO files(id, version_id, file_name, size, sha256_plain, sha256_cipher,
             storage_path, file_nonce, cek_wrapped, cek_wrap_nonce, created_at, sort_order,
             watchface_id, photo_album, bin_version)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            enc.file_id,
            version_id,
            part.fname,
            part.bytes.len() as i64,
            info.sha256,
            enc.sha_cipher,
            enc.storage.to_string_lossy(),
            &enc.enc.file_nonce[..],
            &enc.enc.cek_wrapped,
            &enc.enc.cek_wrap_nonce[..],
            now(),
            sort_order,
            info.watchface_id,
            info.photo_album as i64,
            info.version_label
        ],
    )
    .map_err(ApiError::from)?;
    Ok(())
}

/// POST /admin/versions/upload —— 上传即加密。
/// **一次请求可以带多个 `file`**：同一设备因内存限制拆成两个独立表盘时，
/// 它们同属一个版本（一个购买链接），按**文件名自然排序**依次安装。
///
/// 落库规则（2026-10-17 起）：按**表盘 id** 认包，只替换上传上来的那一个包，
/// 没上传的包原样保留；没有命中任何现有包 id 的作为新包追加。
/// 版本闸门也按包比（用 `files.bin_version`，老数据见 `backfill.rs`）；
/// 整行版本取各包版本的最大值。`force=true` 可强制替换同版本/旧版本内容。
pub async fn upload_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let (mut parts, fields) = read_upload(&mut multipart, true).await?;
    // 安装顺序 = 文件名自然排序（用户约定：文件名决定顺序）
    parts.sort_by(|a, b| natural_key(&a.fname).cmp(&natural_key(&b.fname)));

    let mut infos = Vec::with_capacity(parts.len());
    for p in &parts {
        let info = crate::bin_inspect::inspect(&p.bytes)
            .map_err(|e| ApiError::bad_request(format!("{}: {e}", p.fname)))?;
        infos.push(info);
    }
    // 上传入口就拦住「装不上的包」：App 安装前用 corelib 解析表盘 id，只认 9/12 位；
    // 10/11 位或全零会被判成 invalid watchface id。空 id 仍放行（老包/特殊工程的历史兼容）。
    for (p, info) in parts.iter().zip(infos.iter()) {
        let wid = info.watchface_id.trim();
        if wid.is_empty() || valid_watchface_id_strict(wid) {
            continue;
        }
        let reason = if wid == "000000000000" {
            "是全零（导出时没有生成表盘 id）".to_string()
        } else {
            format!(
                "是 {} 位（{}），不是合法的 9/12 位字母数字",
                wid.chars().count(),
                wid
            )
        };
        return Err(ApiError::bad_request(format!(
            "包 {} 的表盘 id {}；这种包装到手环上会报 invalid watchface id、装不上。\
             请在设计工具里重新导出（表盘 id 应为 9 或 12 位）后再上传。",
            p.fname, reason
        )));
    }
    let info = infos[0].clone();
    let fname = parts[0].fname.clone();
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
    let (vprod, vname, vgroup, _vwfid) = vrow.ok_or_else(|| ApiError::bad_request("设备行不存在"))?;
    if vprod != product_id {
        return Err(ApiError::bad_request("设备行不属于该资源"));
    }

    // 同一批的每个包都必须归到这一行（否则买家装了不配套的两个包）
    for (det, p) in dets.iter().zip(parts.iter()).skip(1) {
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
    }
    // 两个包是「两个独立表盘」——表盘 id 不能相同，否则设备上会互相顶掉、留档也会覆盖。
    // 按 id 匹配替换之后，这一条比原来更要紧：同 id 的两个包会指向同一行。
    {
        let mut seen: HashSet<String> = HashSet::new();
        let mut invalid = 0usize;
        for (i, p) in parts.iter().enumerate() {
            let wid = infos[i].watchface_id.trim();
            if wid.is_empty() || wid == "000000000000" {
                invalid += 1;
                continue;
            }
            if !seen.insert(wid.to_string()) {
                return Err(ApiError::bad_request(format!(
                    "同一批的两个包表盘 id 相同（{wid}）：设备上会互相覆盖、编辑留档也会丢一个。
                     拆包时每个包要生成各自的表盘 id（{}）。",
                    p.fname
                )));
            }
        }
        if invalid > 1 {
            return Err(ApiError::bad_request(format!(
                "同一批有 {invalid} 个包没有可识别的表盘 id，认不出它们各自是哪个表盘。
                 请重新导出带表盘 id 的 .bin，或一次只传一个无 id 的包。"
            )));
        }
    }

    // 当前版本行（一个设备行只保留一条最新版本）
    let current: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, version FROM versions WHERE variant_id = ?1 ORDER BY id DESC LIMIT 1",
            [&variant_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    // 这一行现有的全部包：按包 id 判断「替换哪一个」，没上传的原样保留
    let mut existing: Vec<ExistingFile> = Vec::new();
    if let Some((version_id, _)) = &current {
        let mut stmt = conn
            .prepare(
                "SELECT id, file_name, sha256_plain, storage_path, sort_order,
                        watchface_id, bin_version
                 FROM files WHERE version_id = ?1 ORDER BY sort_order, id",
            )
            .map_err(ApiError::from)?;
        existing = stmt
            .query_map([version_id], |r| {
                Ok(ExistingFile {
                    id: r.get(0)?,
                    file_name: r.get(1)?,
                    sha: r.get(2)?,
                    storage_path: r.get(3)?,
                    sort_order: r.get(4)?,
                    watchface_id: r.get(5)?,
                    bin_version: r.get(6)?,
                })
            })
            .map_err(ApiError::from)?
            .filter_map(|r| r.ok())
            .collect();
    }

    let incoming: Vec<IncomingFile> = parts
        .iter()
        .zip(infos.iter())
        .map(|(p, i)| IncomingFile {
            file_name: p.fname.clone(),
            sha: i.sha256.clone(),
            watchface_id: i.watchface_id.trim().to_string(),
            version: i.version_label.clone(),
        })
        .collect();
    let cur_ver = current.as_ref().map(|(_, v)| v.as_str());
    let plan = plan_merge(&existing, &incoming, cur_ver, force).map_err(|e| match e.kind {
        "duplicate" => ApiError::conflict("duplicate", e.message),
        "version_not_newer" => ApiError::conflict("version_not_newer", e.message),
        _ => ApiError::bad_request(e.message),
    })?;

    // 只加密真正要落盘的新内容（替换/新增）；未变的包不重新加密
    let mut encs: Vec<Option<EncPart>> = (0..parts.len()).map(|_| None).collect();
    for slot in &plan.slots {
        let Some(ii) = slot.incoming_idx else { continue };
        if !matches!(slot.action, PkgAction::Replace | PkgAction::Add) {
            continue;
        }
        let p = &parts[ii];
        let enc = crate::crypto::encrypt_file(&state.kek, &p.bytes)?;
        let file_id = new_id("f_");
        let storage = state.cipher_dir.join(format!("{file_id}.enc"));
        std::fs::write(&storage, &enc.cipher)
            .map_err(|e| ApiError::internal(format!("写密文失败: {e}")))?;
        let sha_cipher = crate::crypto::sha256_hex(&enc.cipher);
        encs[ii] = Some(EncPart { file_id, storage, enc, sha_cipher });
    }
    let canvas = info.canvas.map(|(w, h)| format!("{w}x{h}"));

    let tx = conn.unchecked_transaction().map_err(ApiError::from)?;
    let version_id = if let Some((vid, _)) = &current {
        tx.execute(
            "UPDATE versions SET version = ?2 WHERE id = ?1",
            params![vid, &plan.version],
        )
        .map_err(ApiError::from)?;
        *vid
    } else {
        tx.execute(
            "INSERT INTO versions(variant_id, version, notes, is_latest, bin_fmt_version,
                 bin_style_count, bin_canvas, bin_name, created_at)
             VALUES(?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8)",
            params![
                variant_id,
                &plan.version,
                fields.get("notes").cloned().filter(|s| !s.is_empty()),
                info.fmt_version,
                info.style_count as i64,
                canvas,
                info.name,
                now()
            ],
        )
        .map_err(ApiError::from)?;
        tx.last_insert_rowid()
    };

    let mut old_paths: Vec<String> = Vec::new();
    let mut merged_files: Vec<Value> = Vec::with_capacity(plan.slots.len());
    for slot in &plan.slots {
        let (file_id, file_name, sha, wfid, version_label, action) = match slot.action {
            PkgAction::Kept => {
                let e = &existing[slot.existing_idx.expect("kept 缺现有包")];
                tx.execute(
                    "UPDATE files SET sort_order = ?2 WHERE id = ?1",
                    params![&e.id, slot.sort_order],
                )
                .map_err(ApiError::from)?;
                (
                    e.id.clone(),
                    e.file_name.clone(),
                    e.sha.clone(),
                    e.watchface_id.clone().unwrap_or_default(),
                    e.bin_version.clone().unwrap_or_default(),
                    "kept",
                )
            }
            PkgAction::Unchanged => {
                let e = &existing[slot.existing_idx.expect("unchanged 缺现有包")];
                let ii = slot.incoming_idx.expect("unchanged 缺上传包");
                tx.execute(
                    "UPDATE files SET sort_order = ?2, file_name = ?3, bin_version = ?4 WHERE id = ?1",
                    params![&e.id, slot.sort_order, parts[ii].fname, infos[ii].version_label],
                )
                .map_err(ApiError::from)?;
                (
                    e.id.clone(),
                    parts[ii].fname.clone(),
                    infos[ii].sha256.clone(),
                    infos[ii].watchface_id.trim().to_string(),
                    infos[ii].version_label.clone(),
                    "unchanged",
                )
            }
            PkgAction::Replace => {
                let e = &existing[slot.existing_idx.expect("replace 缺现有包")];
                let ii = slot.incoming_idx.expect("replace 缺上传包");
                let enc = encs[ii].as_ref().expect("replace 缺密文");
                old_paths.push(e.storage_path.clone());
                tx.execute("DELETE FROM files WHERE id = ?1", [&e.id])
                    .map_err(ApiError::from)?;
                insert_file_row(&tx, version_id, enc, &parts[ii], &infos[ii], slot.sort_order)?;
                (
                    enc.file_id.clone(),
                    parts[ii].fname.clone(),
                    infos[ii].sha256.clone(),
                    infos[ii].watchface_id.trim().to_string(),
                    infos[ii].version_label.clone(),
                    "replaced",
                )
            }
            PkgAction::Add => {
                let ii = slot.incoming_idx.expect("add 缺上传包");
                let enc = encs[ii].as_ref().expect("add 缺密文");
                insert_file_row(&tx, version_id, enc, &parts[ii], &infos[ii], slot.sort_order)?;
                (
                    enc.file_id.clone(),
                    parts[ii].fname.clone(),
                    infos[ii].sha256.clone(),
                    infos[ii].watchface_id.trim().to_string(),
                    infos[ii].version_label.clone(),
                    "added",
                )
            }
        };
        merged_files.push(json!({
            "fileId": file_id,
            "fileName": file_name,
            "sha256Plain": sha,
            "watchfaceId": wfid,
            "version": version_label,
            "action": action,
            "sort": slot.sort_order,
        }));
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

    let replaced = current.as_ref().map(|(_, v)| v.clone());
    Ok(Json(json!({
        "ok": true,
        "variantId": variant_id,
        "variantName": vname,
        "createdRow": created_row,
        "version": plan.version,
        "replacedVersion": replaced,
        "forced": force,
        "count": merged_files.len(),
        // 合并后这一行的全部包（按安装顺序），action = kept / unchanged / replaced / added
        "files": merged_files,
        "file": merged_files.first().cloned().unwrap_or(Value::Null),
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
                    if !sum.replayed.is_empty() {
                        println!(
                            "[afdian] ⚠️ 本轮拒绝 {} 笔订单重放（权益已归属其他账号）：{:?}",
                            sum.replayed.len(),
                            sum.replayed
                        );
                    }
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

/// 「过程事件」：链路中间态或部署自检探针，不代表这次操作最终失败。
///
/// 为什么单列：上线首日 `lost_auto`（掉线后自动重连）一条码就占了诊断量的一半、
/// 24 台设备里 18 台都有——它和「装不上」混在同一个 `kind='fail'` 里，首页那条
/// 「N 条客户端失败」会一直虚高，真正的失败反而被淹没。原始数据照旧保留，
/// 只是统计口径分开（管理页勾选「包含过程事件」仍能看到全部）。
///
/// 判定与 SQL 里的 `PROGRESS_EVENT_SQL` 必须一致。
pub fn is_progress_event(step: &str, code: &str) -> bool {
    matches!(code, "lost_auto" | "timeout15s" | "lost")
        || step.starts_with("scrub-check")
        || step.starts_with("post-hardening-check")
}

/// 与 [`is_progress_event`] 同一套判定的 SQL 片段（stats 用聚合查询，不能逐行回 Rust）。
const PROGRESS_EVENT_SQL: &str = "(COALESCE(code,'') IN ('lost_auto','timeout15s','lost') \
     OR step LIKE 'scrub-check%' OR step LIKE 'post-hardening-check%')";

pub async fn stats(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let conn = state.conn();
    let one = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let since7d = "ts >= strftime('%s','now') - 7*86400";
    let fail_sql = format!(
        "SELECT COUNT(*) FROM client_events WHERE kind='fail' AND {since7d} AND NOT {PROGRESS_EVENT_SQL}"
    );
    // 按「台设备」而不是「条数」报：一台手机反复重试 8 次，不等于 8 个人装不上。
    let fail_devices_sql = format!(
        "SELECT COUNT(DISTINCT COALESCE(NULLIF(device_fp,''), NULLIF(ip,''), 'unknown')) \
         FROM client_events WHERE kind='fail' AND {since7d} AND NOT {PROGRESS_EVENT_SQL}"
    );
    let progress_sql =
        format!("SELECT COUNT(*) FROM client_events WHERE kind='fail' AND {since7d} AND {PROGRESS_EVENT_SQL}");
    Ok(Json(json!({
        "resources": one("SELECT COUNT(*) FROM products"),
        "deviceRows": one("SELECT COUNT(*) FROM variants"),
        "packages": one("SELECT COUNT(*) FROM files"),
        "subjects": one("SELECT COUNT(*) FROM subjects"),
        "activeLicenses": one("SELECT COUNT(*) FROM licenses WHERE status='active'"),
        "activeActivations": one("SELECT COUNT(*) FROM activations WHERE status='active'"),
        "downloads": one("SELECT COUNT(*) FROM download_logs"),
        // 近 7 天客户端**真失败**打点：>0 时首页会亮一条（以前这类信号在 App 里就没了）。
        // 过程事件（自动重连/15 秒中间态/自检探针）单列成 progressEvents7d，不再撑大失败数。
        "failEvents7d": one(&fail_sql),
        "failDevices7d": one(&fail_devices_sql),
        "progressEvents7d": one(&progress_sql),
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
    /// `count`（默认，按次数）| `time`（按最近发生时间）
    #[serde(default)]
    pub sort: Option<String>,
    /// 1 = 连过程事件一起返回（自动重连 / 15 秒中间态 / 自检探针）；默认 0 = 只看真失败。
    /// 用 i64 而不是 bool：前端拼查询串时 `1/0` 最省事，serde_urlencoded 对 bool 只认 true/false。
    #[serde(default)]
    pub include_progress: Option<i64>,
}

/// 一条打点（含「是谁」的字段）。
struct EventRow {
    ts: i64,
    kind: String,
    step: String,
    code: String,
    message: String,
    device_model: String,
    app_version: String,
    device_fp: String,
    subject: String,
    ip: String,
    phone_model: String,
    phone_os: String,
}

impl EventRow {
    /// 区分「谁」的键：优先用 App 安装时生成的设备指纹（同一台手机稳定、不同手机必不同）；
    /// 老版本没上报指纹时退回出口 IP（CGNAT 下可能把两个人并成一个，但总比没有好）。
    fn device_key(&self) -> String {
        if !self.device_fp.is_empty() {
            format!("fp:{}", self.device_fp)
        } else if !self.ip.is_empty() {
            format!("ip:{}", self.ip)
        } else {
            "unknown".to_string()
        }
    }

    fn phone(&self) -> String {
        match (self.phone_model.is_empty(), self.phone_os.is_empty()) {
            (false, false) => format!("{} · {}", self.phone_model, self.phone_os),
            (false, true) => self.phone_model.clone(),
            (true, false) => self.phone_os.clone(),
            _ => String::new(),
        }
    }

    /// 单行显示用的「人」标签：设备指纹前 8 位（或 IP）+ 手机型号 + 账号。
    fn who(&self) -> String {
        let dev = if !self.device_fp.is_empty() {
            format!(
                "设备 {}",
                self.device_fp.chars().take(8).collect::<String>()
            )
        } else if !self.ip.is_empty() {
            format!("IP {}", self.ip)
        } else {
            "未知设备".to_string()
        };
        let mut parts = vec![dev];
        let phone = self.phone();
        if !phone.is_empty() {
            parts.push(phone);
        }
        if !self.subject.is_empty() {
            parts.push(format!("账号 {}", self.subject));
        }
        parts.join(" · ")
    }
}

struct StepAgg<'a> {
    count: i64,
    devices: HashSet<String>,
    /// rows 已按时间倒序，第一次遇到的就是最近一条
    latest: &'a EventRow,
}

struct DeviceAgg<'a> {
    count: i64,
    first_ts: i64,
    last_ts: i64,
    latest: &'a EventRow,
    steps: HashMap<(String, String), i64>,
}

/// GET /admin/events —— App 上报的失败打点（近 N 天）。
///
/// 回答三个问题：① 该先修哪个（按「步骤 + 错误码」聚合）② 到底报了什么（最近几条原文）
/// ③ **是谁报的**（按设备指纹聚合；老数据没有指纹时退回 IP）。
/// `sort=count` 按次数（默认，适合「先修最多的」）；`sort=time` 按最近发生时间（适合「看最新问题」）。
/// 聚合在 Rust 里做：量小（个人项目），比一条花哨的 SQL 好读、好改。
pub async fn events_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    require_admin(&state, &headers)?;
    let days = q.days.unwrap_or(7).clamp(1, 90);
    let limit = q.limit.unwrap_or(30).clamp(1, 200) as usize;
    let sort = if q.sort.as_deref() == Some("time") {
        "time"
    } else {
        "count"
    };
    let include_progress = q.include_progress.unwrap_or(0) != 0;
    let since = now() - days * 86_400;
    let conn = state.conn();

    let total_all: i64 = conn.query_row(
        "SELECT COUNT(*) FROM client_events WHERE ts >= ?1",
        [since],
        |r| r.get(0),
    )?;
    // 过程事件的总数要单独报给页面：默认折叠时用户得知道「藏了多少条」。
    let progress_total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM client_events WHERE ts >= ?1 AND {PROGRESS_EVENT_SQL}"),
        [since],
        |r| r.get(0),
    )?;
    let total = if include_progress {
        total_all
    } else {
        total_all - progress_total
    };

    // 5000 条足够覆盖个人项目一个月的量；聚合只在这批里做（更早的只影响分页外的长尾）。
    let mut stmt = conn.prepare(
        "SELECT ts, COALESCE(kind,''), step, COALESCE(code,''), COALESCE(message,''),
                COALESCE(device_model,''), COALESCE(app_version,''), COALESCE(device_fp,''),
                COALESCE(subject_id,''), COALESCE(ip,''), COALESCE(phone_model,''), COALESCE(phone_os,'')
         FROM client_events WHERE ts >= ?1 ORDER BY ts DESC LIMIT 5000",
    )?;
    let mut rows: Vec<EventRow> = stmt
        .query_map([since], |r| {
            Ok(EventRow {
                ts: r.get(0)?,
                kind: r.get(1)?,
                step: r.get(2)?,
                code: r.get(3)?,
                message: r.get(4)?,
                device_model: r.get(5)?,
                app_version: r.get(6)?,
                device_fp: r.get(7)?,
                subject: r.get(8)?,
                ip: r.get(9)?,
                phone_model: r.get(10)?,
                phone_os: r.get(11)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    // 默认只看真失败：自动重连/15 秒中间态会把「按设备」「最近原文」全刷满。
    // 原始数据不删，勾选「包含过程事件」随时能看。
    if !include_progress {
        rows.retain(|r| !is_progress_event(&r.step, &r.code));
    }

    let mut step_map: HashMap<(String, String), StepAgg> = HashMap::new();
    let mut device_map: HashMap<String, DeviceAgg> = HashMap::new();
    for r in &rows {
        let se = step_map
            .entry((r.step.clone(), r.code.clone()))
            .or_insert_with(|| StepAgg {
                count: 0,
                devices: HashSet::new(),
                latest: r,
            });
        se.count += 1;
        se.devices.insert(r.device_key());

        let de = device_map
            .entry(r.device_key())
            .or_insert_with(|| DeviceAgg {
                count: 0,
                first_ts: r.ts,
                last_ts: r.ts,
                latest: r,
                steps: HashMap::new(),
            });
        de.count += 1;
        de.first_ts = de.first_ts.min(r.ts);
        de.last_ts = de.last_ts.max(r.ts);
        *de.steps.entry((r.step.clone(), r.code.clone())).or_insert(0) += 1;
    }

    let mut by_step: Vec<((String, String), StepAgg)> = step_map.into_iter().collect();
    if sort == "time" {
        by_step.sort_by(|a, b| {
            b.1.latest
                .ts
                .cmp(&a.1.latest.ts)
                .then(b.1.count.cmp(&a.1.count))
        });
    } else {
        by_step.sort_by(|a, b| {
            b.1.count
                .cmp(&a.1.count)
                .then(b.1.latest.ts.cmp(&a.1.latest.ts))
        });
    }

    let mut by_device: Vec<(String, DeviceAgg)> = device_map.into_iter().collect();
    if sort == "time" {
        by_device.sort_by(|a, b| b.1.last_ts.cmp(&a.1.last_ts));
    } else {
        by_device.sort_by(|a, b| {
            b.1.count
                .cmp(&a.1.count)
                .then(b.1.last_ts.cmp(&a.1.last_ts))
        });
    }

    Ok(Json(json!({
        "days": days,
        "since": since,
        "total": total,
        "sort": sort,
        "includeProgress": include_progress,
        "progressTotal": progress_total,
        "devices": by_device.len(),
        "byStep": by_step.iter().take(40).map(|((step, code), a)| json!({
            "step": step, "code": code, "count": a.count,
            "deviceCount": a.devices.len(), "lastTs": a.latest.ts,
            "progress": is_progress_event(step, code),
            "message": a.latest.message, "deviceModel": a.latest.device_model,
            "appVersion": a.latest.app_version, "phone": a.latest.phone(),
            "deviceFp": a.latest.device_fp, "subject": a.latest.subject,
            "ip": a.latest.ip, "who": a.latest.who(),
        })).collect::<Vec<_>>(),
        "byDevice": by_device.iter().take(40).map(|(key, a)| {
            let mut steps: Vec<(&String, &String, &i64)> = a.steps.iter()
                .map(|((s, c), n)| (s, c, n)).collect();
            steps.sort_by(|x, y| y.2.cmp(x.2));
            json!({
                "key": key, "count": a.count,
                "firstTs": a.first_ts, "lastTs": a.last_ts,
                "deviceFp": a.latest.device_fp, "subject": a.latest.subject,
                "ip": a.latest.ip, "phone": a.latest.phone(),
                "deviceModel": a.latest.device_model, "appVersion": a.latest.app_version,
                "lastStep": a.latest.step, "lastCode": a.latest.code,
                "lastMessage": a.latest.message, "who": a.latest.who(),
                "steps": steps.iter().take(6).map(|(s, c, n)| json!({
                    "step": s, "code": c, "count": n,
                })).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "recent": rows.iter().take(limit).map(|r| json!({
            "ts": r.ts, "kind": r.kind, "step": r.step, "code": r.code,
            "progress": is_progress_event(&r.step, &r.code),
            "message": r.message, "deviceModel": r.device_model,
            "appVersion": r.app_version, "deviceFp": r.device_fp,
            "subject": r.subject, "ip": r.ip, "phone": r.phone(), "who": r.who(),
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
    let (parts, fields) = read_upload(&mut multipart, true).await?;
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
    // APK 由 nginx 直接静态分发（见 deploy/nginx-api.conf）：服务以 wf + UMask=0027 运行，
    // 新文件默认 640 wf:wf，nginx(www-data) 读不到 → 显式给只读权限，否则新版下载 404。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o644));
    }
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
    use super::*;

    fn ev(fp: &str, ip: &str, subject: &str, phone: &str, os: &str) -> EventRow {
        EventRow {
            ts: 0,
            kind: "fail".into(),
            step: "connect".into(),
            code: "failed".into(),
            message: String::new(),
            device_model: String::new(),
            app_version: String::new(),
            device_fp: fp.into(),
            subject: subject.into(),
            ip: ip.into(),
            phone_model: phone.into(),
            phone_os: os.into(),
        }
    }

    #[test]
    fn event_identity_prefers_fingerprint_over_ip() {
        // 两台不同手机共用出口 IP（CGNAT）：必须靠设备指纹区分成两个人。
        // 2026-10-17 真实踩过：两个买家同一分钟在同一个段里报错，按 IP 看像同一个人。
        let a = ev("aaaaaaaaaaaaaaaa", "1.1.1.1", "", "", "");
        let b = ev("bbbbbbbbbbbbbbbb", "1.1.1.1", "", "", "");
        assert_ne!(a.device_key(), b.device_key());
        assert!(a.who().starts_with("设备 aaaaaaaa"));

        // 老版本没上报指纹 → 退回 IP（至少还能归并）
        let old = ev("", "2.2.2.2", "", "", "");
        assert_eq!(old.device_key(), "ip:2.2.2.2");
        assert!(old.who().contains("IP 2.2.2.2"));

        // 手机型号/系统合并显示（第一次连不上时手环型号为空，靠它认人）
        let p = ev(
            "cccccccccccccccc",
            "",
            "s_abc",
            "OnePlus PJZ110",
            "Android 17 (SDK 37)",
        );
        assert_eq!(p.phone(), "OnePlus PJZ110 · Android 17 (SDK 37)");
        assert!(p.who().contains("OnePlus PJZ110"));
        assert!(p.who().contains("账号 s_abc"));
    }

    fn ex(id: &str, sha: &str, sort: i64, wid: &str, ver: &str) -> ExistingFile {
        ExistingFile {
            id: id.into(),
            file_name: format!("{id}.bin"),
            sha: sha.into(),
            storage_path: format!("/tmp/{id}.enc"),
            sort_order: sort,
            watchface_id: Some(wid.into()),
            bin_version: Some(ver.into()),
        }
    }

    fn inc(name: &str, sha: &str, wid: &str, ver: &str) -> IncomingFile {
        IncomingFile {
            file_name: name.into(),
            sha: sha.into(),
            watchface_id: wid.into(),
            version: ver.into(),
        }
    }

    /// 用户的实际场景：A=1.0.2 不动，B 从 1.0.1 升到 1.0.2，只传 B。
    #[test]
    fn partial_update_only_replaces_matched_package() {
        let existing = vec![
            ex("a", "sha-a", 0, "111111111111", "1.0.2"),
            ex("b", "sha-b", 1, "222222222222", "1.0.1"),
        ];
        let incoming = vec![inc("b-new.bin", "sha-b2", "222222222222", "1.0.2")];
        let plan = plan_merge(&existing, &incoming, Some("1.0.2"), false).unwrap();

        assert_eq!(plan.slots.len(), 2);
        assert_eq!(plan.slots[0].action, PkgAction::Kept);
        assert_eq!(plan.slots[1].action, PkgAction::Replace);
        assert_eq!(plan.slots[1].existing_idx, Some(1));
        // 整行版本 = 各包最大值，仍然是 1.0.2
        assert_eq!(plan.version, "1.0.2");
    }

    /// 同版本号但内容变了：默认拦下，force 才放行（与旧行为一致）
    #[test]
    fn same_version_new_content_needs_force() {
        let existing = vec![ex("b", "sha-b", 0, "222222222222", "1.0.2")];
        let incoming = vec![inc("b.bin", "sha-b2", "222222222222", "1.0.2")];
        let err = plan_merge(&existing, &incoming, Some("1.0.2"), false).unwrap_err();
        assert_eq!(err.kind, "version_not_newer");

        let plan = plan_merge(&existing, &incoming, Some("1.0.2"), true).unwrap();
        assert_eq!(plan.slots[0].action, PkgAction::Replace);
    }

    /// 原样重传：不重新加密，也不允许整批空转
    #[test]
    fn unchanged_batch_is_duplicate() {
        let existing = vec![
            ex("a", "sha-a", 0, "111111111111", "1.0.2"),
            ex("b", "sha-b", 1, "222222222222", "1.0.2"),
        ];
        let incoming = vec![
            inc("a.bin", "sha-a", "111111111111", "1.0.2"),
            inc("b.bin", "sha-b", "222222222222", "1.0.2"),
        ];
        let err = plan_merge(&existing, &incoming, Some("1.0.2"), true).unwrap_err();
        assert_eq!(err.kind, "duplicate");
    }

    /// 只传没见过的 id → 当新包追加，不碰已有包
    #[test]
    fn unknown_id_appends_new_package() {
        let existing = vec![ex("a", "sha-a", 0, "111111111111", "1.0.2")];
        let incoming = vec![inc("c.bin", "sha-c", "333333333333", "1.0.3")];
        let plan = plan_merge(&existing, &incoming, Some("1.0.2"), false).unwrap();

        assert_eq!(plan.slots.len(), 2);
        assert_eq!(plan.slots[0].action, PkgAction::Kept);
        assert_eq!(plan.slots[1].action, PkgAction::Add);
        assert_eq!(plan.slots[1].sort_order, 1);
        assert_eq!(plan.version, "1.0.3");
    }

    /// 没有表盘 id：首次上传允许；单包行按替换处理；多包行拒绝（避免误伤）
    #[test]
    fn missing_id_single_package_ok_multi_rejected() {
        let first = plan_merge(
            &[],
            &[inc("new.bin", "sha-new", "000000000000", "1.0.0")],
            None,
            false,
        )
        .unwrap();
        assert_eq!(first.slots[0].action, PkgAction::Add);

        let single = vec![ex("a", "sha-a", 0, "111111111111", "1.0.2")];
        let inc_old = vec![inc("a.bin", "sha-a2", "000000000000", "1.0.3")];
        let plan = plan_merge(&single, &inc_old, Some("1.0.2"), false).unwrap();
        assert_eq!(plan.slots[0].action, PkgAction::Replace);

        let multi = vec![
            ex("a", "sha-a", 0, "111111111111", "1.0.2"),
            ex("b", "sha-b", 1, "222222222222", "1.0.2"),
        ];
        let err = plan_merge(&multi, &inc_old, Some("1.0.2"), false).unwrap_err();
        assert_eq!(err.kind, "no_id");
    }

    /// 替换留在原槽位，新包追加到末尾；上传顺序不改变已有安装顺序
    #[test]
    fn replacement_keeps_slot_and_add_appends() {
        let existing = vec![
            ex("a", "sha-a", 0, "111111111111", "1.0.2"),
            ex("b", "sha-b", 1, "222222222222", "1.0.1"),
        ];
        let incoming = vec![
            inc("b.bin", "sha-b2", "222222222222", "1.0.2"),
            inc("c.bin", "sha-c", "333333333333", "1.0.3"),
        ];
        let plan = plan_merge(&existing, &incoming, Some("1.0.2"), true).unwrap();
        let acts: Vec<_> = plan.slots.iter().map(|s| (s.action, s.sort_order)).collect();
        assert_eq!(
            acts,
            vec![
                (PkgAction::Kept, 0),
                (PkgAction::Replace, 1),
                (PkgAction::Add, 2)
            ]
        );
    }

    /// 一起传「没变的 A + 变了的 B」：A 记为未变，不重复加密
    #[test]
    fn batch_with_one_unchanged_is_not_duplicate() {
        let existing = vec![
            ex("a", "sha-a", 0, "111111111111", "1.0.2"),
            ex("b", "sha-b", 1, "222222222222", "1.0.1"),
        ];
        let incoming = vec![
            inc("a.bin", "sha-a", "111111111111", "1.0.2"),
            inc("b.bin", "sha-b2", "222222222222", "1.0.2"),
        ];
        let plan = plan_merge(&existing, &incoming, Some("1.0.2"), false).unwrap();
        assert_eq!(plan.slots[0].action, PkgAction::Unchanged);
        assert_eq!(plan.slots[1].action, PkgAction::Replace);
    }

    #[test]
    fn progress_events_do_not_count_as_real_failures() {
        // 掉线自动重连 / 15 秒中间态 / 自检探针 → 过程事件，不撑大首页失败数
        assert!(is_progress_event("connect", "lost_auto"));
        assert!(is_progress_event("connect", "timeout15s"));
        assert!(is_progress_event("connect", "lost"));
        assert!(is_progress_event("scrub-check", "E1"));
        assert!(is_progress_event("post-hardening-check", ""));
        // 真正的失败必须留在统计里
        assert!(!is_progress_event("connect", "failed"));
        assert!(!is_progress_event("connect", "giveup45s"));
        assert!(!is_progress_event("install_paid", "link_lost"));
        assert!(!is_progress_event("paid_list", "other"));
        assert!(!is_progress_event("login", "2fa_failed"));
    }

    #[test]
    fn upload_rejects_wrong_length_watchface_ids() {
        // 合法：9/12 位字母数字
        assert!(valid_watchface_id_strict("156780069701"));
        assert!(valid_watchface_id_strict("123456789"));
        // 2026-10-17 线上踩到的两类坏包：10/11 位、全零
        assert!(!valid_watchface_id_strict("15675874000"));
        assert!(!valid_watchface_id_strict("1791000001"));
        assert!(!valid_watchface_id_strict("000000000000"));
        assert!(!valid_watchface_id_strict(""));
        assert!(!valid_watchface_id_strict("12345678_01"));
    }

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
