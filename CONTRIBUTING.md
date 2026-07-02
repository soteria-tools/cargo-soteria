# Contributing to cargo-soteria

`cargo-soteria` is a thin Cargo subcommand that downloads, installs, and runs
the pre-built `soteria-rust` analyzer. It contains no analysis logic of its own.
This document covers how it works, the project layout, and how to build, test,
and extend it.

## How it works

1. **`cargo soteria setup`** downloads a `soteria-rust` bundle for your platform
   from the [soteria](https://github.com/soteria-tools/soteria) GitHub releases
   and extracts it to `~/.soteria/soteria-release/` (`bin/`, `lib/`, `plugins/`).
   Without a flag it installs the latest supported stable release; `--release
   nightly` or `--release X.Y.Z` picks another (installing an unsupported one
   warns and asks to confirm, or pass `--yes`). It then verifies the Rust
   toolchain and pre-builds the analyzer plugins so the first real run is fast.
   `version.json` records the release id for update detection and `VERSION` the
   human-readable name. (`SOTERIA_HOME` overrides the install base.)

2. **`cargo soteria [args]`** runs the crate's symbolic tests in parallel
   (`src/run.rs`):
   - **Discover** the entry points once with
     `soteria-rust compile --list-tests .` — a one-line JSON array on stdout.
     User arguments such as `--filter` are forwarded, so discovery respects
     them.
   - **Fan out** across a worker pool (`-j`/`--jobs`, default = CPUs / 4),
     running `soteria-rust exec . --no-compile --no-compile-plugins
     --filter ^test$` per test. `--no-compile` makes each worker reuse the
     ULLBC compiled during discovery instead of re-invoking cargo/charon, which
     would otherwise race on the crate's shared target directory. The filter is
     anchored and `Str`-regex-escaped so each worker runs exactly one entry
     point.
   - **Report** results as each test finishes, then a summary. Exit code:
     `0` all passed · `1` some failed · `2` some crashed · `130` interrupted.
   - **Ctrl-C** SIGKILLs every worker's process group, so no `soteria-rust`
     (or its z3/charon children) survives the interrupt.

Before invoking `soteria-rust`, the wrapper sets:

| Variable | Value |
|---|---|
| `DYLD_LIBRARY_PATH` (macOS) / `LD_LIBRARY_PATH` (Linux) | `~/.soteria/soteria-release/lib/` |
| `SOTERIA_Z3_PATH`, `SOTERIA_OBOL_PATH`, `SOTERIA_CHARON_PATH` | paths under `bin/` |
| `SOTERIA_RUST_PLUGINS` | `~/.soteria/soteria-release/plugins/` |

## Project structure

```
cargo-soteria/
├── Cargo.toml
├── build.rs                # compile-time check that the target platform is supported
├── src/
│   ├── main.rs             # CLI dispatch, setup/unsetup, download & install, env setup
│   ├── run.rs              # parallel test runner (-j/--jobs, Ctrl-C teardown)
│   └── help.rs             # `cargo soteria --help` rendering
└── tests/
    ├── integration.rs      # end-to-end + deterministic (fake-shim) tests
    └── fixtures/
        ├── simple-crate/   # 2-test crate
        ├── many-tests/     # ~30-test crate (mix of passing & failing)
        └── fake-soteria-rust.sh  # fake analyzer that drives the runner deterministically
```

## Building

```bash
cd cargo-soteria
cargo build --release
cargo install --path .   # install locally for testing
cargo check              # faster than a full build
cargo clippy
```

## Testing

```bash
# Deterministic runner tests (no network, no real soteria-rust):
cargo test --test integration -- \
  parallel_classifies_and_survives_crashes interrupt_kills_running_workers

# Online — downloads the nightly release from GitHub (~27MB) and runs analysis:
cargo test --test integration online_install_and_run -- --nocapture

# Local — install from a soteria checkout with packages/soteria-rust/ pre-built
# (run `make package-soteria-rust` in the checkout first):
SOTERIA_LOCAL_PATH=/path/to/soteria cargo test --test integration local_install_and_run -- --nocapture
```

Each test installs into a fresh temp `SOTERIA_HOME`, so it never touches your
real `~/.soteria`. The `parallel_classifies_and_survives_crashes` and
`interrupt_kills_running_workers` tests install `tests/fixtures/fake-soteria-rust.sh`
to drive the runner — verifying outcome classification, crash-resilience, and
that Ctrl-C leaves no worker processes alive — without needing the real analyzer.

### Testing against a local soteria build

```bash
cargo soteria setup --local /path/to/soteria
```

This installs from `packages/soteria-rust/` in a local soteria checkout instead
of downloading from GitHub — useful for testing in-progress soteria changes.

## Adding a platform

`soteria-rust` bundles are downloaded per platform. To add one:

1. Build the soteria package on the target platform (via CI, or
   `make package-soteria-rust` in a soteria checkout).
2. Add the platform to `platform_asset_suffix()` in `src/setup.rs` and to the
   supported-target check in `build.rs`.
3. Add the target triple to `supported-platforms` in `Cargo.toml`.

## Docker image

The image is built and smoke-tested by `.github/workflows/docker.yml` (every
push/PR) and published nightly by `.github/workflows/nightly.yml`. To build and
test it locally:

```bash
cd cargo-soteria
GITHUB_TOKEN=$(gh auth token) docker build --platform linux/amd64 \
  --secret id=github_token,env=GITHUB_TOKEN -t cargo-soteria:test .

# Smoke test against a fixture crate:
docker run --rm -v "$PWD/tests/fixtures/simple-crate:/workspace" cargo-soteria:test
```

## License

Apache-2.0
