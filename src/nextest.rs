//! `cargo soteria nextest` — drive [cargo-nextest] against Soteria's symbolic
//! tests.
//!
//! Soteria's tests live behind `#[cfg(soteria)]` and are compiled and executed
//! by `soteria-rust` (via charon), not by libtest — so a *native*
//! `cargo`/nextest build of the crate sees **zero** tests. We bridge the gap the
//! same way `cargo miri nextest` does: run `cargo nextest` with this binary
//! injected as cargo's `target.runner`, forcing an explicit `--target` so the
//! runner actually fires. nextest then invokes the runner during *both* of its
//! phases (see <https://nexte.st/docs/features/target-runners/>):
//!
//!   * **list** — `<runner> <bin> --list --format terse`
//!     → `soteria-rust compile --list-tests .`, reprinted as `name: test`
//!   * **run**  — `<runner> <bin> <name> --exact --nocapture`
//!     → `soteria-rust exec . --no-compile --filter ^name$`, exit 0 = pass
//!
//! The native test binary nextest builds is just a vehicle for the runner to
//! hang off — the runner ignores its path and gets everything from soteria-rust.
//! The single list-phase compile populates the crate's ULLBC cache, which the
//! per-test `--no-compile` runs reuse (the same trick `base_runner.rs` relies
//! on to avoid each worker re-invoking charon). Test discovery and the
//! `--filter` escaping are shared via `runner_common`.
//!
//! [cargo-nextest]: https://nexte.st

use std::process::{Command, Stdio};

use colored::Colorize;

use crate::common::{fail, package_dir};
use crate::runner_common::{self, anchored_filter, soteria_rust_command};

// ── `cargo soteria nextest [args…]` — the wrapper ─────────────────────────────

/// Run `cargo nextest [args…]` with ourselves wired in as the cargo target
/// runner, so nextest drives the crate's symbolic tests through soteria-rust.
/// `args` (e.g. `run`, plus any filters/flags) are forwarded to nextest verbatim.
/// Diverges.
pub fn run(args: &[String]) -> ! {
    ensure_installed();
    ensure_nextest();

    // Split at the first `--`, mirroring `cargo test -- <args>`: everything
    // before it is forwarded to `cargo nextest`; everything after is forwarded
    // verbatim to *every* soteria-rust call (the list-phase compile *and* each
    // per-test exec). So `cargo soteria nextest run -- --kani` lists and runs
    // the crate's Kani harnesses under nextest. nextest controls the runner's
    // argv, so we hand these flags to the runner out-of-band via an env var.
    let (nextest_args, soteria_args) = split_soteria_args(args);

    let triple = host_triple();
    let exe = current_exe();

    // cargo `--config` takes a TOML `KEY=VALUE`; the runner is an array of
    // [program, arg…], so nextest invokes `<exe> __nextest-runner <bin> <args…>`.
    let runner_cfg = format!(
        "target.{triple}.runner=[{}, {}]",
        toml_str(&exe),
        toml_str(RUNNER_FLAG),
    );

    let mut cmd = Command::new("cargo");
    cmd.arg("nextest");
    cmd.args(&nextest_args);
    // Scope to the lib unit-test target unless the user already selected
    // targets: every probed test binary returns the *same* full soteria list,
    // so without this a crate with extra test targets would list each test
    // several times. The lib unit-test binary is the single hook we need.
    if !selects_targets(&nextest_args) {
        cmd.arg("--lib");
    }
    // A target runner only fires for an explicit, non-host `--target`, so force
    // the host triple (exactly as cargo-miri does).
    cmd.arg("--target").arg(&triple);
    cmd.arg("--config").arg(&runner_cfg);
    // Pass the post-`--` soteria-rust flags down to the runner. cargo nextest
    // inherits this env and hands it to the target-runner children it spawns
    // (both list and run phases), where `extra_soteria_args()` reads it back.
    if !soteria_args.is_empty() {
        cmd.env(EXTRA_ARGS_ENV, encode_extra_args(&soteria_args));
    }

    match cmd.status() {
        Ok(st) => std::process::exit(st.code().unwrap_or(1)),
        Err(e) => fail(&format!("Failed to run `cargo nextest`: {e}")),
    }
}

/// Split `cargo soteria nextest` args at the first `--`: `(before, after)`.
/// `before` goes to `cargo nextest`; `after` is the soteria-rust flag bag
/// (empty when there's no `--`).
fn split_soteria_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    }
}

/// Env var carrying the post-`--` soteria-rust flags from the `nextest` wrapper
/// down to the target-runner subprocess (JSON-encoded `Vec<String>`).
const EXTRA_ARGS_ENV: &str = "__SOTERIA_NEXTEST_EXTRA_ARGS";

fn encode_extra_args(extra: &[String]) -> String {
    serde_json::to_string(extra).unwrap_or_default()
}

/// The soteria-rust flags the wrapper stashed in [`EXTRA_ARGS_ENV`], to be
/// forwarded to every soteria-rust invocation the runner makes.
fn extra_soteria_args() -> Vec<String> {
    std::env::var(EXTRA_ARGS_ENV)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// True if the user already passed a cargo target-selection flag, in which case
/// we must not add our own `--lib`.
fn selects_targets(args: &[String]) -> bool {
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--lib"
                | "--bin"
                | "--bins"
                | "--test"
                | "--tests"
                | "--bench"
                | "--benches"
                | "--example"
                | "--examples"
                | "--all-targets"
        ) || a.starts_with("--bin=")
            || a.starts_with("--test=")
            || a.starts_with("--bench=")
            || a.starts_with("--example=")
    })
}

// ── `__nextest-runner <bin> <args…>` — the hidden cargo target runner ─────────

/// The verb that marks this binary as nextest's target runner (the second
/// element of the injected `runner` array). Also matched in `main()`'s dispatch.
pub const RUNNER_FLAG: &str = "__nextest-runner";

/// Hidden mode invoked by nextest as `<exe> __nextest-runner <test-bin> <args…>`.
/// `args` is everything after the flag: `[<test-bin>, <protocol args…>]`. We
/// ignore the (native, soteria-less) test binary and translate nextest's libtest
/// protocol into soteria-rust calls. Diverges.
pub fn runner(args: &[String]) -> ! {
    // Drop the test-binary path; the protocol args are what matter.
    let proto = args.get(1..).unwrap_or(&[]);

    if proto.iter().any(|a| a == "--list") {
        list_phase(proto);
    }
    run_phase(proto);
}

/// List phase: emit one `name: test` line per discovered entry point, and
/// nothing else on stdout (nextest requires clean stdout; soteria's compile
/// progress stays on stderr).
fn list_phase(proto: &[String]) -> ! {
    // soteria has no notion of `#[ignore]`; when nextest asks for the ignored
    // set, report none.
    if proto.iter().any(|a| a == "--ignored") {
        std::process::exit(0);
    }

    // Inherit stderr so the (one-time) compile progress streams as debug output;
    // our stdout must carry only the `name: test` lines. The compile must use
    // the same soteria flags as the per-test execs (e.g. `--kani`), so the
    // listed entry points match what the run phase will analyse.
    let tests = runner_common::discover_tests(&extra_soteria_args(), true)
        .unwrap_or_else(|e| fail(&e.message()));

    let mut out = String::new();
    for t in &tests {
        out.push_str(t);
        out.push_str(": test\n");
    }
    print!("{out}");
    std::process::exit(0);
}

/// Run phase: execute the single named test under soteria-rust. nextest reads
/// the exit code as pass (0) / fail (non-zero); we propagate soteria's own code
/// (1 = bug found, 2 = soteria crash, 3 = charon crash), mapping a fatal signal
/// to a generic failure.
fn run_phase(proto: &[String]) -> ! {
    let name = proto
        .iter()
        .find(|a| !a.starts_with('-'))
        .unwrap_or_else(|| fail("No test name passed to the soteria nextest runner."));

    let status = soteria_rust_command()
        .arg("exec")
        .arg(".")
        .arg("--no-compile")
        .arg("--no-compile-plugins")
        .args(extra_soteria_args())
        .arg("--filter")
        .arg(anchored_filter(name))
        .stdin(Stdio::null())
        .status()
        .unwrap_or_else(|e| fail(&format!("Failed to run soteria-rust: {e}")));

    std::process::exit(status.code().unwrap_or(1));
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn ensure_installed() {
    if !package_dir().join("bin").join("soteria-rust").exists() {
        eprintln!("{} Soteria is not installed.", "✗".red().bold());
        eprintln!(
            "  Run {} to download and install it.",
            "cargo soteria setup".cyan().bold()
        );
        std::process::exit(1);
    }
}

fn ensure_nextest() {
    let ok = Command::new("cargo")
        .args(["nextest", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        fail(
            "cargo-nextest is not installed.\n  \
             Install it with `cargo install cargo-nextest --locked`, or see \
             https://nexte.st/docs/installation/.",
        );
    }
}

/// The host target triple, parsed from `rustc -vV`. Needed both for the forced
/// `--target` and for the `target.<triple>.runner` config key.
fn host_triple() -> String {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .unwrap_or_else(|e| {
            fail(&format!(
                "Failed to run rustc to detect the host target: {e}.\n  Is a Rust toolchain installed?"
            ))
        });
    if !out.status.success() {
        fail("`rustc -vV` failed; cannot detect the host target triple.");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_host_triple(&text)
        .unwrap_or_else(|| fail("Could not find the host target in `rustc -vV` output."))
}

fn parse_host_triple(rustc_vv: &str) -> Option<String> {
    rustc_vv
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|s| s.trim().to_string())
}

fn current_exe() -> String {
    std::env::current_exe()
        .unwrap_or_else(|e| {
            fail(&format!(
                "Could not determine cargo-soteria's own path: {e}"
            ))
        })
        .to_string_lossy()
        .into_owned()
}

/// Render `s` as a TOML basic string (quoted, with `\` and `"` escaped) so paths
/// with spaces survive cargo's `--config` parsing.
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_user_target_selection() {
        assert!(selects_targets(&["run".into(), "--lib".into()]));
        assert!(selects_targets(&[
            "run".into(),
            "--test".into(),
            "it".into()
        ]));
        assert!(selects_targets(&["run".into(), "--bin=foo".into()]));
        assert!(selects_targets(&["run".into(), "--all-targets".into()]));
        // package selection is not target selection
        assert!(!selects_targets(&["run".into(), "-p".into(), "pkg".into()]));
        assert!(!selects_targets(&["run".into(), "mytest".into()]));
    }

    #[test]
    fn parses_rustc_host() {
        let vv = "rustc 1.79.0\nbinary: rustc\nhost: aarch64-apple-darwin\nrelease: 1.79.0\n";
        assert_eq!(parse_host_triple(vv).unwrap(), "aarch64-apple-darwin");
        assert!(parse_host_triple("no host here").is_none());
    }

    #[test]
    fn splits_soteria_args_at_dashdash() {
        let split = |a: &[&str]| {
            let owned: Vec<String> = a.iter().map(|s| s.to_string()).collect();
            split_soteria_args(&owned)
        };
        // No `--`: everything is a nextest arg.
        assert_eq!(
            split(&["run", "mytest"]),
            (vec!["run".into(), "mytest".into()], vec![])
        );
        // `--` separates nextest args from the soteria-rust flag bag.
        assert_eq!(
            split(&["run", "--", "--kani"]),
            (vec!["run".into()], vec!["--kani".into()])
        );
        // A trailing `--` yields an empty (but present) soteria bag.
        assert_eq!(split(&["run", "--"]), (vec!["run".into()], vec![]));
        // Only the first `--` splits; later ones stay in the soteria bag.
        assert_eq!(
            split(&["run", "--", "--kani", "--", "x"]),
            (
                vec!["run".into()],
                vec!["--kani".into(), "--".into(), "x".into()]
            )
        );
    }

    #[test]
    fn extra_args_roundtrip_through_env() {
        let extra = vec![
            "--kani".to_string(),
            "--filter".to_string(),
            "f".to_string(),
        ];
        let encoded = encode_extra_args(&extra);
        // SAFETY: single-threaded test; we set and immediately read back.
        unsafe { std::env::set_var(EXTRA_ARGS_ENV, &encoded) };
        assert_eq!(extra_soteria_args(), extra);
        unsafe { std::env::remove_var(EXTRA_ARGS_ENV) };
        assert!(extra_soteria_args().is_empty());
    }

    #[test]
    fn toml_escapes_paths() {
        assert_eq!(toml_str("/a/b"), "\"/a/b\"");
        assert_eq!(toml_str("/has space/x"), "\"/has space/x\"");
        assert_eq!(toml_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
