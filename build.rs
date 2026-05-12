fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let supported = matches!(
        (os.as_str(), arch.as_str()),
        ("macos", "aarch64") | ("linux", "x86_64")
    );

    if !supported {
        println!(
            "cargo::error=Unsupported platform: {os}/{arch}. \
             Pre-built binaries are available for macOS ARM64 (aarch64-apple-darwin) \
             and Linux x86_64 (x86_64-unknown-linux-gnu)."
        );
    }
}
