//! `cargo-soteria` entry point: parse the command line and dispatch to the
//! right subsystem. Everything substantive lives in the modules below:
//!
//! - `common`        — install paths, terminal UI primitives, the version
//! - `setup`         — the `setup` / `unsetup` management subcommands
//! - `runner_common` — invoking soteria-rust and discovering a crate's tests
//! - `base_runner`   — the built-in parallel test runner (the default action)
//! - `nextest`       — driving the tests under cargo-nextest
//! - `help`          — the rebranded `--help` output

use clap::Parser;
use colored::Colorize;
use std::env;
use std::process;

use crate::common::package_dir;

mod base_runner;
mod common;
mod help;
mod nextest;
mod runner_common;
mod setup;

// ── CLI definitions ───────────────────────────────────────────────────────────

/// The default action (no subcommand): run the crate's discovered tests in
/// parallel by forwarding `[ARGS]` to `soteria-rust exec .`. cargo-soteria owns
/// only `-j`/`--jobs`; every other argument (e.g. `--kani`, `--filter foo`) is
/// passed through unchanged — so our own option must come *before* the forwarded
/// arguments.
#[derive(Parser)]
#[command(name = "cargo-soteria", disable_help_flag = true)]
struct RunArgs {
    /// Number of tests to analyse concurrently (default: CPUs / 4).
    #[arg(short = 'j', long = "jobs", value_name = "N")]
    jobs: Option<usize>,
    /// Arguments forwarded verbatim to `soteria-rust exec .`.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARGS"
    )]
    rest: Vec<String>,
}

/// `cargo soteria setup [--release RELEASE] [--local PATH] [--yes]`.
#[derive(Parser)]
#[command(name = "cargo-soteria setup", disable_help_flag = true)]
struct SetupArgs {
    /// Install from an in-progress local soteria build instead of a release.
    #[arg(long, value_name = "PATH", conflicts_with = "release")]
    local: Option<String>,
    /// Release to install: `nightly`, or a version like `0.3.5`.
    /// Defaults to the latest patch of the highest supported release.
    #[arg(long, value_name = "RELEASE")]
    release: Option<String>,
    /// Skip confirmation prompts (e.g. when installing an unsupported release).
    #[arg(short = 'y', long = "yes")]
    yes: bool,
}

/// Parse `args` as `T`, supplying `bin` as argv[0] (clap expects the program
/// name first). Exits with clap's usage message on a parse error.
fn parse_args<T: Parser>(bin: &str, args: &[String]) -> T {
    T::parse_from(std::iter::once(bin.to_string()).chain(args.iter().cloned()))
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    // When invoked as `cargo soteria [args...]`, argv is:
    //   cargo-soteria soteria [args...]
    // Strip the "soteria" word that cargo inserts. (When nextest invokes us as
    // its target runner it calls the binary directly, so argv[1] is the runner
    // flag rather than "soteria" — hence the conditional.)
    let argv: Vec<String> = env::args().collect();
    let args: &[String] = if argv.len() > 1 && argv[1] == "soteria" {
        &argv[2..]
    } else {
        &argv[1..]
    };

    // `nextest` and its hidden target runner forward raw arguments to
    // cargo-nextest / the libtest protocol, so they bypass our own parsing.
    // Handled before the `--help` sweep so those modes render their own help.
    match args.first().map(|s| s.as_str()) {
        Some(nextest::RUNNER_FLAG) => nextest::runner(&args[1..]),
        Some("nextest") => nextest::run(&args[1..]),
        _ => {}
    }

    // cargo-soteria has no CLI of its own beyond `-j`: `--help` renders the
    // analyzer's full option reference, rebranded (see src/help.rs).
    if args.iter().any(|a| a == "-h" || a == "--help") {
        help::print_help();
        return;
    }

    match args.first().map(|s| s.as_str()) {
        Some("setup") => {
            let a: SetupArgs = parse_args("cargo soteria setup", &args[1..]);
            setup::cmd_setup(a.local.as_deref(), a.release.as_deref(), a.yes);
        }
        Some("unsetup") => setup::cmd_unsetup(),
        // Default path: discover the crate's tests and analyse them in parallel.
        // The toolchain must be installed first.
        _ => {
            if !package_dir().join("bin").join("soteria-rust").exists() {
                eprintln!("{} Soteria is not installed.", "✗".red().bold());
                eprintln!(
                    "  Run {} to download and install it.",
                    "cargo soteria setup".cyan().bold()
                );
                process::exit(1);
            }
            let a: RunArgs = parse_args("cargo soteria", args);
            let jobs = a
                .jobs
                .map(|j| j.max(1))
                .unwrap_or_else(base_runner::default_jobs);
            base_runner::run(a.rest, jobs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_args(args: &[&str]) -> RunArgs {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_args("cargo soteria", &owned)
    }

    #[test]
    fn jobs_accepts_every_form() {
        // The attached short form `-j4` is intentionally unsupported: a
        // `trailing_var_arg` + `allow_hyphen_values` passthrough positional
        // greedily claims it. The spaced/`=` forms all work.
        for form in [&["-j", "4"][..], &["--jobs", "4"], &["--jobs=4"]] {
            assert_eq!(run_args(form).jobs, Some(4), "form: {form:?}");
        }
    }

    #[test]
    fn no_args_is_empty_passthrough() {
        let a = run_args(&[]);
        assert_eq!(a.jobs, None);
        assert!(a.rest.is_empty());
    }

    #[test]
    fn unknown_flags_pass_through_to_soteria_rust() {
        // Bare passthrough: hyphenated soteria-rust flags land in `rest`, not an
        // "unexpected argument" error.
        let a = run_args(&["--kani", "--filter", "foo"]);
        assert_eq!(a.jobs, None);
        assert_eq!(a.rest, vec!["--kani", "--filter", "foo"]);
    }

    #[test]
    fn jobs_then_passthrough() {
        // Our own option must precede the forwarded args (trailing_var_arg).
        let a = run_args(&["-j", "2", "--kani"]);
        assert_eq!(a.jobs, Some(2));
        assert_eq!(a.rest, vec!["--kani"]);
    }

    #[test]
    fn setup_local_is_optional() {
        let none: SetupArgs = parse_args("cargo soteria setup", &[]);
        assert_eq!(none.local, None);
        let some: SetupArgs = parse_args("cargo soteria setup", &["--local".into(), "/p".into()]);
        assert_eq!(some.local.as_deref(), Some("/p"));
    }

    #[test]
    fn setup_release_and_yes_parse() {
        let none: SetupArgs = parse_args("cargo soteria setup", &[]);
        assert_eq!(none.release, None);
        assert!(!none.yes);

        let a: SetupArgs = parse_args(
            "cargo soteria setup",
            &["--release".into(), "0.3.5".into(), "-y".into()],
        );
        assert_eq!(a.release.as_deref(), Some("0.3.5"));
        assert!(a.yes);

        let nightly: SetupArgs = parse_args(
            "cargo soteria setup",
            &["--release".into(), "nightly".into()],
        );
        assert_eq!(nightly.release.as_deref(), Some("nightly"));
        assert!(!nightly.yes);
    }

    #[test]
    fn setup_release_conflicts_with_local() {
        // `--release` and `--local` are mutually exclusive.
        let res = SetupArgs::try_parse_from([
            "cargo soteria setup",
            "--release",
            "0.3.5",
            "--local",
            "/p",
        ]);
        assert!(res.is_err());
    }
}
