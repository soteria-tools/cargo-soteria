//! The `setup` and `unsetup` management subcommands: downloading (or copying
//! from a local build) the `soteria-rust` toolchain into `~/.soteria/soteria-release/`,
//! recording the installed release for update detection, verifying the Rust
//! toolchain, pre-building the analyzer plugins, and removing it all again.

use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

use colored::Colorize;
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use crate::common::{download_bar, fail, info, ok, package_dir, soteria_base_dir, spinner, warn};
use crate::runner_common::soteria_rust_command;

const REPO_OWNER: &str = "soteria-tools";
const REPO_NAME: &str = "soteria";
/// The moving tag of the rolling nightly release.
const RELEASE_TAG: &str = "nightly";

/// Inclusive window of soteria-rust minor series this cargo-soteria is tested
/// against, as `(major, minor)`. Installing anything outside it — or the
/// nightly — prints a "no guarantee it will work" warning and asks to confirm.
/// These are illustrative; set them to the real supported window as versioning
/// proceeds.
const MIN_SUPPORTED_MINOR: (u64, u64) = (0, 1); // 0.1.x
const MAX_SUPPORTED_MINOR: (u64, u64) = (0, 1); // 0.1.x

// ── release versions ──────────────────────────────────────────────────────────

/// A `major.minor.patch` release version, ordered numerically (the derived `Ord`
/// compares field-by-field, so the declaration order below *is* the ordering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// Parse a clean `X.Y.Z`, tolerating a single leading `v` (the exact tag
    /// format upstream will use is not yet fixed). Anything that isn't exactly
    /// three integer components — `nightly`, `0.3`, `0.3.5-rc1`, `1.2.3.4` — is
    /// rejected (returns `None`), which is what filters the nightly tag out of
    /// stable-release resolution.
    fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None; // reject a 4th component like "1.2.3.4"
        }
        Some(Version {
            major,
            minor,
            patch,
        })
    }

    /// The `(major, minor)` series this version belongs to.
    fn minor_series(&self) -> (u64, u64) {
        (self.major, self.minor)
    }

    /// Whether this version's series lies inside the supported window.
    fn is_supported(&self) -> bool {
        let s = self.minor_series();
        s >= MIN_SUPPORTED_MINOR && s <= MAX_SUPPORTED_MINOR
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Which release the user asked `setup` to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseSpec {
    /// No `--release`: install the latest patch of the highest supported minor.
    Default,
    /// `--release nightly`: the rolling nightly build.
    Nightly,
    /// `--release X.Y.Z`: that exact version.
    Exact(Version),
}

/// Parse the `--release` argument. `Err` carries a user-facing message.
fn parse_release_spec(arg: Option<&str>) -> Result<ReleaseSpec, String> {
    match arg {
        None => Ok(ReleaseSpec::Default),
        Some(s) if s.eq_ignore_ascii_case(RELEASE_TAG) => Ok(ReleaseSpec::Nightly),
        Some(s) => Version::parse(s).map(ReleaseSpec::Exact).ok_or_else(|| {
            format!("Invalid --release value '{s}'. Expected 'nightly' or a version like '0.3.5'.")
        }),
    }
}

/// The parseable versions among a set of releases, ascending — for error text.
fn available_versions(releases: &[GhRelease]) -> Vec<Version> {
    let mut vs: Vec<Version> = releases
        .iter()
        .filter_map(|r| Version::parse(&r.tag_name))
        .collect();
    vs.sort();
    vs
}

/// Find the release whose tag parses to exactly `want`.
fn find_exact(releases: &[GhRelease], want: Version) -> Option<&GhRelease> {
    releases
        .iter()
        .find(|r| Version::parse(&r.tag_name) == Some(want))
}

/// The highest supported stable release (highest version whose series is inside
/// the window). Unparseable tags — including `nightly` — are ignored.
fn pick_default(releases: &[GhRelease]) -> Option<(Version, &GhRelease)> {
    releases
        .iter()
        .filter_map(|r| Version::parse(&r.tag_name).map(|v| (v, r)))
        .filter(|(v, _)| v.is_supported())
        .max_by_key(|(v, _)| *v)
}

/// Render `(major, minor)` as `major.minor` for messages.
fn fmt_series(s: (u64, u64)) -> String {
    format!("{}.{}", s.0, s.1)
}

/// Comma-join versions for an "available: …" message, or `none`.
fn join_versions(versions: &[Version]) -> String {
    if versions.is_empty() {
        "none".to_string()
    } else {
        versions
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Turn a GitHub `published_at` (`2026-03-05T21:59:50Z`) into a filename-safe
/// stamp (`2026-03-05-21-59-50`) for the nightly's VERSION label.
fn sanitize_timestamp(ts: &str) -> String {
    ts.trim_end_matches('Z').replace(['T', ':'], "-")
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

/// Path of the human-readable `VERSION` file inside the install dir.
fn version_name_file() -> PathBuf {
    package_dir().join("VERSION")
}

/// Record the human-readable name of the installed release (e.g. `0.3.0` or
/// `nightly-2026-03-05-21-59-50`) so users and `unsetup` can see what's here.
fn write_version_name(name: &str) {
    fs::write(version_name_file(), format!("{name}\n")).expect("Failed to write VERSION file");
}

/// Read the installed release name from the `VERSION` file, if present.
fn read_version_name() -> Option<String> {
    let contents = fs::read_to_string(version_name_file()).ok()?;
    let trimmed = contents.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

// ── GitHub release API types ──────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct GhRelease {
    id: u64,
    tag_name: String,
    /// `Option` because a *draft* release carries a null `published_at`; a single
    /// null would otherwise break deserializing the whole `list_releases` array.
    published_at: Option<String>,
    assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
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

    let mut req = client.get(&url);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.bearer_auth(token);
    }
    let resp = req.send().unwrap_or_else(|e| {
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
        release
            .published_at
            .as_deref()
            .unwrap_or("unknown")
            .dimmed()
    ));

    release
}

/// List the most recent releases of the repo (`GET /releases?per_page=100`).
/// Used to resolve the default (latest supported stable) and to look up an
/// explicit `--release X.Y.Z`. Only the newest 100 releases are visible — no
/// pagination — which is ample: the nightly is a single moving tag, so the list
/// is that plus the stable releases.
fn list_releases() -> Vec<GhRelease> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases?per_page=100",
        REPO_OWNER, REPO_NAME
    );

    let sp = spinner("Fetching available releases…");

    let client = reqwest::blocking::Client::builder()
        .user_agent("cargo-soteria")
        .build()
        .expect("Failed to build HTTP client");

    let mut req = client.get(&url);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.bearer_auth(token);
    }
    let resp = req.send().unwrap_or_else(|e| {
        sp.finish_and_clear();
        fail(&format!("Failed to reach GitHub API: {e}"));
    });

    if !resp.status().is_success() {
        sp.finish_and_clear();
        fail(&format!(
            "GitHub API returned {} listing releases for {}/{}.",
            resp.status(),
            REPO_OWNER,
            REPO_NAME
        ));
    }

    let releases = resp.json::<Vec<GhRelease>>().unwrap_or_else(|e| {
        sp.finish_and_clear();
        fail(&format!("Failed to parse GitHub releases response: {e}"));
    });

    sp.finish_and_clear();
    releases
}

/// The platform-specific suffix of the soteria-rust bundle asset, e.g.
/// `macos-arm64.zip`. Both the nightly (`soteria-rust-macos-arm64.zip`) and a
/// versioned (`soteria-rust-v0.1.0-macos-arm64.zip`) asset end with it.
fn platform_asset_suffix() -> &'static str {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    match (os, arch) {
        ("macos", "aarch64") => "macos-arm64.zip",
        ("linux", "x86_64") => "linux-x86_64.zip",
        _ => fail(&format!(
            "Unsupported platform: {os}/{arch}. \
             Pre-built binaries are available for macOS ARM64 (aarch64-apple-darwin) \
             and Linux x86_64 (x86_64-unknown-linux-gnu). \
             See https://github.com/{REPO_OWNER}/{REPO_NAME} for updates."
        )),
    }
}

/// Find the soteria-rust bundle for this platform among a release's assets.
/// Versioned releases embed the version in the filename
/// (`soteria-rust-v0.1.0-macos-arm64.zip`) while nightly does not
/// (`soteria-rust-macos-arm64.zip`), so match on the `soteria-rust-` prefix plus
/// the platform suffix rather than an exact name. The prefix guard avoids
/// picking the sibling `soteria-c-…` bundle, which shares the platform suffix.
fn find_platform_asset(release: &GhRelease) -> &GhAsset {
    let suffix = platform_asset_suffix();
    release
        .assets
        .iter()
        .find(|a| a.name.starts_with("soteria-rust-") && a.name.ends_with(suffix))
        .unwrap_or_else(|| {
            let available: Vec<_> = release.assets.iter().map(|a| a.name.as_str()).collect();
            fail(&format!(
                "No soteria-rust bundle for '{}' found in the '{}' release.\nAvailable: {}",
                suffix,
                release.tag_name,
                available.join(", ")
            ))
        })
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
    write_version_name(&format!("local-{commit_sha}"));
}

// ── setup command ─────────────────────────────────────────────────────────────

fn prompt_yes_no(question: &str) -> bool {
    print!("{} {} ", "?".yellow().bold(), question);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Warn that the requested release is untested, then either honour `--yes` or
/// ask the user to confirm. Aborts (exit 0) if they decline.
fn confirm_or_abort(yes: bool, warning: &str) {
    warn(warning);
    warn("There is no guarantee it will work with this cargo-soteria.");
    if yes {
        info("Proceeding anyway (--yes).");
        return;
    }
    if !prompt_yes_no("Continue anyway? [y/N]") {
        warn("Aborted.");
        process::exit(0);
    }
    println!();
}

/// Resolve a [`ReleaseSpec`] to the concrete GitHub release to install and the
/// human-readable label to record in the `VERSION` file. Prints the untested
/// warning + confirmation (honouring `yes`) *before* any download for nightly
/// and out-of-window versions.
fn resolve_release(spec: ReleaseSpec, yes: bool) -> (GhRelease, String) {
    match spec {
        ReleaseSpec::Nightly => {
            confirm_or_abort(
                yes,
                "Installing the nightly (unversioned, rolling) release.",
            );
            let release = fetch_nightly_release();
            let stamp = release
                .published_at
                .as_deref()
                .map(sanitize_timestamp)
                .unwrap_or_else(|| "unknown".to_string());
            let label = format!("nightly-{stamp}");
            (release, label)
        }
        ReleaseSpec::Exact(want) => {
            if !want.is_supported() {
                confirm_or_abort(
                    yes,
                    &format!(
                        "Release {want} is outside the supported range {}.x – {}.x.",
                        fmt_series(MIN_SUPPORTED_MINOR),
                        fmt_series(MAX_SUPPORTED_MINOR),
                    ),
                );
            }
            let releases = list_releases();
            let release = find_exact(&releases, want).cloned().unwrap_or_else(|| {
                fail(&format!(
                    "No release {want} found for {REPO_OWNER}/{REPO_NAME}.\nAvailable versions: {}",
                    join_versions(&available_versions(&releases))
                ))
            });
            (release, want.to_string())
        }
        ReleaseSpec::Default => {
            let releases = list_releases();
            let (version, release) = pick_default(&releases)
                .map(|(v, r)| (v, r.clone()))
                .unwrap_or_else(|| {
                    fail(&format!(
                        "No supported stable release found (need {}.x – {}.x).\n\
                         Available versions: {}\n\
                         Use `--release nightly` or `--release X.Y.Z` to pick one explicitly.",
                        fmt_series(MIN_SUPPORTED_MINOR),
                        fmt_series(MAX_SUPPORTED_MINOR),
                        join_versions(&available_versions(&releases)),
                    ))
                });
            ok(&format!(
                "Selected release {}",
                version.to_string().cyan().bold()
            ));
            (release, version.to_string())
        }
    }
}

/// Pre-build the soteria-rust plugin crate so the first real run doesn't pay
/// the compilation cost. Without this, `soteria-rust` builds the plugins
/// lazily on first `exec`.
fn build_plugins() {
    // A full plugin-crate compile: stream cargo's output instead of buffering
    // it behind a spinner, since it can run for minutes on a cold cache.
    info("Building plugins…");
    let status = soteria_rust_command().arg("build-plugins").status();

    match status {
        Ok(s) if s.success() => ok("Plugins built."),
        Ok(s) => fail(&format!(
            "Building plugins failed (exit {})",
            s.code().unwrap_or(1)
        )),
        Err(e) => fail(&format!("Failed to run soteria-rust to build plugins: {e}")),
    }
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

pub fn cmd_setup(local_path: Option<&str>, release: Option<&str>, yes: bool) {
    println!();
    println!("{}", "  Soteria Setup".bold().cyan());
    println!("{}", "  ─────────────".dimmed());
    println!();

    // `--yes` skips every confirmation prompt (needed for non-interactive
    // installs, e.g. the Docker image build).
    let confirm = |question: &str| yes || prompt_yes_no(question);

    // ── local mode ────────────────────────────────────────────────────────────
    if let Some(path_str) = local_path {
        let local_path = Path::new(path_str);
        if !local_path.exists() {
            fail(&format!("Path '{}' does not exist.", path_str));
        }

        let dest = package_dir();
        if dest.exists() && !confirm("Soteria is already installed. Update from local path? [y/N]")
        {
            warn("Aborted.");
            process::exit(0);
        }
        if dest.exists() {
            println!();
        }

        install_from_local(local_path);
        println!();
        check_toolchain();
        build_plugins();
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
    let spec = parse_release_spec(release).unwrap_or_else(|e| fail(&e));

    // Resolve the requested release (printing the untested warning + confirming
    // for nightly / out-of-window versions before any download).
    let (release, version_name) = resolve_release(spec, yes);

    // Check if already installed
    if let Some(installed) = read_installed_version() {
        if installed.release_id == release.id {
            warn(&format!(
                "Already up to date  (published {})",
                installed.published_at.dimmed()
            ));
            if !confirm("Re-install anyway? [y/N]") {
                println!();
                info("Nothing to do.");
                process::exit(0);
            }
        } else {
            warn(&format!(
                "Installed release will be replaced  {} → {}",
                installed.published_at.dimmed(),
                release.published_at.as_deref().unwrap_or("unknown").cyan()
            ));
            if !confirm("Continue? [y/N]") {
                println!();
                warn("Aborted.");
                process::exit(0);
            }
        }
        println!();
    }

    // Find the right asset (name varies between nightly and versioned releases).
    let asset = find_platform_asset(&release);
    let data = download_bytes(&asset.browser_download_url, &asset.name, asset.size);

    let dest = package_dir();
    install_package(&data, &dest);

    write_version_info(&VersionInfo {
        release_tag: release.tag_name.clone(),
        published_at: release.published_at.clone().unwrap_or_default(),
        release_id: release.id,
    });
    write_version_name(&version_name);

    println!();
    check_toolchain();
    build_plugins();
    println!();
    ok(&format!(
        "{} ({})",
        "Soteria installed successfully.".bold(),
        version_name.cyan()
    ));
    println!();
    info(&format!(
        "Run {} to start analysing a project.",
        "cargo soteria".cyan().bold()
    ));
    println!();
}

// ── unsetup command ───────────────────────────────────────────────────────────

pub fn cmd_unsetup() {
    let base = soteria_base_dir();

    if !base.exists() {
        info("Soteria is not set up — nothing to remove.");
        return;
    }

    let size = get_dir_size(&base)
        .map(format_size)
        .unwrap_or_else(|_| "unknown".to_string());

    println!();
    info(&format!(
        "Found Soteria install at {}",
        base.display().to_string().cyan()
    ));
    info(&format!("Total size on disk: {}", size.bold()));

    // Show the installed release name, read from the VERSION file.
    if let Some(name) = read_version_name() {
        info(&format!("Installed release: {}", name.cyan()));
    }
    println!();

    if !prompt_yes_no(&format!(
        "Remove {} and everything under it? [y/N]",
        base.display()
    )) {
        info("Cancelled — nothing was removed.");
        return;
    }

    fs::remove_dir_all(&base).unwrap_or_else(|e| {
        fail(&format!("Failed to remove '{}': {e}", base.display()));
    });

    println!();
    ok(&format!(
        "Soteria uninstalled — freed {} ({}).",
        size.bold(),
        base.display()
    ));
}

/// Recursively sum the size of every regular file under `path`.
fn get_dir_size(path: &Path) -> io::Result<u64> {
    let mut total = 0;
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                total += get_dir_size(&entry.path())?;
            } else {
                total += metadata.len();
            }
        }
    }
    Ok(total)
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    fn rel(tag: &str) -> GhRelease {
        GhRelease {
            id: 0,
            tag_name: tag.to_string(),
            published_at: Some("2026-01-01T00:00:00Z".to_string()),
            assets: vec![],
        }
    }

    fn asset(name: &str) -> GhAsset {
        GhAsset {
            name: name.to_string(),
            browser_download_url: String::new(),
            size: 0,
        }
    }

    #[test]
    fn find_platform_asset_matches_versioned_and_nightly() {
        // Suffix is host-specific; build names from it so the test is portable.
        let suffix = platform_asset_suffix();

        // Versioned release: the rust bundle embeds the version, and a sibling
        // soteria-c bundle shares the platform suffix and must NOT be chosen.
        let mut versioned = rel("v0.1.0");
        versioned.assets = vec![
            asset("SHA256SUMS.txt"),
            asset(&format!("soteria-c-v0.1.0-{suffix}")),
            asset(&format!("soteria-rust-v0.1.0-{suffix}")),
        ];
        assert_eq!(
            find_platform_asset(&versioned).name,
            format!("soteria-rust-v0.1.0-{suffix}")
        );

        // Nightly release: no version segment in the name.
        let mut nightly = rel("nightly");
        nightly.assets = vec![asset(&format!("soteria-rust-{suffix}"))];
        assert_eq!(
            find_platform_asset(&nightly).name,
            format!("soteria-rust-{suffix}")
        );
    }

    #[test]
    fn version_parses_clean_and_v_prefixed() {
        assert_eq!(Version::parse("0.3.5"), Some(v(0, 3, 5)));
        assert_eq!(Version::parse("v0.3.5"), Some(v(0, 3, 5)));
        assert_eq!(Version::parse(" 1.20.300 "), Some(v(1, 20, 300)));
    }

    #[test]
    fn version_rejects_non_semver() {
        for bad in [
            "nightly",
            "0.3",
            "0.3.5-rc1",
            "1.2.3.4",
            "",
            "x.y.z",
            "0.3.x",
        ] {
            assert_eq!(Version::parse(bad), None, "should reject {bad:?}");
        }
    }

    #[test]
    fn version_orders_numerically() {
        assert!(Version::parse("0.3.10") > Version::parse("0.3.9"));
        assert!(Version::parse("0.4.0") > Version::parse("0.3.99"));
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
    }

    #[test]
    fn support_window_uses_constants() {
        // Constant-relative so it holds for whatever window is configured.
        let at_min = v(MIN_SUPPORTED_MINOR.0, MIN_SUPPORTED_MINOR.1, 0);
        let at_max = v(MAX_SUPPORTED_MINOR.0, MAX_SUPPORTED_MINOR.1, 99);
        let above_max = v(MAX_SUPPORTED_MINOR.0, MAX_SUPPORTED_MINOR.1 + 1, 0);
        assert!(at_min.is_supported(), "min series must be supported");
        assert!(at_max.is_supported(), "max series must be supported");
        assert!(
            !above_max.is_supported(),
            "series above max must be rejected"
        );
        assert!(!v(999, 0, 0).is_supported());
    }

    #[test]
    fn parse_release_spec_variants() {
        assert_eq!(parse_release_spec(None), Ok(ReleaseSpec::Default));
        assert_eq!(
            parse_release_spec(Some("nightly")),
            Ok(ReleaseSpec::Nightly)
        );
        assert_eq!(
            parse_release_spec(Some("NIGHTLY")),
            Ok(ReleaseSpec::Nightly)
        );
        assert_eq!(
            parse_release_spec(Some("0.3.5")),
            Ok(ReleaseSpec::Exact(v(0, 3, 5)))
        );
        assert!(parse_release_spec(Some("bogus")).is_err());
    }

    #[test]
    fn pick_default_takes_highest_supported() {
        // Two patches at the max supported minor, plus one above the window
        // (ignored) and the nightly tag (unparseable, ignored). Built relative
        // to the constants so it holds for whatever window is configured.
        let (maj, min) = MAX_SUPPORTED_MINOR;
        let lo = format!("{maj}.{min}.1");
        let hi = format!("v{maj}.{min}.4");
        let above = format!("{maj}.{}.0", min + 1);
        let releases = [rel("nightly"), rel(&lo), rel(&hi), rel(&above)];
        let (ver, _) = pick_default(&releases).expect("a supported release");
        assert_eq!(ver, v(maj, min, 4));
    }

    #[test]
    fn pick_default_none_when_window_empty() {
        // Only the nightly and versions far above any realistic window.
        let releases = [rel("nightly"), rel("998.0.0"), rel("999.9.9")];
        assert!(pick_default(&releases).is_none());
    }

    #[test]
    fn find_exact_matches_across_tag_styles() {
        let releases = [rel("0.2.5"), rel("v0.3.1")];
        assert!(find_exact(&releases, v(0, 3, 1)).is_some());
        assert!(find_exact(&releases, v(0, 2, 5)).is_some());
        assert!(find_exact(&releases, v(0, 9, 9)).is_none());
    }

    #[test]
    fn available_versions_sorted_and_filtered() {
        let releases = [rel("0.3.1"), rel("nightly"), rel("0.2.5")];
        assert_eq!(available_versions(&releases), vec![v(0, 2, 5), v(0, 3, 1)]);
    }

    #[test]
    fn sanitize_timestamp_is_filename_safe() {
        assert_eq!(
            sanitize_timestamp("2026-03-05T21:59:50Z"),
            "2026-03-05-21-59-50"
        );
    }
}
