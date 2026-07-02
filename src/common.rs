//! Cross-cutting helpers shared across the binary: the install-path resolution,
//! the terminal UI primitives (spinners, the download bar, the `✓`/`·`/`!`/`✗`
//! status lines), and this crate's own version (shown in the offline help).

use std::env;
use std::path::PathBuf;
use std::process;
use std::time::Duration;

use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

/// Version of this crate. Shown in the offline `--help` banner (it no longer
/// names the install directory — see [`package_dir`]).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Name of the single directory the installed soteria-rust toolchain lives in.
/// A fixed name (rather than a per-version folder) keeps only one copy on disk;
/// the actual release installed is recorded in the `VERSION` file inside it.
pub const RELEASE_DIR: &str = "soteria-release";

// ── path helpers ──────────────────────────────────────────────────────────────

/// The root of the install tree: `$SOTERIA_HOME` if set, else `~/.soteria`.
pub fn soteria_base_dir() -> PathBuf {
    if let Ok(home) = env::var("SOTERIA_HOME") {
        return PathBuf::from(home);
    }
    let home = env::var("HOME").expect("HOME environment variable not set");
    PathBuf::from(home).join(".soteria")
}

/// The install directory for the toolchain this binary manages:
/// `<base>/soteria-release`. A fixed folder that is overwritten on each install.
pub fn package_dir() -> PathBuf {
    soteria_base_dir().join(RELEASE_DIR)
}

// ── subprocesses ──────────────────────────────────────────────────────────────

/// A cargo command, honouring `$CARGO` (falling back to `cargo`).
pub fn cargo_command() -> process::Command {
    process::Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

// ── progress bars ─────────────────────────────────────────────────────────────

/// A braille spinner with a trailing message, for indeterminate work.
pub fn spinner(msg: &str) -> ProgressBar {
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

/// A byte-oriented progress bar for downloads, with rate and ETA.
pub fn download_bar(total: u64, msg: &str) -> ProgressBar {
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

// ── status lines ──────────────────────────────────────────────────────────────

pub fn ok(msg: &str) {
    println!("{} {}", "✓".green().bold(), msg);
}

pub fn info(msg: &str) {
    println!("{} {}", "·".cyan(), msg);
}

pub fn warn(msg: &str) {
    println!("{} {}", "!".yellow().bold(), msg);
}

/// Print an error to stderr and exit with status 1.
pub fn fail(msg: &str) -> ! {
    eprintln!("{} {}", "✗".red().bold(), msg);
    process::exit(1);
}
