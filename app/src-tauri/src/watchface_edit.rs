//! 表盘编辑模型：从 .bin / 设备信息推导「编辑器需要的数据」。
//!
//! 组成（与 AstroBox 的 capability 等价，但数据来源是我们自己的 .bin 解析）：
//! - canvas：画布尺寸 —— 优先用设备型号的屏幕尺寸（型号表见下），
//!   本地预览时退化为「背景图尺寸 / 预览图长宽比推算」
//! - styles：样式列表（名称 + 预览图 PNG + 图片数）
//! - wallpaper：相册/背景图槽位矩形 —— 取「样式里面积最大的静态元素」，
//!   其 TargetId 指向的 Image（AWOS 实测：192×490 的元素 #0 = 整画布）
//! - widgets：数据项组件（形状/数据源/九宫格位）
//!
//! 型号表来源：AstroBox 前端内置表（逐条核对过），imageFormat 全部为 6 = PNG。

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::watchface_bin::{self, BinHeader, BlockRecord, StyleSection};

/// 设备型号/代号 → (宽, 高)。
///
/// 代号表来源：设计流水线的 `xiaomi-watchface-skill/data/devices.json`（28 个型号，2026-10-06 逐条校正）。
/// 手表 S3/S4 系列实际为 **464×464**（旧表误写 466）。
pub fn device_screen(model: &str) -> Option<(u32, u32)> {
    let m = model.to_ascii_lowercase();
    // 长代号在前，避免 n65 抢先匹配 n65a/n65b/n65c
    const TABLE: &[(&str, u32, u32)] = &[
        // 手表（圆）
        ("n62s", 464, 464),
        ("n62", 464, 464),
        ("o62m", 464, 464),
        ("o62", 464, 464),
        ("o63", 464, 464),
        ("q63", 464, 464),
        ("m62", 466, 466),
        ("n60", 466, 466),
        ("q61", 480, 480),
        ("l61", 480, 480),
        ("o61", 480, 480),
        ("p62", 480, 480),
        // 红米手表（方）
        ("m65s", 390, 450),
        ("n65a", 320, 385),
        ("n65b", 410, 502),
        ("n65c", 390, 450),
        ("n65", 390, 450),
        ("o65m", 432, 514),
        ("o65", 432, 514),
        ("p65", 432, 514),
        ("l67", 280, 456),
        ("m67", 336, 480),
        ("n67", 336, 480),
        ("p67", 336, 480),
        // 手环（长条）
        ("m66", 192, 490),
        ("n66", 192, 490),
        ("o66", 212, 520),
        ("q66", 212, 520),
    ];
    for (code, w, h) in TABLE {
        if m.contains(code) {
            return Some((*w, *h));
        }
    }
    // 兼容用户手输的老式名称
    let alias = |ids: &[&str]| ids.iter().any(|id| m.contains(id));
    if alias(&["xmb8p", "xmb9p", "xmb10p"]) {
        return Some((336, 480));
    }
    if alias(&["xmb9"]) {
        return Some((192, 490));
    }
    // 手环 10 / 11 同族（212×520 跑道屏，用户 2026-10-06 确认）
    if alias(&["xmb10", "xmb11", "手环11", "手环 11", "band 11", "band11"]) {
        return Some((212, 520));
    }
    if alias(&["xmrw5", "xmrw6"]) {
        return Some((432, 514));
    }
    if alias(&["xmrw4"]) {
        return Some((390, 450));
    }
    if alias(&["xms5", "xmws5"]) {
        return Some((480, 480));
    }
    if alias(&["xmh1", "xmws3", "xmws4"]) {
        return Some((466, 466));
    }
    None
}

/// 设备屏幕：宽高 + 圆角 + 形状 —— 编辑页预览框与裁剪蒙版按它等比缩放/裁切。
///
/// 圆角取**外壳物理圆角**：设计流水线《设计稿交付映射规范》写明
/// 「336×480 族 R45 / 432×514 族 R104」，工具机型表（devices.json）里的 48/103/108
/// 是工具自己那套数值，**不是**用户看到的屏幕圆角，别混用。
/// 跑道屏 / 圆表给的是「大值」，CSS `border-radius` 会自动截到半圆 / 正圆，不用自己 clamp。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
    /// 外壳物理圆角（px）
    pub radius: u32,
    /// `circle`（圆表）| `run`（跑道屏）| `square`（方屏）
    pub shape: &'static str,
}

/// 型号 → 屏幕信息（型号不在表里时返回 None，调用方可用 .bin 推出的尺寸兜底）。
pub fn device_screen_info(model: &str) -> Option<ScreenInfo> {
    let (width, height) = device_screen(model)?;
    Some(screen_info_for_size(width, height))
}

/// 尺寸 → 形状/圆角。按**尺寸**判定（不按型号），这样表里没有的新机型
/// （如手环 11）只要规格与某代同族，圆角/形状也能自动对上。
pub fn screen_info_for_size(width: u32, height: u32) -> ScreenInfo {
    let shape = if width == height {
        "circle"
    } else if (width, height) == (212, 520) || (width, height) == (192, 490) {
        "run"
    } else {
        "square"
    };
    let radius = match (width, height) {
        (212, 520) => 104, // 手环 10 / 11 族：两端半圆
        (192, 490) => 280, // 手环 8 / 9：R280（= 设计稿规范定稿表）
        (336, 480) => 45,  // 手环 8 Pro / 9 Pro / 10 Pro
        (432, 514) => 104, // 红米手表 5 / 5 esim / 6
        (390, 450) => 90,  // 红米手表 4 等
        (320, 385) => 82,
        (410, 502) => 116,
        (280, 456) => 36,  // 无权威值：按短边 13% 估
        _ if width == height => width / 2, // 圆表：正圆
        _ => ((width.min(height) as f64) * 0.13).round() as u32, // 兜底：短边 13%
    };
    ScreenInfo {
        width,
        height,
        radius,
        shape,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleInfo {
    pub index: usize,
    pub name: String,
    pub image_count: usize,
    pub preview_png_base64: Option<String>,
    pub preview_width: u32,
    pub preview_height: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetInfo {
    pub index: usize,
    pub shape: u8,
    pub data_src: u8,
    pub digits: u8,
    pub type_id: u8,
    pub align: u8,
    pub x: u32,
    pub y: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditModel {
    pub watchface_id: String,
    pub name: String,
    pub canvas_width: u32,
    pub canvas_height: u32,
    /// 画布尺寸来源：device（型号表）/ background（背景图）
    pub canvas_source: String,
    pub style_count: usize,
    pub styles: Vec<StyleInfo>,
    /// 相册/背景图槽位（相册表盘换壁纸用）
    pub wallpaper: Option<Rect>,
    pub wallpaper_image_id: Option<u32>,
    pub image_format: u32,
    pub widgets: Vec<WidgetInfo>,
}

pub fn resolve_image_size(
    d: &[u8],
    style: &StyleSection,
    image_id_low: u32,
) -> Option<(u32, u32)> {
    // 在块表 2（images）里按 idx 找
    let block = style.blocks.iter().find(|b| b.index == 2 && b.count > 0 && b.offset > 0)?;
    for rec in watchface_bin::block_records(d, block.count, block.offset) {
        if rec.idx as u32 == image_id_low {
            if let Ok(head) = watchface_bin::parse_image_head(d, rec.data_offset as usize) {
                return Some((head.width as u32, head.height as u32));
            }
        }
    }
    None
}

/// 从样式元素里找相册/背景图槽位：
/// 元素 = {u32 targetId, u16 x, u16 y}；targetId 低 24 位为资源 idx（高字节 0x02=图片 / 0x08=槽位）。
/// 取面积最大的「图片元素」作为壁纸区域（AWOS 实测：唯一图片元素 = 整画布背景）。
fn find_wallpaper(d: &[u8], style: &StyleSection) -> Option<(Rect, u32)> {
    let block = style.blocks.iter().find(|b| b.index == 0 && b.count > 0 && b.offset > 0)?;
    let mut best: Option<(Rect, u32)> = None;
    for rec in watchface_bin::block_records(d, block.count, block.offset) {
        let off = rec.data_offset as usize;
        if off + 8 > d.len() {
            continue;
        }
        let target = u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]]);
        let x = u16::from_le_bytes([d[off + 4], d[off + 5]]) as u32;
        let y = u16::from_le_bytes([d[off + 6], d[off + 7]]) as u32;
        // 高字节 0x02（或带 0x80 标志位）= 图片引用
        let kind = (target >> 24) & 0x7F;
        if kind != 0x02 {
            continue;
        }
        let low = target & 0x00FF_FFFF;
        let Some((w, h)) = resolve_image_size(d, style, low) else {
            continue;
        };
        let area = (w as u64) * (h as u64);
        let rect = Rect { x, y, w, h };
        match &best {
            Some((r, _)) if (r.w as u64) * (r.h as u64) >= area => {}
            _ => best = Some((rect, low)),
        }
    }
    // 兜底：样式头的 backImageId 也是图片引用（0x82xxxxxx → 低 24 位）
    if best.is_none() && style.back_image_id != 0 && style.back_image_id != 0x8000_0000 {
        let low = style.back_image_id & 0x00FF_FFFF;
        if let Some((w, h)) = resolve_image_size(d, style, low) {
            best = Some((Rect { x: 0, y: 0, w, h }, low));
        }
    }
    best
}

fn parse_widgets(d: &[u8], style: &StyleSection) -> Vec<WidgetInfo> {
    let mut out = Vec::new();
    let Some(block) = style.blocks.iter().find(|b| b.index == 7 && b.count > 0 && b.offset > 0) else {
        return out;
    };
    for rec in watchface_bin::block_records(d, block.count, block.offset) {
        let off = rec.data_offset as usize;
        if off + 4 > d.len() {
            continue;
        }
        let shape = d[off];
        let data_src = d[off + 1];
        let digits = d[off + 2];
        let flags = d[off + 3];
        let (x, y) = if rec.data_len >= 0x18 {
            (
                u16::from_le_bytes([d[off + 0x14], d[off + 0x15]]) as u32,
                u16::from_le_bytes([d[off + 0x16], d[off + 0x17]]) as u32,
            )
        } else {
            (0, 0)
        };
        out.push(WidgetInfo {
            index: rec.idx as usize,
            shape,
            data_src,
            digits,
            type_id: flags >> 4,
            align: flags & 0x03,
            x,
            y,
        });
    }
    out
}

/// 构建编辑模型
/// - `model`: 设备型号/代号（如 n67、xmb10），有则用型号表定画布
/// - `style_index`: 当前样式（用于取壁纸槽位与组件）
pub fn build_edit_model(
    d: &[u8],
    model: Option<&str>,
    style_index: usize,
) -> Result<EditModel> {
    let header: BinHeader = watchface_bin::parse_header(d)?;
    let styles_raw = watchface_bin::walk_styles(d, &header)?;
    if styles_raw.is_empty() {
        return Err(anyhow!("没有样式段"));
    }
    let style_index = style_index.min(styles_raw.len() - 1);

    // 每个样式的预览图 + 图片数
    let mut styles = Vec::new();
    for (_, st) in &styles_raw {
        let (preview_png_base64, pw, ph) = match watchface_bin::decode_preview_for_style(d, st) {
            Ok((rgba, w, h)) => {
                let png = watchface_bin::encode_png_pub(&rgba, w, h).ok();
                (png, w, h)
            }
            Err(_) => (None, 0, 0),
        };
        let image_count = st
            .blocks
            .iter()
            .find(|b| b.index == 2)
            .map(|b| b.count as usize)
            .unwrap_or(0);
        styles.push(StyleInfo {
            index: st.index,
            name: st.name.clone(),
            image_count,
            preview_png_base64,
            preview_width: pw,
            preview_height: ph,
        });
    }

    let (_, cur_style) = &styles_raw[style_index];
    let wallpaper = find_wallpaper(d, cur_style);
    let widgets = parse_widgets(d, cur_style);

    // 画布尺寸：型号表优先；否则用壁纸图尺寸；再否则用预览图尺寸
    // 画布尺寸优先级：背景图（文件内自洽，保证与壁纸区域一致）>
    // 设备型号（仅当文件里没有明确背景图时兜底）> 预览图 > 默认
    let (canvas_width, canvas_height, canvas_source) = if let Some((rect, _)) = &wallpaper {
        (rect.w, rect.h, "background".to_string())
    } else if let Some((w, h)) = model.and_then(device_screen) {
        (w, h, "device".to_string())
    } else if let Some(s) = styles.iter().find(|s| s.index == style_index && s.preview_width > 0) {
        (s.preview_width, s.preview_height, "preview".to_string())
    } else {
        (466, 466, "default".to_string())
    };

    Ok(EditModel {
        watchface_id: header.watchface_id,
        name: header.name,
        canvas_width,
        canvas_height,
        canvas_source,
        style_count: styles_raw.len(),
        styles,
        wallpaper: wallpaper.as_ref().map(|(r, _)| r.clone()),
        wallpaper_image_id: wallpaper.map(|(_, id)| id),
        image_format: 6, // PNG（全部设备一致，见型号表）
        widgets,
    })
}

/// 供未来设备端使用：从设备表盘列表条目里拿型号（占位，见 bridge）
pub fn model_from_device_name(name: &str) -> Option<(u32, u32)> {
    device_screen(name)
}

#[allow(dead_code)]
fn unused(_: &BlockRecord) {}
