//! 小米表盘 .bin（V2 家族，魔数 5A A5 34 12）解析与图片解码。
//!
//! 格式与算法来源（均为独立实现，仅依据格式事实）：
//! - 容器布局：UnpackMiColorFace（m0tral，C#）的解析逻辑 + `XiaomiWatch.Common.dll` 的 IL 反汇编
//! - RLE v10 语义与 indexed8（BGRA 调色板 + 8bit 索引）：同上 IL，并经 UnpackMiColorFace 输出逐像素回归验证
//! - 详见 `docs/表盘bin格式-逆向笔记.md`
//!
//! 已验证：手环 9 Pro 照片表盘样例 12 条图片记录全部正确解码（11/12 与参考工具逐像素 100% 一致，
//! 第 12 条的差异已定位为参考工具自身的缓冲区边界瑕疵）。

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

pub const MAGIC_BIN: u32 = 0x1234_A55A;
pub const MAGIC_RLE: u32 = 0x5AA5_21E0;
pub const HEADER_SIZE: usize = 0xA8;
pub const STYLE_STRIDE: usize = 0xA0;
pub const BLOCK_TABLE_COUNT: usize = 10;

/// 已知的块表含义（对应样式段内 10 张 {count, offset} 表）
pub const BLOCK_NAMES: [&str; BLOCK_TABLE_COUNT] = [
    "elements",  // 0 静态元素 {targetId, x, y}
    "unknown1",  // 1
    "images",    // 2 单张图片
    "imageLists",// 3 图片组（数字/图标序列）
    "unknown4",  // 4
    "apps",      // 5 跳转应用
    "unknown6",  // 6
    "widgets",   // 7 组件（数据项）
    "unknown8",  // 8
    "actions",   // 9 动作按钮
];

fn u16le(d: &[u8], off: usize) -> Result<u16> {
    let b = d
        .get(off..off + 2)
        .ok_or_else(|| anyhow!("读取 u16 越界 (0x{off:x})"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32le(d: &[u8], off: usize) -> Result<u32> {
    let b = d
        .get(off..off + 4)
        .ok_or_else(|| anyhow!("读取 u32 越界 (0x{off:x})"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn cstr64(d: &[u8], off: usize) -> String {
    let end = (off + 64).min(d.len());
    let raw = &d[off.min(d.len())..end];
    let cut = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..cut]).trim().to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinHeader {
    pub version: u32,
    pub style_count: usize,
    pub sub_version: u16,
    pub shift_words: u8,
    pub shift_words_unk: u8,
    pub preview_offset: u32,
    pub watchface_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockTable {
    pub index: usize,
    pub name: String,
    pub count: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRecord {
    pub idx: u16,
    /// 记录项的 u16 字段（+2）：高字节 = 资源类型、低字节的 bit3.. = **配色组序号**
    /// （见 `color_group_of`）。块 0/7/8/9 恒为「类型 << 8」。
    pub unk: u16,
    pub id: u32,
    pub data_offset: u32,
    pub data_len: u32,
}

/// 记录项的配色组序号：`unk`（+2 的 u16）低字节的 bit3 起。
///
/// 实测（工程「测试工程3」的 3 配色表盘，2026-10-06）：
/// - 图片块（2）`unk` = `0x0200 / 0x0208 / 0x0210` → 配色 0 / 1 / 2
/// - 图片组块（3）`unk` = `0x0300 / 0x0308 / 0x0310` → 同理
/// - 引擎侧 uid 同源：`uid = 0x02000000 | (cg << 19) | idx`（工程 `uidmap.map` 的
///   `2000001 / 2080001 / 2100001` 三个系列），即图片/图片组**按配色各存一份记录**，
///   元素/数据项里的引用不带配色位 —— 渲染时用「元素引用 + 当前配色」去取记录。
pub fn color_group_of(unk: u16) -> u32 {
    ((unk >> 3) & 0x1F) as u32
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleSection {
    pub index: usize,
    pub name: String,
    pub back_image_id: u32,
    pub preview_offset: u32,
    pub blocks: Vec<BlockTable>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    pub style: usize,
    pub block: usize,
    pub idx: u16,
    /// 配色组序号（`color_group_of(rec.unk)`；0 = 基础配色）
    pub color_group: u32,
    pub id: u32,
    pub offset: u32,
    pub width: u16,
    pub height: u16,
    pub format_byte: u8,
    pub flags: u8,
    pub data_len: u32,
    pub raw_len: usize,
    pub decoded: bool,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// RLE v10 解压（语义经参考工具逐像素验证，勿改动分支方向）：
/// - `ctrl < 0x80`：重复段 —— 读 `unit` 字节模式，重复 `ctrl` 次，输入位置仅前进 `unit`
/// - `ctrl >= 0x80`：字面量段 —— 顺序复制 `(ctrl & 0x7F)` 组 `unit` 字节
///
/// 以 `out_len` 为输出目标（与设备端一致），不按记录声明长度截断输入。
pub fn uncompress_rle_v10(src: &[u8], out_len: usize, unit: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(out_len);
    if unit == 0 {
        return out;
    }
    let n = src.len();
    let mut pos = 0usize;
    while pos < n && out.len() < out_len {
        let ctrl = src[pos];
        pos += 1;
        if ctrl & 0x80 == 0 {
            // 重复段
            if pos + unit > n {
                break;
            }
            let pat = &src[pos..pos + unit];
            for _ in 0..ctrl {
                out.extend_from_slice(pat);
            }
            pos += unit;
        } else {
            // 字面量段
            for _ in 0..(ctrl & 0x7F) {
                if pos + unit > n {
                    return out;
                }
                out.extend_from_slice(&src[pos..pos + unit]);
                pos += unit;
            }
        }
    }
    out.truncate(out_len);
    out
}

/// indexed8：1024 字节调色板（256×4，**BGRA** 通道序）+ 宽×高 个 8bit 索引
pub fn indexed8_to_rgba(raw: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let need = 1024 + (width as usize) * (height as usize);
    if raw.len() < need {
        bail!("indexed8 数据不足：需要 {need} 字节，实际 {}", raw.len());
    }
    let pal = &raw[..1024];
    let idx = &raw[1024..need];
    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
    for (i, &k) in idx.iter().enumerate() {
        let p = (k as usize) * 4;
        let (b, g, r, a) = (pal[p], pal[p + 1], pal[p + 2], pal[p + 3]);
        let o = i * 4;
        rgba[o] = r;
        rgba[o + 1] = g;
        rgba[o + 2] = b;
        rgba[o + 3] = a;
    }
    Ok(rgba)
}

// ---------------------------------------------------------------- 容器解析

pub fn parse_header(d: &[u8]) -> Result<BinHeader> {
    if d.len() < HEADER_SIZE {
        bail!("文件过小（{} 字节），不是有效 .bin", d.len());
    }
    let magic = u32le(d, 0)?;
    if magic != MAGIC_BIN {
        bail!("魔数不匹配（0x{magic:08x}），不是 V2 表盘 .bin");
    }
    Ok(BinHeader {
        version: u32le(d, 4)?,
        shift_words_unk: d[0x18],
        style_count: d[0x1C] as usize,
        shift_words: d[0x1D],
        sub_version: u16le(d, 0x1E)?,
        preview_offset: u32le(d, 0x20)?,
        watchface_id: cstr64(d, 0x28),
        name: cstr64(d, 0x68),
    })
}

/// 判断从 `off` 起 8 字节是否像一张块表 {u32 count, u32 offset}。
/// 用于探测样式段里块表的实际数量（见 `walk_styles` 注释）。
fn looks_like_table(d: &[u8], off: usize) -> bool {
    if off + 8 > d.len() {
        return false;
    }
    let count = u32le(d, off).unwrap_or(u32::MAX);
    let offset = u32le(d, off + 4).unwrap_or(u32::MAX) as usize;
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

/// 顺序遍历所有样式段，返回 (基址, StyleSection)。
///
/// 样式段长度**不是常数**：实测存在两种布局——
///   · 10 张块表：样式名 @+0x58，步长 0xA0（如 AWOS-照片表盘，ver 0x10002）
///   · 12 张块表：样式名 @+0x68，步长 0xB0（如 时序，ver 0x10004）
/// 统一规律：`步长 = 名字偏移 + 0x48`（名字后 0x44 处是 themeData u32，再 4 字节结尾；
/// themeData 低字节>>2 > 1 时还有额外补充，见下）。
/// 块表数量用 `looks_like_table` 就地探测（样式名的字节不会被误判成表）。
pub fn walk_styles(d: &[u8], h: &BinHeader) -> Result<Vec<(usize, StyleSection)>> {
    let mut base = HEADER_SIZE + (h.shift_words as usize + h.shift_words_unk as usize) * 4;
    let mut out = Vec::with_capacity(h.style_count);
    for index in 0..h.style_count {
        if base + 0x100 > d.len() {
            bail!("样式 {index} 越界（0x{base:x}）");
        }
        // 探测块表数量：基础 10 张，后续位置仍像表则计入（上限 16 张）
        let mut table_count = BLOCK_TABLE_COUNT;
        while table_count < 16 && looks_like_table(d, base + 8 + table_count * 8) {
            table_count += 1;
        }
        let name_off = base + 8 + table_count * 8;
        let mut blocks = Vec::with_capacity(table_count);
        for i in 0..table_count {
            let off = base + 8 + i * 8;
            blocks.push(BlockTable {
                index: i,
                name: BLOCK_NAMES.get(i).copied().unwrap_or("extra").to_string(),
                count: u32le(d, off)?,
                offset: u32le(d, off + 4)?,
            });
        }
        let style = StyleSection {
            index,
            name: cstr64(d, name_off),
            back_image_id: u32le(d, base)?,
            preview_offset: u32le(d, base + 4)?,
            blocks,
        };
        out.push((base, style));

        // 步进到下一个样式段
        base = name_off + 0x44;
        if base + 4 > d.len() {
            break;
        }
        let theme_data = u32le(d, base)?;
        base += 4;
        let shift_theme = ((theme_data & 0xFF) >> 2) as usize;
        if shift_theme > 1 {
            base += shift_theme * 4;
        }
    }
    Ok(out)
}

/// 兼容旧调用：取第 `index` 个样式段
pub fn parse_style(d: &[u8], h: &BinHeader, index: usize) -> Result<StyleSection> {
    let styles = walk_styles(d, h)?;
    styles
        .into_iter()
        .find(|(_, s)| s.index == index)
        .map(|(_, s)| s)
        .ok_or_else(|| anyhow!("样式 {index} 不存在（共 {} 个）", h.style_count))
}

/// 块表项（16 字节）：{u16 idx, u16 ?, u32 id, u32 dataOffset, u32 dataLen}
pub fn block_records(d: &[u8], count: u32, offset: u32) -> Vec<BlockRecord> {
    let mut out = Vec::new();
    for i in 0..count as usize {
        let off = offset as usize + i * 0x10;
        if off + 0x10 > d.len() {
            break;
        }
        let idx = u16le(d, off).unwrap_or(0);
        let unk = u16le(d, off + 2).unwrap_or(0);
        let id = u32le(d, off + 4).unwrap_or(0);
        let data_offset = u32le(d, off + 8).unwrap_or(0);
        let data_len = u32le(d, off + 0xC).unwrap_or(0);
        out.push(BlockRecord {
            idx,
            unk,
            id,
            data_offset,
            data_len,
        });
    }
    out
}

// ---------------------------------------------------------------- 图片解码

/// 图片记录头（0x14 字节）+ 压缩流：
/// `+0` 格式字节 / `+1` 标志（bit2..5=压缩方式）/ `+4` u16 宽 / `+6` u16 高 /
/// `+8` u32 数据长度 / `+0xC` RLE 魔数 / `+0x10` 流头（unit=低4位, out_len=>>4）/ `+0x14` 压缩流
#[derive(Debug, Clone, Copy)]
pub struct ImageRecordHead {
    pub format_byte: u8,
    pub flags: u8,
    pub width: u16,
    pub height: u16,
    pub data_len: u32,
    pub is_rle: bool,
    pub unit: usize,
    pub out_len: usize,
}

pub fn parse_image_head(d: &[u8], offset: usize) -> Result<ImageRecordHead> {
    if offset + 0x14 > d.len() {
        bail!("图片记录头越界 (0x{offset:x})");
    }
    let data_len = u32le(d, offset + 8)?;
    let magic = u32le(d, offset + 0x0C)?;
    let (is_rle, unit, out_len) = if magic == MAGIC_RLE {
        let hdr = u32le(d, offset + 0x10)?;
        (true, (hdr & 0xF) as usize, (hdr >> 4) as usize)
    } else {
        (false, 0, data_len as usize)
    };
    Ok(ImageRecordHead {
        format_byte: d[offset],
        flags: d[offset + 1],
        width: u16le(d, offset + 4)?,
        height: u16le(d, offset + 6)?,
        data_len,
        is_rle,
        unit,
        out_len,
    })
}

pub fn raw_payload<'a>(d: &'a [u8], offset: usize, head: &ImageRecordHead) -> Result<&'a [u8]> {
    if head.is_rle {
        let start = offset + 0x14;
        let end = d.len();
        d.get(start..end)
            .ok_or_else(|| anyhow!("RLE 流越界 (0x{start:x})"))
    } else {
        let start = offset + 0x0C;
        let end = start + head.data_len as usize;
        d.get(start..end)
            .ok_or_else(|| anyhow!("原始负载越界 (0x{start:x}+{})", head.data_len))
    }
}

/// 轻量探测：只解析记录头判断"能否解码"，不做实际解压与调色板展开。
/// （手机端 `inspect` 用；大表盘有数千张图，全解会卡顿）
pub fn peek_image(d: &[u8], offset: usize) -> Result<(ImageRecordHead, bool, String)> {
    let head = parse_image_head(d, offset)?;
    let (w, h) = (head.width as u32, head.height as u32);
    if head.format_byte == 0x10 {
        let expect = 1024 + (w as usize) * (h as usize);
        if head.out_len == expect {
            return Ok((head, true, String::new()));
        }
        return Ok((
            head,
            false,
            format!(
                "indexed8 长度不符：{} / 期望 {}（可能为其他像素格式）",
                head.out_len, expect
            ),
        ));
    }
    Ok((
        head,
        false,
        format!(
            "像素格式 0x{:02x} 待实现（{}x{}, {} 字节）",
            head.format_byte, w, h, head.out_len
        ),
    ))
}

/// 解码一条图片记录为 RGBA。当前支持 indexed8（format 0x10）；
/// 其他像素格式返回 `Ok(None)` 并给出 note（后续按 IL 逐个补齐）。
pub fn decode_image(d: &[u8], offset: usize) -> Result<(ImageRecordHead, Option<Vec<u8>>, String)> {
    let head = parse_image_head(d, offset)?;
    let payload = raw_payload(d, offset, &head)?;
    let raw = if head.is_rle {
        uncompress_rle_v10(payload, head.out_len, head.unit)
    } else {
        payload.to_vec()
    };
    let (w, h) = (head.width as u32, head.height as u32);
    if head.format_byte == 0x10 {
        let expect = 1024 + (w as usize) * (h as usize);
        if raw.len() == expect {
            let rgba = indexed8_to_rgba(&raw, w, h)?;
            return Ok((head, Some(rgba), String::new()));
        }
        return Ok((
            head,
            None,
            format!(
                "indexed8 长度不符：解出 {} / 期望 {}（可能为其他像素格式）",
                raw.len(),
                expect
            ),
        ));
    }
    Ok((
        head,
        None,
        format!(
            "像素格式 0x{:02x} 待实现（解出 {} 字节, {}x{}）",
            head.format_byte,
            raw.len(),
            w,
            h
        ),
    ))
}

/// 解码某个样式的预览图（样式段 +0x04 的 previewImageOffset 指向的记录）
pub fn decode_preview_for_style(d: &[u8], style: &StyleSection) -> Result<(Vec<u8>, u32, u32)> {
    let off = style.preview_offset as usize;
    if off == 0 || off + 0x14 > d.len() {
        bail!("样式 {} 没有预览图", style.index);
    }
    let (head, rgba, note) = decode_image(d, off)?;
    let rgba = rgba.ok_or_else(|| anyhow!("样式预览图暂不支持：{note}"))?;
    Ok((rgba, head.width as u32, head.height as u32))
}

/// 解码表盘预览图（头部 0x20 指向的记录，结构与图片记录相同）
pub fn decode_preview(d: &[u8], h: &BinHeader) -> Result<DecodedImage> {
    let off = h.preview_offset as usize;
    let (head, rgba, note) = decode_image(d, off).context("解码预览图")?;
    let rgba = rgba.ok_or_else(|| anyhow!("预览图暂不支持：{note}"))?;
    Ok(DecodedImage {
        width: head.width as u32,
        height: head.height as u32,
        rgba,
    })
}

// ---------------------------------------------------------------- 上层 API

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinSummary {
    pub header: BinHeader,
    pub styles: Vec<StyleSection>,
    pub style_images: Vec<Vec<ImageInfo>>,
    pub preview: Option<PreviewInfo>,
    /// 配色组数量（> 1 = 多配色表盘；图片/图片组块按配色各存一套记录）
    pub color_groups: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewInfo {
    pub width: u32,
    pub height: u32,
    pub png_base64: String,
}

pub fn encode_png_pub(rgba: &[u8], width: u32, height: u32) -> Result<String> {
    encode_png(rgba, width, height)
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<String> {
    use base64::Engine;
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| anyhow!("RGBA 尺寸不匹配"))?;
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .context("PNG 编码失败")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(buf.get_ref()))
}

/// 解析 .bin，返回摘要（含每样式图片元数据与预览图 PNG）
pub fn inspect(d: &[u8]) -> Result<BinSummary> {
    let header = parse_header(d)?;
    let mut styles = Vec::new();
    let mut style_images = Vec::new();
    let mut color_groups = 1usize;
    for (_base, style) in walk_styles(d, &header)? {
        let i = style.index;
        // 只有块表 2（images）是图片记录；其余块（元素/组件/图片组/动作）结构不同
        let images_block = style.blocks.iter().find(|b| b.index == 2 && b.count > 0 && b.offset > 0);
        let mut infos = Vec::new();
        if let Some(b) = images_block {
            for rec in block_records(d, b.count, b.offset) {
                if rec.data_offset as usize + 0x14 > d.len() {
                    continue;
                }
                let (head, decoded, note) = match peek_image(d, rec.data_offset as usize) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if head.width == 0 || head.height == 0 || head.width > 2048 || head.height > 2048 {
                    continue;
                }
                let cg = color_group_of(rec.unk);
                color_groups = color_groups.max(cg as usize + 1);
                infos.push(ImageInfo {
                    style: i,
                    block: b.index,
                    idx: rec.idx,
                    color_group: cg,
                    id: rec.id,
                    offset: rec.data_offset,
                    width: head.width,
                    height: head.height,
                    format_byte: head.format_byte,
                    flags: head.flags,
                    data_len: head.data_len,
                    raw_len: head.out_len,
                    decoded,
                    note,
                });
            }
        }
        styles.push(style);
        style_images.push(infos);
    }
    let preview = decode_preview(d, &header)
        .ok()
        .and_then(|p| {
            encode_png(&p.rgba, p.width, p.height)
                .ok()
                .map(|b64| PreviewInfo {
                    width: p.width,
                    height: p.height,
                    png_base64: b64,
                })
        });
    Ok(BinSummary {
        header,
        styles,
        style_images,
        preview,
        color_groups,
    })
}

/// 缩略图信息（前端图墙用；只有能解码的图片才有 png_base64）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbInfo {
    pub idx: u16,
    /// 配色组序号（多配色表盘用）
    pub color_group: u32,
    pub width: u16,
    pub height: u16,
    pub format_byte: u8,
    pub decoded: bool,
    pub note: String,
    pub png_base64: Option<String>,
}

/// 解出某样式 images 块的全部图片，生成不超过 `thumb_max` 边长的缩略图 PNG（base64）
pub fn style_thumbnails(d: &[u8], style_index: usize, thumb_max: u32) -> Result<Vec<ThumbInfo>> {
    let header = parse_header(d)?;
    let style = parse_style(d, &header, style_index)?;   // 内部走 walk_styles（变长样式段）
    let mut out = Vec::new();
    let block = match style.blocks.iter().find(|b| b.index == 2 && b.count > 0 && b.offset > 0) {
        Some(b) => b,
        None => return Ok(out),
    };
    for rec in block_records(d, block.count, block.offset) {
        let (head, rgba, note) = match decode_image(d, rec.data_offset as usize) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if head.width == 0 || head.height == 0 || head.width > 2048 || head.height > 2048 {
            continue;
        }
        let png_base64 = rgba.and_then(|rgba| {
            let img = image::RgbaImage::from_raw(head.width as u32, head.height as u32, rgba)?;
            let (w, h) = (head.width as u32, head.height as u32);
            let max = thumb_max.max(16);
            let (tw, th) = if w.max(h) <= max {
                (w, h)
            } else if w >= h {
                (max, ((h as f64) * (max as f64) / (w as f64)).round().max(1.0) as u32)
            } else {
                (((w as f64) * (max as f64) / (h as f64)).round().max(1.0) as u32, max)
            };
            let thumb = if (tw, th) == (w, h) {
                image::DynamicImage::ImageRgba8(img)
            } else {
                image::DynamicImage::ImageRgba8(image::imageops::resize(
                    &img,
                    tw,
                    th,
                    image::imageops::FilterType::Triangle,
                ))
            };
            let mut buf = std::io::Cursor::new(Vec::new());
            thumb.write_to(&mut buf, image::ImageFormat::Png).ok()?;
            use base64::Engine;
            Some(base64::engine::general_purpose::STANDARD.encode(buf.get_ref()))
        });
        out.push(ThumbInfo {
            idx: rec.idx,
            color_group: color_group_of(rec.unk),
            width: head.width,
            height: head.height,
            format_byte: head.format_byte,
            decoded: png_base64.is_some(),
            note,
            png_base64,
        });
    }
    Ok(out)
}

/// 导出某样式的全部图片（indexed8 → PNG，其余按原始字节存 .raw），返回 (导出文件数, 报告)
pub fn extract_style_images(
    d: &[u8],
    style_index: usize,
    out_dir: &std::path::Path,
) -> Result<(usize, Vec<ImageInfo>)> {
    let header = parse_header(d)?;
    let style = parse_style(d, &header, style_index)?;   // 内部走 walk_styles（变长样式段）
    std::fs::create_dir_all(out_dir)?;
    let mut written = 0usize;
    let mut report = Vec::new();
    for b in style.blocks.iter().filter(|b| b.index == 2) {
        if b.count == 0 || b.offset == 0 {
            continue;
        }
        for rec in block_records(d, b.count, b.offset) {
            let (head, rgba, note) = match decode_image(d, rec.data_offset as usize) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if head.width == 0 || head.height == 0 {
                continue;
            }
            // 多配色表盘里同一 idx 每个配色一份记录 → 文件名带上配色，避免互相覆盖
            let base = format!("b{}_c{}_{:04}", b.index, color_group_of(rec.unk), rec.idx);
            let decoded = rgba.is_some();
            if let Some(rgba) = rgba {
                let img = image::RgbaImage::from_raw(head.width as u32, head.height as u32, rgba)
                    .ok_or_else(|| anyhow!("RGBA 尺寸不匹配"))?;
                let path = out_dir.join(format!("{base}.png"));
                img.save_with_format(&path, image::ImageFormat::Png)?;
                written += 1;
            } else {
                let raw = raw_payload(d, rec.data_offset as usize, &head)?;
                let path = out_dir.join(format!("{base}.raw"));
                std::fs::write(&path, raw)?;
                written += 1;
            }
            report.push(ImageInfo {
                style: style_index,
                block: b.index,
                idx: rec.idx,
                color_group: color_group_of(rec.unk),
                id: rec.id,
                offset: rec.data_offset,
                width: head.width,
                height: head.height,
                format_byte: head.format_byte,
                flags: head.flags,
                data_len: head.data_len,
                raw_len: head.out_len,
                decoded,
                note,
            });
        }
    }
    Ok((written, report))
}
