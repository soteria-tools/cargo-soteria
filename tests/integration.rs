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
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

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
