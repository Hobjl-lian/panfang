//! 表盘图层模型（**逐样式**）+ 预览渲染器。
//!
//! 全部规则来自 14 个真实工程的统计 + AstroBox 反编译产物（manifest.xml）与
//! AstroBox 渲染图（preview_theme_N.png）的**像素级对照**（2026-10-06）。
//!
//! ## 关键结论（务必遵守）
//! 1. 块 2/3/7/8/9 的记录 `idx` 是**全局 id**（跨样式共享；14 个工程零冲突），
//!    只有块 0（元素）是**逐样式**列表。图层必须逐样式读 —— 否则切样式后元素错位。
//! 2. 元素记录 `{u32 targetId, u16 x, u16 y}`，targetId 高字节 = 资源类型：
//!    `0x02` 图片 / `0x07` 数据项（块7 记录）/ `0x08` 槽位 / `0x09` 动作（块9 记录）。
//! 3. **槽位里存的是动作（块 9）序号**，不是组件序号。动作 = 复合组件：
//!    ```text
//!    +0x00 name[32] 显示名（"玻璃-细"…）
//!    +0x20 u32 ?
//!    +0x24 u32 预览图 ref（0x02xxxxxx）
//!    +0x28 u32 flags（低 8 位 = 项数 N）
//!    +0x2C N × {u16 x, u16 y, u32 ref}
//!    之后 u32 ?  +  u16 高  +  u16 宽   （记录总长 = 0x34 + 8N）
//!    ```
//! 4. 动作内**所有项都在 (0,0) 且 N ≥ 2** → 横向 flex 行：行内间距由动作的「宽」反解
//!    `gap = (宽 - Σ项宽) / (N-1)`（实测 178/186/206 三例精确吻合）；项在行内垂直居中。
//!    否则按项的 (x, y) 绝对定位。
//! 5. 数据项绘制：
//!    · 数字：`step = 帧宽 + (字距码 - 256)`（字距码在记录 +13），
//!      总宽 = Σ帧宽 + (N-1)×step；对齐 `0=右 / 1=左 / 2=居中`（16 个工程统计）
//!    · 值列表（ImageValues）：单帧，**以锚点为左上角**绘制（不居中）
//! 6. 背景 = 样式头 backImageId（`0x82xxxxxx`）；息屏样式（预览偏移 0）= 纯黑。

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use crate::watchface_bin;
use crate::watchface_edit::{device_screen, ScreenInfo};

// ---------------------------------------------------------------- 模型（前端）

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrayModel {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub frames: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataItemModel {
    /// 块 7 记录 idx（元素/动作里的 `0x07xxxxxx` 引用它）
    pub index: u32,
    pub kind: String,   // "number" | "values"
    pub source: String, // timeHour / dateWeek / …
    pub array: u32,
    pub digits: u32,
    pub space: i32, // 字距码-256（负值 = 重叠）
    pub step: i32,  // 实际步进 = 帧宽 + space
    pub align: u32, // 0=右 1=左 2=居中
    pub leading_zero: bool,
    pub parameter: u32,
}

/// 动作内的一个项：kind = data(数据项) / image(图片) / action(嵌套动作)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionItemModel {
    pub kind: String,
    pub r: u32,
    pub x: i32,
    pub y: i32,
}

/// 动作（块 9）= AstroBox 的 Widget：可编辑容器的候选项
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionModel {
    pub index: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// 横向 flex 行（所有项都在 (0,0)）
    pub flex_row: bool,
    pub gap: i32,
    pub preview_image: Option<u32>,
    pub items: Vec<ActionItemModel>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotModel {
    pub index: u32,
    /// 候选动作序号（默认取第一个）
    pub actions: Vec<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerModel {
    pub kind: String, // "image" | "slot" | "data" | "action"
    pub r: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeModelV2 {
    pub index: usize,
    pub name: String,
    pub kind: String, // "normal" | "aod"
    pub bg_image: Option<u32>,
    pub bg_color: Option<String>,
    pub layers: Vec<LayerModel>,
    pub preview_png_base64: Option<String>,
    /// 该样式实际引用的容器槽位（layers 里 kind=="slot" 的 r）—— 编辑页按样式过滤容器卡片
    pub slots: Vec<u32>,
    /// 该样式图片/精灵记录里出现过的配色组 —— 编辑页按样式过滤配色选项
    pub color_groups: Vec<u32>,
}

/// 配色（colorGroup）一组。**.bin 里没有配色的名字/色值**（只存在设计工程
/// `editor.config.json` 的 `colorGroupTable` 里），所以名字用序号，另给一张该配色的
/// 壁纸缩略图当「色样」，让用户看得出区别。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorGroupModel {
    pub index: u32,
    pub name: String,
    /// 该配色的背景图缩略图（PNG base64），无背景图时为 None
    pub swatch_png_base64: Option<String>,
    /// 该配色自己存了多少条图片记录（不含的 idx 会回退用基础配色的图）
    pub image_count: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerModelDoc {
    pub watchface_id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub themes: Vec<ThemeModelV2>,
    pub arrays: Vec<ArrayModel>,
    pub slots: Vec<SlotModel>,
    pub actions: Vec<ActionModel>,
    pub data_items: Vec<DataItemModel>,
    /// 配色列表（长度 ≥ 1；> 1 时前端显示配色选择）
    pub color_groups: Vec<ColorGroupModel>,
    /// **是不是照片表盘**（可在手环上换壁纸）。
    ///
    /// 判据 = 亮屏样式的 `backImageId` 高字节为 **`0x82`**（「可替换的照片容器」），
    /// 普通静态背景是 `0x80`（低 24 位常为 0）。全语料 15/15 与 AstroBox 反编译出的
    /// `description.xml` 的 `watchfaceType`（photoAlbum / normal）一致；设备侧上报的
    /// `WatchFaceItem.support_image_group` 与它同源（照片表盘为 true）。
    /// 只有它为真时，编辑页才显示「壁纸（照片）」这一块。
    pub photo_album: bool,
    /// 屏幕信息（宽高 + 外壳物理圆角 + 形状）：编辑页预览框与裁剪蒙版按它等比缩放/裁切。
    /// 尺寸与渲染出的图一致（同一个 width/height），所以框与图不会错位。
    pub screen: ScreenInfo,
    /// 当前**连接设备**的屏幕信息（型号未知时为 None）。
    /// 与 `screen` 不一致时说明「这个表盘不是为当前设备做的」，前端可以给出提示。
    pub device_screen: Option<ScreenInfo>,
}

// ---------------------------------------------------------------- 表（内部）

fn u16le(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

fn u32le(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// 块 7 记录（数据项）。
/// flags = 类型(高 4 位) | 前导零(bit2) | 对齐(低 2 位)；对齐 0=右 1=左 2=居中。
/// 字距码在 +13，是**有符号字节**（220 → -36，0 → 0，1 → 1）；+16 = 单位图引用。
#[derive(Debug, Clone, Copy)]
struct Rec {
    shape: u8,
    data_src: u8,
    digits: u8,
    flags: u8,
    parameter: u32,
    /// 原始 targetId（高字节 = 引用类型：0x03 图片组 / 0x02 单图）
    target: u32,
    space: i32,
    unit_icon: Option<u32>,
    /// 环形进度（type 4）字段
    bg_image: Option<u32>,
    value_range: i32,
    pivot_x: f32,
    pivot_y: f32,
    angle_start: f32,
    angle_range: f32,
    bar_radius: f32,
    bar_width: f32,
    /// 线性进度（type 5）字段
    start_x: f32,
    start_y: f32,
    end_x: f32,
    end_y: f32,
    value_start: i32,
}

impl Default for Rec {
    fn default() -> Self {
        Rec {
            shape: 0, data_src: 0, digits: 0, flags: 0, parameter: 0, target: 0,
            space: 0, unit_icon: None, bg_image: None, value_range: 0,
            pivot_x: 0.0, pivot_y: 0.0, angle_start: 0.0, angle_range: 0.0,
            bar_radius: 0.0, bar_width: 0.0, start_x: 0.0, start_y: 0.0,
            end_x: 0.0, end_y: 0.0, value_start: 0,
        }
    }
}

impl Rec {
    /// 1=数字 2=值列表 4=环形进度（3=指针 5=线性进度 6=文本，未实现，退化画贴图）
    fn type_id(&self) -> u8 {
        self.flags >> 4
    }
    fn align(&self) -> u32 {
        (self.flags & 0x03) as u32
    }
    fn leading_zero(&self) -> bool {
        self.flags & 0x04 != 0
    }
    fn is_values(&self) -> bool {
        self.type_id() == 2
    }
    /// 引用类型：0x03 = 图片组，0x02 = 单图
    fn target_kind(&self) -> u8 {
        ((self.target >> 24) & 0x7F) as u8
    }
    fn target_idx(&self) -> u32 {
        self.target & 0x00FF_FFFF
    }
}

#[derive(Debug, Clone, Copy)]
struct ActItem {
    r: u32,
    x: i32,
    y: i32,
}

fn ref_kind(r: u32) -> &'static str {
    match (r >> 24) & 0x7F {
        0x02 => "image",
        0x07 => "data",
        0x09 => "action",
        _ => "unknown",
    }
}

#[derive(Debug, Clone)]
struct Act {
    name: String,
    preview: Option<u32>,
    items: Vec<ActItem>,
    w: u32,
    h: u32,
}

impl Act {
    /// 所有项都在原点（且不止一项）→ 横向 flex 行
    fn flex_row(&self) -> bool {
        self.items.len() > 1 && self.items.iter().all(|i| i.x == 0 && i.y == 0)
    }
}

/// 跨样式合并的全局表（同 idx 内容一致，已用 14 个工程验证）
///
/// **多配色**（实测「测试工程3」，2026-10-06）：图片（块 2）/图片组（块 3）的记录项
/// 低字节带配色序号（`watchface_bin::color_group_of`），同一 `idx` **每个配色一份记录**；
/// 元素/数据项里的引用**不带配色位**。所以这里按 `idx → [(配色, 记录)]` 存，
/// 取图时用「当前配色」，该配色没有这条记录时回退到配色 0（工程里只有配色 0 有
/// 组件缩略图这类编辑器专用素材，手环侧同理按基础配色取）。
struct Tables {
    /// idx -> 各配色的图片记录负载偏移（同一 idx 每配色一条；顺序 = 文件里的记录顺序）
    images: HashMap<u32, Vec<(u32, usize)>>,
    /// idx -> 各配色的图片组 (w, h, 各帧字节范围)
    arrays: HashMap<u32, Vec<(u32, (u32, u32, Vec<(usize, usize)>))>>,
    recs: HashMap<u32, Rec>,
    acts: HashMap<u32, Act>,
    slots: HashMap<u32, Vec<u32>>,
    /// 动作项尺寸：8（多数）或 12（"时序"这类 12 张块表的变体）
    act_item_size: usize,
    /// 配色组数量（≥1；1 = 单配色表盘）
    color_groups: u32,
    /// 当前取图用的配色（渲染按配色选图，而取图调用点遍布渲染器 → 用 Cell 免去层层传参）
    cg: std::cell::Cell<u32>,
}

/// 动作项尺寸判定：对整文件的动作记录，统计「记录长 = 0x2C + 尺寸×项数 [+ 8]」
/// 且所有项引用合法（0x02/0x07/0x09）的命中数，取多者（平手取 8）。
fn detect_action_item_size(d: &[u8], styles: &[(usize, watchface_bin::StyleSection)]) -> usize {
    let valid = |r: u32| matches!((r >> 24) & 0x7F, 0x02 | 0x07 | 0x09);
    let mut score = [0usize, 0usize]; // [8 字节, 12 字节]
    for (_, st) in styles.iter() {
        let Some(b) = st.blocks.iter().find(|b| b.index == 9 && b.count > 0) else {
            continue;
        };
        for rec in watchface_bin::block_records(d, b.count, b.offset) {
            let o = rec.data_offset as usize;
            let len = rec.data_len as usize;
            if len < 0x30 || o + 0x30 > d.len() {
                continue;
            }
            let n = (u32le(d, o + 0x28) & 0xFF) as usize;
            if n == 0 || n > 64 {
                continue;
            }
            for (i, isz) in [(0usize, 8usize), (1, 12)] {
                let base = 0x2C + isz * n;
                if len == base || len == base + 8 {
                    let ok = (0..n).all(|k| valid(u32le(d, o + 0x2C + isz * k + 4)));
                    if ok {
                        score[i] += 1;
                    }
                }
            }
        }
    }
    if score[1] > score[0] {
        12
    } else {
        8
    }
}

/// 解析图片组记录 → (w, h, 各帧的字节范围)
fn parse_array_frames(
    d: &[u8],
    data_off: usize,
    data_len: usize,
) -> Option<(u32, u32, Vec<(usize, usize)>)> {
    if data_len < 12 || data_off + 12 > d.len() {
        return None;
    }
    let w = u16le(d, data_off + 4) as u32;
    let h = u16le(d, data_off + 6) as u32;
    let total = u32le(d, data_off + 8) as usize;
    if total + 12 > data_len {
        return None;
    }
    let n = (data_len - total - 12) / 4;
    if n == 0 || w == 0 || h == 0 {
        return None;
    }
    let mut frames = Vec::with_capacity(n);
    let mut pos = data_off + 12 + n * 4;
    for i in 0..n {
        let size = u32le(d, data_off + 12 + i * 4) as usize;
        frames.push((pos, size));
        pos += size;
    }
    Some((w, h, frames))
}

impl Tables {
    fn build(d: &[u8]) -> Result<Self> {
        let header = watchface_bin::parse_header(d)?;
        let styles = watchface_bin::walk_styles(d, &header)?;
        let mut t = Tables {
            arrays: HashMap::new(),
            images: HashMap::new(),
            recs: HashMap::new(),
            acts: HashMap::new(),
            slots: HashMap::new(),
            act_item_size: detect_action_item_size(d, &styles),
            color_groups: 1,
            cg: std::cell::Cell::new(0),
        };
        for (_, st) in styles.iter() {
            let block = |i: usize| st.blocks.iter().find(|b| b.index == i && b.count > 0);
            if let Some(b) = block(2) {
                for rec in watchface_bin::block_records(d, b.count, b.offset) {
                    let cg = watchface_bin::color_group_of(rec.unk);
                    t.color_groups = t.color_groups.max(cg + 1);
                    let e = t.images.entry(rec.idx as u32).or_default();
                    if !e.iter().any(|(c, _)| *c == cg) {
                        e.push((cg, rec.data_offset as usize));
                    }
                }
            }
            if let Some(b) = block(3) {
                for rec in watchface_bin::block_records(d, b.count, b.offset) {
                    let cg = watchface_bin::color_group_of(rec.unk);
                    t.color_groups = t.color_groups.max(cg + 1);
                    let e = t.arrays.entry(rec.idx as u32).or_default();
                    if e.iter().any(|(c, _)| *c == cg) {
                        continue;
                    }
                    if let Some((w, h, frames)) =
                        parse_array_frames(d, rec.data_offset as usize, rec.data_len as usize)
                    {
                        e.push((cg, (w, h, frames)));
                    }
                }
            }
            if let Some(b) = block(7) {
                for rec in watchface_bin::block_records(d, b.count, b.offset) {
                    let idx = rec.idx as u32;
                    if t.recs.contains_key(&idx) {
                        continue;
                    }
                    let o = rec.data_offset as usize;
                    if rec.data_len < 12 || o + 14 > d.len() {
                        continue;
                    }
                    let long = rec.data_len >= 20;
                    let unit = if long { u32le(d, o + 16) } else { 0 };
                    let flags = d[o + 3];
                    let type_id = flags >> 4;
                    let mut r = Rec {
                        shape: d[o],
                        data_src: d[o + 1],
                        digits: d[o + 2],
                        flags,
                        parameter: if long { u16le(d, o + 6) as u32 } else { 0 },
                        target: u32le(d, o + 8),
                        space: if long && type_id != 4 { d[o + 13] as i8 as i32 } else { 0 },
                        unit_icon: if long && type_id != 4 && (unit >> 24) & 0x7F == 0x02 {
                            Some(unit & 0x00FF_FFFF)
                        } else {
                            None
                        },
                        ..Default::default()
                    };
                    // 指针（type 3，32 字节，实测偏移）：
                    // +C 值起点(u32) / +11 值域(u16) / +14,+16 轴心 / +18,+1A 角度×10
                    if type_id == 3 && rec.data_len >= 28 {
                        r.value_start = u32le(d, o + 12) as i32;
                        r.value_range = u16le(d, o + 17) as i32;
                        r.pivot_x = u16le(d, o + 20) as f32;
                        r.pivot_y = u16le(d, o + 22) as f32;
                        r.angle_start = u16le(d, o + 24) as f32 / 10.0;
                        r.angle_range = u16le(d, o + 26) as f32 / 10.0;
                    }
                    // 线性进度（type 5，40 字节，实测偏移）：
                    // +8 进度图 / +C 背景图 / +14 值起点(u32) / +19 值域(u16) /
                    // +1B,+1D 线起点 / +20,+22 线终点 / +26 线宽
                    if type_id == 5 && rec.data_len >= 40 {
                        let bar = u32le(d, o + 8);
                        let bg = u32le(d, o + 12);
                        if (bar >> 24) & 0x7F == 0x02 {
                            r.target = bar;
                        }
                        if (bg >> 24) & 0x7F == 0x02 {
                            r.bg_image = Some(bg & 0x00FF_FFFF);
                        }
                        r.value_start = u32le(d, o + 20) as i32;
                        r.value_range = u16le(d, o + 25) as i32;
                        r.start_x = u16le(d, o + 27) as f32;
                        r.start_y = u16le(d, o + 29) as f32;
                        r.end_x = u16le(d, o + 32) as f32;
                        r.end_y = u16le(d, o + 34) as f32;
                        r.bar_width = u16le(d, o + 38) as f32;
                    }
                    // 环形进度（type 4）：引用字段位置随记录变体浮动（+8 或 +9），
                    // 用「连续两个 0x02xxxxxx 引用」定位；尾部字段位置固定（实测 +26 起）。
                    if type_id == 4 && rec.data_len >= 40 {
                        for off in [8usize, 9, 10, 11, 12] {
                            let bar = u32le(d, o + off);
                            let bg = u32le(d, o + off + 4);
                            if (bar >> 24) & 0x7F == 0x02 && (bg >> 24) & 0x7F == 0x02 {
                                r.target = bar;
                                r.bg_image = Some(bg & 0x00FF_FFFF);
                                break;
                            }
                        }
                        r.value_range = u16le(d, o + 26) as i32;
                        r.pivot_x = u16le(d, o + 29) as f32;
                        r.pivot_y = u16le(d, o + 31) as f32;
                        r.angle_start = u16le(d, o + 33) as f32 / 10.0;
                        r.angle_range = u16le(d, o + 35) as f32 / 10.0;
                        r.bar_radius = u16le(d, o + 37) as f32;
                        r.bar_width = u16le(d, o + 39) as f32;
                    }
                    t.recs.insert(idx, r);
                }
            }
            if let Some(b) = block(8) {
                for rec in watchface_bin::block_records(d, b.count, b.offset) {
                    let idx = rec.idx as u32;
                    if t.slots.contains_key(&idx) {
                        continue;
                    }
                    let o = rec.data_offset as usize;
                    if o + 4 > d.len() {
                        continue;
                    }
                    let n = u32le(d, o) as usize;
                    let mut ids = Vec::with_capacity(n);
                    for i in 0..n {
                        let p = o + 4 + i * 4;
                        if p + 4 > d.len() {
                            break;
                        }
                        ids.push(u32le(d, p) & 0x00FF_FFFF);
                    }
                    t.slots.insert(idx, ids);
                }
            }
            if let Some(b) = block(9) {
                for rec in watchface_bin::block_records(d, b.count, b.offset) {
                    let idx = rec.idx as u32;
                    if t.acts.contains_key(&idx) {
                        continue;
                    }
                    let o = rec.data_offset as usize;
                    let len = rec.data_len as usize;
                    if len < 0x30 || o + 0x30 > d.len() {
                        continue;
                    }
                    let name = d[o..o + 32]
                        .split(|b| *b == 0)
                        .next()
                        .map(|s| String::from_utf8_lossy(s).to_string())
                        .unwrap_or_default();
                    let preview_ref = u32le(d, o + 0x24);
                    let flags = u32le(d, o + 0x28);
                    let n = (flags & 0xFF) as usize;
                    // 项尺寸随版本变体：8 字节（多数）或 12 字节（如"时序"，12 张块表）。
                    // 由文件级统计判定（见 detect_action_item_size）。
                    let isz = t.act_item_size;
                    let n = n.min(len.saturating_sub(0x2C) / isz).min(64);
                    let mut items = Vec::with_capacity(n);
                    for i in 0..n {
                        let p = o + 0x2C + i * isz;
                        items.push(ActItem {
                            x: u16le(d, p) as i32,
                            y: u16le(d, p + 2) as i32,
                            r: u32le(d, p + 4),
                        });
                    }
                    // 项之后的尾部两段都可选（实测）：
                    //   ① 若剩余首字段形如图片引用 0x02xxxxxx → 附加图（4 字节）
                    //   ② 之后若还有 4 字节 → {u16 高, u16 宽}
                    // 另有变体把「高,宽」直接放在项后（首字段非图片引用），随后可能还有 4 字节附加。
                    let mut tail = o + 0x2C + n * isz;
                    let read_hw = |p: usize| -> Option<(u32, u32)> {
                        if p + 4 > o + len {
                            return None;
                        }
                        let (h, w) = (u16le(d, p) as u32, u16le(d, p + 2) as u32);
                        if h == 0 || w == 0 || h > 2048 || w > 2048 {
                            return None;
                        }
                        Some((h, w))
                    };
                    if tail + 4 <= o + len && (u32le(d, tail) >> 24) & 0x7F == 0x02 {
                        tail += 4; // 附加图引用（组件缩略图）
                    }
                    let (h, w) = if let Some(hw) = read_hw(tail) {
                        hw
                    } else {
                        // 尺寸字段为 0 的变体：再跳过 4 字节取尺寸
                        read_hw(tail + 4).unwrap_or((0, 0))
                    };
                    t.acts.insert(
                        idx,
                        Act {
                            name,
                            preview: if (preview_ref >> 24) & 0x7F == 0x02 {
                                Some(preview_ref & 0x00FF_FFFF)
                            } else {
                                None
                            },
                            items,
                            w,
                            h,
                        },
                    );
                }
            }
        }
        Ok(t)
    }

    /// 按配色从「同一 idx 的多套记录」里挑一套：精确配色 → 配色 0 → 任意一套
    fn pick<'b, T>(list: &'b [(u32, T)], cg: u32) -> Option<&'b T> {
        if let Some((_, v)) = list.iter().find(|(c, _)| *c == cg) {
            return Some(v);
        }
        if let Some((_, v)) = list.iter().find(|(c, _)| *c == 0) {
            return Some(v);
        }
        list.first().map(|(_, v)| v)
    }

    fn color_group(&self) -> u32 {
        self.cg.get()
    }

    fn set_color_group(&self, cg: u32) {
        self.cg.set(cg);
    }

    fn image_off_at(&self, idx: u32, cg: u32) -> Option<usize> {
        Self::pick(self.images.get(&idx)?, cg).copied()
    }

    fn image_size_at(&self, d: &[u8], idx: u32, cg: u32) -> Option<(u32, u32)> {
        let off = self.image_off_at(idx, cg)?;
        let head = watchface_bin::parse_image_head(d, off).ok()?;
        Some((head.width as u32, head.height as u32))
    }

    fn image_size(&self, d: &[u8], idx: u32) -> Option<(u32, u32)> {
        self.image_size_at(d, idx, self.color_group())
    }

    fn image_rgba_at(&self, d: &[u8], idx: u32, cg: u32) -> Option<(Vec<u8>, u32, u32)> {
        let off = self.image_off_at(idx, cg)?;
        let (_, rgba, _) = watchface_bin::decode_image(d, off).ok()?;
        let rgba = rgba?;
        let (w, h) = self.image_size_at(d, idx, cg)?;
        Some((rgba, w, h))
    }

    fn image_rgba(&self, d: &[u8], idx: u32) -> Option<(Vec<u8>, u32, u32)> {
        self.image_rgba_at(d, idx, self.color_group())
    }

    /// 图片组（按当前配色）
    fn array_at(&self, idx: u32, cg: u32) -> Option<&(u32, u32, Vec<(usize, usize)>)> {
        Self::pick(self.arrays.get(&idx)?, cg)
    }

    fn array(&self, idx: u32) -> Option<&(u32, u32, Vec<(usize, usize)>)> {
        self.array_at(idx, self.color_group())
    }

    /// 数据项 target 的尺寸（高字节决定是图片组 0x03 还是单图 0x02）
    fn target_size(&self, d: &[u8], target: u32) -> Option<(u32, u32)> {
        match ((target >> 24) & 0x7F) as u8 {
            0x03 => self
                .array(target & 0x00FF_FFFF)
                .map(|(w, h, _)| (*w, *h)),
            0x02 => self.image_size(d, target & 0x00FF_FFFF),
            _ => None,
        }
    }

    /// 数据项 target 的帧数（单图 = 1 帧）
    fn target_frames(&self, target: u32) -> usize {
        match ((target >> 24) & 0x7F) as u8 {
            0x03 => self
                .array(target & 0x00FF_FFFF)
                .map(|(_, _, f)| f.len())
                .unwrap_or(0),
            0x02 => 1,
            _ => 0,
        }
    }
}

/// 解码图片组的一帧 → RGBA
fn decode_array_frame(d: &[u8], frame_off: usize, w: u32, h: u32) -> Option<Vec<u8>> {
    if frame_off + 8 > d.len() || u32le(d, frame_off) != watchface_bin::MAGIC_RLE {
        return None;
    }
    let hdr = u32le(d, frame_off + 4);
    let unit = (hdr & 0xF) as usize;
    let out_len = (hdr >> 4) as usize;
    let raw = watchface_bin::uncompress_rle_v10(&d[frame_off + 8..], out_len, unit);
    if raw.len() != 1024 + (w as usize) * (h as usize) {
        return None;
    }
    watchface_bin::indexed8_to_rgba(&raw, w, h).ok()
}

// ---------------------------------------------------------------- 数据源字典

/// (shape, dataSrc) → 数据源名。取自 14 个真实工程与 AstroBox manifest 的对照统计。
fn source_by_code(shape: u8, ds: u8) -> String {
    let s = match (shape, ds) {
        (0x08, 0x11) => "timeHour",
        (0x09, 0x11) => "timeHourLow",
        (0x0a, 0x11) => "timeHourHigh",
        (0x10, 0x11) => "timeMinute",
        (0x11, 0x11) => "timeMinuteLow",
        (0x12, 0x11) => "timeMinuteHigh",
        (0x18, 0x11) => "timeSecond",
        (0x08, 0x12) => "dateYear",
        (0x10, 0x12) => "dateMonth",
        (0x18, 0x12) => "dateDay",
        (0x20, 0x12) => "dateWeek",
        (0x20, 0x13) => "miscTimeSection",
        (0x08, 0x21) => "healthStepCount",
        (0x10, 0x21) => "healthStepProgress",
        (0x20, 0x21) => "healthStepTarget",
        (0x10, 0x22) => "healthHeartRateZone",
        (0x10, 0x23) => "healthCalorieProgress",
        (0x10, 0x24) => "healthStandProgress",
        (0x08, 0x26) => "healthPressureIndex",
        (0x18, 0x28) => "healthSleepTargetProgress",
        (0x10, 0x29) => "healthExerciseProgress",
        (0x20, 0x2b) => "source_2b20",
        (0x08, 0x22) => "healthHeartRate",
        (0x18, 0x22) => "healthHeartRateMin",
        (0x20, 0x22) => "healthHeartRateMax",
        (0x08, 0x23) => "healthCalorieValue",
        (0x18, 0x23) => "healthCalorieTarget",
        (0x08, 0x24) => "healthStandCount",
        (0x18, 0x24) => "healthStandTarget",
        (0x08, 0x28) => "healthSleepDuration",
        (0x18, 0x2b) => "source_2b18",
        (0x08, 0x41) => "systemStatusBattery",
        (0x20, 0x31) => "weatherCurrentTemperature",
        (0x28, 0x31) => "weatherCurrentHumidity",
        (0x30, 0x31) => "weatherCurrentWeather",
        (0x48, 0x31) => "weatherCurrentWindLevel",
        (0x50, 0x31) => "weatherCurrentAirQualityIndex",
        (0x78, 0x31) => "weatherCurrentUVIndex",
        (0xd0, 0x31) => "weatherCurrentTemperatureFahrenheit",
        (0x18, 0x32) => "weatherTodayTemperatureMax",
        (0x20, 0x32) => "weatherTodayTemperatureMin",
        (0x68, 0x32) => "weatherTodayTemperatureMaxFahrenheit",
        (0x70, 0x32) => "weatherTodayTemperatureMinFahrenheit",
        _ => return format!("source_{:02x}{:02x}", shape, ds),
    };
    s.to_string()
}

/// 示例值（预览用；与 AstroBox 预览一致的 09:28 / 16 日 / 周日）
fn example_value(source: &str) -> i64 {
    match source {
        "timeHour" | "timeHourLow" | "timeHourHigh" => 9,
        "timeMinute" | "timeMinuteLow" | "timeMinuteHigh" => 28,
        "timeSecond" => 45,
        "dateDay" => 16,
        "dateMonth" => 10,
        "dateYear" => 25,
        "dateWeek" => 0, // 帧序号：周日
        "miscTimeSection" => 0, // 时段皮肤：白天
        "systemStatusBattery" => 76,
        "healthStepCount" => 8888,
        "healthStepTarget" => 10000,
        "healthHeartRate" => 72,
        "healthHeartRateMin" => 58,
        "healthHeartRateMax" => 126,
        "healthCalorieValue" => 320,
        "healthCalorieTarget" => 500,
        "healthStandCount" => 6,
        "healthStandTarget" => 12,
        "healthSleepDuration" => 7,
        // 进度类（环形/线性进度条）：用百分比，让预览里的弧线可见
        "healthStepProgress"
        | "healthCalorieProgress"
        | "healthStandProgress"
        | "healthExerciseProgress"
        | "healthHeartRateZone"
        | "healthSleepTargetProgress"
        | "healthPressureIndex" => 65,
        "weatherCurrentTemperature" | "weatherTodayTemperatureMax" => 26,
        "weatherTodayTemperatureMin" => 18,
        "weatherCurrentWeather" => 0,
        "weatherCurrentHumidity" => 62,
        "weatherCurrentWindLevel" => 3,
        "weatherCurrentAirQualityIndex" => 56,
        "weatherCurrentUVIndex" => 4,
        _ => 88,
    }
}

/// 数字型数据项 → 待绘制的字符串（字符 → 帧序号：'0'-'9'=0-9, '-'=10, '.'=11）。
/// 仅在 leadingZero 置位时补零（bit2 of flags）。
fn number_text(rec: &Rec, source: &str) -> String {
    let v = example_value(source);
    let mut s = v.to_string();
    let want = rec.digits.max(1) as usize;
    if rec.leading_zero() && s.len() < want {
        s = format!("{}{}", "0".repeat(want - s.len()), s);
    }
    s
}

/// 数字型数据项的绘制宽度（含 `space` 分隔与单位图）
fn number_width(rec: &Rec, t: &Tables, d: &[u8], text: &str, frame_w: u32) -> i32 {
    let n = text.chars().count() as i32;
    let mut w = frame_w as i32 + (n - 1).max(0) * (frame_w as i32 + rec.space);
    if let Some(u) = rec.unit_icon {
        if let Some((uw, _)) = t.image_size(d, u) {
            w += rec.space + uw as i32;
        }
    }
    w
}

fn char_frame(c: char) -> usize {
    match c {
        '0'..='9' => (c as u8 - b'0') as usize,
        '-' => 10,
        '.' => 11,
        _ => 0,
    }
}

// ---------------------------------------------------------------- 推导

pub fn derive_model(d: &[u8], device_model: Option<&str>) -> Result<LayerModelDoc> {
    let header = watchface_bin::parse_header(d)?;
    let styles = watchface_bin::walk_styles(d, &header)?;
    let t = Tables::build(d)?;

    // 数组 / 数据项 / 动作 / 槽位（全局表，按 idx 排序输出）
    let mut arrays: Vec<ArrayModel> = t
        .arrays
        .iter()
        .filter_map(|(idx, list)| {
            // 多配色时同一 idx 多套记录；模型里只报一套（尺寸/帧数各配色一致）
            let (_, (w, h, f)) = list.first()?;
            Some(ArrayModel {
                index: *idx,
                width: *w,
                height: *h,
                frames: f.len(),
            })
        })
        .collect();
    arrays.sort_by_key(|a| a.index);

    let mut data_items: Vec<DataItemModel> = t
        .recs
        .iter()
        .map(|(idx, r)| {
            let (aw, _ah) = t.target_size(d, r.target).unwrap_or((0, 0));
            let source = source_by_code(r.shape, r.data_src);
            DataItemModel {
                index: *idx,
                kind: match r.type_id() {
                    2 => "values".into(),
                    4 => "arcProgress".into(),
                    3 => "pointer".into(),
                    5 => "lineProgress".into(),
                    6 => "text".into(),
                    _ => "number".into(),
                },
                source,
                array: r.target,
                digits: r.digits.max(1) as u32,
                space: r.space,
                step: aw as i32 + r.space,
                align: r.align(),
                leading_zero: true,
                parameter: r.parameter,
            }
        })
        .collect();
    data_items.sort_by_key(|x| x.index);

    let mut actions: Vec<ActionModel> = t
        .acts
        .iter()
        .map(|(idx, a)| ActionModel {
            index: *idx,
            name: a.name.clone(),
            width: a.w,
            height: a.h,
            flex_row: a.flex_row(),
            gap: group_gap(&t, d, a, 0),
            preview_image: a.preview,
            items: a
                .items
                .iter()
                .map(|i| ActionItemModel {
                    kind: ref_kind(i.r).to_string(),
                    r: i.r & 0x00FF_FFFF,
                    x: i.x,
                    y: i.y,
                })
                .collect(),
        })
        .collect();
    actions.sort_by_key(|a| a.index);

    let mut slots: Vec<SlotModel> = t
        .slots
        .iter()
        .map(|(idx, list)| SlotModel {
            index: *idx,
            actions: list.clone(),
        })
        .collect();
    slots.sort_by_key(|s| s.index);

    // 主题（逐样式读元素列表）
    let mut themes = Vec::new();
    let mut photo_album = false;
    for (_, st) in styles.iter() {
        let is_aod = st.preview_offset == 0;
        // 照片表盘判据：亮屏样式的 backImageId 高字节 = 0x82（可替换的照片容器）
        if !is_aod && (st.back_image_id >> 24) == 0x82 {
            photo_album = true;
        }
        let mut layers: Vec<LayerModel> = Vec::new();
        if let Some(b) = st.blocks.iter().find(|b| b.index == 0 && b.count > 0) {
            for rec in watchface_bin::block_records(d, b.count, b.offset) {
                let o = rec.data_offset as usize;
                if o + 8 > d.len() {
                    continue;
                }
                let target = u32le(d, o);
                let x = u16le(d, o + 4) as i32;
                let y = u16le(d, o + 6) as i32;
                let kind = match (target >> 24) & 0x7F {
                    0x02 => "image",
                    0x04 => "sprite", // 动画精灵：图片组的一帧
                    0x07 => "data",
                    0x08 => "slot",
                    0x09 => "action",
                    _ => continue,
                };
                layers.push(LayerModel {
                    kind: kind.to_string(),
                    r: target & 0x00FF_FFFF,
                    x,
                    y,
                });
            }
        }
        // 背景：样式头 backImageId（0x82xxxxxx）优先，否则取面积最大的图片元素
        let bg_from_header = {
            let low = st.back_image_id & 0x00FF_FFFF;
            if (st.back_image_id >> 24) & 0x7F == 0x02 && low != 0 {
                t.image_size(d, low).map(|_| low)
            } else {
                None
            }
        };
        // 没有明确背景图时：取「图片元素」与「动画精灵」中面积最大的那个当背景
        // （精灵更大时说明背景是动画，不做照片替换，仅用于画布尺寸）
        let bg_image = bg_from_header.or_else(|| {
            let best_image = layers
                .iter()
                .filter(|l| l.kind == "image")
                .filter_map(|l| t.image_size(d, l.r).map(|(w, h)| (l.r, w as u64 * h as u64)))
                .max_by_key(|(_, area)| *area);
            let best_sprite = layers
                .iter()
                .filter(|l| l.kind == "sprite")
                .filter_map(|l| t.array(l.r).map(|(w, h, _)| (*w as u64) * (*h as u64)))
                .max();
            match (best_image, best_sprite) {
                (Some((idx, ia)), Some(sa)) if ia >= sa => Some(idx),
                (Some((idx, _)), None) => Some(idx),
                _ => None,
            }
        });
        let preview_png_base64 = watchface_bin::decode_preview_for_style(d, st)
            .ok()
            .and_then(|(rgba, w, h)| watchface_bin::encode_png_pub(&rgba, w, h).ok());
        // 每样式的容器槽位与配色组：容器取 layers 里引用的 slot；配色取该样式
        // 图片（块2）/ 精灵（块3）记录里出现过的 colorGroup —— 样式没有的编辑页就不显示
        let mut used_slots: Vec<u32> = layers
            .iter()
            .filter(|l| l.kind == "slot")
            .map(|l| l.r)
            .collect();
        used_slots.sort_unstable();
        used_slots.dedup();
        let mut used_cgs: Vec<u32> = Vec::new();
        for bi in [2usize, 3] {
            if let Some(b) = st.blocks.iter().find(|b| b.index == bi && b.count > 0) {
                for rec in watchface_bin::block_records(d, b.count, b.offset) {
                    let cg = watchface_bin::color_group_of(rec.unk);
                    if !used_cgs.contains(&cg) {
                        used_cgs.push(cg);
                    }
                }
            }
        }
        used_cgs.sort_unstable();
        themes.push(ThemeModelV2 {
            index: st.index,
            name: st.name.clone(),
            kind: if is_aod { "aod".into() } else { "normal".into() },
            bg_image: if is_aod { None } else { bg_image },
            bg_color: if is_aod { Some("#000000".into()) } else { None },
            layers,
            preview_png_base64,
            slots: used_slots,
            color_groups: used_cgs,
        });
    }

    // 画布尺寸：背景图 > 动画精灵(图片组) > 最大的图片元素 > 设备型号 > 默认
    let normal = themes
        .iter()
        .find(|th| th.kind == "normal")
        .or_else(|| themes.first());
    let bg_size = normal
        .and_then(|th| th.bg_image)
        .and_then(|idx| t.image_size(d, idx))
        .or_else(|| {
            normal.and_then(|th| {
                th.layers
                    .iter()
                    .filter(|l| l.kind == "sprite")
                    .filter_map(|l| t.array(l.r).map(|(w, h, _)| (*w, *h)))
                    .max_by_key(|(w, h)| (*w as u64) * (*h as u64))
            })
        })
        .or_else(|| {
            normal.and_then(|th| {
                th.layers
                    .iter()
                    .filter(|l| l.kind == "image")
                    .filter_map(|l| t.image_size(d, l.r))
                    .max_by_key(|(w, h)| (*w as u64) * (*h as u64))
            })
        });
    let (width, height) = if let Some((w, h)) = bg_size {
        (w, h)
    } else if let Some((w, h)) = device_model.and_then(device_screen) {
        (w, h)
    } else {
        (466, 466)
    };

    // 配色列表：数量来自图片/图片组记录里的配色位；色样 = 该配色的背景图缩略图
    //   实测「测试工程3」：3 个配色，壁纸各一张（配色1 灰蓝 / 金黄 / 黑），
    //   其余元素图按 recolorEnable 由官方构建工具生成（这里直接读成品 .bin，无需重着色）。
    let mut color_groups: Vec<ColorGroupModel> = Vec::new();
    for k in 0..t.color_groups.max(1) {
        let image_count = t
            .images
            .values()
            .filter(|list| list.iter().any(|(c, _)| *c == k))
            .count() as u32;
        let swatch_png_base64 = normal
            .and_then(|th| th.bg_image)
            .and_then(|bg| t.image_rgba_at(d, bg, k))
            .and_then(|(rgba, w, h)| thumbnail_png(&rgba, w, h, 96));
        color_groups.push(ColorGroupModel {
            index: k,
            name: format!("配色{}", k + 1),
            swatch_png_base64,
            image_count,
        });
    }

    Ok(LayerModelDoc {
        watchface_id: header.watchface_id,
        name: header.name,
        width,
        height,
        themes,
        arrays,
        slots,
        actions,
        data_items,
        color_groups,
        photo_album,
        screen: crate::watchface_edit::screen_info_for_size(width, height),
        device_screen: device_model.and_then(crate::watchface_edit::device_screen_info),
    })
}

/// 缩略图 PNG（base64）：长边缩到 `max`，用于配色色样这类小图（不追求画质）
fn thumbnail_png(rgba: &[u8], w: u32, h: u32, max: u32) -> Option<String> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let max = max.max(16);
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
}


// ---------------------------------------------------------------- 渲染

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
                for c in 0..3 {
                    let dv = canvas[di + c] as u32;
                    let sv = src[si + c] as u32;
                    canvas[di + c] = ((sv * a + dv * (255 - a)) / 255) as u8;
                }
                canvas[di + 3] = 255;
            }
        }
    }
}

/// 项尺寸：(宽, 高)。数字 = Σ帧宽 + (N-1)×step；图片 = 图片尺寸；动作 = 声明尺寸（flex 行则累加）
fn measure_item(t: &Tables, d: &[u8], item: &ActItem, depth: u32) -> (u32, u32) {
    if depth > 8 {
        return (0, 0);
    }
    match ref_kind(item.r) {
        "data" => {
            let Some(rec) = t.recs.get(&(item.r & 0x00FF_FFFF)) else {
                return (0, 0);
            };
            let (fw, fh) = t.target_size(d, rec.target).unwrap_or((0, 0));
            if rec.type_id() == 4 {
                // 环形进度：尺寸 = 贴图尺寸（背景/进度弧同尺寸）
                return (fw, fh);
            }
            if rec.type_id() == 5 {
                // 线性进度：尺寸 = 背景图尺寸
                if let Some(bg) = rec.bg_image {
                    if let Some(size) = t.image_size(d, bg) {
                        return size;
                    }
                }
                return (fw, fh);
            }
            if rec.is_values() {
                return (fw, fh);
            }
            let s = number_text(rec, &source_by_code(rec.shape, rec.data_src));
            let w = number_width(rec, t, d, &s, fw).max(0) as u32;
            let mut h = fh;
            if let Some(u) = rec.unit_icon {
                if let Some((_, uh)) = t.image_size(d, u) {
                    h = h.max(uh);
                }
            }
            (w, h)
        }
        "image" => t.image_size(d, item.r & 0x00FF_FFFF).unwrap_or((0, 0)),
        "action" => {
            let Some(a) = t.acts.get(&(item.r & 0x00FF_FFFF)) else {
                return (0, 0);
            };
            if a.flex_row() {
                let sum: i32 = a
                    .items
                    .iter()
                    .map(|i| measure_item(t, d, i, depth + 1).0 as i32)
                    .sum();
                let gap = group_gap(t, d, a, depth + 1);
                ((sum + gap * (a.items.len() as i32 - 1)).max(0) as u32, a.h)
            } else {
                (a.w, a.h)
            }
        }
        _ => (0, 0),
    }
}

/// flex 行的间距：`gap = (宽 - Σ项宽) / (项数-1)`（实测 178/186/206 三例精确吻合）
fn group_gap(t: &Tables, d: &[u8], a: &Act, depth: u32) -> i32 {
    if !a.flex_row() {
        return 0;
    }
    let sum: i32 = a
        .items
        .iter()
        .map(|i| measure_item(t, d, i, depth).0 as i32)
        .sum();
    let n = a.items.len() as i32;
    if n < 2 {
        return 0;
    }
    (a.w as i32 - sum) / (n - 1)
}

/// 照片变换（裁剪界面产出）：在「cover 铺满背景层」的基础上再做缩放/平移/旋转
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoTransform {
    /// 额外缩放（1 = 恰好 cover）
    pub scale: f32,
    /// 平移（背景层像素；正值 = 图片右/下移）
    pub dx: f32,
    pub dy: f32,
    /// 旋转（度）
    pub rotation: f32,
}

impl Default for PhotoTransform {
    fn default() -> Self {
        PhotoTransform {
            scale: 1.0,
            dx: 0.0,
            dy: 0.0,
            rotation: 0.0,
        }
    }
}

/// 渲染选项
pub struct RenderOpts<'a> {
    /// 用户照片（RGBA）
    pub photo: Option<(&'a [u8], u32, u32)>,
    /// 照片变换（裁剪界面产出）
    pub photo_transform: Option<PhotoTransform>,
    /// 容器选择：slot idx → 动作 idx
    pub slot_choices: Option<&'a HashMap<u32, u32>>,
    /// 配色序号（多配色表盘；缺省 0）
    pub color_group: Option<u32>,
}

impl<'a> Default for RenderOpts<'a> {
    fn default() -> Self {
        RenderOpts {
            photo: None,
            photo_transform: None,
            slot_choices: None,
            color_group: None,
        }
    }
}

struct Renderer<'a> {
    d: &'a [u8],
    t: &'a Tables,
    canvas: Vec<u8>,
    cw: u32,
    ch: u32,
}

impl<'a> Renderer<'a> {
    /// 绘制动作（复合组件）：flex 行 → 按推导间距横向排布并垂直居中；否则按项坐标绝对定位
    fn draw_action(&mut self, act_idx: u32, x: i32, y: i32, depth: u32) {
        if depth > 8 {
            return;
        }
        let Some(a) = self.t.acts.get(&act_idx).cloned() else {
            return;
        };
        if a.flex_row() {
            let gap = group_gap(self.t, self.d, &a, depth);
            let mut cx = 0i32;
            for item in a.items.iter() {
                let (iw, ih) = measure_item(self.t, self.d, item, depth);
                let (iw, ih) = (iw as i32, ih as i32);
                let iy = y + (a.h as i32 - ih) / 2;
                self.draw_item(item, x + cx, iy, depth);
                cx += iw + gap;
            }
        } else {
            for item in a.items.iter() {
                self.draw_item(item, x + item.x, y + item.y, depth);
            }
        }
    }

    fn draw_item(&mut self, item: &ActItem, x: i32, y: i32, depth: u32) {
        match ref_kind(item.r) {
            "data" => {
                let idx = item.r & 0x00FF_FFFF;
                if let Some(rec) = self.t.recs.get(&idx).copied() {
                    self.draw_data(&rec, x, y);
                }
            }
            "image" => {
                let idx = item.r & 0x00FF_FFFF;
                if let Some((rgba, w, h)) = self.t.image_rgba(self.d, idx) {
                    let (cw, ch) = (self.cw, self.ch);
                    blit(&mut self.canvas, cw, ch, &rgba, w, h, x, y);
                }
            }
            "action" => self.draw_action(item.r & 0x00FF_FFFF, x, y, depth + 1),
            _ => {}
        }
    }

    /// 绘制 target 的第 k 帧（图片组按帧取；单图只有第 0 帧）
    fn draw_frame(&mut self, target: u32, k: usize, x: i32, y: i32) {
        match ((target >> 24) & 0x7F) as u8 {
            0x03 => {
                let idx = target & 0x00FF_FFFF;
                if let Some((aw, ah, frames)) = self.t.array(idx).cloned() {
                    if frames.is_empty() {
                        return;
                    }
                    let k = k.min(frames.len() - 1);
                    let (off, _) = frames[k];
                    if let Some(rgba) = decode_array_frame(self.d, off, aw, ah) {
                        let (cw, ch) = (self.cw, self.ch);
                        blit(&mut self.canvas, cw, ch, &rgba, aw, ah, x, y);
                    }
                }
            }
            0x02 => {
                if let Some((rgba, w, h)) = self.t.image_rgba(self.d, target & 0x00FF_FFFF) {
                    let (cw, ch) = (self.cw, self.ch);
                    blit(&mut self.canvas, cw, ch, &rgba, w, h, x, y);
                }
            }
            _ => {}
        }
    }

    /// 绘制数据项：环形进度 / 数字（按 align + step 逐位）/ 值列表（单帧、左上角锚定）
    fn draw_data(&mut self, rec: &Rec, x: i32, y: i32) {
        let source = source_by_code(rec.shape, rec.data_src);
        // 环形进度条（type 4）
        if rec.type_id() == 4 {
            self.draw_arc(rec, &source, x, y);
            return;
        }
        // 指针（type 3）
        if rec.type_id() == 3 {
            self.draw_pointer(rec, &source, x, y);
            return;
        }
        // 线性进度条（type 5）
        if rec.type_id() == 5 {
            self.draw_line(rec, &source, x, y);
            return;
        }
        let n_frames = self.t.target_frames(rec.target);
        if n_frames == 0 {
            return;
        }
        // 值列表 / 指针 / 文本等：取单帧，左上角锚定
        if rec.is_values() || (rec.type_id() != 1 && rec.target_kind() == 0x03) {
            let k = (example_value(&source).max(0) as usize).min(n_frames - 1);
            self.draw_frame(rec.target, k, x, y);
            return;
        }
        let (aw, _ah) = self.t.target_size(self.d, rec.target).unwrap_or((0, 0));
        if aw == 0 {
            return;
        }
        let text = number_text(rec, &source);
        let step = aw as i32 + rec.space;
        let total = number_width(rec, self.t, self.d, &text, aw);
        // 0=右 1=左 2=居中（3 未观测到，按居中兜底）
        let start = match rec.align() {
            0 => -total,
            2 | 3 => -total / 2,
            _ => 0,
        };
        let mut cx = 0i32;
        for ch in text.chars() {
            let k = char_frame(ch).min(n_frames - 1);
            self.draw_frame(rec.target, k, x + start + cx, y);
            cx += step;
        }
        // 单位图（如 "%"）跟在数字之后，间隔同样用 space
        if let Some(u) = rec.unit_icon {
            if let Some((rgba, uw, uh)) = self.t.image_rgba(self.d, u) {
                let (cw, chh) = (self.cw, self.ch);
                blit(
                    &mut self.canvas,
                    cw,
                    chh,
                    &rgba,
                    uw,
                    uh,
                    x + start + cx + rec.space,
                    y,
                );
                let _ = uh;
            }
        }
    }

    /// 指针（type 3）：贴图绕 pivot 旋转（对齐 AstroBox drawPointer）
    fn draw_pointer(&mut self, rec: &Rec, source: &str, x: i32, y: i32) {
        let Some((rgba, w, h)) = self.t.image_rgba(self.d, rec.target_idx()) else {
            return;
        };
        if rec.target_kind() != 0x02 {
            return;
        }
        let range = if rec.value_range > 0 {
            rec.value_range as f32
        } else {
            return;
        };
        let v = example_value(source) as f32 - rec.value_start as f32;
        let ratio = (v / range as f32).clamp(0.0, 1.0);
        let angle = (rec.angle_start + rec.angle_range * ratio).to_radians();
        let (cx, cy) = (x as f32 + rec.pivot_x, y as f32 + rec.pivot_y);
        let (cos_a, sin_a) = (angle.cos(), angle.sin());
        let (iw, ih) = (w as i32, h as i32);
        for py in 0..ih {
            for px in 0..iw {
                // 图片像素 → 旋转后落在画布的位置（绕 pivot）
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let dx = fx * cos_a - fy * sin_a;
                let dy = fx * sin_a + fy * cos_a;
                let (out_x, out_y) = (cx + dx, cy + dy);
                let (ox, oy) = (out_x as i32, out_y as i32);
                if ox < 0 || oy < 0 || ox >= self.cw as i32 || oy >= self.ch as i32 {
                    continue;
                }
                let si = ((py as u32 * w + px as u32) * 4) as usize;
                let a = rgba[si + 3] as u32;
                if a == 0 {
                    continue;
                }
                let di = ((oy as u32 * self.cw + ox as u32) * 4) as usize;
                if a == 255 {
                    self.canvas[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
                } else {
                    for c in 0..3 {
                        let dv = self.canvas[di + c] as u32;
                        let sv = rgba[si + c] as u32;
                        self.canvas[di + c] = ((sv * a + dv * (255 - a)) / 255) as u8;
                    }
                    self.canvas[di + 3] = 255;
                }
            }
        }
    }

    /// 线性进度条（type 5）：背景图 + 被「线段」裁剪的进度图（对齐 drawLineProgress）
    fn draw_line(&mut self, rec: &Rec, source: &str, x: i32, y: i32) {
        // 背景
        if let Some(bg) = rec.bg_image {
            if let Some((rgba, w, h)) = self.t.image_rgba(self.d, bg) {
                let (cw, ch) = (self.cw, self.ch);
                blit(&mut self.canvas, cw, ch, &rgba, w, h, x, y);
            }
        }
        let Some((rgba, w, h)) = self.t.image_rgba(self.d, rec.target_idx()) else {
            return;
        };
        if rec.value_range <= 0 {
            return;
        }
        let v = example_value(source) as f32 - rec.value_start as f32;
        let ratio = (v / rec.value_range as f32).clamp(0.0, 1.0);
        // 线段终点按比例内插（起点固定）
        let (sx, sy) = (rec.start_x, rec.start_y);
        let (ex, ey) = (
            rec.start_x + (rec.end_x - rec.start_x) * ratio,
            rec.start_y + (rec.end_y - rec.start_y) * ratio,
        );
        let half = rec.bar_width.max(1.0) / 2.0;
        let (dx, dy) = (ex - sx, ey - sy);
        let len2 = dx * dx + dy * dy;
        let (iw, ih) = (w as i32, h as i32);
        for py in 0..ih {
            for px in 0..iw {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                // 到线段的距离（含圆头端点）
                let t = if len2 > 0.0 {
                    (((fx - sx) * dx + (fy - sy) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (nx, ny) = (sx + dx * t, sy + dy * t);
                let dist = ((fx - nx).powi(2) + (fy - ny).powi(2)).sqrt();
                if dist > half {
                    continue;
                }
                let si = ((py as u32 * w + px as u32) * 4) as usize;
                let a = rgba[si + 3] as u32;
                if a == 0 {
                    continue;
                }
                let (ox, oy) = (x + px, y + py);
                if ox < 0 || oy < 0 || ox >= self.cw as i32 || oy >= self.ch as i32 {
                    continue;
                }
                let di = ((oy as u32 * self.cw + ox as u32) * 4) as usize;
                if a == 255 {
                    self.canvas[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
                } else {
                    for c in 0..3 {
                        let dv = self.canvas[di + c] as u32;
                        let sv = rgba[si + c] as u32;
                        self.canvas[di + c] = ((sv * a + dv * (255 - a)) / 255) as u8;
                    }
                    self.canvas[di + 3] = 255;
                }
            }
        }
    }

    /// 环形进度条：先画背景环贴图，再画被「进度弧」裁剪的进度贴图
    /// （对齐 AstroBox：角度以 12 点为 0，角度值 = 度×10 存储）
    fn draw_arc(&mut self, rec: &Rec, source: &str, x: i32, y: i32) {
        if let Some(bg) = rec.bg_image {
            if let Some((rgba, w, h)) = self.t.image_rgba(self.d, bg) {
                let (cw, ch) = (self.cw, self.ch);
                blit(&mut self.canvas, cw, ch, &rgba, w, h, x, y);
            }
        }
        if rec.target_kind() != 0x02 {
            return;
        }
        let Some((rgba, w, h)) = self.t.image_rgba(self.d, rec.target_idx()) else {
            return;
        };
        let range = if rec.value_range > 0 {
            rec.value_range as f32
        } else {
            100.0
        };
        let ratio = (example_value(source) as f32 / range).clamp(0.0, 1.0);
        let (cx, cy) = (rec.pivot_x, rec.pivot_y);
        let (r0, bw) = (rec.bar_radius, rec.bar_width.max(1.0));
        let (a0, span) = (rec.angle_start - 90.0, rec.angle_range * ratio);
        for py in 0..h as i32 {
            for px in 0..w as i32 {
                let dx = px as f32 - cx;
                let dy = py as f32 - cy;
                let r = (dx * dx + dy * dy).sqrt();
                if (r - r0).abs() > bw / 2.0 {
                    continue;
                }
                let ang = dy.atan2(dx).to_degrees();
                let rel = (ang - a0).rem_euclid(360.0);
                if rel > span.max(0.0) {
                    continue;
                }
                let si = ((py as u32 * w + px as u32) * 4) as usize;
                let a = rgba[si + 3] as u32;
                if a == 0 {
                    continue;
                }
                let (dx2, dy2) = (x + px, y + py);
                if dx2 < 0 || dy2 < 0 || dx2 >= self.cw as i32 || dy2 >= self.ch as i32 {
                    continue;
                }
                let di = ((dy2 as u32 * self.cw + dx2 as u32) * 4) as usize;
                if a == 255 {
                    self.canvas[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
                } else {
                    for c in 0..3 {
                        let dv = self.canvas[di + c] as u32;
                        let sv = rgba[si + c] as u32;
                        self.canvas[di + c] = ((sv * a + dv * (255 - a)) / 255) as u8;
                    }
                    self.canvas[di + 3] = 255;
                }
            }
        }
    }
}

/// 渲染预览：背景（用户照片或原背景图）→ 逐层（图片 / 槽位→动作→数据项）
pub fn render_preview(
    d: &[u8],
    model: &LayerModelDoc,
    theme_index: usize,
    opts: &RenderOpts,
) -> Result<(Vec<u8>, u32, u32)> {
    let (cw, ch) = (model.width, model.height);
    let t = Tables::build(d)?;
    // 多配色：本次渲染按所选配色取图（该配色没存的 idx 回退基础配色，见 Tables::pick）
    let cg = opts.color_group.unwrap_or(0).min(t.color_groups.saturating_sub(1));
    t.set_color_group(cg);
    let theme = model
        .themes
        .iter()
        .find(|t| t.index == theme_index)
        .or_else(|| model.themes.iter().find(|t| t.kind == "normal"))
        .ok_or_else(|| anyhow::anyhow!("样式不存在"))?;

    // 画布底色：息屏样式为纯黑，其余先铺背景
    let is_aod = theme.kind == "aod";
    let mut canvas = vec![0u8; (cw as usize) * (ch as usize) * 4];
    if is_aod {
        for px in canvas.chunks_mut(4) {
            px[3] = 255;
        }
    } else if let Some(bg_idx) = theme.bg_image {
        let pos = theme
            .layers
            .iter()
            .find(|l| l.kind == "image" && l.r == bg_idx)
            .map(|l| (l.x, l.y))
            .unwrap_or((0, 0));
        match opts.photo {
            Some((px, pw, ph)) => {
                draw_photo(&mut canvas, cw, ch, px, pw, ph, pos.0, pos.1, opts.photo_transform)
            }
            None => {
                if let Some((rgba, iw, ih)) = t.image_rgba(d, bg_idx) {
                    blit(&mut canvas, cw, ch, &rgba, iw, ih, pos.0, pos.1);
                }
            }
        }
    }

    let mut r = Renderer {
        d,
        t: &t,
        canvas,
        cw,
        ch,
    };

    for layer in theme.layers.iter() {
        match layer.kind.as_str() {
            "image" => {
                if theme.bg_image == Some(layer.r) {
                    continue;
                }
                if let Some((rgba, iw, ih)) = t.image_rgba(d, layer.r) {
                    blit(&mut r.canvas, cw, ch, &rgba, iw, ih, layer.x, layer.y);
                }
            }
            "slot" => {
                let Some(slot) = t.slots.get(&layer.r) else {
                    continue;
                };
                if slot.is_empty() {
                    continue;
                }
                // 可编辑容器：默认取第一个候选（= AstroBox 行为），其余由用户选择
                let chosen = opts
                    .slot_choices
                    .and_then(|m| m.get(&layer.r).copied())
                    .filter(|c| slot.contains(c))
                    .unwrap_or(slot[0]);
                r.draw_action(chosen, layer.x, layer.y, 0);
            }
            "sprite" => {
                // 动画精灵：取图片组第 1 帧（静态预览）
                if let Some((aw, ah, frames)) = t.array(layer.r).cloned() {
                    if let Some((off, _)) = frames.first() {
                        if let Some(rgba) = decode_array_frame(d, *off, aw, ah) {
                            blit(&mut r.canvas, cw, ch, &rgba, aw, ah, layer.x, layer.y);
                        }
                    }
                }
            }
            "data" => {
                if let Some(rec) = t.recs.get(&layer.r).copied() {
                    r.draw_data(&rec, layer.x, layer.y);
                }
            }
            "action" => r.draw_action(layer.r, layer.x, layer.y, 0),
            _ => {}
        }
    }
    Ok((r.canvas, cw, ch))
}

/// 用户照片铺满背景层区域：基础 cover 裁切 + 可选缩放/平移/旋转（裁剪界面产出）
///
/// 反向映射：目标像素 → （去平移、反旋转、除缩放）→ 源图坐标。
/// 超出源图范围的部分留空（背景保持原样），避免边缘拉伸。
fn draw_photo(
    canvas: &mut [u8],
    cw: u32,
    ch: u32,
    src: &[u8],
    sw: u32,
    sh: u32,
    x: i32,
    y: i32,
    tr: Option<PhotoTransform>,
) {
    if sw == 0 || sh == 0 {
        return;
    }
    // 基础缩放：**原生尺寸居中**（1 源图像素 = 1 画布像素，不强行铺满）——
    // 与前端裁剪视图同一套基准；卖家按表盘尺寸出的图进来就是刚好镶满
    let tr = tr.unwrap_or_default();
    let scale = tr.scale.max(0.01);
    let (theta, (sdx, sdy)) = (tr.rotation.to_radians(), (tr.dx, tr.dy));
    let (cos_t, sin_t) = (theta.cos(), theta.sin());
    let (cx, cy) = (cw as f32 / 2.0, ch as f32 / 2.0);
    let (scx, scy) = (sw as f32 / 2.0, sh as f32 / 2.0);
    for yy in 0..ch as i32 {
        let dy = y + yy;
        if dy < 0 || dy >= ch as i32 {
            continue;
        }
        for xx in 0..cw as i32 {
            let dx = x + xx;
            if dx < 0 || dx >= cw as i32 {
                continue;
            }
            // 目标点相对背景中心的偏移（减去用户平移），再反旋转、除缩放
            let u = xx as f32 - cx - sdx;
            let v = yy as f32 - cy - sdy;
            let ru = u * cos_t + v * sin_t;
            let rv = -u * sin_t + v * cos_t;
            let sx = ru / scale + scx;
            let sy = rv / scale + scy;
            if sx < 0.0 || sy < 0.0 || sx >= sw as f32 || sy >= sh as f32 {
                continue;
            }
            let si = ((sy as u32 * sw + sx as u32) * 4) as usize;
            let di = ((dy as u32 * cw + dx as u32) * 4) as usize;
            let a = src[si + 3] as u32;
            if a == 255 {
                canvas[di..di + 4].copy_from_slice(&src[si..si + 4]);
            } else if a > 0 {
                for c in 0..3 {
                    let dv = canvas[di + c] as u32;
                    let sv = src[si + c] as u32;
                    canvas[di + c] = ((sv * a + dv * (255 - a)) / 255) as u8;
                }
                canvas[di + 3] = 255;
            }
        }
    }
}

/// 合成待下发的壁纸：把用户照片按裁剪变换铺进背景层区域（**不含**表盘元素——
/// 设备端会自己把时间等元素画在照片上面）。
/// 返回 RGBA 画布与尺寸；编码成 PNG 由调用方负责。
pub fn compose_photo_canvas(
    d: &[u8],
    model: &LayerModelDoc,
    theme_index: usize,
    photo: (&[u8], u32, u32),
    tr: Option<PhotoTransform>,
) -> Result<(Vec<u8>, u32, u32)> {
    let _ = d;
    let (cw, ch) = (model.width, model.height);
    let theme = model
        .themes
        .iter()
        .find(|t| t.index == theme_index)
        .or_else(|| model.themes.iter().find(|t| t.kind == "normal"))
        .ok_or_else(|| anyhow::anyhow!("样式不存在"))?;
    anyhow::ensure!(theme.kind == "normal", "息屏样式没有可替换的背景");

    let mut canvas = vec![0u8; (cw as usize) * (ch as usize) * 4];
    // 照片没盖到的地方填不透明黑：上传的壁纸是「整屏画面」，不留透明区，
    // 免得设备端把旧壁纸透出来（预览与手环实际显示也就一致了）。
    for px in canvas.chunks_exact_mut(4) {
        px[3] = 255;
    }
    // 背景 = 背景层矩形；无明确背景图时铺满整画布
    let (bx, by) = theme
        .layers
        .iter()
        .find(|l| l.kind == "image" && Some(l.r) == theme.bg_image)
        .map(|l| (l.x, l.y))
        .unwrap_or((0, 0));
    draw_photo(&mut canvas, cw, ch, photo.0, photo.1, photo.2, bx, by, tr);
    Ok((canvas, cw, ch))
}
