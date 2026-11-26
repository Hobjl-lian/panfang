//! 表盘 .bin 解包/预览的前端命令（与 `watchface_bin` 模块配套）。
//!
//! 安全性原则：设备留档和视频成品在 Rust 内存内解密与缓存（只传不透明引用给前端），
//! 不跨 IPC 返回明文 `.bin`；仅用户本地自选未加密文件时才接受 `data_b64`。

use base64::Engine;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use crate::watchface_bin;

static BIN_CACHE: OnceLock<Mutex<VecDeque<(String, Arc<Vec<u8>>)>>> = OnceLock::new();

fn bin_cache() -> &'static Mutex<VecDeque<(String, Arc<Vec<u8>>)>> {
    BIN_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(2)))
}

fn decode_b64(data_b64: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = data_b64
        .rsplit_once(',')
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_else(|| data_b64.to_string());
    base64::engine::general_purpose::STANDARD
        .decode(cleaned.trim())
        .map_err(|e| format!("base64 解码失败: {e}"))
}

fn resolve_bin_bytes(
    app: &tauri::AppHandle,
    state: &crate::bridge::AppState,
    data_b64: Option<String>,
    archive_addr: Option<String>,
    archive_face_id: Option<String>,
    video_project_id: Option<String>,
) -> Result<Arc<Vec<u8>>, String> {
    if let (Some(addr), Some(fid)) = (
        archive_addr.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        archive_face_id.as_deref().map(str::trim).filter(|s| !s.is_empty()),
    ) {
        let key = format!("archive:{addr}:{fid}");
        {
            let cache = bin_cache().lock();
            if let Some((_, hit)) = cache.iter().find(|(k, _)| k == &key) {
                return Ok(hit.clone());
            }
        }
        let (bytes, _) = crate::bridge::read_archive_plain(app, addr, fid)?;
        let arc = Arc::new(bytes);
        let mut cache = bin_cache().lock();
        cache.retain(|(k, _)| k != &key);
        if cache.len() >= 2 {
            cache.pop_front();
        }
        cache.push_back((key, arc.clone()));
        return Ok(arc);
    }
    if let Some(pid) = video_project_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let key = format!("video:{pid}");
        {
            let cache = bin_cache().lock();
            if let Some((_, hit)) = cache.iter().find(|(k, _)| k == &key) {
                return Ok(hit.clone());
            }
        }
        let bytes = crate::video::read_video_project_bin(state, pid)?;
        let arc = Arc::new(bytes);
        let mut cache = bin_cache().lock();
        cache.retain(|(k, _)| k != &key);
        if cache.len() >= 2 {
            cache.pop_front();
        }
        cache.push_back((key, arc.clone()));
        return Ok(arc);
    }
    let b64 = data_b64
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "缺少表盘数据源".to_string())?;
    Ok(Arc::new(decode_b64(&b64)?))
}

fn decode_photo_checked(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let dims = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("照片格式不可读: {e}"))?
        .into_dimensions()
        .map_err(|e| format!("照片解码失败: {e}"))?;
    if dims.0 > 12_000 || dims.1 > 12_000 || (dims.0 as u64 * dims.1 as u64) > 24_000_000 {
        return Err("照片分辨率过高，请裁切或换一张再试".into());
    }
    let im = image::load_from_memory(bytes).map_err(|e| format!("照片解码失败: {e}"))?;
    let rgba = im.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((rgba.into_raw(), w, h))
}

/// 解析 .bin 摘要：头部信息 + 每样式元数据 + 预览图 PNG
#[tauri::command]
pub async fn watchface_bin_inspect(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::bridge::AppState>,
    data_b64: Option<String>,
    archive_addr: Option<String>,
    archive_face_id: Option<String>,
    video_project_id: Option<String>,
) -> Result<watchface_bin::BinSummary, String> {
    let data = resolve_bin_bytes(&app, &state, data_b64, archive_addr, archive_face_id, video_project_id)?;
    tauri::async_runtime::spawn_blocking(move || watchface_bin::inspect(&data))
        .await
        .map_err(|e| format!("解析任务失败: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

/// 某样式 images 块的缩略图（base64 PNG），用于前端图墙
/// （开发期命令：界面已改用 watchface_layer_model 的预览图）
#[tauri::command]
pub async fn watchface_bin_style_thumbs(
    data_b64: String,
    style_index: usize,
    thumb_max: Option<u32>,
) -> Result<Vec<watchface_bin::ThumbInfo>, String> {
    crate::dev_only("批量导出样式缩略图")?;
    let data = decode_b64(&data_b64)?;
    let thumb_max = thumb_max.unwrap_or(160);
    tauri::async_runtime::spawn_blocking(move || {
        watchface_bin::style_thumbnails(&data, style_index, thumb_max)
    })
    .await
    .map_err(|e| format!("解析任务失败: {e}"))?
    .map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Serialize)]
pub struct ExtractReport {
    pub written: usize,
    pub out_dir: String,
    pub images: Vec<watchface_bin::ImageInfo>,
}

/// 导出某样式全部图片到目录（indexed8 → PNG；其他格式存 .raw）
/// **正式包关闭**：这条是「一键拿走全部素材原图」的口子（设计流水线用它出素材）
#[tauri::command]
pub async fn watchface_bin_extract_style(
    data_b64: String,
    style_index: usize,
    out_dir: String,
) -> Result<ExtractReport, String> {
    crate::dev_only("导出样式素材图")?;
    let data = decode_b64(&data_b64)?;
    let dir = std::path::PathBuf::from(&out_dir);
    tauri::async_runtime::spawn_blocking(move || {
        watchface_bin::extract_style_images(&data, style_index, &dir)
    })
    .await
    .map_err(|e| format!("导出任务失败: {e}"))?
    .map(|(written, images)| ExtractReport {
        written,
        out_dir,
        images,
    })
    .map_err(|e| format!("{e:#}"))
}

/// 表盘编辑模型：画布尺寸 / 样式列表（含预览图）/ 壁纸槽位 / 数据项组件
/// （开发期命令：正式包用 watchface_layer_model，界面不再调用这条）
#[tauri::command]
pub async fn watchface_edit_model(
    data_b64: String,
    model: Option<String>,
    style_index: Option<usize>,
) -> Result<crate::watchface_edit::EditModel, String> {
    crate::dev_only("编辑模型（旧版）")?;
    let data = decode_b64(&data_b64)?;
    let style_index = style_index.unwrap_or(0);
    tauri::async_runtime::spawn_blocking(move || {
        crate::watchface_edit::build_edit_model(&data, model.as_deref(), style_index)
    })
    .await
    .map_err(|e| format!("解析任务失败: {e}"))?
    .map_err(|e| format!("{e:#}"))
}


/// 图层模型（对齐 AstroBox 反编译产物结构）：主题/图层/槽位/组件/数据项/图片组
#[tauri::command]
pub async fn watchface_layer_model(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::bridge::AppState>,
    data_b64: Option<String>,
    archive_addr: Option<String>,
    archive_face_id: Option<String>,
    video_project_id: Option<String>,
    device_model: Option<String>,
) -> Result<crate::watchface_render::LayerModelDoc, String> {
    let data = resolve_bin_bytes(&app, &state, data_b64, archive_addr, archive_face_id, video_project_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::watchface_render::derive_model(&data, device_model.as_deref())
    })
    .await
    .map_err(|e| format!("解析任务失败: {e}"))?
    .map_err(|e| format!("{e:#}"))
}

/// 渲染预览（PNG base64）：背景（用户照片或原背景图）→ 逐层（图片 / 槽位→动作→数据项）
///
/// `slot_choices`：可编辑容器的选择（槽位序号 → 动作序号）；不传则每槽取第一个候选。
#[tauri::command]
pub async fn watchface_compose_photo(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::bridge::AppState>,
    data_b64: Option<String>,
    archive_addr: Option<String>,
    archive_face_id: Option<String>,
    video_project_id: Option<String>,
    theme_index: Option<usize>,
    photo_b64: String,
    photo_transform: Option<crate::watchface_render::PhotoTransform>,
    device_model: Option<String>,
) -> Result<String, String> {
    let data = resolve_bin_bytes(&app, &state, data_b64, archive_addr, archive_face_id, video_project_id)?;
    let photo = decode_b64(&photo_b64)?;
    let theme_index = theme_index.unwrap_or(0);
    tauri::async_runtime::spawn_blocking(move || {
        let model = crate::watchface_render::derive_model(&data, device_model.as_deref())?;
        let photo_decoded = decode_photo_checked(&photo).map_err(|e| anyhow::anyhow!(e))?;
        let (rgba, w, h) = crate::watchface_render::compose_photo_canvas(
            &data,
            &model,
            theme_index,
            (&photo_decoded.0, photo_decoded.1, photo_decoded.2),
            photo_transform,
        )?;
        watchface_bin::encode_png_pub(&rgba, w, h)
    })
    .await
    .map_err(|e| format!("合成任务失败: {e}"))?
    .map_err(|e| format!("{e:#}"))
}

/// 表盘自带的默认壁纸（照片表盘的背景图）→ PNG base64。
///
/// 「恢复默认照片」用：用户把壁纸换掉之后，从留档 `.bin` 里把原图取回来。
/// 只画背景层、不画表盘元素；多配色表盘按 `color_group` 取对应配色。
#[tauri::command]
pub async fn watchface_default_wallpaper(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::bridge::AppState>,
    data_b64: Option<String>,
    archive_addr: Option<String>,
    archive_face_id: Option<String>,
    video_project_id: Option<String>,
    theme_index: Option<usize>,
    device_model: Option<String>,
    color_group: Option<u32>,
) -> Result<String, String> {
    let data = resolve_bin_bytes(&app, &state, data_b64, archive_addr, archive_face_id, video_project_id)?;
    let theme_index = theme_index.unwrap_or(0);
    tauri::async_runtime::spawn_blocking(move || {
        let model = crate::watchface_render::derive_model(&data, device_model.as_deref())?;
        let (rgba, w, h) = crate::watchface_render::default_wallpaper(
            &data,
            &model,
            theme_index,
            color_group,
        )?;
        watchface_bin::encode_png_pub(&rgba, w, h)
    })
    .await
    .map_err(|e| format!("解析任务失败: {e}"))?
    .map_err(|e| format!("{e:#}"))
}

/// 渲染预览（PNG base64）：背景（用户照片或原背景图）→ 逐层（图片 / 槽位→动作→数据项）
///
/// `slot_choices`：可编辑容器的选择（槽位序号 → 动作序号）；不传则每槽取第一个候选。
/// `color_group`：多配色表盘选第几套配色（0 起；不传 = 0）。
#[tauri::command]
pub async fn watchface_render_preview(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::bridge::AppState>,
    data_b64: Option<String>,
    archive_addr: Option<String>,
    archive_face_id: Option<String>,
    video_project_id: Option<String>,
    theme_index: Option<usize>,
    photo_b64: Option<String>,
    device_model: Option<String>,
    slot_choices: Option<std::collections::HashMap<u32, u32>>,
    photo_transform: Option<crate::watchface_render::PhotoTransform>,
    color_group: Option<u32>,
) -> Result<String, String> {
    let data = resolve_bin_bytes(&app, &state, data_b64, archive_addr, archive_face_id, video_project_id)?;
    let photo = match photo_b64 {
        Some(p) if !p.is_empty() => Some(decode_b64(&p)?),
        _ => None,
    };
    let theme_index = theme_index.unwrap_or(0);
    tauri::async_runtime::spawn_blocking(move || {
        let model = crate::watchface_render::derive_model(&data, device_model.as_deref())?;
        let photo_decoded = match photo {
            Some(bytes) => Some(decode_photo_checked(&bytes).map_err(|e| anyhow::anyhow!(e))?),
            None => None,
        };
        let opts = crate::watchface_render::RenderOpts {
            photo: photo_decoded.as_ref().map(|(d, w, h)| (d.as_slice(), *w, *h)),
            photo_transform,
            slot_choices: slot_choices.as_ref(),
            color_group,
        };
        let (rgba, w, h) =
            crate::watchface_render::render_preview(&data, &model, theme_index, &opts)?;
        watchface_bin::encode_png_pub(&rgba, w, h)
    })
    .await
    .map_err(|e| format!("渲染任务失败: {e}"))?
    .map_err(|e| format!("{e:#}"))
}
