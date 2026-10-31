//! 照片表盘「壁纸生成器」：按后台下发的方案配置把用户照片合成为成品壁纸。
//!
//! 语义与底图编辑网站（`照片表盘管理库/watchface-editor`）保持一致：
//!
//! 1. **基准**：每张照片先按「填满自己的参考框」缩放（cover），再乘 1.15。
//!    底图的参考框 = 整张画布；相框里那张的参考框 = 画框窗口。
//! 2. **用户调整**：缩放（相对基准的倍数）、旋转、压暗、模糊（高斯）、水平/垂直翻转、
//!    拖动平移（位移记在各自参考框的坐标系里）。
//! 3. **自动倾斜**：相框带倾角时，框内照片跟着一起倾斜（它画在画框的旋转坐标系里），
//!    用户的旋转滑杆是在这个基础上叠加的二次调整。
//! 4. **合成顺序**：底图 → 相框内照片（裁进画框圆角）→ 相框素材盖在最上层。
//!
//! 画布尺寸、参考框位置、相框素材全部来自后台配置——**App 里不写死任何表盘的参数**。

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// cover 之后额外放大的倍数（网站里写死的 1.15：填满参考框再放大一点，留出拖动余量）
pub const FIT_ZOOM: f32 = 1.15;

// ------------------------------------------------------------------ 配置

fn default_version() -> i64 {
    1
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GenCanvas {
    pub w: u32,
    pub h: u32,
    /// 整张画布的圆角（0 = 不裁；一般交给相框素材自己的形状）
    #[serde(default)]
    pub radius: f32,
}

/// 一个可调量的范围与默认值（与 AstroBox CreatorConsole 同一形状：
/// 纯数字 = 不给调，带 `adjustable: true` 才在界面上出控件）
#[derive(Deserialize, Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct NumCtl {
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub adjustable: bool,
}

impl Default for NumCtl {
    fn default() -> Self {
        NumCtl {
            default: 1.0,
            // 缩放下限 0.2：横向照片在竖长画布上要能退到「看得到整张」（0.5 只够看到一半）
            min: 0.2,
            max: 3.0,
            step: 0.01,
            adjustable: true,
        }
    }
}

impl NumCtl {
    /// 取用户值（缺省用默认值），并夹到配置声明的范围内
    pub fn resolve(&self, v: Option<f32>) -> f32 {
        let lo = self.min.min(self.max);
        let hi = self.min.max(self.max);
        v.unwrap_or(self.default).clamp(lo, hi)
    }
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct BoolCtl {
    pub default: bool,
    pub adjustable: bool,
}

impl Default for BoolCtl {
    fn default() -> Self {
        BoolCtl {
            default: false,
            adjustable: true,
        }
    }
}

/// 一张照片的六个基础功能（拍立得两张图各自一套）
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct GenControls {
    /// 缩放：相对「填满参考框 ×1.15」的倍数
    pub scale: NumCtl,
    /// 旋转（度）
    pub rotation: NumCtl,
    /// 压暗（0~1：0 = 不压，1 = 全黑）
    pub darken: NumCtl,
    /// 模糊（画布像素，高斯）
    pub blur: NumCtl,
    pub flip_x: BoolCtl,
    pub flip_y: BoolCtl,
}

impl Default for GenControls {
    fn default() -> Self {
        GenControls {
            scale: NumCtl {
                default: 1.0,
                min: 0.2,
                max: 3.0,
                step: 0.01,
                adjustable: true,
            },
            rotation: NumCtl {
                default: 0.0,
                min: -180.0,
                max: 180.0,
                step: 1.0,
                adjustable: true,
            },
            darken: NumCtl {
                default: 0.0,
                min: 0.0,
                max: 1.0,
                step: 0.01,
                adjustable: true,
            },
            blur: NumCtl {
                default: 0.0,
                min: 0.0,
                max: 40.0,
                step: 1.0,
                adjustable: true,
            },
            flip_x: BoolCtl::default(),
            flip_y: BoolCtl::default(),
        }
    }
}

/// 一张照片（`base` = 它的参考框是谁）
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GenPhoto {
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// `canvas` = 铺整张画布（底图）；`frame` = 放进相框窗口
    pub base: String,
    #[serde(default)]
    pub controls: GenControls,
}

/// 相框窗口：位置是**未旋转时的左上角**，`rot` 绕窗口中心旋转
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GenWindow {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    #[serde(default)]
    pub radius: f32,
    #[serde(default)]
    pub rot: f32,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GenColor {
    pub i: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub swatch: String,
    /// 素材文件名（配置里只准出现相对文件名，不准出现 URL）
    pub src: String,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GenFrame {
    /// 只有「有照片要放进画框」的方案才需要窗口（Edge-UI 那种单图 + 花边模板就没有）
    #[serde(default)]
    pub window: Option<GenWindow>,
    #[serde(default)]
    pub colors: Vec<GenColor>,
}

/// 一个表盘的壁纸方案
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct GenConfig {
    #[serde(default = "default_version")]
    pub version: i64,
    pub canvas: GenCanvas,
    pub photos: Vec<GenPhoto>,
    #[serde(default)]
    pub frame: Option<GenFrame>,
}

impl GenConfig {
    /// 这个方案需要几张照片
    pub fn photo_count(&self) -> usize {
        self.photos.len()
    }

    /// 找某个相框色号（缺省取第一个）
    pub fn color(&self, idx: Option<i64>) -> Option<&GenColor> {
        let f = self.frame.as_ref()?;
        match idx {
            Some(i) => f.colors.iter().find(|c| c.i == i).or_else(|| f.colors.first()),
            None => f.colors.first(),
        }
    }

    /// 配置自身的最低限度检查（服务端已校验过一遍，这里防的是「配置写坏了」）
    pub fn check(&self) -> Result<()> {
        if self.canvas.w < 16 || self.canvas.h < 16 || self.canvas.w > 4096 || self.canvas.h > 4096 {
            bail!("画布尺寸不合法: {}x{}", self.canvas.w, self.canvas.h);
        }
        if self.photos.is_empty() || self.photos.len() > 4 {
            bail!("照片数量不合法: {}", self.photos.len());
        }
        for p in &self.photos {
            if p.base != "canvas" && p.base != "frame" {
                bail!("照片 {} 的基准不合法: {}", p.id, p.base);
            }
            if p.base == "frame" {
                let w = self.frame.as_ref().and_then(|f| f.window.as_ref());
                let Some(w) = w else {
                    bail!("照片 {} 要求放进画框，但方案里没写画框窗口", p.id);
                };
                if w.w <= 0.0 || w.h <= 0.0 {
                    bail!("相框窗口尺寸不合法");
                }
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ 用户参数

/// 用户在界面上调出来的值（缺省 = 用配置里的默认值）
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct GenParams {
    pub scale: Option<f32>,
    pub rotation: Option<f32>,
    pub darken: Option<f32>,
    pub blur: Option<f32>,
    pub flip_x: Option<bool>,
    pub flip_y: Option<bool>,
    /// 拖动平移（参考框像素；相框里那张的位移记在画框的旋转坐标系里）
    pub dx: Option<f32>,
    pub dy: Option<f32>,
}

/// 一张照片 + 它的参数
pub struct PhotoInput<'a> {
    /// 解码后的 RGBA 与尺寸
    pub rgba: Option<(&'a [u8], u32, u32)>,
    pub params: GenParams,
}

// ------------------------------------------------------------------ 合成

/// cover 基准：把 `sw×sh` 的图填满 `ref_w×ref_h` 再放大 1.15
pub fn cover_scale(ref_w: f32, ref_h: f32, sw: u32, sh: u32) -> f32 {
    if sw == 0 || sh == 0 {
        return 1.0;
    }
    (ref_w / sw as f32).max(ref_h / sh as f32) * FIT_ZOOM
}

/// 合成成品壁纸：返回（RGBA 画布，宽，高）
///
/// `inputs` 与 `cfg.photos` 一一对应（少传 = 该层不画）；`frame_asset` 是相框素材。
pub fn compose(
    cfg: &GenConfig,
    inputs: &[PhotoInput],
    frame_asset: Option<(&[u8], u32, u32)>,
) -> Result<(Vec<u8>, u32, u32)> {
    cfg.check()?;
    let (cw, ch) = (cfg.canvas.w, cfg.canvas.h);
    // 整屏画面：不留透明区（免得设备端把旧壁纸透出来）
    let mut canvas = vec![0u8; (cw as usize) * (ch as usize) * 4];
    for px in canvas.chunks_exact_mut(4) {
        px[3] = 255;
    }
    // 画布圆角（配置里默认 0 = 不裁）
    let canvas_clip = if cfg.canvas.radius > 0.5 {
        Some(round_rect_mask(
            cw,
            ch,
            GenWindow {
                x: 0.0,
                y: 0.0,
                w: cw as f32,
                h: ch as f32,
                radius: cfg.canvas.radius,
                rot: 0.0,
            },
        ))
    } else {
        None
    };

    // 框内照片的裁剪遮罩（画框窗口）只算一次
    let frame_clip = match (&cfg.frame, frame_asset) {
        (Some(f), Some(_)) => f.window.as_ref().map(|w| {
            compose_clip(
                canvas_clip.as_deref(),
                &round_rect_mask(cw, ch, w.clone()),
            )
        }),
        _ => None,
    };

    for (i, p) in cfg.photos.iter().enumerate() {
        let Some(inp) = inputs.get(i) else { continue };
        let Some((src, sw, sh)) = inp.rgba else { continue };
        let t = &p.controls;
        // 参考框：底图 = 整张画布；框内照片 = 相框窗口（位置/倾角都听配置的）
        let (ref_rect, base_rot, clip) = match p.base.as_str() {
            "frame" => {
                let Some(f) = &cfg.frame else {
                    bail!("配置里没有相框，照片 {} 无处安放", p.id);
                };
                let Some(w) = f.window.as_ref() else {
                    bail!("配置里没写画框窗口，照片 {} 无处安放", p.id);
                };
                let Some(clip) = frame_clip.clone() else {
                    bail!("相框素材还没下齐，先下载再生成");
                };
                ((w.x, w.y, w.w, w.h), w.rot, Some(clip))
            }
            _ => ((0.0, 0.0, cw as f32, ch as f32), 0.0, canvas_clip.clone()),
        };
        draw_photo_layer(
            &mut canvas, cw, ch, src, sw, sh, ref_rect, base_rot, clip, &inp.params, t,
        )?;
    }

    // 相框素材盖在最上层（尺寸与画布一致时就是 (0,0)，不一致则居中）
    if let Some((f, fw, fh)) = frame_asset {
        let x = ((cw as i32 - fw as i32) as f32 / 2.0).round() as i32;
        let y = ((ch as i32 - fh as i32) as f32 / 2.0).round() as i32;
        blit(&mut canvas, cw, ch, f, fw, fh, x, y);
    }
    Ok((canvas, cw, ch))
}

/// 两个遮罩取交（画布圆角 ∩ 相框窗口）
fn compose_clip(a: Option<&[u8]>, b: &[u8]) -> Vec<u8> {
    match a {
        Some(a) => a.iter().zip(b.iter()).map(|(x, y)| ((*x as u32 * *y as u32) / 255) as u8).collect(),
        None => b.to_vec(),
    }
}

/// 画一层照片：反向映射（目标像素 → 反平移、反旋转、除缩放、反翻转 → 源图坐标）
#[allow(clippy::too_many_arguments)]
fn draw_photo_layer(
    canvas: &mut [u8],
    cw: u32,
    ch: u32,
    src: &[u8],
    sw: u32,
    sh: u32,
    ref_rect: (f32, f32, f32, f32),
    base_rot: f32,
    clip: Option<Vec<u8>>,
    p: &GenParams,
    t: &GenControls,
) -> Result<()> {
    let (rx, ry, rw, rh) = ref_rect;
    let user_scale = t.scale.resolve(p.scale).max(0.02);
    let darken = t.darken.resolve(p.darken).clamp(0.0, 1.0);
    let blur = t.blur.resolve(p.blur).max(0.0);
    let flip_x = p.flip_x.unwrap_or(t.flip_x.default);
    let flip_y = p.flip_y.unwrap_or(t.flip_y.default);
    let rot = base_rot + t.rotation.resolve(p.rotation);
    let (dx, dy) = (p.dx.unwrap_or(0.0), p.dy.unwrap_or(0.0));

    // 总缩放（画布像素 / 源图像素）
    let mut s = cover_scale(rw, rh, sw, sh) * user_scale;
    if !s.is_finite() || s <= 0.0 {
        bail!("照片缩放不合法");
    }

    // 缩小超过一点点就先高质量重采样一次：直接抽点会出摩尔纹（照片往往比画布大得多）
    let resized;
    let (src, mut sw, mut sh) = if s < 0.95 {
        let tw = ((sw as f32 * s).round() as u32).max(1);
        let th = ((sh as f32 * s).round() as u32).max(1);
        let img = image::RgbaImage::from_raw(sw, sh, src.to_vec())
            .ok_or_else(|| anyhow::anyhow!("照片尺寸不匹配"))?;
        let out = image::imageops::resize(&img, tw, th, image::imageops::FilterType::Lanczos3);
        let used = s * (sw as f32 / tw as f32); // 重采样后的等效缩放
        resized = out.into_raw();
        let dims = (tw, th);
        s = used;
        (resized.as_slice(), dims.0, dims.1)
    } else {
        (src, sw, sh)
    };
    sw = sw.max(1) as u32;
    sh = sh.max(1) as u32;

    // 画进临时层（预乘 alpha，便于模糊），再整层合成到画布
    let n = (cw as usize) * (ch as usize) * 4;
    let mut layer = vec![0u8; n];
    let theta = rot.to_radians();
    let (cos_t, sin_t) = (theta.cos(), theta.sin());
    let (cx, cy) = (rx + rw / 2.0, ry + rh / 2.0);
    let (scx, scy) = (sw as f32 / 2.0, sh as f32 / 2.0);
    let keep = 1.0 - darken;

    for yy in 0..ch {
        for xx in 0..cw {
            // 目标点相对参考框中心的偏移（减去用户平移），再反旋转、除缩放
            let u = (xx as f32 + 0.5) - cx - dx;
            let v = (yy as f32 + 0.5) - cy - dy;
            let ru = u * cos_t + v * sin_t;
            let rv = -u * sin_t + v * cos_t;
            let mut sx = ru / s + scx;
            let mut sy = rv / s + scy;
            if flip_x {
                sx = (sw as f32 - 1.0) - sx;
            }
            if flip_y {
                sy = (sh as f32 - 1.0) - sy;
            }
            if sx < 0.0 || sy < 0.0 || sx > (sw as f32 - 1.0) || sy > (sh as f32 - 1.0) {
                continue;
            }
            let mut px = sample_bilinear(src, sw, sh, sx, sy);
            if darken > 0.0 {
                for c in 0..3 {
                    px[c] *= keep;
                }
            }
            let di = ((yy as usize * cw as usize + xx as usize) * 4) as usize;
            // 预乘（模糊要按预乘做，否则透明区会把颜色拽黑）
            let a = px[3];
            layer[di] = (px[0] * a / 255.0).round() as u8;
            layer[di + 1] = (px[1] * a / 255.0).round() as u8;
            layer[di + 2] = (px[2] * a / 255.0).round() as u8;
            layer[di + 3] = a.round() as u8;
        }
    }

    if blur > 0.5 {
        gauss_blur_rgba(&mut layer, cw, ch, blur);
    }

    // 合成到画布（source-over，预乘直接相加）
    for idx in 0..(cw as usize * ch as usize) {
        if let Some(m) = &clip {
            if m[idx] == 0 {
                continue;
            }
        }
        let di = idx * 4;
        let sa = match &clip {
            Some(m) => layer[di + 3] as u32 * m[idx] as u32 / 255,
            None => layer[di + 3] as u32,
        };
        if sa == 0 {
            continue;
        }
        if sa == 255 {
            canvas[di..di + 4].copy_from_slice(&layer[di..di + 4]);
            continue;
        }
        let inv = 255 - sa;
        for c in 0..3 {
            canvas[di + c] = (layer[di + c] as u32 + canvas[di + c] as u32 * inv / 255).min(255) as u8;
        }
        canvas[di + 3] = (sa + canvas[di + 3] as u32 * inv / 255).min(255) as u8;
    }
    Ok(())
}

/// 双线性采样（返回 0~255 的 RGBA 浮点）
fn sample_bilinear(src: &[u8], sw: u32, sh: u32, fx: f32, fy: f32) -> [f32; 4] {
    let x0 = fx.floor().max(0.0) as u32;
    let y0 = fy.floor().max(0.0) as u32;
    let x1 = (x0 + 1).min(sw - 1);
    let y1 = (y0 + 1).min(sh - 1);
    let x0 = x0.min(sw - 1);
    let y0 = y0.min(sh - 1);
    let tx = (fx - x0 as f32).clamp(0.0, 1.0);
    let ty = (fy - y0 as f32).clamp(0.0, 1.0);
    let at = |x: u32, y: u32, c: usize| src[((y as usize * sw as usize + x as usize) * 4) + c] as f32;
    let mut out = [0f32; 4];
    for c in 0..4 {
        let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
        let bot = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
        out[c] = top * (1.0 - ty) + bot * ty;
    }
    out
}

/// 高斯模糊（三次方框模糊逼近，O(1)/像素：手机上拖动滑杆要跟手）
fn gauss_blur_rgba(buf: &mut [u8], w: u32, h: u32, sigma: f32) {
    let (w, h) = (w as usize, h as usize);
    let mut f = vec![0f32; w * h * 4];
    for i in 0..w * h * 4 {
        f[i] = buf[i] as f32;
    }
    for box_w in boxes_for_gauss(sigma) {
        let r = (box_w / 2) as isize;
        box_blur_h(&mut f, w, h, r);
        box_blur_v(&mut f, w, h, r);
    }
    for i in 0..w * h * 4 {
        buf[i] = f[i].round().clamp(0.0, 255.0) as u8;
    }
}

/// 三个方框模糊的宽度（Kutskir 的 Fastest Gaussian Blur 算法）
fn boxes_for_gauss(sigma: f32) -> Vec<i32> {
    let n = 3usize;
    let sigma = sigma.max(0.1) as f64;
    let w_ideal = (12.0 * sigma * sigma / n as f64 + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma
        - (n as f64) * (wl * wl) as f64
        - 4.0 * (n as f64) * wl as f64
        - 3.0 * n as f64)
        / (-4.0 * wl as f64 - 4.0);
    let m = m_ideal.round() as i32;
    (0..n)
        .map(|i| if (i as i32) < m { wl } else { wu })
        .collect()
}

fn box_blur_h(f: &mut [f32], w: usize, h: usize, r: isize) {
    if r <= 0 {
        return;
    }
    let mut tmp = vec![0f32; w * h * 4];
    for y in 0..h {
        for c in 0..4 {
            let row = y * w * 4 + c;
            let mut sum = 0f32;
            // 边界：把首像素按半径重复
            for i in -r..=r {
                let x = i.clamp(0, w as isize - 1) as usize;
                sum += f[row + x * 4];
            }
            let div = (2 * r + 1) as f32;
            for x in 0..w {
                tmp[row + x * 4] = sum / div;
                let add = (x as isize + r + 1).clamp(0, w as isize - 1) as usize;
                let sub = (x as isize - r).clamp(0, w as isize - 1) as usize;
                sum += f[row + add * 4] - f[row + sub * 4];
            }
        }
    }
    f.copy_from_slice(&tmp);
}

fn box_blur_v(f: &mut [f32], w: usize, h: usize, r: isize) {
    if r <= 0 {
        return;
    }
    let mut tmp = vec![0f32; w * h * 4];
    for x in 0..w {
        for c in 0..4 {
            let mut sum = 0f32;
            for i in -r..=r {
                let y = i.clamp(0, h as isize - 1) as usize;
                sum += f[(y * w + x) * 4 + c];
            }
            let div = (2 * r + 1) as f32;
            for y in 0..h {
                tmp[(y * w + x) * 4 + c] = sum / div;
                let add = (y as isize + r + 1).clamp(0, h as isize - 1) as usize;
                let sub = (y as isize - r).clamp(0, h as isize - 1) as usize;
                sum += f[(add * w + x) * 4 + c] - f[(sub * w + x) * 4 + c];
            }
        }
    }
    f.copy_from_slice(&tmp);
}

/// 旋转圆角矩形的覆盖率遮罩（255 = 全可见；边缘 4× 超采样做抗锯齿）
fn round_rect_mask(cw: u32, ch: u32, win: GenWindow) -> Vec<u8> {
    let mut mask = vec![255u8; (cw as usize) * (ch as usize)];
    let hw = win.w / 2.0;
    let hh = win.h / 2.0;
    let r = win.radius.clamp(0.0, hw.min(hh));
    let (cx, cy) = (win.x + hw, win.y + hh);
    let theta = win.rot.to_radians();
    let (cos_t, sin_t) = (theta.cos(), theta.sin());
    let offs = [0.25f32, 0.75];
    for yy in 0..ch {
        for xx in 0..cw {
            let mut hit = 0u32;
            for oy in offs {
                for ox in offs {
                    let u = xx as f32 + ox - cx;
                    let v = yy as f32 + oy - cy;
                    let ru = u * cos_t + v * sin_t;
                    let rv = -u * sin_t + v * cos_t;
                    let qx = ru.abs() - (hw - r);
                    let qy = rv.abs() - (hh - r);
                    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt()
                        + qx.max(qy).min(0.0)
                        - r;
                    if outside <= 0.0 {
                        hit += 1;
                    }
                }
            }
            mask[yy as usize * cw as usize + xx as usize] = (hit * 255 / 4) as u8;
        }
    }
    mask
}

/// 整块贴上（相框素材）
fn blit(canvas: &mut [u8], cw: u32, ch: u32, src: &[u8], sw: u32, sh: u32, x: i32, y: i32) {
    for yy in 0..sh as i32 {
        let dy = y + yy;
        if dy < 0 || dy >= ch as i32 {
            continue;
        }
        for xx in 0..sw as i32 {
            let dx = x + xx;
            if dx < 0 || dx >= cw as i32 {
                continue;
            }
            let si = ((yy as u32 * sw + xx as u32) * 4) as usize;
            let a = src[si + 3] as u32;
            if a == 0 {
                continue;
            }
            let di = ((dy as u32 * cw + dx as u32) * 4) as usize;
            if a == 255 {
                canvas[di..di + 4].copy_from_slice(&src[si..si + 4]);
            } else {
                let inv = 255 - a;
                for c in 0..3 {
                    canvas[di + c] = ((src[si + c] as u32 * a + canvas[di + c] as u32 * inv) / 255) as u8;
                }
                canvas[di + 3] = (a + canvas[di + 3] as u32 * inv / 255).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_1() -> GenConfig {
        serde_json::from_str(
            r#"{"version":1,"canvas":{"w":212,"h":520},
                "photos":[{"id":"bg","label":"底图","base":"canvas"}]}"#,
        )
        .unwrap()
    }

    /// 默认值：一张比画布大的方图，应正好「填满画布 ×1.15」，四边都超出画布
    #[test]
    fn cover_fills_and_overflows() {
        let cfg = cfg_1();
        // 400x400 纯红图
        let sw = 400u32;
        let mut src = vec![0u8; (sw * sw * 4) as usize];
        for px in src.chunks_exact_mut(4) {
            px[0] = 255;
            px[3] = 255;
        }
        let inputs = vec![PhotoInput {
            rgba: Some((&src, sw, sw)),
            params: GenParams::default(),
        }];
        let (out, w, h) = compose(&cfg, &inputs, None).unwrap();
        assert_eq!((w, h), (212, 520));
        // 每行都是红的（铺满 + 不透明）
        for y in [0u32, 100, 519] {
            let i = ((y * w) * 4) as usize;
            assert_eq!(&out[i..i + 4], &[255, 0, 0, 255][..], "y={y} 应为红");
        }
        // ×1.15 后短边被裁：400×400 的图在 212 宽上 cover=520/400=1.3，×1.15=1.495
        let s = cover_scale(212.0, 520.0, 400, 400);
        assert!((s - 1.495).abs() < 1e-3, "cover 倍数 {s}");
    }

    /// 翻转：左右两半不同色的图，水平翻转后颜色对调
    #[test]
    fn flip_swaps_halves() {
        let cfg = cfg_1();
        let sw = 200u32;
        let mut src = vec![0u8; (sw * sw * 4) as usize];
        for y in 0..sw {
            for x in 0..sw {
                let i = ((y * sw + x) * 4) as usize;
                let v = if x < sw / 2 { 255 } else { 0 };
                src[i] = v;
                src[i + 3] = 255;
            }
        }
        let inputs = vec![PhotoInput {
            rgba: Some((&src, sw, sw)),
            params: GenParams::default(),
        }];
        let (out, w, _) = compose(&cfg, &inputs, None).unwrap();
        let mid = ((260 * w + 106) * 4) as usize; // 画布中线（参考框中心）
        let left = out[mid - 40 * 4];
        let right = out[mid + 40 * 4];
        assert!(left > right * 2 || right > left * 2, "翻转前左右应不同");

        let inputs2 = vec![PhotoInput {
            rgba: Some((&src, sw, sw)),
            params: GenParams {
                flip_x: Some(true),
                ..Default::default()
            },
        }];
        let (out2, _, _) = compose(&cfg, &inputs2, None).unwrap();
        let l2 = out2[mid - 40 * 4];
        let r2 = out2[mid + 40 * 4];
        assert!(l2.abs_diff(left) > 100 && r2.abs_diff(right) > 100, "翻转后应互换");
    }

    /// 相框：框内照片裁进窗口，画框外看不到它
    #[test]
    fn frame_photo_is_clipped() {
        let cfg: GenConfig = serde_json::from_str(
            r#"{"version":1,"canvas":{"w":212,"h":520},
                "photos":[{"id":"bg","base":"canvas"},
                          {"id":"inner","base":"frame"}],
                "frame":{"window":{"x":9,"y":90,"w":192,"h":302,"radius":17,"rot":0},
                         "colors":[{"i":1,"name":"红色","src":"1.png"}]}}"#,
        )
        .unwrap();
        let sw = 100u32;
        let green = vec![0u8, 255, 0, 255].repeat((sw * sw) as usize);
        let red = vec![255u8, 0, 0, 255].repeat((sw * sw) as usize);
        let inputs = vec![
            PhotoInput {
                rgba: Some((&red, sw, sw)),
                params: GenParams::default(),
            },
            PhotoInput {
                rgba: Some((&green, sw, sw)),
                params: GenParams::default(),
            },
        ];
        // 相框素材：全透明占位（blit 会整块跳过，等价于「只有形状没有画」）
        let empty = vec![0u8; (212 * 520 * 4) as usize];
        let (out, w, _) = compose(&cfg, &inputs, Some((&empty, 212, 520))).unwrap();
        let at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            (out[i], out[i + 1])
        };
        let (r, g) = at(106, 240); // 画框中心
        assert!(g > 200 && r < 60, "画框内应是框内照片（绿），实际 r={r} g={g}");
        let (r2, g2) = at(10, 10); // 画框外
        assert!(r2 > 200 && g2 < 60, "画框外应是底图（红），实际 r={r2} g={g2}");
        // 素材缺失时不许「闷头画」：直接报错
        assert!(compose(&cfg, &inputs, None).is_err(), "缺素材应报错");
    }

    /// 相框带倾角：框内照片跟着倾斜（窗口角部附近应露出底图）
    #[test]
    fn tilted_frame_rotates_inner_photo() {
        let mk = |rot: f32| -> GenConfig {
            serde_json::from_value(serde_json::json!({
                "version":1,"canvas":{"w":212,"h":520},
                "photos":[{"id":"bg","base":"canvas"},{"id":"inner","base":"frame"}],
                "frame":{"window":{"x":9,"y":90,"w":192,"h":302,"radius":0,"rot":rot},
                         "colors":[{"i":1,"src":"1.png"}]}
            }))
            .unwrap()
        };
        assert!(mk(6.0).check().is_ok());
        let sw = 100u32;
        let green = vec![0u8, 255, 0, 255].repeat((sw * sw) as usize);
        let red = vec![255u8, 0, 0, 255].repeat((sw * sw) as usize);
        let inputs = || {
            vec![
                PhotoInput {
                    rgba: Some((&red, sw, sw)),
                    params: GenParams::default(),
                },
                PhotoInput {
                    rgba: Some((&green, sw, sw)),
                    params: GenParams::default(),
                },
            ]
        };
        let empty = vec![0u8; (212 * 520 * 4) as usize];
        let (a, w, _) = compose(&mk(0.0), &inputs(), Some((&empty, 212, 520))).unwrap();
        let (b, _, _) = compose(&mk(6.0), &inputs(), Some((&empty, 212, 520))).unwrap();
        // 窗口中心 (105,241) + (94,-149)：无倾角时在窗口内（绿），转 6° 后转出去（红）
        let p = ((92 * w + 199) * 4) as usize;
        assert_eq!(a[p], 0, "无倾角：该点在画框内（绿）");
        assert!(b[p] > 200, "转过 6°：该点应露出底图（红），实际 {}", b[p]);
    }

    /// 压暗：0.5 应把白色变成约一半亮度
    #[test]
    fn darken_halves_brightness() {
        let cfg: GenConfig = serde_json::from_str(
            r#"{"version":1,"canvas":{"w":212,"h":520},
                "photos":[{"id":"bg","base":"canvas",
                  "controls":{"darken":{"default":0.5,"min":0,"max":1,"step":0.01,"adjustable":true}}}]}"#,
        )
        .unwrap();
        let sw = 50u32;
        let white = vec![255u8; (sw * sw * 4) as usize];
        let inputs = vec![PhotoInput {
            rgba: Some((&white, sw, sw)),
            params: GenParams::default(),
        }];
        let (out, w, _) = compose(&cfg, &inputs, None).unwrap();
        let i = ((260 * w + 106) * 4) as usize;
        assert!((out[i] as i32 - 128).abs() <= 2, "应压到 ~128，实际 {}", out[i]);
    }

    /// 模糊：一块纯色中央开个洞，模糊后洞会变浅（且不越界）
    #[test]
    fn blur_smears_edges() {
        let mk = |blur: f32| -> GenConfig {
            serde_json::from_value(serde_json::json!({
                "version":1,"canvas":{"w":212,"h":520},
                "photos":[{"id":"bg","base":"canvas","controls":{
                    "blur":{"default":blur,"min":0,"max":40,"step":1,"adjustable":true}}}]
            }))
            .unwrap()
        };
        let sw = 200u32;
        let mut src = vec![255u8; (sw * sw * 4) as usize];
        for y in 90..110 {
            for x in 90..110 {
                let i = ((y * sw + x) * 4) as usize;
                src[i] = 0;
                src[i + 1] = 0;
                src[i + 2] = 0;
            }
        }
        let inputs = vec![PhotoInput {
            rgba: Some((&src, sw, sw)),
            params: GenParams::default(),
        }];
        let (sharp, w, _) = compose(&mk(0.0), &inputs, None).unwrap();
        let (soft, _, _) = compose(&mk(8.0), &inputs, None).unwrap();
        // 黑块边缘外侧 2 个源像素处（×2.99 ≈ 画布上 6 px）：不模糊是白的，模糊后被黑块染灰
        let i = ((296 * w + 106) * 4) as usize;
        assert!(sharp[i] > 240, "不模糊时边缘外仍是白的");
        assert!(
            soft[i] < sharp[i] - 40,
            "模糊后边缘应被黑块染灰（{} → {}）",
            sharp[i],
            soft[i]
        );
    }

    /// 尺寸永远对：不同画布都返回配置里的尺寸
    #[test]
    fn canvas_size_is_exact() {
        for (w, h) in [(212u32, 520u32), (192, 490), (336, 480), (432, 514)] {
            let cfg: GenConfig = serde_json::from_value(serde_json::json!({
                "version":1,"canvas":{"w":w,"h":h},
                "photos":[{"id":"bg","base":"canvas"}]
            }))
            .unwrap();
            let sw = 33u32;
            let src = vec![200u8; (sw * sw * 4) as usize];
            let inputs = vec![PhotoInput {
                rgba: Some((&src, sw, sw)),
                params: GenParams::default(),
            }];
            let (out, ow, oh) = compose(&cfg, &inputs, None).unwrap();
            assert_eq!((ow, oh), (w, h));
            assert_eq!(out.len(), (w * h * 4) as usize);
        }
    }
}
