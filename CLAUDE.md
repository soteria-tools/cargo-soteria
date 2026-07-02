# CLAUDE.md

`cargo-soteria` is a Cargo subcommand that downloads, installs, and runs the
pre-built `soteria-rust` analyzer (from the upstream
[`soteria-tools/soteria`](https://github.com/soteria-tools/soteria) repo). It
contains no analysis logic of its own — it is purely a packaging/runner wrapper.

## Commands

```bash
# Build
cargo build --release

# Install locally for testing
cargo install --path .

# Check (faster than build)
cargo check
cargo clippy
```

Integration tests live in `tests/integration.rs` and exercise the full binary against the fixture crate in `tests/fixtures/simple-crate/`.

```bash
# Online test — downloads from the GitHub nightly release (~25 MB):
cargo test --test integration online_install_and_run -- --nocapture

# End-to-end nextest test — real cargo-nextest drives the symbolic tests through
# the custom runner (installs the toolchain online; skipped if cargo-nextest is
# absent):
cargo test --test integration nextest_online_install_and_run -- --nocapture

# Local test — requires packages/soteria-rust/ to be pre-built in a soteria checkout:
SOTERIA_LOCAL_PATH=/path/to/soteria cargo test --test integration local_install_and_run -- --nocapture

# Run all:
SOTERIA_LOCAL_PATH=/path/to/soteria cargo test --test integration -- --nocapture
```

Each test installs into a fresh temp directory via `SOTERIA_HOME` so it never touches `~/.soteria`. `local_install_and_run` is silently skipped when `SOTERIA_LOCAL_PATH` is not set (it points at a separate checkout of the upstream soteria repo). `nextest_online_install_and_run` runs `cargo soteria nextest run` against an isolated copy of the simple-crate fixture and asserts both entry points pass through the real cargo-nextest handshake.

Three further tests need neither the network nor a real soteria-rust: `parallel_classifies_and_survives_crashes` and `interrupt_kills_running_workers` install a fake `soteria-rust` (`tests/fixtures/fake-soteria-rust.sh`) into a temp `SOTERIA_HOME` and drive the parallel runner deterministically — verifying outcome classification, crash-resilience, and that Ctrl-C leaves no worker processes alive. `type_error_is_propagated` checks that when discovery fails because the crate doesn't compile, the full rustc diagnostic (which soteria-rust prints to *stdout*, not stderr) reaches the user — not just our terse "discovery failed" line. It drives the fake against `tests/fixtures/type-error/` (a crate with a deliberate `E0308` type error; its `soteria-compile-error.txt` is the canned diagnostic the fake emits on a compile failure) and compares the runner's stderr to the committed `expected-stderr.txt` snapshot (regenerate with `UPDATE_SNAPSHOTS=1`). The parallel runner is exercised end-to-end against the real analyzer by two fixture crates: `tests/fixtures/many-tests/` (~30 tests, a mix of passing and failing) and `tests/fixtures/many-slow-tests/` (used by the Docker smoke test for sustained-load behavior).

Plain `#[test]` units run under a bare `cargo test` (`cargo test --bins`) with no soteria-rust install: `runner_common` covers filter-anchoring/escaping (`anchors_and_escapes`) and test-list parsing (`parses_test_list_json`); `base_runner` covers user-flag stripping (`strips_user_filter_and_exclude`); `main.rs` covers the clap arg parsing; `nextest` covers target-selection detection, `rustc -vV` parsing, and TOML escaping.

## Architecture

`cargo-soteria` is a Cargo subcommand that manages downloading, installing, and running the pre-built `soteria-rust` tool.

**One binary, split into focused modules** (`src/`):
- `main.rs` — only CLI definitions (clap-derive `RunArgs`/`SetupArgs`) and the
  argument parsing + dispatch in `main()`.
- `common.rs` — cross-cutting helpers: install-path resolution
  (`soteria_base_dir`/`package_dir`), the `VERSION` const, terminal UI
  (`spinner`, `download_bar`, the `ok`/`info`/`warn`/`fail` status lines).
- `setup.rs` — the `setup`/`unsetup` subcommands (GitHub release fetch,
  download/extract or local copy, version tracking, toolchain check,
  plugin pre-build, uninstall).
- `runner_common.rs` — logic shared by both runners: `soteria_rust_command()`
  (the env-configured invocation), `discover_tests()` (compile + list the
  crate's entry points), `parse_test_list`, `anchored_filter`.
- `base_runner.rs` — the built-in parallel test runner (the default action).
- `nextest.rs` — the cargo-nextest integration.
- `help.rs` — the rebranded `--help` output.

`cargo soteria unsetup` removes the whole `~/.soteria/` (showing location, total
size, and the installed release read from the `VERSION` file, then asking for
confirmation).

**Runtime flow when a user runs `cargo soteria [args...]`:**

1. Cargo invokes `cargo-soteria soteria [args...]`; `main()` strips the `soteria` word Cargo inserts, then dispatches on the first arg. Argument parsing is clap-derive (`RunArgs`, `SetupArgs` in `src/main.rs`); the default (no-subcommand) path owns only `-j`/`--jobs` and forwards everything else verbatim to `soteria-rust exec .` via a `trailing_var_arg` bag, so our own flags must precede the forwarded ones. `nextest`/`__nextest-runner` are dispatched before parsing and forward raw args.
2. If the first arg is `setup`, calls `cmd_setup()`:
   - Resolves which release to install from the `--release` flag: no flag →
     the latest patch of the highest supported minor (`pick_default` over
     `GET /releases?per_page=100`); `--release nightly` → the rolling nightly;
     `--release X.Y.Z` → that exact version. Installing the nightly or a version
     outside `MIN_SUPPORTED_MINOR..=MAX_SUPPORTED_MINOR` warns and asks to
     confirm (`--yes` skips the prompt).
   - Downloads the platform bundle picked by `find_platform_asset()`, which
     matches a `soteria-rust-` prefix + platform suffix (`macos-arm64.zip` /
     `linux-x86_64.zip`). This tolerates both the nightly's unversioned name
     (`soteria-rust-macos-arm64.zip`) and a versioned release's embedded version
     (`soteria-rust-v0.1.0-macos-arm64.zip`), while the prefix guard skips the
     sibling `soteria-c-…` bundle
   - Extracts atomically to `~/.soteria/soteria-release/` (a single fixed folder,
     overwritten each install) via a `.installing` temp dir
   - Writes `version.json` (release ID, for update detection) and a plain-text
     `VERSION` file (human-readable name, e.g. `0.3.0` or `nightly-<date-time>`)
     inside that folder
   - Runs `obol toolchain-path` to verify the Rust toolchain is present
   - Runs `soteria-rust build-plugins` to pre-compile the plugin crate, so the
     first real run doesn't pay the compilation cost (it would otherwise build
     plugins lazily on first `exec`)
3. Otherwise, runs the crate's symbolic tests **in parallel** (`src/base_runner.rs`, with discovery shared from `src/runner_common.rs`):
   - **Discover:** `soteria-rust compile --list-tests .` compiles the crate once
     and prints the entry points as a one-line JSON array on stdout (progress on
     stderr). User flags (e.g. `--filter`) are forwarded here, so discovery
     respects them.
   - **Fan out:** a worker pool (`-j`/`--jobs`, default = available CPUs / 4)
     runs, per test, `soteria-rust exec . --no-compile --no-compile-plugins
     --filter ^test$`. Passing `--no-compile` makes each worker reuse the ULLBC
     compiled in the discover step instead of re-invoking cargo/charon (which
     would race on the crate's shared target dir). The filter is anchored and
     `Str`-regex-escaped so a worker runs exactly one entry point.
   - **Report:** results stream as each test finishes (status + captured
     diagnostics), then a summary. Exit code: `0` all passed · `1` some failed ·
     `2` some crashed · `130` interrupted.
   - **Ctrl-C:** each worker child is spawned in its own process group; the
     handler `killpg`s every group with SIGKILL, so no soteria-rust (or its
     z3/charon children) survives the interrupt.

   All soteria-rust invocations set up the environment first:
   - `DYLD_LIBRARY_PATH` (macOS) / `LD_LIBRARY_PATH` (Linux) → `~/.soteria/soteria-release/lib/`
   - `SOTERIA_Z3_PATH`, `SOTERIA_OBOL_PATH`, `SOTERIA_CHARON_PATH` → paths under `bin/`
   - `SOTERIA_RUST_PLUGINS` → `~/.soteria/soteria-release/plugins/`

**Install directory structure** (`~/.soteria/soteria-release/`):
```
bin/       soteria-rust, z3, obol, charon, *-driver
lib/       bundled shared libs — .dylib (macOS) / .so (Linux): libgmp, libz3, etc.
plugins/   soteria/, soteria_macros/, std/, kani/, kani_macros/
version.json   machine-readable: release_tag, published_at, release_id
VERSION        human-readable release name (0.3.0 or nightly-<date-time>)
```

**Version tracking:** `version.json` stores the GitHub release ID; on `setup`, if the installed release ID matches the one being installed, the user is asked to confirm before re-downloading. The single fixed `soteria-release/` folder (not per-version) is overwritten on each install, so only one toolchain sits on disk.

**Platform constraint:** `build.rs` emits a compile error for any target other than `aarch64-apple-darwin` (macOS ARM64) or `x86_64-unknown-linux-gnu` (Linux x86_64). The runtime (`platform_asset_suffix()`) also checks and exits early for unsupported platforms. The macOS path is what `cargo install` produces; the Linux path backs the Docker image.

**Local install workflow** (for testing against an in-progress soteria build):
```bash
cargo soteria setup --local /path/to/soteria
```
This copies `soteria/packages/soteria-rust/` from a local checkout of the upstream soteria repo instead of downloading from GitHub.

### cargo-nextest integration (`src/nextest.rs`)

`cargo soteria nextest [args…]` runs the crate's symbolic tests under
[cargo-nextest](https://nexte.st) instead of the built-in parallel runner.

Soteria-rust flags (e.g. `--kani`) go *after* a `--`, mirroring `cargo test --
<args>`: `cargo soteria nextest run -- --kani` lists and runs the crate's Kani
harnesses under nextest. Args before `--` go to `cargo nextest`; everything
after is the soteria-rust flag bag, forwarded to *every* soteria-rust call.

The catch: Soteria's tests live behind `#[cfg(soteria)]` and are compiled and
executed by `soteria-rust` (via charon), **not** by libtest — so a *native*
`cargo`/nextest build of the crate sees **zero** tests. The integration bridges
this exactly like `cargo miri nextest` does, via cargo's **target runner**
mechanism (which nextest honors during *both* its list and run phases —
https://nexte.st/docs/features/target-runners/):

1. **Wrapper** (`nextest::run`): splits the args at the first `--` (front →
   `cargo nextest`, tail → the soteria-rust flag bag), then shells out to `cargo
   nextest [front…]`, forcing `--target <host-triple>` (a runner only fires for
   an explicit, non-host target) and injecting `--config
   target.<triple>.runner=["<self>", "__nextest-runner"]`. Defaults to `--lib`
   unless the user already selected targets — every probed test binary would
   otherwise return the *same* full soteria list, duplicating each test. The
   host triple comes from `rustc -vV`; `<self>` is `std::env::current_exe()`.
   Errors early if soteria isn't installed or `cargo nextest` isn't on PATH.
   nextest controls the runner's argv, so the flag bag is passed to the runner
   out-of-band: JSON-encoded into `__SOTERIA_NEXTEST_EXTRA_ARGS`, which cargo
   nextest inherits and hands to the target-runner children it spawns.
2. **Runner** (`nextest::runner`, hidden `__nextest-runner` verb dispatched in
   `main()`): the program nextest invokes as
   `<self> __nextest-runner <native-test-bin> <protocol-args…>`. It *ignores*
   the native test binary (just a vehicle) and translates nextest's libtest
   protocol to soteria-rust:
   - `--list --format terse` → `soteria-rust compile --list-tests .`, reprinted
     as `name: test` lines (clean stdout only; compile progress to stderr).
     `--ignored` returns empty (soteria has no `#[ignore]`).
   - `<name> --exact --nocapture` → `soteria-rust exec . --no-compile
     --no-compile-plugins --filter ^name$`, propagating soteria's exit code
     (nextest reads 0 = pass, non-zero = fail).

   Both translations splice in the `__SOTERIA_NEXTEST_EXTRA_ARGS` flag bag, so a
   post-`--` flag like `--kani` reaches the list-phase compile *and* every exec.

The single list-phase compile populates the crate's ULLBC cache; per-test
`--no-compile` runs reuse it (the same trick the built-in runner relies on).
`runner_common::{soteria_rust_command, discover_tests, parse_test_list, anchored_filter}` are shared with `src/base_runner.rs`.
The protocol translation is covered without nextest/the real analyzer by the
`nextest_runner_*` tests in `tests/integration.rs` (which drive the hidden
runner against the fake soteria-rust), and the full real handshake by
`nextest_online_install_and_run`.

### Key constants (`src/setup.rs`, except the install dir in `src/common.rs`)

| Constant | Value |
|---|---|
| `REPO_OWNER` | `soteria-tools` |
| `REPO_NAME` | `soteria` |
| `RELEASE_TAG` | `nightly` (the moving nightly tag) |
| `MIN_SUPPORTED_MINOR` / `MAX_SUPPORTED_MINOR` | `(0,1)` / `(0,3)` — inclusive `(major,minor)` window; installs outside it warn (illustrative, edit as versioning proceeds) |
| Install dir (`common::RELEASE_DIR`) | `~/.soteria/soteria-release/` |

### Adding a new platform

1. Build the soteria package on the target platform (the upstream soteria repo's CI, or `make package-soteria-rust` in a soteria checkout).
2. Add platform detection in `platform_asset_suffix()` and the `build.rs` check.
3. Update `supported-platforms` in `Cargo.toml`.

### Docker image

`cargo-soteria` is also published as a container at
`ghcr.io/soteria-tools/cargo-soteria` (linux/amd64 only). The image runs the
Linux soteria-rust bundle and ships a turnkey toolchain, so users don't need a
local Rust install or to run `setup`.

**`Dockerfile`** — two stages:
- *builder*: `rust:1-bookworm`, `cargo install --path . --bin cargo-soteria`.
- *runtime*: `ubuntu:24.04` (Noble required — obol needs GLIBC ≥ 2.39, which
  bookworm lacks). Bootstraps rustup with `--default-toolchain none`, then bakes
  in `cargo-soteria setup --release nightly --yes` so the soteria-rust nightly
  ships in the image and users never run `setup` (`--release nightly` because the
  default is now a stable release; `--yes` because the nightly's confirmation
  prompt can't be answered in a non-interactive build). Because `setup` also runs `build-plugins`, the
  plugin crate is compiled at image-build time, so analysis starts fast on the
  first `docker run` instead of paying a multi-minute compile. The bootstrap
  `stable` toolchain is dropped in the
  same layer as `setup`. `GITHUB_TOKEN` is passed as a build secret
  (`--mount=type=secret,id=github_token`), optional, never baked into a layer.

**Non-root runtime.** The image runs as the unprivileged `soteria` user
(uid 1001) so artifacts written into a bind-mounted `/workspace` are owned by a
normal UID, not root. Because of this, all baked state is relocated out of
`/root` (mode 700, unreachable by other UIDs) via `ENV` that persists into
runtime:

| Var | Value |
|---|---|
| `RUSTUP_HOME` | `/usr/local/rustup` |
| `CARGO_HOME` | `/usr/local/cargo` |
| `SOTERIA_HOME` | `/opt/soteria` (→ install at `/opt/soteria/soteria-release/`) |
| `HOME` | `/home/soteria` |

`/opt/soteria`, `/usr/local/rustup`, `/usr/local/cargo` are made world-writable
(`chmod -R a+rwX`). This is **load-bearing, not just convenience**: the plugin
crates are pre-built during `setup` (each as its own crate under
`/opt/soteria/soteria-release/plugins/<name>/` with its own `Cargo.lock` + `target/`), but
soteria-rust may still touch/rebuild them at run time, so the install dir
cannot be read-only. Users needing host-UID ownership can override with
`docker run --user $(id -u):$(id -g)`.

**CI/CD workflows** (in `.github/workflows/`):
- `ci.yml` — on every push/PR to `master`: a `check` job (`cargo check`, `cargo
  clippy -D warnings`, `cargo fmt --check`, `cargo test --bins` for the unit
  tests), and an `integration` job on a `macos-latest` × `ubuntu-latest` matrix
  that installs cargo-nextest (`taiki-e/install-action@nextest`) and runs the
  full `tests/integration.rs` suite — including the online install and the
  end-to-end nextest handshake.
- `docker.yml` — builds + smoke-tests the image on every push/PR to `master`.
  Never publishes.
- `nightly.yml` — daily (03:00 UTC, after the soteria nightly release at 02:00)
  builds, smoke-tests, then publishes `:nightly` (moving tag) plus an immutable
  `:YYYY-MM-DD` tag (via `type=raw` so `workflow_dispatch` behaves identically
  to the schedule). A parallel `prune` job
  (`snok/container-retention-policy@v3`) keeps a rolling ~7-day rollback
  window. Two non-obvious constraints baked into that job's comments:
  build steps set `provenance: false` because the prune action is **not**
  referrer-aware and would corrupt attestation child manifests; and
  `image-tags: "!nightly"` is a no-op with the temporal `GITHUB_TOKEN` (the
  `!`/`*` operators need a PAT/App token), so `:nightly` is protected only by
  being repointed daily under the cut-off.

**Build & test the image locally:**
```bash
GITHUB_TOKEN=$(gh auth token) docker build --platform linux/amd64 \
  --secret id=github_token,env=GITHUB_TOKEN -t cargo-soteria:test .

# Smoke test against the fixture crate:
docker run --rm -v "$PWD/tests/fixtures/simple-crate:/workspace" cargo-soteria:test
```

## Upstream soteria reference

`cargo-soteria` downloads pre-built bundles from the upstream
[`soteria-tools/soteria`](https://github.com/soteria-tools/soteria) repo — that
project is developed separately and is not part of this repository. The nightly
GitHub release (tag `nightly`) is produced by its `.github/workflows/nightly.yml`:

- Runs daily at 02:00 UTC; skips if no new commits since last nightly
- Calls the reusable `build.yml` which: sets up OCaml/opam, builds with Dune, installs Obol and Charon at pinned commits, runs `make package-soteria-rust`
- The package is created by `packaging/soteria-rust/package.ml` (collecting shared-lib dependencies — `otool -L` on macOS, with a Linux dylib manifest checked by `packaging/soteria-rust/dune`), then assembles `packages/soteria-rust/{bin,lib,plugins}/`
- Built on both macOS-ARM64 and Linux-x86_64 runners, zipped as `soteria-rust-macos-arm64.zip` and `soteria-rust-linux-x86_64.zip`, and uploaded as prerelease assets — `cargo-soteria`'s `find_platform_asset()` matches these by `soteria-rust-` prefix + platform suffix. Versioned releases embed the version (`soteria-rust-v0.1.0-macos-arm64.zip`), which the same matcher handles
