/// Integration tests for cargo-soteria.
///
/// Each test:
///   1. Spins up a fresh SOTERIA_HOME (temp dir) so it never touches the real ~/.soteria
///   2. Runs `cargo-soteria setup` to install Soteria
///   3. Runs `cargo-soteria` on the fixture crate and asserts success
///
/// The local-install test is skipped unless SOTERIA_LOCAL_PATH is set to the
/// root of a soteria checkout that already has `packages/soteria-rust/` built
/// (run `make package-soteria-rust` first).
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn cargo_soteria_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cargo-soteria"))
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simple-crate")
}

/// Create a unique temp directory used as SOTERIA_HOME for one test run.
fn fresh_soteria_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("soteria-test-{}-{}", std::process::id(), n));
    fs::create_dir_all(&dir).expect("failed to create temp SOTERIA_HOME");
    dir
}

fn run_setup(args: &[&str], soteria_home: &PathBuf) {
    let status = Command::new(cargo_soteria_bin())
        .arg("setup")
        .args(args)
        .env("SOTERIA_HOME", soteria_home)
        .status()
        .expect("failed to spawn cargo-soteria setup");
    assert!(
        status.success(),
        "cargo-soteria setup {:?} failed with {}",
        args,
        status
    );
}

fn run_analysis(soteria_home: &PathBuf) {
    let status = Command::new(cargo_soteria_bin())
        .current_dir(fixture_dir())
        .env("SOTERIA_HOME", soteria_home)
        .status()
        .expect("failed to spawn cargo-soteria");
    assert!(
        status.success(),
        "cargo-soteria analysis failed with {}",
        status
    );
}

/// Downloads the nightly release from GitHub and runs analysis on the fixture crate.
#[test]
fn online_install_and_run() {
    let home = fresh_soteria_home();
    run_setup(&[], &home);
    run_analysis(&home);
    fs::remove_dir_all(&home).ok();
}

/// Installs from a local soteria checkout and runs analysis on the fixture crate.
///
/// Set SOTERIA_LOCAL_PATH to the root of a soteria checkout where
/// `make package-soteria-rust` has already been run.
#[test]
fn local_install_and_run() {
    let local_path = match std::env::var("SOTERIA_LOCAL_PATH") {
        Ok(p) => p,
        Err(_) => {
            println!("Skipping: SOTERIA_LOCAL_PATH not set");
            return;
        }
    };
    let home = fresh_soteria_home();
    run_setup(&["--local", &local_path], &home);
    run_analysis(&home);
    fs::remove_dir_all(&home).ok();
}

fn nextest_available() -> bool {
    Command::new("cargo")
        .args(["nextest", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Copy a fixture crate into a fresh temp dir (excluding any stale `target/`),
/// so a native cargo/nextest build doesn't race the shared fixture's build dir.
fn copy_fixture_to_temp(name: &str) -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dst = std::env::temp_dir().join(format!("soteria-fixture-{}-{}", std::process::id(), n));
    copy_dir_excluding_target(&src, &dst);
    dst
}

fn copy_dir_excluding_target(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create fixture copy dir");
    for entry in fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("read fixture entry");
        if entry.file_name() == "target" {
            continue;
        }
        let s = entry.path();
        let d = dst.join(entry.file_name());
        if s.is_dir() {
            copy_dir_excluding_target(&s, &d);
        } else {
            fs::copy(&s, &d).expect("copy fixture file");
        }
    }
}

/// End-to-end: install the real toolchain, then drive `cargo soteria nextest
/// run` through the *real* cargo-nextest against the simple-crate fixture (two
/// always-passing tests). Verifies the full custom-runner handshake — nextest's
/// list and run phases both routed through our `__nextest-runner` into
/// soteria-rust. Skipped when cargo-nextest isn't installed.
#[test]
fn nextest_online_install_and_run() {
    if !nextest_available() {
        println!("Skipping: cargo-nextest not installed");
        return;
    }
    let home = fresh_soteria_home();
    run_setup(&[], &home);

    let crate_dir = copy_fixture_to_temp("simple-crate");
    let out = Command::new(cargo_soteria_bin())
        .args(["nextest", "run"])
        .current_dir(&crate_dir)
        .env("SOTERIA_HOME", &home)
        .env("NO_COLOR", "1")
        .output()
        .expect("run cargo soteria nextest run");

    // nextest writes its UI to stderr; the runner streams soteria to both.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "`cargo soteria nextest run` failed ({}):\n{combined}",
        out.status
    );
    // Both #[soteria::test] entry points were discovered (via our list phase)…
    for t in ["double_negation_is_identity", "abs_diff_is_symmetric"] {
        assert!(
            combined.contains(t),
            "missing test {t} in output:\n{combined}"
        );
    }
    // …and both ran and passed (run phase).
    assert!(
        combined.contains("2 tests run") && combined.contains("2 passed"),
        "expected a 2-passed summary:\n{combined}"
    );

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&crate_dir).ok();
}

// ── deterministic parallel-runner tests (no network, no real soteria) ─────────
//
// These install a fake `soteria-rust` (see tests/fixtures/fake-soteria-rust.sh)
// into a temp SOTERIA_HOME so we can drive every outcome — pass, fail, crash,
// and a slow test we interrupt — without the real analyzer.

/// Install the fake soteria-rust at `$SOTERIA_HOME/<version>/bin/soteria-rust`.
fn install_fake_soteria(home: &Path) {
    let bin_dir = home.join(env!("CARGO_PKG_VERSION")).join("bin");
    fs::create_dir_all(&bin_dir).expect("create fake bin dir");
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-soteria-rust.sh");
    let dst = bin_dir.join("soteria-rust");
    fs::copy(&src, &dst).expect("copy fake soteria-rust");
    fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).expect("chmod fake");
}

/// Poll for the child to exit, up to `timeout`. Returns the exit code, or kills
/// the child and returns `None` on timeout.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<i32> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

/// A crash (or anomaly) in one test must not stop the others, and each outcome
/// must be classified and counted correctly.
#[test]
fn parallel_classifies_and_survives_crashes() {
    let home = fresh_soteria_home();
    install_fake_soteria(&home);

    // Default fake list: 2 pass, 1 fail, 1 soteria-crash, 1 charon-crash, 1 anomaly.
    let out = Command::new(cargo_soteria_bin())
        .args(["-j", "3"])
        .current_dir(fixture_dir())
        .env("SOTERIA_HOME", &home)
        .env("NO_COLOR", "1")
        .output()
        .expect("run cargo-soteria with fake");

    let stdout = String::from_utf8_lossy(&out.stdout);

    // All six tests were reported despite the crashes in the middle.
    for t in [
        "pass_one",
        "pass_two",
        "fail_one",
        "crash_one",
        "charon_one",
        "anomaly_one",
    ] {
        assert!(stdout.contains(t), "missing {t} in output:\n{stdout}");
    }

    // Outcome tallies (crash + charon = 2 crashed; anomaly = 1 errored).
    assert!(stdout.contains("2 passed"), "output:\n{stdout}");
    assert!(stdout.contains("1 failed"), "output:\n{stdout}");
    assert!(stdout.contains("2 crashed"), "output:\n{stdout}");
    assert!(stdout.contains("1 errored"), "output:\n{stdout}");

    // A crash/error present => exit code 2.
    assert_eq!(out.status.code(), Some(2), "output:\n{stdout}");

    fs::remove_dir_all(&home).ok();
}

/// When the crate fails to compile, `soteria-rust compile` aborts before it can
/// list any tests, printing the rustc diagnostic to *stdout* and exiting 3. The
/// runner must surface that full diagnostic — not just its own terse "discovery
/// failed" line — so the user sees the real error.
///
/// Driven by the fake soteria-rust against the `type-error` fixture (whose
/// `soteria-compile-error.txt` supplies the canned diagnostic), and checked
/// against the committed `expected-stderr.txt` snapshot. Regenerate the snapshot
/// with `UPDATE_SNAPSHOTS=1 cargo test --test integration type_error`.
#[test]
fn type_error_is_propagated() {
    let home = fresh_soteria_home();
    install_fake_soteria(&home);

    let crate_dir = copy_fixture_to_temp("type-error");
    let out = Command::new(cargo_soteria_bin())
        .current_dir(&crate_dir)
        .env("SOTERIA_HOME", &home)
        .env("NO_COLOR", "1")
        .output()
        .expect("run cargo-soteria on the type-error fixture");

    // Discovery failed → exit 1 (the `fail()` path), nothing on stdout.
    assert_eq!(out.status.code(), Some(1), "expected discovery to fail");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.trim().is_empty(), "unexpected stdout:\n{stdout}");

    let actual = String::from_utf8_lossy(&out.stderr);
    let snapshot = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/type-error/expected-stderr.txt");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        fs::write(&snapshot, actual.as_bytes()).expect("write snapshot");
    } else {
        let expected = fs::read_to_string(&snapshot).expect("read snapshot");
        assert_eq!(
            actual,
            expected,
            "stderr did not match snapshot {}\n--- actual ---\n{actual}",
            snapshot.display()
        );
    }

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&crate_dir).ok();
}

// ── nextest custom-runner protocol (no network, no real soteria) ──────────────
//
// `cargo soteria nextest` injects this binary as cargo's target runner; nextest
// then invokes `cargo-soteria __nextest-runner <test-bin> <protocol-args>` in
// both its list and run phases. These tests drive that hidden mode directly
// against the fake soteria-rust, so they verify the libtest-protocol translation
// without needing cargo-nextest or the real analyzer installed.

/// Invoke the hidden runner with a dummy test-binary path (which it ignores) and
/// the given protocol args. Returns (exit code, stdout).
fn run_nextest_runner(home: &Path, proto: &[&str]) -> (Option<i32>, String) {
    let mut args = vec!["__nextest-runner", "/dummy/native-test-bin"];
    args.extend_from_slice(proto);
    let out = Command::new(cargo_soteria_bin())
        .args(&args)
        .current_dir(fixture_dir())
        .env("SOTERIA_HOME", home)
        .env("NO_COLOR", "1")
        .output()
        .expect("run cargo-soteria __nextest-runner");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The list phase must emit exactly one `name: test` line per entry point on
/// stdout — and nothing else (nextest requires clean stdout).
#[test]
fn nextest_runner_lists_tests_in_terse_format() {
    let home = fresh_soteria_home();
    install_fake_soteria(&home);

    let (code, stdout) = run_nextest_runner(&home, &["--list", "--format", "terse"]);
    assert_eq!(
        code,
        Some(0),
        "list phase should succeed; stdout:\n{stdout}"
    );

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "m::pass_one: test",
            "m::pass_two: test",
            "m::fail_one: test",
            "m::crash_one: test",
            "m::charon_one: test",
            "m::anomaly_one: test",
        ],
        "unexpected terse listing:\n{stdout}"
    );

    fs::remove_dir_all(&home).ok();
}

/// nextest asks for the ignored set separately; soteria has none, so the runner
/// must print nothing and succeed.
#[test]
fn nextest_runner_lists_no_ignored_tests() {
    let home = fresh_soteria_home();
    install_fake_soteria(&home);

    let (code, stdout) = run_nextest_runner(&home, &["--list", "--format", "terse", "--ignored"]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.trim().is_empty(),
        "expected no ignored tests:\n{stdout}"
    );

    fs::remove_dir_all(&home).ok();
}

/// The run phase must propagate soteria-rust's exit code so nextest sees
/// pass (0) vs fail (non-zero), preserving the crash codes.
#[test]
fn nextest_runner_propagates_exit_codes() {
    let home = fresh_soteria_home();
    install_fake_soteria(&home);

    for (name, expected) in [
        ("m::pass_one", 0),   // pass
        ("m::fail_one", 1),   // bug found
        ("m::crash_one", 2),  // soteria crash
        ("m::charon_one", 3), // charon crash
    ] {
        let (code, _) = run_nextest_runner(&home, &[name, "--exact", "--nocapture"]);
        assert_eq!(code, Some(expected), "test {name} should exit {expected}");
    }

    fs::remove_dir_all(&home).ok();
}

/// Flags placed after `--` (e.g. `cargo soteria nextest run -- --kani`) must
/// reach *both* phases of the runner: the list-phase `compile` (so the right
/// entry points are discovered) and each `exec` (so they're analysed the same
/// way). `nextest::run` stashes them in `__SOTERIA_NEXTEST_EXTRA_ARGS`; here we
/// set that env directly and check the `--kani`-aware fake reacts in both
/// phases.
#[test]
fn nextest_runner_threads_post_dashdash_flags() {
    let home = fresh_soteria_home();
    install_fake_soteria(&home);

    // JSON encoding of ["--kani"], matching `nextest::encode_extra_args`. The
    // env var name mirrors `nextest::EXTRA_ARGS_ENV`.
    let extra_env = ("__SOTERIA_NEXTEST_EXTRA_ARGS", r#"["--kani"]"#);

    // List phase: `--kani` reached `compile`, so the fake lists kani harnesses.
    let out = Command::new(cargo_soteria_bin())
        .args(["__nextest-runner", "/dummy", "--list", "--format", "terse"])
        .current_dir(fixture_dir())
        .env("SOTERIA_HOME", &home)
        .env(extra_env.0, extra_env.1)
        .env("NO_COLOR", "1")
        .output()
        .expect("run list phase");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "list phase failed:\n{stdout}");
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec!["kani::harness_a: test", "kani::harness_b: test"],
        "list phase didn't forward --kani to compile:\n{stdout}"
    );

    // Run phase: `--kani` reached `exec` too (the fake prefixes a `[kani]` line).
    let out = Command::new(cargo_soteria_bin())
        .args([
            "__nextest-runner",
            "/dummy",
            "kani::harness_a",
            "--exact",
            "--nocapture",
        ])
        .current_dir(fixture_dir())
        .env("SOTERIA_HOME", &home)
        .env(extra_env.0, extra_env.1)
        .env("NO_COLOR", "1")
        .output()
        .expect("run run phase");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "run phase failed:\n{stdout}");
    assert!(
        stdout.contains("[kani] kani::harness_a"),
        "run phase didn't forward --kani to exec:\n{stdout}"
    );

    fs::remove_dir_all(&home).ok();
}

/// Ctrl-C while tests are running must terminate promptly and leave no worker
/// processes alive.
#[test]
fn interrupt_kills_running_workers() {
    let home = fresh_soteria_home();
    install_fake_soteria(&home);

    // Six slow tests, four workers: four are sleeping when we interrupt.
    let list = r#"["m::slow_1","m::slow_2","m::slow_3","m::slow_4","m::slow_5","m::slow_6"]"#;
    let mut child = Command::new(cargo_soteria_bin())
        .args(["-j", "4"])
        .current_dir(fixture_dir())
        .env("SOTERIA_HOME", &home)
        .env("FAKE_TEST_LIST", list)
        .env("NO_COLOR", "1")
        .spawn()
        .expect("spawn cargo-soteria");
    let pid = child.id();

    // Let discovery finish and the workers start sleeping.
    std::thread::sleep(Duration::from_millis(1500));

    // Send SIGINT, as Ctrl-C would.
    let killed = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .expect("send SIGINT");
    assert!(killed.success());

    // It must exit promptly (well under the 30s sleeps) with code 130.
    let code = wait_for_exit(&mut child, Duration::from_secs(10));
    assert_eq!(code, Some(130), "expected prompt exit 130 on interrupt");

    // No fake worker (its argv contains the temp SOTERIA_HOME path) survives.
    std::thread::sleep(Duration::from_millis(300));
    let leftover = Command::new("pgrep")
        .args(["-f", home.to_str().unwrap()])
        .output()
        .expect("pgrep");
    let survivors = String::from_utf8_lossy(&leftover.stdout);
    assert!(
        survivors.trim().is_empty(),
        "workers survived the interrupt: {survivors:?}"
    );

    fs::remove_dir_all(&home).ok();
}
