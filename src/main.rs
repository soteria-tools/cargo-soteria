use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::Duration;
use zip::ZipArchive;

/// Version of this crate, used as the install subdirectory under ~/.soteria/
const VERSION: &str = env!("CARGO_PKG_VERSION");

const REPO_OWNER: &str = "soteria-tools";
const REPO_NAME: &str = "soteria";
const RELEASE_TAG: &str = "nightly";

// ── UI helpers ────────────────────────────────────────────────────────────────

fn spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

fn download_bar(total: u64, msg: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{msg}\n  {bar:40.cyan/dim} {bytes}/{total_bytes}  {bytes_per_sec}  eta {eta}",
            )
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "),
    );
    pb.set_message(msg.to_string());
    pb
}

fn ok(msg: &str) {
    println!("{} {}", "✓".green().bold(), msg);
}

fn info(msg: &str) {
    println!("{} {}", "·".cyan(), msg);
}

fn warn(msg: &str) {
    println!("{} {}", "!".yellow().bold(), msg);
}

fn fail(msg: &str) -> ! {
    eprintln!("{} {}", "✗".red().bold(), msg);
    process::exit(1);
}

// ── path helpers ──────────────────────────────────────────────────────────────

fn soteria_base_dir() -> PathBuf {
    let home = env::var("HOME").expect("HOME environment variable not set");
    PathBuf::from(home).join(".soteria")
}

fn package_dir() -> PathBuf {
    soteria_base_dir().join(VERSION)
}

// ── version tracking ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct VersionInfo {
    /// Tag name of the release that was installed (e.g. "nightly")
    release_tag: String,
    /// The published_at timestamp of the release
    published_at: String,
    /// The numeric GitHub release ID — used to detect updates
    release_id: u64,
}

fn version_file() -> PathBuf {
    package_dir().join("version.json")
}

fn read_installed_version() -> Option<VersionInfo> {
    let contents = fs::read_to_string(version_file()).ok()?;
    serde_json::from_str(&contents).ok()
}

fn write_version_info(info: &VersionInfo) {
    let contents = serde_json::to_string_pretty(info).expect("Failed to serialize version info");
    fs::write(version_file(), contents).expect("Failed to write version.json");
}

// ── GitHub release API types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GhRelease {
    id: u64,
    tag_name: String,
    published_at: String,
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

// ── GitHub API helpers ────────────────────────────────────────────────────────

fn fetch_nightly_release() -> GhRelease {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/tags/{}",
        REPO_OWNER, REPO_NAME, RELEASE_TAG
    );

    let sp = spinner(&format!(
        "Fetching {} release info…",
        RELEASE_TAG.cyan().bold()
    ));

    let client = reqwest::blocking::Client::builder()
        .user_agent("cargo-soteria")
        .build()
        .expect("Failed to build HTTP client");

    let resp = client.get(&url).send().unwrap_or_else(|e| {
        sp.finish_and_clear();
        fail(&format!("Failed to reach GitHub API: {e}"));
    });

    if !resp.status().is_success() {
        sp.finish_and_clear();
        fail(&format!(
            "GitHub API returned {} for release '{}'. Make sure the release exists on {}/{}.",
            resp.status(),
            RELEASE_TAG,
            REPO_OWNER,
            REPO_NAME
        ));
    }

    let release = resp.json::<GhRelease>().unwrap_or_else(|e| {
        sp.finish_and_clear();
        fail(&format!("Failed to parse GitHub release response: {e}"));
    });

    sp.finish_and_clear();
    ok(&format!(
        "Release {} · published {}",
        release.tag_name.cyan().bold(),
        release.published_at.dimmed()
    ));

    release
}

/// Determine the expected asset name for the current platform.
fn expected_asset_name() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    if os != "macos" || arch != "aarch64" {
        fail(&format!(
            "Unsupported platform: {os}/{arch}. \
             Pre-built binaries are currently only available for macOS on Apple Silicon (macos/aarch64). \
             See https://github.com/{REPO_OWNER}/{REPO_NAME} for updates."
        ));
    }

    format!("soteria-rust-{}.zip", os)
}

/// Download `url` with a live progress bar. Returns the raw bytes.
fn download_bytes(url: &str, asset_name: &str, total_size: u64) -> Vec<u8> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("cargo-soteria")
        .build()
        .expect("Failed to build HTTP client");

    let resp = client.get(url).send().unwrap_or_else(|e| {
        fail(&format!("Download failed: {e}"));
    });

    if !resp.status().is_success() {
        fail(&format!("Download returned HTTP {}", resp.status()));
    }

    let pb = download_bar(
        total_size,
        &format!("  {} {}", "↓".cyan().bold(), asset_name.bold()),
    );

    let mut buf = Vec::with_capacity(total_size as usize);
    let mut reader = resp;
    let mut chunk = [0u8; 65536];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                pb.inc(n as u64);
            }
            Err(e) => {
                pb.finish_and_clear();
                fail(&format!("Error reading download: {e}"));
            }
        }
    }

    pb.finish_and_clear();
    ok(&format!(
        "Downloaded {} ({})",
        asset_name.bold(),
        format_bytes(buf.len() as u64).dimmed()
    ));

    buf
}

fn format_bytes(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1} MB", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1} KB", n as f64 / 1_000.0)
    } else {
        format!("{} B", n)
    }
}

// ── extraction ────────────────────────────────────────────────────────────────

/// Extract a zip archive into `dest`, stripping the top-level directory.
/// The zip is expected to contain a single top-level directory (e.g. `soteria-rust/`)
/// whose contents are placed directly into `dest`.
fn extract_zip(data: &[u8], dest: &Path) {
    let cursor = Cursor::new(data);
    let mut archive = ZipArchive::new(cursor).expect("Failed to open zip archive");

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("Failed to read zip entry");
        let raw_path = file.mangled_name();

        // Strip the top-level directory component (e.g. "soteria-rust/")
        let mut components = raw_path.components();
        components.next(); // skip first component
        let stripped: PathBuf = components.collect();
        if stripped.as_os_str().is_empty() {
            continue; // was the top-level dir itself
        }

        let out_path = dest.join(&stripped);

        if file.is_dir() {
            fs::create_dir_all(&out_path).expect("Failed to create directory");
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).expect("Failed to create parent directory");
            }
            let mut out_file = fs::File::create(&out_path).expect("Failed to create file");
            io::copy(&mut file, &mut out_file).expect("Failed to write file");
        }
    }
}

/// chmod +x all files under `dest/bin/`.
fn make_bins_executable(dest: &Path) {
    let bin_dir = dest.join("bin");
    if bin_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&bin_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let mut perms = fs::metadata(&path)
                        .expect("Failed to read file metadata")
                        .permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&path, perms).expect("Failed to set executable permission");
                }
            }
        }
    }
}

/// Extract archive bytes into dest atomically via a temp directory.
fn install_package(data: &[u8], dest: &Path) {
    let sp = spinner("Extracting…");

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).expect("Failed to create ~/.soteria directory");
    }

    let temp_dest = dest.with_extension("installing");
    if temp_dest.exists() {
        fs::remove_dir_all(&temp_dest).ok();
    }
    fs::create_dir_all(&temp_dest).expect("Failed to create temp installation directory");

    extract_zip(data, &temp_dest);
    make_bins_executable(&temp_dest);

    // Remove existing install and move temp into place
    if dest.exists() {
        fs::remove_dir_all(dest).expect("Failed to remove existing installation");
    }
    fs::rename(&temp_dest, dest).unwrap_or_else(|_| {
        if !dest.exists() {
            panic!("Failed to move installed package to {}", dest.display());
        }
        fs::remove_dir_all(&temp_dest).ok();
    });

    sp.finish_and_clear();
    ok(&format!(
        "Installed to {}",
        dest.display().to_string().dimmed()
    ));
}

// ── copy from local path ──────────────────────────────────────────────────────

fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("Failed to create destination directory");
    for entry in fs::read_dir(src).expect("Failed to read source directory") {
        let entry = entry.expect("Failed to read directory entry");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_all(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).expect("Failed to copy file");
        }
    }
}

fn install_from_local(local_path: &Path) {
    let dest = package_dir();

    let source = local_path.join("packages").join("soteria-rust");
    if !source.exists() {
        fail(&format!(
            "Expected package directory not found at {}.\nBuild the package first (e.g. 'make package-soteria-rust').",
            source.display()
        ));
    }

    info(&format!(
        "Source: {}",
        source.display().to_string().dimmed()
    ));

    let sp = spinner("Copying…");
    let temp_dest = dest.with_extension("installing");
    if temp_dest.exists() {
        fs::remove_dir_all(&temp_dest).ok();
    }
    copy_dir_all(&source, &temp_dest);
    make_bins_executable(&temp_dest);

    if dest.exists() {
        fs::remove_dir_all(&dest).expect("Failed to remove existing installation");
    }
    fs::rename(&temp_dest, &dest).unwrap_or_else(|_| {
        if !dest.exists() {
            panic!("Failed to move installed package to {}", dest.display());
        }
        fs::remove_dir_all(&temp_dest).ok();
    });
    sp.finish_and_clear();
    ok(&format!(
        "Installed to {}",
        dest.display().to_string().dimmed()
    ));

    // Try to get git metadata for version.json

    let commit_sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(local_path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let published_at = Command::new("git")
        .args(["log", "-1", "--format=%aI"])
        .current_dir(local_path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    write_version_info(&VersionInfo {
        release_tag: format!("local:{}", commit_sha),
        published_at,
        release_id: 0,
    });
}

// ── setup command ─────────────────────────────────────────────────────────────

fn prompt_yes_no(question: &str) -> bool {
    print!("{} {} ", "?".yellow().bold(), question);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

fn check_toolchain() {
    let sp = spinner("Checking that the right toolchain is installed…");

    let obol = package_dir().join("bin").join("obol");
    let output = Command::new(&obol).arg("toolchain-path").output();

    sp.finish_and_clear();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // obol may print installation progress before the final path;
            // print any such lines verbatim, then use the last line as the path.
            let mut lines = stdout.lines().filter(|l| !l.is_empty()).peekable();
            let mut last = String::new();
            while let Some(line) = lines.next() {
                if lines.peek().is_some() {
                    println!("  {}", line);
                } else {
                    last = line.to_string();
                }
            }
            ok(&format!("Toolchain found at {}", last.dimmed()));
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            fail(&format!(
                "Toolchain check failed (exit {}){}",
                out.status.code().unwrap_or(1),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            ));
        }
        Err(e) => {
            fail(&format!("Failed to run obol: {e}"));
        }
    }
}

fn cmd_setup(local_path: Option<&str>) {
    println!();
    println!("{}", "  Soteria Setup".bold().cyan());
    println!("{}", "  ─────────────".dimmed());
    println!();

    // ── local mode ────────────────────────────────────────────────────────────
    if let Some(path_str) = local_path {
        let local_path = Path::new(path_str);
        if !local_path.exists() {
            fail(&format!("Path '{}' does not exist.", path_str));
        }

        let dest = package_dir();
        if dest.exists() {
            if !prompt_yes_no("Soteria is already installed. Update from local path? [y/N]") {
                warn("Aborted.");
                process::exit(0);
            }
            println!();
        }

        install_from_local(local_path);
        println!();
        check_toolchain();
        println!();
        ok(&format!(
            "{}",
            "Soteria installed successfully from local build.".bold()
        ));
        println!();
        info(&format!(
            "Run {} to start analysing a project.",
            "cargo soteria".cyan().bold()
        ));
        println!();
        return;
    }

    // ── remote mode ───────────────────────────────────────────────────────────
    let release = fetch_nightly_release();

    // Check if already installed
    if let Some(installed) = read_installed_version() {
        if installed.release_id == release.id {
            warn(&format!(
                "Already up to date  (published {})",
                installed.published_at.dimmed()
            ));
            if !prompt_yes_no("Re-install anyway? [y/N]") {
                println!();
                info("Nothing to do.");
                process::exit(0);
            }
        } else {
            warn(&format!(
                "Update available  {} → {}",
                installed.published_at.dimmed(),
                release.published_at.cyan()
            ));
            if !prompt_yes_no("Update now? [y/N]") {
                println!();
                warn("Aborted.");
                process::exit(0);
            }
        }
        println!();
    }

    // Find the right asset
    let asset_name = expected_asset_name();
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .unwrap_or_else(|| {
            let available: Vec<_> = release.assets.iter().map(|a| a.name.as_str()).collect();
            fail(&format!(
                "No asset '{}' found in the '{}' release.\nAvailable: {}",
                asset_name,
                RELEASE_TAG,
                available.join(", ")
            ));
        });

    let data = download_bytes(&asset.browser_download_url, &asset.name, asset.size);

    let dest = package_dir();
    install_package(&data, &dest);

    write_version_info(&VersionInfo {
        release_tag: release.tag_name.clone(),
        published_at: release.published_at.clone(),
        release_id: release.id,
    });

    println!();
    check_toolchain();
    println!();
    ok(&format!("{}", "Soteria installed successfully.".bold()));
    println!();
    info(&format!(
        "Run {} to start analysing a project.",
        "cargo soteria".cyan().bold()
    ));
    println!();
}

// ── unsetup command ───────────────────────────────────────────────────────────

fn cmd_unsetup() {
    let dest = package_dir();

    if !dest.exists() {
        info("Soteria is not set up.");
        return;
    }

    fs::remove_dir_all(&dest).unwrap_or_else(|e| {
        fail(&format!("Failed to remove '{}': {e}", dest.display()));
    });

    ok(&format!(
        "Soteria uninstalled  (removed {}).",
        dest.display()
    ));
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    // When invoked as `cargo soteria [args...]`, argv is:
    //   cargo-soteria soteria [args...]
    // Strip the "soteria" word that cargo inserts.
    let args: Vec<String> = env::args().collect();
    let args: &[String] = if args.len() > 1 && args[1] == "soteria" {
        &args[2..]
    } else {
        &args[1..]
    };

    // Dispatch subcommands
    if args.first().map(|s| s.as_str()) == Some("setup") {
        let local_path = args
            .windows(2)
            .find(|w| w[0] == "--local")
            .map(|w| w[1].as_str());
        cmd_setup(local_path);
        return;
    }

    if args.first().map(|s| s.as_str()) == Some("unsetup") {
        cmd_unsetup();
        return;
    }

    // Default: run soteria-rust, erroring if not installed
    let pkg = package_dir();
    let soteria_rust_bin = pkg.join("bin").join("soteria-rust");

    if !soteria_rust_bin.exists() {
        eprintln!("{} Soteria is not installed.", "✗".red().bold());
        eprintln!(
            "  Run {} to download and install it.",
            "cargo soteria setup".cyan().bold()
        );
        process::exit(1);
    }

    let bin_dir = pkg.join("bin");
    let lib_dir = pkg.join("lib");
    let plugins_dir = pkg.join("plugins");

    let lib_path_var = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let existing_lib_path = env::var(lib_path_var).unwrap_or_default();
    let new_lib_path = if existing_lib_path.is_empty() {
        lib_dir.to_string_lossy().to_string()
    } else {
        format!("{}:{}", lib_dir.display(), existing_lib_path)
    };

    let status = Command::new(&soteria_rust_bin)
        .arg("exec")
        .arg(".")
        .args(args)
        .env(lib_path_var, &new_lib_path)
        .env("SOTERIA_Z3_PATH", bin_dir.join("z3"))
        .env("SOTERIA_OBOL_PATH", bin_dir.join("obol"))
        .env("SOTERIA_CHARON_PATH", bin_dir.join("charon"))
        .env("SOTERIA_RUST_PLUGINS", &plugins_dir)
        .status();

    match status {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("{} Failed to execute soteria-rust: {e}", "✗".red().bold());
            eprintln!("  Binary: {}", soteria_rust_bin.display());
            process::exit(1);
        }
    }
}
