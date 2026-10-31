// 本插件不向 JS 暴露命令（只给主 crate 的 Rust 代码用），所以 COMMANDS 为空。
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
