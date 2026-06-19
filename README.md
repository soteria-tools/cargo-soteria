# Soteria

A Cargo subcommand for running [Soteria](https://github.com/soteria-tools/soteria) analysis on Rust projects.

## Overview

`cargo-soteria` provides a convenient way to run soteria symbolic execution on your Rust crates. `cargo soteria setup` downloads pre-built binaries of Soteria and all its dependencies, so you don't need to install OCaml or build the tool from source.

## Installation

```bash
cargo install soteria
cargo soteria setup # Installs binaries
# Run the setup command again to install the latest nightly release any time.
```

The setup installs the latest nightly release (~27MB compressed, ~85MB uncompressed) with all necessary tools.

## Uninstallation

To uninstall (including the toolchain installed in `~/.soteria/`):

```bash
cargo soteria unsetup
cargo uninstall soteria
```

`cargo soteria unsetup` will show you what will be removed (location, total size, and installed versions) and ask for confirmation before deleting anything.

## Usage

Run soteria analysis on your crate:

```bash
cd your-rust-project/
cargo soteria
```

`cargo soteria` discovers every symbolic test in the crate and analyses them in parallel,
(one `soteria-rust` process per test) streaming each result as it
finishes. All other arguments are forwarded to the workers.

Use `-j`/`--jobs` to control how many tests run at once (default: a quarter of
the available CPUs).

```bash
cargo soteria -j 8
```

### Common Options

- `-j, --jobs <N>`: Number of tests to analyse concurrently (default: CPUs / 4)
- `--filter=<pattern>`: Only analyse tests whose name matches the regex
- `--kani`: Compatibility layer with Kani harnesses

See `cargo soteria --help` for all available options.

## Example Test

Create a simple symbolic test using the Soteria API:

```rust
// src/lib.rs

#[soteria::test]
fn verify_addition() {
    let a: u32 = soteria::nondet_bytes();
    let b: u32 = soteria::nondet_bytes();
    
    soteria::assume(a < 1000);
    soteria::assume(b < 1000);
    
    let result = a + b;
    assert!(result >= a);
    assert!(result >= b);
}
```

Run the analysis:

```bash
cargo soteria
```

Output:
```
  Soteria running 1 test · 1 worker

  ✓ verify_addition  0.21s

  ── Summary ──────────────────────────
  1 passed   in 0.2s
```

When a test fails or the analyzer crashes, its diagnostics are printed inline
under the result, and the run finishes with a list of the failing tests and a
non-zero exit code (`1` if any failed, `2` if any crashed, `130` if interrupted).

### Nextest

If you have [nextest](https://nexte.st) installed, Soteria provides a nextest adapter
```
cargo soteria nextest --help
cargo soteria nextest list # list all tests
cargo soteria nextest run # run all tests
cargo soteria nextest run --help # See how to filter tests etc..
```


## Docker

A pre-built image is published to GitHub Container Registry with soteria-rust
already baked in:

```bash
docker run --rm -v "$PWD:/workspace" \
  ghcr.io/soteria-tools/cargo-soteria:nightly --kani
```

The crate in the current directory is mounted at `/workspace` (the image's
working directory). Any arguments after the image name are forwarded to
`cargo soteria`.

**Tags:**

- `:nightly` — moving tag, rebuilt daily with that day's soteria-rust nightly.
- `:YYYY-MM-DD` — immutable per-day tag, for pinning or rolling back. A rolling
  ~7-day window of dated tags is retained.

**Runs as a non-root user.** The container runs as the unprivileged `soteria`
user, so build artifacts written into your mounted directory (`target/`,
`Cargo.lock`, `*.llbc.json`, …) are **not** owned by root. If you need them
owned by your host user specifically, pass your UID/GID:

```bash
docker run --rm -u "$(id -u):$(id -g)" -v "$PWD:/workspace" \
  ghcr.io/soteria-tools/cargo-soteria:nightly
```

**Platform:** the image is `linux/amd64` only. On Apple Silicon / other hosts
it runs under emulation (Docker pulls the amd64 image automatically).

## Supported Platforms

- **macOS ARM64** (Apple Silicon)
- **Linux x86_64**

## Support

For issues related to:
- **cargo-soteria installation/usage**: open an issue in this repository.
- **soteria-rust analysis**: see the [soteria documentation](https://github.com/soteria-tools/soteria).
- **Verification harness APIs**: see the [soteria test examples](https://github.com/soteria-tools/soteria/tree/main/soteria-rust/test/cram).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for how cargo-soteria works internally,
the project layout, and how to build, test, and extend it.

## License

Apache-2.0

## Related Projects

- [soteria](https://github.com/soteria-tools/soteria) — The main soteria verification framework
- [Kani](https://github.com/model-checking/kani) — Rust verification tool (API compatibility)
- [Miri](https://github.com/rust-lang/miri) — Rust interpreter (API compatibility)
