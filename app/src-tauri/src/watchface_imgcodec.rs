//! 设备壁纸的图像编码。
//!
//! 设备通过 `WatchFaceItem.support_image_format`（`pb::xiaomi::protocol::ImageFormat`）
//! 声明它要什么像素格式。手环 10 NFC 实测上报 `ARGB_8565_LE`(7)，即 3 字节/像素：
//! `[RGB565 低字节][RGB565 高字节][A]`，其中 16 位字 = `R5<<11 | G6<<5 | B5`。
//!
//! 字节序依据（两处独立实现互相印证）：
//! - `XiaomiWatch.Common.dll` → `BinaryExtender::Rgb565AlphaToRGBA`（解码）
//!   与 `MiBand9RleCompressor::PackRgbaToRgb565Alpha`（编码）互为逆运算：
//!   低 5 位字段 ↔ 4 字节缓冲的第 0 字节，高 5 位字段 ↔ 第 2 字节；
//!   而该 4 字节缓冲的通道序已由 indexed8 的 CLUT（B,G,R,A，逐像素验证）与
//!   BMP 写出路径确定为 BGRA（即 Android ARGB_8888 的内存序）→ 第 0 字节 = B。
//! - AstroBox `libapp_lib.so` 的 `watchface_edit::imgcodec::pack_8565`（arm64 反汇编）：
//!   输入 RGBA（R 在前），format 第 0 位 = 1（即 7 = ARGB_8565_LE）时
//!   `out[0] = (B>>3) | ((G>>2)<<5)`、`out[1] = (R&0xF8) | (G>>5)`、`out[2] = A`。
//!
//! 取整方式照抄 AstroBox：直接右移截断（`R>>3`），不是 `round(R*31/255)`。

use anyhow::anyhow;

/// 编码整画布 RGBA（4 字节/像素，R,G,B,A 顺序）为设备要的字节流。
pub fn encode_canvas(rgba: &[u8], w: u32, h: u32, format: i32) -> anyhow::Result<Vec<u8>> {
    let n = (w as usize) * (h as usize);
    anyhow::ensure!(
        rgba.len() >= n * 4,
        "画布数据长度不符：{} 字节，应为 {}×{}×4 = {}",
        rgba.len(),
        w,
        h,
        n * 4
    );
    let px = &rgba[..n * 4];
    match format {
        // ARGB_8565_LE —— 手环 9/10/RW5 实测格式
        7 => {
            let mut out = Vec::with_capacity(n * 3);
            for p in px.chunks_exact(4) {
                let (r, g, b, a) = (p[0], p[1], p[2], p[3]);
                let v: u16 = (((r as u16) >> 3) << 11) | (((g as u16) >> 2) << 5) | ((b as u16) >> 3);
                out.push((v & 0xFF) as u8);
                out.push((v >> 8) as u8);
                out.push(a);
            }
            Ok(out)
        }
        // ARGB_8565（大端变体，未实测；按 pack_8565 的另一分支）
        8 => {
            let mut out = Vec::with_capacity(n * 3);
            for p in px.chunks_exact(4) {
                let (r, g, b, a) = (p[0], p[1], p[2], p[3]);
                let v: u16 = (((r as u16) >> 3) << 11) | (((g as u16) >> 2) << 5) | ((b as u16) >> 3);
                out.push((v >> 8) as u8);
                out.push((v & 0xFF) as u8);
                out.push(a);
            }
            Ok(out)
        }
        // RGB565（小端，丢 alpha）
        0 => {
            let mut out = Vec::with_capacity(n * 2);
            for p in px.chunks_exact(4) {
                let v: u16 =
                    (((p[0] as u16) >> 3) << 11) | (((p[1] as u16) >> 2) << 5) | ((p[2] as u16) >> 3);
                out.extend_from_slice(&v.to_le_bytes());
            }
            Ok(out)
        }
        // RGB565_SWAP（大端）
        1 => {
            let mut out = Vec::with_capacity(n * 2);
            for p in px.chunks_exact(4) {
                let v: u16 =
                    (((p[0] as u16) >> 3) << 11) | (((p[1] as u16) >> 2) << 5) | ((p[2] as u16) >> 3);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Ok(out)
        }
        // ARGB_8888_LE：就是 Android ARGB_8888 的内存序（B,G,R,A）
        2 => {
            let mut out = Vec::with_capacity(n * 4);
            for p in px.chunks_exact(4) {
                out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
            Ok(out)
        }
        other => Err(anyhow!(
            "暂不支持设备要求的图像格式 {other}（仅实现 0/1/2/7/8；5=JPEG、6=PNG 直接原样上传）"
        )),
    }
}

/// 编码后的字节流做 md5（设备用它作为图片 id，需在 EditRequest 里先声明）
pub fn md5_hex(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format7_lane_order() {
        // 纯红 → v = 0xF800 → [0x00, 0xF8, 0xFF]
        let out = encode_canvas(&[255, 0, 0, 255], 1, 1, 7).unwrap();
        assert_eq!(out, vec![0x00, 0xF8, 0xFF]);
        // 纯蓝 → v = 0x001F → [0x1F, 0x00, 0xFF]
        let out = encode_canvas(&[0, 0, 255, 255], 1, 1, 7).unwrap();
        assert_eq!(out, vec![0x1F, 0x00, 0xFF]);
        // 纯绿 → v = 0x07E0 → [0xE0, 0x07, 0xFF]
        let out = encode_canvas(&[0, 255, 0, 255], 1, 1, 7).unwrap();
        assert_eq!(out, vec![0xE0, 0x07, 0xFF]);
    }
}
