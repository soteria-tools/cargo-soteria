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
//!     → `soteria-rust compile --list-tests . [--test <t>]`, reprinted as `name: test`
//!   * **run**  — `<runner> <bin> <name> --exact --nocapture`
//!     → `soteria-rust exec . --no-compile [--test <t>] --filter ^name$`, exit 0 = pass
//!
//! The native test binary nextest builds is just a vehicle for the runner to
//! hang off — the runner ignores its path and gets most things from
//! soteria-rust. The only thing nextest is used for is the binary ID, read via
//! the `NEXTEST_BINARY_ID` field set on cargo-nextest 0.9.138 and above. This
//! is used to determine the soteria `--test` profile, since soteria-rust
//! analyses one target per invocation.
//!
//! Each binary's list-phase compile populates that target's ULLBC cache, which
//! the per-test `--no-compile` runs reuse (the same idea `base_runner.rs`
//! relies on to avoid each worker re-invoking charon).
//!
//! [cargo-nextest]: https://nexte.st

use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};

use colored::Colorize;

use crate::common::{cargo_command, fail, package_dir};
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

    let mut cmd = cargo_command();
    cmd.arg("nextest");
    cmd.args(&nextest_args);
    // If the user didn't name any targets, let nextest build its default set
    // and let the runner pick the proof targets per binary, from each package's
    // `[package.metadata.soteria.default-targets]`. For `--test` targets,
    // soteria-rust reads both `#[soteria::test]` and ordinary `#[test]`, and
    // most integration test targets aren't meant to run symbolically, so the
    // runner skips the ones a package didn't declare. This allows the command
    // from a virtual workspace root, where there's no single current package.
    // (The user can override this with `--lib`/`--test`.)
    //
    // In order to determine the list of default targets, the runner needs
    // access to `cargo metadata`. Nextest also needs `cargo metadata`, and
    // fetching it can be slow. So fetch it once, write it to a temp file, and
    // pass the same file both to nextest (`--cargo-metadata`) and the runner
    // (`SOTERIA_CARGO_METADATA`). `SOTERIA_USE_DEFAULT_TARGETS` is always set
    // so the runner knows whether to do the appropriate filtering. The temp
    // file is retained until nextest exits.
    let use_default_targets = !selects_targets(&nextest_args);
    cmd.env(
        USE_DEFAULT_TARGETS_ENV,
        if use_default_targets { "1" } else { "0" },
    );
    let metadata = if use_default_targets {
        let metadata = WrapperCargoMetadata::fetch();
        cmd.arg("--cargo-metadata").arg(metadata.path());
        cmd.env(CARGO_METADATA_ENV, metadata.path());
        Some(metadata)
    } else {
        None
    };
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

    let status = cmd.status();
    // `process::exit` below skips destructors, so delete the metadata temp file
    // explicitly now that nextest has consumed it.
    drop(metadata);
    match status {
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

// ── default target selection (`[package.metadata.soteria.default-targets]`) ────

/// The crate's declared default proof targets, read from
/// `[package.metadata.soteria.default-targets]`:
///
/// ```toml
/// [package.metadata.soteria.default-targets]
/// lib = true            # analyse src/ `#[soteria::test]` proofs (the default)
/// test = ["soteria"]    # integration-test targets that are proof harnesses
/// ```
///
/// Both keys are optional.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultTargets {
    #[serde(default = "default_lib")]
    lib: bool,
    #[serde(default)]
    test: Vec<String>,
}

/// Return the default value for `lib`: true.
fn default_lib() -> bool {
    true
}

impl Default for DefaultTargets {
    /// Return the default value.
    ///
    /// A package with no `[default-targets]` table behaves like an empty one:
    /// `lib = true` (analyse the library), but do not look at integration-test
    /// targets.
    fn default() -> Self {
        DefaultTargets {
            lib: default_lib(),
            test: Vec::new(),
        }
    }
}

/// The `cargo metadata` fetched in the outer invocation, written to a temp file
/// so the metadata is available both to nextest and to our target runner.
///
/// The runner reads it to decide which targets contain proofs (see
/// [`ProbedBinary::is_default_target`]).
struct WrapperCargoMetadata {
    /// The verbatim `cargo metadata` JSON on disk.
    file: tempfile::NamedTempFile,
}

impl WrapperCargoMetadata {
    /// Run and capture `cargo metadata`.
    fn fetch() -> Self {
        let output = cargo_command()
            // Do not pass in --no-deps here -- nextest requires dependencies to
            // be present in the metadata.
            .args(["metadata", "--format-version", "1"])
            .output()
            .unwrap_or_else(|e| fail(&format!("Could not run `cargo metadata`: {e}")));
        if !output.status.success() {
            fail(&format!(
                "`cargo metadata` failed ({}).\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let mut file = tempfile::Builder::new()
            .prefix("soteria-cargo-metadata-")
            .suffix(".json")
            .tempfile()
            .unwrap_or_else(|e| {
                fail(&format!(
                    "Could not create a temp file for cargo metadata: {e}"
                ))
            });
        file.write_all(&output.stdout)
            .and_then(|()| file.flush())
            .unwrap_or_else(|e| {
                fail(&format!(
                    "Could not write cargo metadata to a temp file: {e}"
                ))
            });
        WrapperCargoMetadata { file }
    }

    /// The path to the cargo metadata file.
    fn path(&self) -> &std::path::Path {
        self.file.path()
    }
}

// ── `__nextest-runner <bin> <args…>` — the hidden cargo target runner ─────────

/// The verb that marks this binary as nextest's target runner (the second
/// element of the injected `runner` array). Also matched in `main()`'s dispatch.
pub const RUNNER_FLAG: &str = "__nextest-runner";

/// Hidden mode invoked by nextest as `<exe> __nextest-runner <test-bin> <args…>`.
/// `args` is everything after the flag: `[<test-bin>, <protocol args…>]`. We
/// ignore the (native, soteria-less) test binary; what matters is the cargo
/// target it stands for, read from `NEXTEST_BINARY_ID` (see [`ProbedBinary`]).
/// Diverges.
pub fn runner(args: &[String]) -> ! {
    // Drop the test-binary path; the protocol args are what matter.
    let proto = args.get(1..).unwrap_or(&[]);
    let binary = ProbedBinary::from_env();

    // If the wrapper says to use default targets, skip any binary that isn't a
    // default target.
    if use_default_targets() && !binary.is_default_target(&RunnerCargoMetadata::load()) {
        std::process::exit(0);
    }

    let profile = binary.test_target();
    if proto.iter().any(|a| a == "--list") {
        list_phase(proto, profile);
    }
    run_phase(proto, profile);
}

/// Return true if the wrapper asked us to filter to the default proof targets.
fn use_default_targets() -> bool {
    match std::env::var(USE_DEFAULT_TARGETS_ENV).as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        Ok(other) => fail(&format!(
            "{USE_DEFAULT_TARGETS_ENV} should be `0` or `1`, but is `{other}`. \
             Are you using `cargo soteria nextest`?"
        )),
        Err(_) => fail(&format!(
            "{USE_DEFAULT_TARGETS_ENV} is not set. Are you using \
            `cargo soteria nextest`?"
        )),
    }
}

/// Nextest's binary ID environment variable, set for each probed binary in both
/// the list and run phases.
const BINARY_ID_ENV: &str = "NEXTEST_BINARY_ID";

/// The environment variable set in the wrapper to determine whether the runner
/// should filter to declared proof targets.
const USE_DEFAULT_TARGETS_ENV: &str = "SOTERIA_USE_DEFAULT_TARGETS";

/// If SOTERIA_USE_DEFAULT_TARGETS=1, the environment variable set by [`run`] to
/// the path of the shared `cargo metadata` JSON, so the runner can read each
/// package's `[default-targets]`.
const CARGO_METADATA_ENV: &str = "SOTERIA_CARGO_METADATA";

/// The cargo target a probed binary represents, parsed from
/// `NEXTEST_BINARY_ID`.
struct ProbedBinary {
    package: String,
    target: TargetKind,
}

/// Which kind of cargo target a probed binary is.
enum TargetKind {
    /// `pkg`: the library / proc-macro unit-test binary; analysed as crate source.
    Lib,
    /// `pkg::name`: the integration-test target `tests/name/`.
    IntegrationTest(String),
    /// `pkg::kind/name`: a bin/example/bench unit-test binary. The string is the
    /// kind (`bin`/`example`/`bench`). soteria-rust can't analyse these.
    Other(String),
}

impl ProbedBinary {
    fn from_env() -> Self {
        let id = std::env::var(BINARY_ID_ENV).unwrap_or_else(|_| {
            fail(&format!(
                "{BINARY_ID_ENV} is not set while listing tests.\n  \
                 `cargo soteria nextest` requires cargo-nextest 0.9.138 or newer; see \
                 https://nexte.st/docs/installation/pre-built-binaries/ or install from \
                 source with `cargo install cargo-nextest --locked`."
            ))
        });
        Self::parse(&id)
    }

    /// Parse the `NEXTEST_BINARY_ID` format.
    fn parse(id: &str) -> Self {
        match id.split_once("::") {
            None => ProbedBinary {
                package: id.to_string(),
                target: TargetKind::Lib,
            },
            Some((package, rest)) => {
                let target = match rest.split_once('/') {
                    None => TargetKind::IntegrationTest(rest.to_string()),
                    Some((kind, _name)) => TargetKind::Other(kind.to_string()),
                };
                ProbedBinary {
                    package: package.to_string(),
                    target,
                }
            }
        }
    }

    /// Return the soteria `--test` profile.
    ///
    /// * `None` means the lib target.
    /// * `Some(name)` means the integration-test target `name`.
    ///
    /// At the moment, soteria doesn't support other targets like `bin` or
    /// `example`. In normal use this should never occur.
    fn test_target(&self) -> Option<&str> {
        match &self.target {
            TargetKind::Lib => None,
            TargetKind::IntegrationTest(name) => Some(name),
            TargetKind::Other(kind) => fail(&format!(
                "cargo-soteria can't analyse {kind} targets (in package `{package}`); \
                 only the library and integration tests are supported.",
                package = self.package
            )),
        }
    }

    /// Return true if this binary is a default target.
    fn is_default_target(&self, metadata: &RunnerCargoMetadata) -> bool {
        let declared = metadata.default_targets_for(&self.package);
        match &self.target {
            TargetKind::Lib => declared.lib,
            TargetKind::IntegrationTest(name) => declared.test.iter().any(|t| t == name),
            TargetKind::Other(_) => false,
        }
    }
}

/// The subset of the shared cargo metadata the runner reads.
///
/// This is a lighter form of the full `cargo metadata` output.
#[derive(serde::Deserialize)]
struct RunnerCargoMetadata {
    packages: Vec<MetadataPackage>,
    workspace_members: HashSet<String>,
}

/// One entry of `cargo metadata`'s `packages` array (only the fields we read).
#[derive(serde::Deserialize)]
struct MetadataPackage {
    name: String,
    id: String,
    /// The `[package.metadata]` table, or `None`. (`cargo metadata` emits JSON
    /// `null` here when the table is absent.)
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

impl RunnerCargoMetadata {
    fn load() -> Self {
        let path = std::env::var(CARGO_METADATA_ENV).unwrap_or_else(|_| {
            fail(&format!(
                "{CARGO_METADATA_ENV} is unset while {USE_DEFAULT_TARGETS_ENV} is on. \
                 This is a bug in `cargo soteria nextest`."
            ))
        });
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| fail(&format!("Could not read cargo metadata from {path}: {e}")));
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| fail(&format!("Could not parse cargo metadata from {path}: {e}")))
    }

    /// The named package's effective `[default-targets]`.
    ///
    /// The lookup is restricted to workspace members so a same-named transitive
    /// dependency can't shadow the workspace package nextest actually probed.
    fn default_targets_for(&self, package: &str) -> DefaultTargets {
        let pkg = self
            .packages
            .iter()
            .find(|p| p.name == package && self.workspace_members.contains(&p.id))
            .unwrap_or_else(|| {
                fail(&format!(
                    "Workspace package `{package}` (from {BINARY_ID_ENV}) is not in the cargo \
                     metadata. This is a bug in `cargo soteria nextest`."
                ))
            });
        match pkg
            .metadata
            .as_ref()
            .and_then(|m| m.get("soteria"))
            .and_then(|s| s.get("default-targets"))
        {
            None => DefaultTargets::default(),
            Some(declaration) => serde_json::from_value(declaration.clone()).unwrap_or_else(|e| {
                fail(&format!(
                    "Invalid `[package.metadata.soteria.default-targets]` for package \
                     `{package}`: {e}\n  Expected `lib` (a boolean) and/or `test` (an \
                     array of integration-test target names)."
                ))
            }),
        }
    }
}

/// List phase: emit one `name: test` line per discovered entry point, and
/// nothing else on stdout (nextest requires clean stdout; soteria's compile
/// progress stays on stderr).
fn list_phase(proto: &[String], test_target: Option<&str>) -> ! {
    // soteria has no notion of `#[ignore]`; when nextest asks for the ignored
    // set, report none.
    if proto.iter().any(|a| a == "--ignored") {
        std::process::exit(0);
    }

    // Inherit stderr so the (one-time) compile progress streams as debug output;
    // our stdout must carry only the `name: test` lines. The compile must use
    // the same soteria flags as the per-test execs (e.g. `--kani`), so the
    // listed entry points match what the run phase will analyse.
    let tests = runner_common::discover_tests(&extra_soteria_args(), test_target, true)
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
fn run_phase(proto: &[String], test_target: Option<&str>) -> ! {
    let name = proto
        .iter()
        .find(|a| !a.starts_with('-'))
        .unwrap_or_else(|| fail("No test name passed to the soteria nextest runner."));

    let mut cmd = soteria_rust_command();
    cmd.arg("exec")
        .arg(".")
        .arg("--no-compile")
        .arg("--no-compile-plugins");
    if let Some(profile) = test_target {
        cmd.arg("--test").arg(profile);
    }
    let status = cmd
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
    let ok = cargo_command()
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
    fn binary_id_parses_to_target() {
        // Library and proc-macro unit tests use just the package name.
        let b = ProbedBinary::parse("iddqd");
        assert_eq!(b.package, "iddqd");
        assert!(matches!(b.target, TargetKind::Lib));
        assert_eq!(b.test_target(), None);

        // Integration tests: `pkg::name`.
        let b = ProbedBinary::parse("iddqd::soteria");
        assert_eq!(b.package, "iddqd");
        assert!(matches!(&b.target, TargetKind::IntegrationTest(n) if n == "soteria"));
        assert_eq!(b.test_target(), Some("soteria"));

        // Other kinds (`pkg::kind/name`).
        let b = ProbedBinary::parse("iddqd::bin/cli");
        assert_eq!(b.package, "iddqd");
        assert!(matches!(b.target, TargetKind::Other(kind) if kind == "bin"));
    }

    #[test]
    fn is_default_target_per_package() {
        // Two workspace packages: one declares `lib = false, test = ["soteria"]`,
        // the other has no declaration (so the defaults: `lib = true`, no test
        // targets — its `metadata` is JSON `null`, as cargo emits it).
        let metadata: RunnerCargoMetadata = serde_json::from_value(serde_json::json!({
            "workspace_members": [
                "path+file:///declared#0.1.0",
                "path+file:///bare#0.1.0"
            ],
            "packages": [
                {
                    "name": "declared",
                    "id": "path+file:///declared#0.1.0",
                    "metadata": {
                        "soteria": {
                            "default-targets": { "lib": false, "test": ["soteria"] }
                        }
                    }
                },
                {
                    "name": "bare",
                    "id": "path+file:///bare#0.1.0",
                    "metadata": null
                }
            ]
        }))
        .unwrap();
        let is_dt = |id: &str| ProbedBinary::parse(id).is_default_target(&metadata);

        // `declared`: lib opted out, only the `soteria` test target counts.
        assert!(!is_dt("declared")); // lib = false
        assert!(is_dt("declared::soteria")); // in `test`
        assert!(!is_dt("declared::integration")); // not in `test`
        assert!(!is_dt("declared::bin/cli")); // not a soteria target

        // `bare`: no declaration, so the lib is analysed by default, but test
        // targets are not.
        assert!(is_dt("bare")); // default lib = true
        assert!(!is_dt("bare::soteria")); // default test = []
    }

    #[test]
    fn default_targets_ignores_same_named_dependency() {
        // `cargo metadata` is fetched with dependencies, so `packages` can list a
        // dependency whose name collides with a workspace member's. The lookup
        // must resolve to the workspace member — identified by its id being in
        // `workspace_members` — not the dependency, even though the dependency is
        // listed first (so a name-only lookup would pick the wrong one).
        let metadata: RunnerCargoMetadata = serde_json::from_value(serde_json::json!({
            "workspace_members": ["path+file:///ws#collide@0.1.0"],
            "packages": [
                {
                    // A same-named dependency, listed first; must be ignored.
                    "name": "collide",
                    "id": "registry+https://example.com#collide@2.0.0",
                    "metadata": {
                        "soteria": { "default-targets": { "lib": true, "test": ["other"] } }
                    }
                },
                {
                    // The real workspace member: lib opted out, one test target.
                    "name": "collide",
                    "id": "path+file:///ws#collide@0.1.0",
                    "metadata": {
                        "soteria": { "default-targets": { "lib": false, "test": ["soteria"] } }
                    }
                }
            ]
        }))
        .unwrap();
        let is_dt = |id: &str| ProbedBinary::parse(id).is_default_target(&metadata);

        // The workspace member's declaration wins, not the dependency's.
        assert!(!is_dt("collide")); // workspace member: lib = false
        assert!(is_dt("collide::soteria")); // workspace member: in `test`
        assert!(!is_dt("collide::other")); // only the dependency declared this
    }

    #[test]
    fn default_targets_parse_from_metadata_json() {
        // lib + test.
        let json = serde_json::json!({ "lib": true, "test": ["soteria"] });
        let parsed: DefaultTargets = serde_json::from_value(json).unwrap();
        assert!(parsed.lib);
        assert_eq!(parsed.test, vec!["soteria".to_string()]);

        // lib defaults to true.
        let test_only = serde_json::json!({ "test": ["soteria"] });
        let parsed: DefaultTargets = serde_json::from_value(test_only).unwrap();
        assert!(parsed.lib);
        assert_eq!(parsed.test, vec!["soteria".to_string()]);

        // Opting the library out is explicit.
        let no_lib = serde_json::json!({ "lib": false, "test": ["soteria"] });
        let parsed: DefaultTargets = serde_json::from_value(no_lib).unwrap();
        assert!(!parsed.lib);

        // A typoed key is rejected rather than silently ignored.
        let bad = serde_json::json!({ "lib": true, "tests": ["soteria"] });
        assert!(serde_json::from_value::<DefaultTargets>(bad).is_err());
    }

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
