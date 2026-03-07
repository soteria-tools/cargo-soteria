# cargo-soteria

A Cargo subcommand for running [soteria-rust](https://github.com/soteria-tools/soteria) analysis on Rust projects.

## Overview

`cargo-soteria` provides a convenient way to run soteria symbolic execution on your Rust crates. It ships with pre-built binaries of soteria-rust and all its dependencies, so you don't need to install OCaml or build the tool from source.

## Installation

```bash
cargo install soteria
```

The binary is ~27MB and contains a compressed package (~85MB uncompressed) with all necessary tools:
- `soteria-rust` — the main analysis binary
- `z3` — SMT solver
- `obol` and `charon` — Rust IR frontends
- Verification plugins (kani, miri, rusteria)

On first run, the package is automatically extracted to `~/.soteria/<version>/`.

## Uninstallation

To uninstall (including extracted packages in `~/.soteria/`):

```bash
soteria-cleanup
cargo uninstall soteria
```

The `soteria-cleanup` utility will show you what will be removed and ask for confirmation before deleting anything.

## Usage

Run soteria analysis on your crate:

```bash
cd your-rust-project/
cargo soteria --kani
```

This is equivalent to running:
```bash
soteria-rust cargo . --kani
```

All arguments after `cargo soteria` are forwarded to `soteria-rust`.

### Common Options

- `--kani` — Use Kani verification harnesses
- `--miri` — Use Miri compatibility mode
- `--filter=<pattern>` — Filter test functions by regex
- `--help` — Show full soteria-rust help

See `cargo soteria --help` for all available options.

## Example Test

Create a simple verification test using the Kani API:

```rust
// src/lib.rs

#[kani::proof]
fn verify_addition() {
    let a: u32 = kani::any();
    let b: u32 = kani::any();
    
    kani::assume(a < 1000);
    kani::assume(b < 1000);
    
    let result = a + b;
    assert!(result >= a);
    assert!(result >= b);
}
```

Run the analysis:

```bash
cargo soteria --kani
```

Output:
```
Compiling... done in 3.45s
note: verify_addition: done in 0.18s, ran 2 branches
PC 1: (V|1| <u 0x000003e8) /\ (V|2| <u 0x000003e8)
PC 2: ...
```

## Architecture Support

Currently supported platforms:
- **macOS ARM64** (Apple Silicon)

The package structure allows easy addition of more platforms. Each architecture-specific package is built separately to keep binaries small and installation fast.

## How It Works

1. **Build time**: The build script (`build.rs`) detects your target OS/architecture and compresses the appropriate pre-built binary package into a tar.gz archive.

2. **Compile time**: The archive is embedded directly into the `cargo-soteria` binary using `include_bytes!`.

3. **First run**: On first execution, the binary extracts the package to `~/.soteria/<version>/` and sets executable permissions.

4. **Every run**: The binary sets up required environment variables and executes `soteria-rust cargo .` with your arguments.

## Environment Variables

The following variables are automatically set when running soteria-rust:

- `DYLD_LIBRARY_PATH` (macOS) / `LD_LIBRARY_PATH` (Linux) — Points to bundled dynamic libraries
- `SOTERIA_Z3_PATH` — Path to the Z3 SMT solver
- `SOTERIA_OBOL_PATH` — Path to the Obol frontend
- `SOTERIA_CHARON_PATH` — Path to the Charon frontend  
- `SOTERIA_RUST_PLUGINS` — Path to verification plugins (kani, miri, rusteria)

You can override these if needed, but the defaults work out of the box.

## Project Structure

```
cargo-soteria/
├── Cargo.toml          # Package metadata and dependencies
├── build.rs            # Compresses platform-specific packages at build time
├── packages/           # Pre-built binaries per platform
│   └── macos/
│       └── aarch64/
│           ├── bin/    # soteria-rust, z3, obol, charon, etc.
│           ├── lib/    # Dynamic libraries (libgmp, etc.)
│           └── plugins/# Verification API crates (kani, miri, rusteria)
└── src/
    └── main.rs         # Extracts package, sets env vars, execs soteria-rust
```

## Adding New Architectures

To add support for a new platform (e.g., Linux x86_64):

1. Download the soteria-rust package from CI:
   ```bash
   cd cargo-soteria
   gh run list --repo soteria-tools/soteria -b main --limit 5
   gh run download <run_id> \
     --repo soteria-tools/soteria \
     --name "ubuntu-latest-soteria-rust-package" \
     -D packages/linux/x86_64
   ```

2. Build for that target:
   ```bash
   cargo build --target x86_64-unknown-linux-gnu --release
   ```

3. Test:
   ```bash
   cargo install --path . --target x86_64-unknown-linux-gnu
   ```

The build script automatically detects the target platform and selects the appropriate package.

## Limitations

- Currently only supports macOS ARM64
- The binary is large (~27MB) due to embedded toolchain
- First run requires ~85MB disk space for extraction

## Updating Packages

The soteria-rust packages are automatically updated daily via GitHub Actions. When a new version is available from the upstream [soteria repository](https://github.com/soteria-tools/soteria), it is automatically committed to main.

Package versions are tracked using `.package-version.json` files that contain the CI run ID and commit SHA. Updates are only performed if a newer version is available.

### Automatic Updates

The [Update Soteria Packages workflow](.github/workflows/update-packages.yml) runs daily at midnight UTC and:
1. Checks the current package version against the latest soteria CI run
2. If an update is available, downloads the macOS ARM64 package artifact
3. Creates a `.package-version.json` file with version metadata
4. Verifies the package contents
5. Commits and pushes changes directly to main

The workflow is skipped if the packages are already up to date.

You can also trigger this workflow manually:
```bash
./scripts/auto-update-packages.sh
```

### Manual Updates

To manually update the packages, use the provided Python script:

```bash
./scripts/local-update-packages.py
```

This script will:
1. Check the current package version against the latest soteria CI run
2. Exit early if packages are already up to date
3. Prompt you to confirm the download if an update is available
4. Download and extract the package to `packages/macos/aarch64/`
5. Create a `.package-version.json` file with version metadata
6. Verify the package structure and contents
7. Show a summary with next steps

**Requirements for CI download mode:**
- GitHub CLI (`gh`) installed and authenticated
- Python 3.7+

**Building from a local soteria checkout:**

If you have a local checkout of the soteria repository and want to build packages from there instead of downloading from CI:

```bash
./scripts/local-update-packages.py --from-dir /path/to/soteria
```

This will:
1. Run `make package-soteria-rust` in the specified directory
2. Copy the built package to `packages/macos/aarch64/`
3. Extract git commit metadata from the local repository
4. Create a `.package-version.json` file (without a CI run ID)
5. Verify the package structure

This is useful for testing local changes to soteria before they're merged upstream.

**After updating:**
1. Test the build: `cargo build --release`
2. Verify functionality: `cargo install --path .` and test with a sample project
3. Commit the changes: `git add packages/ && git commit -m "Update soteria packages"`

## Support

For issues related to:
- **cargo-soteria installation/usage**: Open an issue in this repository
- **soteria-rust analysis**: See [soteria documentation](https://github.com/soteria-tools/soteria)
- **Verification harness APIs**: See [soteria test examples](https://github.com/soteria-tools/soteria/tree/main/soteria-rust/test/cram)

## License

Apache-2.0

## Related Projects

- [soteria](https://github.com/soteria-tools/soteria) — The main soteria verification framework
- [Kani](https://github.com/model-checking/kani) — Rust verification tool (API compatibility)
- [Miri](https://github.com/rust-lang/miri) — Rust interpreter (API compatibility)

