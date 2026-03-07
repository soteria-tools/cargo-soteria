fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    if os != "macos" || arch != "aarch64" {
        println!(
            "cargo::error=Unsupported platform: {os}/{arch}. \
             Pre-built binaries are currently only available for macOS on Apple Silicon (macos/aarch64)."
        );
    }
}
