//! Cross-cutting helpers shared across the binary: the install-path resolution,
//! the terminal UI primitives (spinners, the download bar, the `✓`/`·`/`!`/`✗`
//! status lines), and the crate version that names the install directory.

use std::env;
use std::path::PathBuf;
use std::process;
use std::time::Duration;

use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

/// Version of this crate, used as the install subdirectory under ~/.soteria/.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── path helpers ──────────────────────────────────────────────────────────────

/// The root of the install tree: `$SOTERIA_HOME` if set, else `~/.soteria`.
pub fn soteria_base_dir() -> PathBuf {
    if let Ok(home) = env::var("SOTERIA_HOME") {
        return PathBuf::from(home);
    }
    let home = env::var("HOME").expect("HOME environment variable not set");
    PathBuf::from(home).join(".soteria")
}

/// The install directory for the version this binary manages: `<base>/<VERSION>`.
pub fn package_dir() -> PathBuf {
    soteria_base_dir().join(VERSION)
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
