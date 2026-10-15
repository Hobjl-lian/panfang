//! .bin 检查器 CLI：读取表盘文件的元数据（用于排障与「识别版本」验证）。
//!
//! ```bash
//! cargo run --bin wfbin -- "D:/设计相关/AWOS-照片表盘/表盘文件/2.0/AWOS-10-v2.bin"
//! ```

use wf_auth_server::bin_inspect;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("用法: wfbin <文件.bin> [路径提示（用于版本号推断，如 3.0/x.bin）]");
        std::process::exit(2);
    };
    let hint = args.next().unwrap_or_default();
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("读取失败 {path}: {e}");
            std::process::exit(1);
        }
    };
    match bin_inspect::inspect(&data) {
        Ok(info) => {
            let file_name = std::path::Path::new(&path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let hint_path = if hint.is_empty() { path.clone() } else { hint };
            println!("文件      {}", path);
            println!("大小      {} 字节", info.size);
            println!("表盘 id   {}", if info.watchface_id.is_empty() { "（空）" } else { &info.watchface_id });
            println!("表盘名称  {}", info.name);
            println!("头部版本  {}  (sub {})", info.fmt_version, info.sub_version);
            println!("样式数    {}", info.style_count);
            match info.canvas {
                Some((w, h)) => println!("画布      {w}×{h}"),
                None => println!("画布      未识别"),
            }
            if !info.device_hints.is_empty() {
                println!("设备提示  {}", info.device_hints.join(" / "));
            }
            println!("内容哈希  {}", info.sha256);
            match bin_inspect::version_hint(&file_name, &hint_path) {
                Some(v) => println!("版本推断  {v}（来自文件名/目录名）"),
                None => println!("版本推断  无（需按同变体历史递增）"),
            }
        }
        Err(e) => {
            eprintln!("解析失败: {e}");
            std::process::exit(1);
        }
    }
}
