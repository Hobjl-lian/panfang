//! 上传时的自动识别：把 .bin 检查结果落到「设备变体 + 发布版本」上。
//!
//! 判据（每一级都可被管理端人工覆盖）：
//! 1. **设备变体**：头部表盘 id（同产品同设备族稳定）→ 画布尺寸 → 文件名标记（9P10P/1011/RW5RW6）
//! 2. **版本新旧**：头部 0x04 版本号（同一工程每次导出 +1，实测 AWOS 1.0/2.0/3.0 = 0x…00/01/02）
//! 3. **发布号**：文件夹/文件名里的 vN 优先；否则按该变体的历史步长递增
//!
//! ⚠️ 实现约束：`AppState.db` 是 `std::sync::Mutex`（不可重入）。本模块所有取数
//! 都在 `snapshot()` 里**一次性**完成，之后纯内存计算。

use crate::bin_inspect::BinInfo;
use crate::state::{ApiResult, AppState};
use rusqlite::OptionalExtension;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    /// 命中的变体（None = 需要管理端指定）
    pub variant_id: Option<String>,
    pub variant_name: Option<String>,
    /// 变体匹配依据：watchface_id（表盘 id）/ canvas（画布尺寸）/ canvas-name（型号名）
    pub variant_match: Option<String>,
    /// 建议的发布版本号
    pub version: Option<String>,
    /// 版本建议依据（给管理端看）
    pub version_reason: Option<String>,
    /// 与已有记录的对比结论：new / duplicate / same_version_diff_content / older / first
    pub relation: String,
    /// 同一变体已存在的同名版本（提示用）
    pub existing_version: Option<String>,
    /// 头部 0x04 版本号（同一工程递增；越低越旧）
    pub fmt_version: Option<String>,
    /// 命中的设备组（资源里还没有对应行时，上传会自动建行）
    pub device_group_id: Option<String>,
    pub device_group_name: Option<String>,
    /// 是否需要在资源里新建该设备行
    pub will_create: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum VersionStep {
    Major,
    Minor,
}

struct Snapshot {
    chosen: Option<(String, String, String)>, // (variant_id, name, 依据)
    /// 命中的设备组（variant 不存在时用它建行）
    group: Option<(String, String)>,
    dup_version: Option<String>,
    /// 新 → 旧（最多 4 条）
    history: Vec<(String, Option<String>)>,
    step: VersionStep,
}

/// 机型名规范化：小写、去空格（"小米手环10 Pro" → "小米手环10pro"）
fn norm_model(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// 解析变体的 device_models（JSON 数组）为规范化机型名
fn models_of(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json)
        .unwrap_or_default()
        .iter()
        .map(|m| norm_model(m))
        .collect()
}

fn parse_fmt_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// 版本号递增：`3.0` → `4.0`（Major）；`3.1` → `3.2`（Minor）。
fn bump_version(prev: &str, step: VersionStep) -> String {
    let parts: Vec<u32> = prev
        .split('.')
        .map(|p| p.trim().parse::<u32>().unwrap_or(0))
        .collect();
    match step {
        VersionStep::Major => format!("{}.0", parts.first().copied().unwrap_or(0) + 1),
        VersionStep::Minor => {
            let major = parts.first().copied().unwrap_or(1);
            let minor = parts.get(1).copied().unwrap_or(0) + 1;
            format!("{major}.{minor}")
        }
    }
}

/// 单次持锁取全所需数据。
fn snapshot(
    state: &AppState,
    product_id: &str,
    file_name: &str,
    info: &BinInfo,
) -> ApiResult<Snapshot> {
    let conn = state.conn();
    let mut stmt = conn.prepare(
        "SELECT id, name, device_models, watchface_id, screen_w, screen_h FROM variants WHERE product_id = ?1",
    )?;
    let variants: Vec<(String, String, String, Option<String>, Option<i64>, Option<i64>)> = stmt
        .query_map([product_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // ---- 变体匹配
    let mut chosen: Option<(String, String, String)> = None;
    let wid = info.watchface_id.trim();
    if !wid.is_empty() && wid != "000000000000" {
        if let Some(v) = variants.iter().find(|v| v.3.as_deref() == Some(wid)) {
            chosen = Some((v.0.clone(), v.1.clone(), "watchface_id".into()));
        }
        // 设备行上只存了排序第一个包的 id；一个设备行可能有 2 个独立表盘，
        // 第二个包的 id 只存在 files 表里 —— 只传第二个包时靠这里归位。
        if chosen.is_none() {
            let hit: Option<(String, String)> = conn
                .query_row(
                    "SELECT va.id, va.name FROM variants va
                     JOIN versions vr ON vr.variant_id = va.id
                     JOIN files f ON f.version_id = vr.id
                     WHERE va.product_id = ?1 AND f.watchface_id = ?2
                     ORDER BY vr.id DESC LIMIT 1",
                    [product_id, wid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, name)) = hit {
                chosen = Some((id, name, "watchface_id".into()));
            }
        }
    }
    // ② 画布尺寸 → 设备组（**按屏幕比对**，不依赖名字；名字只是兜底）
    if chosen.is_none() {
        if let Some((w, h)) = info.canvas {
            let hits: Vec<_> = variants
                .iter()
                .filter(|v| {
                    v.4 == Some(w as i64) && v.5 == Some(h as i64) // screen_w / screen_h
                })
                .collect();
            if hits.len() == 1 {
                let v = hits[0];
                chosen = Some((v.0.clone(), v.1.clone(), "canvas".into()));
            }
            // 多个组同屏（如合并前的手环10 与 11）：交给名字兜底
        }
    }
    // ②′ 名字兜底（型号名与画布提示完全相同）
    if chosen.is_none() {
        for dev in &info.device_hints {
            let want = norm_model(dev);
            if let Some(v) = variants
                .iter()
                .find(|v| models_of(&v.2).iter().any(|m| m == &want))
            {
                chosen = Some((v.0.clone(), v.1.clone(), "canvas-name".into()));
                break;
            }
        }
    }
    // （已删除「按文件名猜设备」的分支：文件名里的数字太容易误伤——
    //   实测把 AWOS-9P10P-v3.bin 误判成「小米手环9」。现在只认
    //   表盘 id、画布尺寸、型号名三种可靠依据。）

    // ---- 若没有命中已有行，就按设备组匹配（资源里还没建行时）
    let groups: Vec<(String, String, String, Option<i64>, Option<i64>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, name, models, screen_w, screen_h FROM device_groups ORDER BY sort",
        )?;
        let v: Vec<(String, String, String, Option<i64>, Option<i64>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .filter_map(|r| r.ok())
            .collect();
        v
    };
    let mut group: Option<(String, String)> = None;
    if chosen.is_none() {
        // 按画布尺寸（唯一）
        if let Some((w, h)) = info.canvas {
            let hits: Vec<_> = groups
                .iter()
                .filter(|g| g.3 == Some(w as i64) && g.4 == Some(h as i64))
                .collect();
            if hits.len() == 1 {
                group = Some((hits[0].0.clone(), hits[0].1.clone()));
            }
        }
        // 按型号名兜底
        if group.is_none() {
            for dev in &info.device_hints {
                let want = norm_model(dev);
                if let Some(g) = groups
                    .iter()
                    .find(|g| models_of(&g.2).iter().any(|m| m == &want))
                {
                    group = Some((g.0.clone(), g.1.clone()));
                    break;
                }
            }
        }
    }

    // ---- 历史 + 内容重复
    let mut dup_version = None;
    let mut history: Vec<(String, Option<String>)> = Vec::new();
    if let Some((vid, _, _)) = &chosen {
        let mut stmt = conn.prepare(
            "SELECT version, bin_fmt_version FROM versions WHERE variant_id = ?1 ORDER BY id DESC LIMIT 4",
        )?;
        history = stmt
            .query_map([vid], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        dup_version = conn
            .query_row(
                "SELECT v.version FROM files f JOIN versions v ON v.id = f.version_id
                 WHERE v.variant_id = ?1 AND f.sha256_plain = ?2 LIMIT 1",
                rusqlite::params![vid, info.sha256],
                |r| r.get(0),
            )
            .optional()?;
    }

    // ---- 步长：最近两次发布的「主版本」是否相同
    let step = if history.len() >= 2 {
        let a: Vec<&str> = history[1].0.split('.').collect();
        let b: Vec<&str> = history[0].0.split('.').collect();
        if a.first() == b.first() {
            VersionStep::Minor
        } else {
            VersionStep::Major
        }
    } else {
        VersionStep::Major
    };

    Ok(Snapshot {
        chosen,
        group,
        dup_version,
        history,
        step,
    })
}

/// 综合判断。
pub fn detect(
    state: &AppState,
    product_id: &str,
    file_name: &str,
    path_hint: &str,
    info: &BinInfo,
) -> ApiResult<Detection> {
    let snap = snapshot(state, product_id, file_name, info)?;
    let hint = crate::bin_inspect::version_hint(file_name, path_hint);

    let mut det = Detection {
        variant_id: snap.chosen.as_ref().map(|c| c.0.clone()),
        variant_name: snap.chosen.as_ref().map(|c| c.1.clone()),
        variant_match: snap.chosen.as_ref().map(|c| c.2.clone()),
        version: hint.clone(),
        version_reason: hint.as_ref().map(|_| "文件名/目录名".to_string()),
        relation: "first".into(),
        existing_version: None,
        fmt_version: Some(info.fmt_version.clone()),
        device_group_id: snap.group.as_ref().map(|g| g.0.clone()),
        device_group_name: snap.group.as_ref().map(|g| g.1.clone()),
        will_create: snap.chosen.is_none() && snap.group.is_some(),
    };

    // 1) 内容重复 → 沿用已有版本号
    if let Some(v) = snap.dup_version {
        det.relation = "duplicate".into();
        det.version = Some(v.clone());
        det.existing_version = Some(v);
        det.version_reason = Some("内容哈希与已有版本一致（同一文件重复上传）".into());
        return Ok(det);
    }

    // 2) 该变体首个版本
    let Some((prev_ver, prev_fmt)) = snap.history.first().cloned() else {
        det.version = det.version.or_else(|| Some("1.0".into()));
        det.version_reason = det.version_reason.or_else(|| Some("该变体首个版本".into()));
        return Ok(det);
    };

    // 3) 与最近一次上传比较头部版本号
    let cur = info.fmt_version_raw;
    let prev = prev_fmt.as_deref().and_then(parse_fmt_hex);
    match prev {
        Some(p) if cur == p => {
            det.relation = "same_version_diff_content".into();
            det.existing_version = Some(prev_ver.clone());
            det.version_reason = Some(format!(
                "头部版本号与 {prev_ver} 相同但内容不同（重构建）；如需新号建议 {}",
                bump_version(&prev_ver, snap.step)
            ));
            if det.version.is_none() {
                det.version = Some(bump_version(&prev_ver, snap.step));
            }
        }
        Some(p) if cur < p => {
            det.relation = "older".into();
            det.existing_version = Some(prev_ver.clone());
            if det.version.is_none() {
                det.version = Some(prev_ver.clone());
            }
            det.version_reason = Some(format!(
                "头部版本号 0x{cur:08X} 低于最近上传的 0x{p:08X}（旧构建）；如确要上传请手动指定版本号"
            ));
        }
        _ => {
            det.relation = "new".into();
            if det.version.is_none() {
                let next = bump_version(&prev_ver, snap.step);
                det.version = Some(next);
                det.version_reason = Some(format!("按该变体历史步长从 {prev_ver} 递增"));
            }
        }
    }
    Ok(det)
}
