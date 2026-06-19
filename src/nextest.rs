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
//! per-test `--no-compile` runs reuse (the same trick `src/run.rs` relies on to
//! avoid each worker re-invoking charon).
//!
//! [cargo-nextest]: https://nexte.st

use std::process::{Command, Stdio};

use colored::Colorize;

use crate::{fail, package_dir, run, soteria_rust_command};

// ── `cargo soteria nextest [args…]` — the wrapper ─────────────────────────────

/// Run `cargo nextest [args…]` with ourselves wired in as the cargo target
/// runner, so nextest drives the crate's symbolic tests through soteria-rust.
/// `args` (e.g. `run`, plus any filters/flags) are forwarded to nextest verbatim.
/// Diverges.
pub fn run(args: &[String]) -> ! {
    ensure_installed();
    ensure_nextest();

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
    cmd.args(args);
    // Scope to the lib unit-test target unless the user already selected
    // targets: every probed test binary returns the *same* full soteria list,
    // so without this a crate with extra test targets would list each test
    // several times. The lib unit-test binary is the single hook we need.
    if !selects_targets(args) {
        cmd.arg("--lib");
    }
    // A target runner only fires for an explicit, non-host `--target`, so force
    // the host triple (exactly as cargo-miri does).
    cmd.arg("--target").arg(&triple);
    cmd.arg("--config").arg(&runner_cfg);

    match cmd.status() {
        Ok(st) => std::process::exit(st.code().unwrap_or(1)),
        Err(e) => fail(&format!("Failed to run `cargo nextest`: {e}")),
    }
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

    let output = soteria_rust_command()
        .arg("compile")
        .arg("--list-tests")
        .arg(".")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit()) // compile progress → stderr (nextest: debug)
        .output()
        .unwrap_or_else(|e| fail(&format!("Failed to run soteria-rust: {e}")));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let tests = run::parse_test_list(&stdout).unwrap_or_else(|| {
        fail(&format!(
            "Could not parse the test list from `soteria-rust compile --list-tests` (exit {}).",
            output.status.code().unwrap_or(-1),
        ))
    });

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
        .arg("--filter")
        .arg(run::anchored_filter(name))
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
    fn toml_escapes_paths() {
        assert_eq!(toml_str("/a/b"), "\"/a/b\"");
        assert_eq!(toml_str("/has space/x"), "\"/has space/x\"");
        assert_eq!(toml_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
