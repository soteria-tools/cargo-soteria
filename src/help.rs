//! `cargo soteria --help` rendering.
//!
//! The wrapper has no real CLI of its own — it forwards to `soteria-rust exec
//! .`. Rather than expose that, we capture the analyzer's `--help` output and
//! rebrand it as `cargo soteria`'s own help (see [`rebrand_help`]), splicing in
//! the wrapper's `setup`/`unsetup` subcommands.

use colored::Colorize;

use crate::common::{info, package_dir, VERSION};
use crate::runner_common::soteria_rust_command;

/// The `COMMANDS` section spliced into the rebranded help, documenting the
/// wrapper's own subcommands. Formatted to match cmdliner's plain man layout
/// (uppercase section header, 7-space indented bodies).
const COMMANDS_SECTION: &str = "\
COMMANDS
       cargo soteria discovers the crate's symbolic tests and runs them in
       parallel — one soteria-rust process per test, with results streamed as
       each finishes. The following options and management subcommands are
       also available:

       -j N, --jobs N
           Number of tests to analyse concurrently. Defaults to a quarter of
           the available CPUs. Press Ctrl-C to stop; all running analyses are
           killed.

       setup [--release RELEASE] [--local PATH] [--yes]
           Download and install the Soteria toolchain into ~/.soteria. Without
           --release, installs the latest supported stable release; pass
           --release nightly or --release X.Y.Z to choose another (installing
           an unsupported one warns and asks to confirm, or use --yes to skip).
           With --local, install from an in-progress Soteria build instead.

       unsetup
           Remove the installed toolchain.

       nextest [ARGS]
           Run the crate's symbolic tests under cargo-nextest instead of the
           built-in parallel runner. Requires cargo-nextest to be installed;
           ARGS (e.g. `run`, plus test-name filters) are forwarded to it.

";

/// Show help for `cargo soteria`. When the toolchain is installed, this renders
/// the analyzer's full option reference rebranded as `cargo soteria`'s own help
/// (see [`rebrand_help`]); otherwise it falls back to a short summary.
///
/// Without this, `cargo soteria --help` falls through to `soteria-rust exec .
/// --help`, which leaks the wrapped `SOTERIA-RUST-EXEC(1)` man page and never
/// mentions the wrapper's own `setup`/`unsetup` commands.
pub fn print_help() {
    let soteria_rust_bin = package_dir().join("bin").join("soteria-rust");
    if !soteria_rust_bin.exists() {
        print_help_offline();
        return;
    }

    // `TERM=dumb` makes cmdliner emit the plain (non-overstruck) man format, so
    // the captured text has no backspace-bold doubling to clean up.
    let output = soteria_rust_command()
        .arg("exec")
        .arg(".")
        .arg("--help")
        .env("TERM", "dumb")
        .output();

    match output {
        Ok(out) if out.status.success() => {
            print!("{}", rebrand_help(&String::from_utf8_lossy(&out.stdout)));
        }
        _ => print_help_offline(),
    }
}

/// Short help shown before the toolchain is installed (the full option
/// reference lives inside the analyzer, which isn't present yet).
fn print_help_offline() {
    println!(
        "\
{name} {version}
Symbolic execution for Rust, powered by Soteria.

{usage}
    cargo soteria [OPTIONS]               Discover & analyse the crate's tests in parallel
    cargo soteria setup [--release REL]   Download & install the Soteria toolchain
    cargo soteria unsetup                 Remove the installed toolchain (asks first)
    cargo soteria nextest run [ARGS]      Run the symbolic tests under cargo-nextest

{options}
    -j, --jobs N                          Tests to analyse concurrently (default: CPUs / 4)
    -h, --help                            Show this help",
        name = "cargo-soteria".bold(),
        version = VERSION,
        usage = "USAGE:".bold().underline(),
        options = "OPTIONS:".bold().underline(),
    );
    println!();
    info(&format!(
        "Run {} to install the toolchain and see all options.",
        "cargo soteria setup".cyan().bold()
    ));
}

/// Rewrite the analyzer's `exec` man page so it reads as `cargo soteria`'s own
/// help: every `soteria-rust exec` reference becomes `cargo soteria`, the
/// implicit `PATH` argument (always `.`) is hidden, and the wrapper's
/// `COMMANDS` section is spliced in after `DESCRIPTION`.
fn rebrand_help(raw: &str) -> String {
    // In cmdliner's plain output, section headers sit at column 0 and are all
    // uppercase (e.g. "SOLVER OPTIONS"); option/body lines are indented.
    let is_section =
        |line: &str| !line.is_empty() && line.chars().all(|c| c.is_ascii_uppercase() || c == ' ');

    let rebrand = |line: &str| {
        line.replace("soteria-rust-exec", "cargo-soteria")
            .replace("soteria-rust exec", "cargo soteria")
            .replace("soteria-rust", "cargo soteria")
            // SYNOPSIS: the path is always "." so hide the PATH argument.
            .replace(
                "cargo soteria [OPTION]\u{2026} PATH",
                "cargo soteria [OPTION]\u{2026}",
            )
    };

    let mut out = String::new();
    let mut lines = raw.lines().peekable();
    while let Some(line) = lines.next() {
        // Drop the ARGUMENTS section wholesale — its only entry is the hidden
        // PATH argument.
        if is_section(line) && line.trim() == "ARGUMENTS" {
            while lines.peek().is_some_and(|n| !is_section(n)) {
                lines.next();
            }
            continue;
        }

        out.push_str(&rebrand(line));
        out.push('\n');

        // Splice the wrapper's own subcommands in right after DESCRIPTION.
        if is_section(line) && line.trim() == "DESCRIPTION" {
            while lines.peek().is_some_and(|n| !is_section(n)) {
                out.push_str(&rebrand(lines.next().unwrap()));
                out.push('\n');
            }
            out.push_str(COMMANDS_SECTION);
        }
    }
    out
}
