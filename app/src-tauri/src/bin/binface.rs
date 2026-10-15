//! .bin 解析验证 CLI（与参考工具 UnpackMiColorFace 的输出对照用）
//!
//! 用法:
//!   binface inspect <file.bin>
//!   binface extract <file.bin> <out_dir> [style_index]

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("用法: binface inspect|extract|model|model2|render|compose ...");
        std::process::exit(2);
    }
    let cmd = &args[1];
    let path = PathBuf::from(&args[2]);
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("读取失败 {}: {e}", path.display());
            std::process::exit(1);
        }
    };

    match cmd.as_str() {
        "inspect" => match wf_installer_lib::watchface_bin::inspect(&data) {
            Ok(summary) => {
                let h = &summary.header;
                println!("表盘 id={} 名称={:?}", h.watchface_id, h.name);
                println!(
                    "版本=0x{:08x} 样式数={} 子版本={} 预览偏移=0x{:x}",
                    h.version, h.style_count, h.sub_version, h.preview_offset
                );
                if let Some(p) = &summary.preview {
                    println!(
                        "预览图 {}x{} (PNG base64 {} 字符)",
                        p.width,
                        p.height,
                        p.png_base64.len()
                    );
                } else {
                    println!("预览图: 未解出");
                }
                for (style, imgs) in summary.styles.iter().zip(summary.style_images.iter()) {
                    let ok = imgs.iter().filter(|i| i.decoded).count();
                    println!(
                        "样式{} {:?} 图片记录 {} 条（已解码 {}）backImageId={}",
                        style.index,
                        style.name,
                        imgs.len(),
                        ok,
                        style.back_image_id
                    );
                    for b in &style.blocks {
                        if b.count > 0 {
                            println!("    [{:>2}] {:>10}: {} 条 @0x{:x}", b.index, b.name, b.count, b.offset);
                        }
                    }
                    for i in imgs.iter().filter(|i| !i.decoded).take(3) {
                        println!(
                            "      ! idx={} {}x{} fmt=0x{:02x} {}",
                            i.idx, i.width, i.height, i.format_byte, i.note
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("解析失败: {e:#}");
                std::process::exit(1);
            }
        },
        "extract" => {
            let out_dir = PathBuf::from(args.get(3).map(|s| s.as_str()).unwrap_or("bin_extract"));
            let style: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            match wf_installer_lib::watchface_bin::extract_style_images(&data, style, &out_dir) {
                Ok((n, report)) => {
                    println!("导出 {n} 个文件 → {}", out_dir.display());
                    for r in &report {
                        println!(
                            "  b{} idx={} {}x{} fmt=0x{:02x} decoded={} {}",
                            r.block, r.idx, r.width, r.height, r.format_byte, r.decoded, r.note
                        );
                    }
                }
                Err(e) => {
                    eprintln!("导出失败: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        "model" => {
            let model = args.get(4).map(|s| s.as_str());
            let style: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
            match wf_installer_lib::watchface_edit::build_edit_model(&data, model, style) {
                Ok(m) => {
                    println!(
                        "编辑模型: {} / {:?}  画布 {}x{}（来源 {}） 样式数 {} 图片格式 {}",
                        m.watchface_id, m.name, m.canvas_width, m.canvas_height,
                        m.canvas_source, m.style_count, m.image_format
                    );
                    match &m.wallpaper {
                        Some(r) => println!("  壁纸槽位: ({},{}) {}x{}  imageId={:?}", r.x, r.y, r.w, r.h, m.wallpaper_image_id),
                        None => println!("  壁纸槽位: 未找到（非相册表盘？）"),
                    }
                    println!("  数据项组件: {} 个", m.widgets.len());
                    for w in m.widgets.iter().take(6) {
                        println!(
                            "    #{} shape=0x{:02x} dataSrc=0x{:02x} digits={} type={} align={} pos=({},{})",
                            w.index, w.shape, w.data_src, w.digits, w.type_id, w.align, w.x, w.y
                        );
                    }
                    for s in &m.styles {
                        println!(
                            "    样式{} {:?} 图片 {}  预览 {}x{} {}",
                            s.index, s.name, s.image_count, s.preview_width, s.preview_height,
                            if s.preview_png_base64.is_some() { "有" } else { "无" }
                        );
                    }
                }
                Err(e) => { eprintln!("构建模型失败: {e:#}"); std::process::exit(1); }
            }
        }
        "model2" => {
            match wf_installer_lib::watchface_render::derive_model(&data, args.get(3).map(|s| s.as_str())) {
                Ok(m) => {
                    println!("图层模型: {} / {:?}  画布 {}x{}", m.watchface_id, m.name, m.width, m.height);
                    println!("  屏幕: {}×{} 圆角 {} 形状 {}", m.screen.width, m.screen.height, m.screen.radius, m.screen.shape);
                    if let Some(ds) = &m.device_screen {
                        let same = ds.width == m.screen.width && ds.height == m.screen.height;
                        println!("  设备屏幕: {}×{} 圆角 {} 形状 {}{}", ds.width, ds.height, ds.radius, ds.shape,
                            if same { "（与表盘一致）" } else { "（与表盘不一致）" });
                    }
                    println!("  配色 {} 组: {:?}", m.color_groups.len(),
                        m.color_groups.iter().map(|c| (c.index, c.name.clone(), c.image_count, c.swatch_png_base64.is_some())).collect::<Vec<_>>());
                    println!("  图片组 {} 个: {:?}", m.arrays.len(),
                        m.arrays.iter().map(|a| (a.index, a.width, a.height, a.frames)).collect::<Vec<_>>());
                    println!("  动作 {} 个 / 数据项 {} 个 / 槽位 {} 个", m.actions.len(), m.data_items.len(), m.slots.len());
                    for a in m.actions.iter() {
                        println!("    动作#{} {:?} {}x{} flex={} gap={} items={:?}", a.index, a.name, a.width, a.height,
                            a.flex_row, a.gap,
                            a.items.iter().map(|i| format!("{}#{}+({},{})", i.kind, i.r, i.x, i.y)).collect::<Vec<_>>());
                    }
                    for d in m.data_items.iter() {
                        println!("    数据项#{} {:?} source={} array={} digits={} space={} step={} align={}",
                            d.index, d.kind, d.source, d.array, d.digits, d.space, d.step, d.align);
                    }
                    for s in m.slots.iter() {
                        println!("    槽位#{} 候选 {:?}", s.index, s.actions);
                    }
                    for t in m.themes.iter() {
                        println!("    样式{} {:?} {} bg={:?} 图层{}: {:?}", t.index, t.name, t.kind, t.bg_image, t.layers.len(),
                            t.layers.iter().map(|l| format!("{}#{}+({},{})", l.kind, l.r, l.x, l.y)).collect::<Vec<_>>());
                    }
                }
                Err(e) => { eprintln!("推导失败: {e:#}"); std::process::exit(1); }
            }
        }
        "render" => {
            let out = args.get(3).cloned().unwrap_or_else(|| "preview.png".into());
            let theme: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let photo_path = args.get(5);
            // 可选：槽位选择 "3=22,4=35"，配色 "cg=1"（多配色表盘）
            let mut color_group: Option<u32> = None;
            let mut choices: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
            if let Some(s) = args.get(6) {
                for kv in s.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
                    if let Some(v) = kv.strip_prefix("cg=") {
                        color_group = v.parse().ok();
                        continue;
                    }
                    if let Some((a, b)) = kv.split_once('=') {
                        if let (Ok(a), Ok(b)) = (a.trim().parse(), b.trim().parse()) {
                            choices.insert(a, b);
                        }
                    }
                }
            }
            match wf_installer_lib::watchface_render::derive_model(&data, None) {
                Ok(m) => {
                    let photo = photo_path.and_then(|p| {
                        image::open(p).ok().map(|im| {
                            let rgba = im.to_rgba8();
                            let (w, h) = rgba.dimensions();
                            (rgba.into_raw(), w, h)
                        })
                    });
                    let opts = wf_installer_lib::watchface_render::RenderOpts {
                        photo: photo.as_ref().map(|(d, w, h)| (d.as_slice(), *w, *h)),
                        // 可选：第 7 个参数 "scale,dx,dy,rotation" 指定照片裁剪变换
                        photo_transform: args.get(7).and_then(|s| {
                            let v: Vec<f32> =
                                s.split(',').filter_map(|t| t.trim().parse().ok()).collect();
                            (v.len() == 4).then(|| {
                                wf_installer_lib::watchface_render::PhotoTransform {
                                    scale: v[0],
                                    dx: v[1],
                                    dy: v[2],
                                    rotation: v[3],
                                }
                            })
                        }),
                        slot_choices: if choices.is_empty() { None } else { Some(&choices) },
                        color_group,
                    };
                    match wf_installer_lib::watchface_render::render_preview(&data, &m, theme, &opts) {
                        Ok((rgba, w, h)) => {
                            let img = image::RgbaImage::from_raw(w, h, rgba)
                                .ok_or("RGBA 尺寸不匹配").unwrap();
                            img.save_with_format(&out, image::ImageFormat::Png)
                                .map_err(|e| format!("保存失败: {e}")).unwrap();
                            println!("已渲染 {w}x{h}（配色 {}）→ {out}", color_group.unwrap_or(0));
                        }
                        Err(e) => { eprintln!("渲染失败: {e:#}"); std::process::exit(1); }
                    }
                }
                Err(e) => { eprintln!("推导失败: {e:#}"); std::process::exit(1); }
            }
        }
        "compose" => {
            // binface compose <bin> <out.png> <themeIdx> <photo> [scale,dx,dy,rot]
            let out = args.get(3).cloned().unwrap_or_else(|| "photo.png".into());
            let theme: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let photo_path = args.get(5).cloned().unwrap_or_else(|| "photo.png".into());
            let tr = args.get(6).and_then(|s| {
                let v: Vec<f32> = s.split(',').filter_map(|t| t.trim().parse().ok()).collect();
                (v.len() == 4).then(|| wf_installer_lib::watchface_render::PhotoTransform {
                    scale: v[0], dx: v[1], dy: v[2], rotation: v[3],
                })
            });
            match wf_installer_lib::watchface_render::derive_model(&data, None) {
                Ok(m) => {
                    let photo = image::open(&photo_path).map(|im| {
                        let rgba = im.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        (rgba.into_raw(), w, h)
                    });
                    let Ok((prgba, pw, ph)) = photo else {
                        eprintln!("照片打开失败: {photo_path}");
                        std::process::exit(1);
                    };
                    match wf_installer_lib::watchface_render::compose_photo_canvas(
                        &data, &m, theme, (&prgba, pw, ph), tr,
                    ) {
                        Ok((rgba, w, h)) => {
                            let img = image::RgbaImage::from_raw(w, h, rgba).unwrap();
                            img.save_with_format(&out, image::ImageFormat::Png)
                                .map_err(|e| format!("保存失败: {e}")).unwrap();
                            println!("已合成壁纸 {w}x{h} → {out}");
                        }
                        Err(e) => { eprintln!("合成失败: {e:#}"); std::process::exit(1); }
                    }
                }
                Err(e) => { eprintln!("推导失败: {e:#}"); std::process::exit(1); }
            }
        }
        other => {
            eprintln!("未知命令: {other}");
            std::process::exit(2);
        }
    }
}
