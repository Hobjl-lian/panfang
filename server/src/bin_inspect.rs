//! .bin 检查器：上传时从文件本身提取可识别元数据。
//!
//! 依据 `docs/表盘bin格式-逆向笔记.md`（容器结构已逐像素验证）与 App 端
//! `app/src-tauri/src/watchface_bin.rs` 的同一套解析规则（此处为服务端精简版，
//! 只读头部 + 样式段 + 背景图尺寸，不做图片解码）。
//!
//! 能识别：魔数/容器格式版本/样式数/表盘 id/表盘名称/**画布尺寸**；
//! 不能识别：创作者的发布版本号（1.0/2.0/3.0 不在文件里，见 `version_hint`）。

use anyhow::{anyhow, bail, Result};
use serde::Serialize;

const MAGIC_BIN: u32 = 0x1234_A55A;
const HEADER_SIZE: usize = 0xA8;
const BLOCK_TABLE_COUNT: usize = 10;

/// 头部 0x04 版本号按 x.y.z 解读（实测：0x00010002 = 1.0.2，每次导出末位 +1）。
pub fn decode_version(raw: u32) -> (u32, u32, u32) {
    ((raw >> 16) & 0xFFFF, (raw >> 8) & 0xFF, raw & 0xFF)
}

/// 规范化成 `1.0.2` 形式。
pub fn version_string(raw: u32) -> String {
    let (a, b, c) = decode_version(raw);
    format!("{a}.{b}.{c}")
}

/// 供排序/比较：数值三元组。
pub fn version_tuple(s: &str) -> (u32, u32, u32) {
    let mut it = s.split('.').map(|x| x.trim().parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinInfo {
    /// 容器格式版本（如 0x00010002）
    pub fmt_version: String,
    /// **版本号**（按 x.y.z 解读，如 "1.0.2"）—— 上传时用它判断新旧
    pub version_label: String,
    pub fmt_version_raw: u32,
    pub sub_version: u16,
    pub style_count: usize,
    /// 表盘 id（头部 0x28，12 位数字；同一表盘同一设备族稳定不变）
    pub watchface_id: String,
    /// 表盘名称（头部 0x68，UTF-8）
    pub name: String,
    /// 画布尺寸（背景图尺寸推导；失败为 null）
    pub canvas: Option<(u32, u32)>,
    /// 由画布尺寸给出的设备族提示（用于自动匹配设备变体）
    pub device_hints: Vec<String>,
    /// 内容哈希（sha256 hex）
    pub sha256: String,
    pub size: u64,
}

fn u16le(d: &[u8], off: usize) -> Option<u16> {
    d.get(off..off + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}
fn u32le(d: &[u8], off: usize) -> Option<u32> {
    d.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn cstr64(d: &[u8], off: usize) -> String {
    let end = (off + 64).min(d.len());
    if off >= d.len() {
        return String::new();
    }
    let raw = &d[off..end];
    let cut = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..cut]).trim().to_string()
}

/// 从 `off` 起 8 字节是否像一张块表 {u32 count, u32 offset}（与 App 端同一判据）。
fn looks_like_table(d: &[u8], off: usize) -> bool {
    let count = match u32le(d, off) {
        Some(v) => v,
        None => return false,
    };
    let offset = match u32le(d, off + 4) {
        Some(v) => v as usize,
        None => return false,
    };
    if count > 0x4000 {
        return false;
    }
    if offset % 8 != 0 || offset >= d.len() {
        return false;
    }
    if count > 0 && (offset as u64) + (count as u64) * 0x10 > d.len() as u64 {
        return false;
    }
    true
}

struct StyleHead {
    back_image_id: u32,
    /// 图片块（index 2 单图 / 3 图片组）的 (idx → data_offset)
    images: Vec<(u32, usize)>,
}

/// 遍历样式段（步长规律见逆向笔记 §1）。
fn walk_styles(d: &[u8], style_count: usize, shift: usize) -> Vec<StyleHead> {
    let mut out = Vec::new();
    let mut base = HEADER_SIZE + shift * 4;
    for _ in 0..style_count {
        if base + 0x100 > d.len() {
            break;
        }
        let mut table_count = BLOCK_TABLE_COUNT;
        while table_count < 16 && looks_like_table(d, base + 8 + table_count * 8) {
            table_count += 1;
        }
        let name_off = base + 8 + table_count * 8;

        let mut images = Vec::new();
        for i in 0..table_count {
            // 只关心图片块：2 = 单图、3 = 图片组
            if i != 2 && i != 3 {
                continue;
            }
            let off = base + 8 + i * 8;
            let count = u32le(d, off).unwrap_or(0) as usize;
            let list_off = u32le(d, off + 4).unwrap_or(0) as usize;
            for r in 0..count.min(0x4000) {
                let rec = list_off + r * 0x10;
                if rec + 0x10 > d.len() {
                    break;
                }
                let idx = u16le(d, rec).unwrap_or(0) as u32;
                let data_offset = u32le(d, rec + 8).unwrap_or(0) as usize;
                if data_offset + 0x14 <= d.len() {
                    images.push((idx, data_offset));
                }
            }
        }

        out.push(StyleHead {
            back_image_id: u32le(d, base).unwrap_or(0),
            images,
        });

        // 步进到下一个样式段
        let mut next = name_off + 0x44;
        if next + 4 > d.len() {
            break;
        }
        let theme_data = u32le(d, next).unwrap_or(0);
        next += 4;
        let shift_theme = ((theme_data & 0xFF) >> 2) as usize;
        if shift_theme > 1 {
            next += shift_theme * 4;
        }
        base = next;
    }
    out
}

/// 图片记录头：+0x04 u16 宽 / +0x06 u16 高。
fn image_size(d: &[u8], off: usize) -> Option<(u32, u32)> {
    let w = u16le(d, off + 4)? as u32;
    let h = u16le(d, off + 6)? as u32;
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    Some((w, h))
}

/// 画布尺寸：优先「样式头 backImageId 指向的图」，否则取面积最大的图片。
fn canvas_of(d: &[u8], styles: &[StyleHead]) -> Option<(u32, u32)> {
    for st in styles {
        let low = st.back_image_id & 0x00FF_FFFF;
        let hi = (st.back_image_id >> 24) & 0x7F;
        if (hi == 0x02 || hi == 0x82) && low != 0 {
            if let Some((idx, off)) = st.images.iter().find(|(idx, _)| *idx == low) {
                if let Some(size) = image_size(d, *off) {
                    return Some(size);
                }
            }
        }
    }
    // 退化：任意样式的最大图片
    styles
        .iter()
        .flat_map(|st| st.images.iter())
        .filter_map(|(_, off)| image_size(d, *off))
        .max_by_key(|(w, h)| (*w as u64) * (*h as u64))
}

/// 画布尺寸 → 设备族提示（与 App 端型号表 `device_screen()` 同源）。
pub fn device_hints(canvas: Option<(u32, u32)>) -> Vec<String> {
    let Some((w, h)) = canvas else {
        return vec![];
    };
    match (w, h) {
        (212, 520) => vec!["小米手环10".into(), "小米手环11".into()],
        (192, 490) => vec!["小米手环9".into()],
        (336, 480) => vec!["小米手环9 Pro".into(), "小米手环10 Pro".into()],
        // 红米5/6 同屏同代号：同一组、共用一份包体
        (432, 514) => vec!["Redmi Watch 5".into(), "Redmi Watch 5 eSIM".into(), "Redmi Watch 6".into()],
        (464, 464) => vec!["Xiaomi Watch S3".into(), "Xiaomi Watch S4".into()],
        (466, 466) => vec!["Xiaomi Watch S3".into()],
        (480, 480) => vec!["Xiaomi Watch 5".into()],
        (390, 450) => vec!["红米手表 4".into()],
        (320, 385) => vec!["红米手表 3".into()],
        (410, 502) => vec!["红米手表 5 青春版".into()],
        (280, 456) => vec!["红米手表 4 青春版".into()],
        _ => vec![],
    }
}

/// 解析 .bin：头部 + 画布（不写任何文件）。
pub fn inspect(data: &[u8]) -> Result<BinInfo> {
    if data.len() < HEADER_SIZE {
        bail!("文件过小（{} 字节），不是有效 .bin", data.len());
    }
    let magic = u32le(data, 0).ok_or_else(|| anyhow!("读取魔数失败"))?;
    if magic != MAGIC_BIN {
        bail!("魔数不匹配（0x{magic:08x}），不是 V2 表盘 .bin");
    }
    let fmt_version_raw = u32le(data, 4).unwrap_or(0);
    let style_count = data[0x1C] as usize;
    let shift = (data[0x18] as usize) + (data[0x1D] as usize);
    let styles = walk_styles(data, style_count, shift);
    let canvas = canvas_of(data, &styles);

    Ok(BinInfo {
        fmt_version: format!("0x{fmt_version_raw:08X}"),
        version_label: version_string(fmt_version_raw),
        fmt_version_raw,
        sub_version: u16le(data, 0x1E).unwrap_or(0),
        style_count,
        watchface_id: cstr64(data, 0x28),
        name: cstr64(data, 0x68),
        canvas,
        device_hints: device_hints(canvas),
        sha256: crate::crypto::sha256_hex(data),
        size: data.len() as u64,
    })
}

// ---------------------------------------------------------------- 版本号推断

fn normalize_version(raw: &str) -> String {
    let v = raw.trim().trim_start_matches(['v', 'V']).trim();
    if v.is_empty() {
        return String::new();
    }
    if v.contains('.') {
        v.to_string()
    } else {
        format!("{v}.0")
    }
}

fn find_version(text: &str) -> Option<String> {
    let bytes: Vec<char> = text.chars().collect();
    // 1) 显式 vN / VN.N
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    for i in 0..bytes.len() {
        if lower[i] == 'v' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == '.') {
                end += 1;
            }
            let raw: String = bytes[start..end].iter().collect();
            if !raw.is_empty() && !raw.ends_with('.') {
                return Some(normalize_version(&raw));
            }
        }
    }
    // 2) 整段就是版本号（文件夹名 "3.0" / "2.0"）
    let trimmed = text.trim();
    if !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.')
        && trimmed.contains('.')
    {
        return Some(normalize_version(trimmed));
    }
    None
}

/// 从「文件夹名 / 文件名」推断发布版本号。
///
/// 已知约定（用户素材库实测）：`3.0/AWOS-9P10P-v3.bin`、`2.0/AWOS-10-v2.bin`。
/// 目录优先于文件名；两者都没有则返回 None（由服务端按同变体递增建议）。
pub fn version_hint(file_name: &str, path_hint: &str) -> Option<String> {
    // 目录：取最后一级
    let dir_last = path_hint
        .replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    if let Some(v) = find_version(&dir_last) {
        return Some(v);
    }
    if let Some(v) = path_hint.split(['/', '\\']).find_map(|seg| find_version(seg)) {
        return Some(v);
    }
    let stem = file_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(file_name);
    find_version(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_hints() {
        assert_eq!(
            version_hint("AWOS-9P10P-v3.bin", "3.0/AWOS-9P10P-v3.bin").as_deref(),
            Some("3.0")
        );
        assert_eq!(
            version_hint("AWOS-10-v2.bin", "").as_deref(),
            Some("2.0")
        );
        assert_eq!(
            version_hint("AWOS-照片表盘-10.bin", "表盘文件").as_deref(),
            None
        );
        assert_eq!(version_hint("x.bin", "2.0").as_deref(), Some("2.0"));
    }

    #[test]
    fn version_decoding() {
        assert_eq!(version_string(0x0001_0002), "1.0.2");
        assert_eq!(version_string(0x0001_0004), "1.0.4");
        assert!(version_tuple("1.0.10") > version_tuple("1.0.9"));
        assert!(version_tuple("1.1.0") > version_tuple("1.0.99"));
    }

    #[test]
    fn hints_by_canvas() {
        assert_eq!(device_hints(Some((212, 520)))[0], "小米手环10");
        // 红米手表5/6 合并后：Redmi Watch 5 / 5 eSIM / Watch 6
        assert_eq!(device_hints(Some((432, 514))).len(), 3);
        assert!(device_hints(None).is_empty());
    }
}
