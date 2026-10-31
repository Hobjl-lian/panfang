fn main() {
    // Android 15+ 的 16KB 内存页设备会拒绝加载 4KB 对齐的 .so（release 构建静默闪退）。
    // NDK r27 及以下需显式指定 16KB 页对齐；r28+ 默认即为 16KB。
    // 注意：不能用 .cargo/config.toml 的 rustflags —— Tauri CLI 会用自己的
    // CARGO_TARGET_<triple>_RUSTFLAGS 环境变量覆盖它；从 build script 发出的
    // rustc-link-arg 指令则独立生效（见 Android 官方 16KB 页文档）。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-arg=-Wl,-z,max-page-size=16384");
        println!("cargo:rustc-link-arg=-Wl,-z,common-page-size=16384");
    }
    tauri_build::build()
}
