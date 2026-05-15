# Stage 1: build cargo-soteria (official Rust image is fine for compiling)
FROM --platform=linux/amd64 rust:1-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo install --path . --bin cargo-soteria

# Stage 2: runtime
# ubuntu:24.04 (Noble) is required: obol needs GLIBC >= 2.39, which bookworm lacks.
FROM --platform=linux/amd64 ubuntu:24.04

# gcc (not build-essential): the Rust toolchain only needs a C compiler/linker
# (cc) to link binaries and build proc-macros/build scripts. build-essential
# additionally pulls g++, make and dpkg-dev (~200 MB) that soteria never uses.
RUN apt-get update && apt-get install -y \
    curl ca-certificates gcc git \
 && rm -rf /var/lib/apt/lists/*

# rustup: obol calls it at setup time to install the required Rust nightly
# toolchain. Bootstrap with NO toolchain (`none`) — installing `stable` here
# would bake a ~1.5 GB toolchain into this layer that we don't need and can't
# reclaim from a later layer. obol installs the pinned nightly itself.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
ENV PATH=/root/.cargo/bin:$PATH

# Minimal profile: the nightly's pinned components (rustc-dev, llvm-tools-preview,
# rust-src, miri) are still installed explicitly via the rust-toolchain pin, but
# the default profile's rust-docs/rustfmt/clippy (unused by soteria) are skipped.
RUN rustup set profile minimal

COPY --from=builder /usr/local/cargo/bin/cargo-soteria /usr/local/bin/cargo-soteria

# Pre-bake the soteria-rust nightly into the image so users never need to run setup.
# GITHUB_TOKEN is mounted as a secret (never baked into the image) and is optional —
# only needed in CI to avoid GitHub API rate-limiting.
#
# `setup` makes obol install the pinned Rust nightly. Once that's done the
# bootstrap `stable` toolchain is dead weight, so drop every non-nightly
# toolchain and make the remaining nightly the default. Done in the same layer
# as setup so the removed files never get baked into an image layer.
RUN --mount=type=secret,id=github_token,required=false \
    TOKEN=$(cat /run/secrets/github_token 2>/dev/null || true) && \
    if [ -n "$TOKEN" ]; then \
        GITHUB_TOKEN="$TOKEN" cargo-soteria setup; \
    else \
        cargo-soteria setup; \
    fi && \
    NIGHTLY="$(rustup toolchain list | sed 's/ (.*)//' | grep '^nightly' | head -n1)" && \
    rustup default "$NIGHTLY" && \
    rustup toolchain list | sed 's/ (.*)//' | grep -v '^nightly' \
        | xargs -r -n1 rustup toolchain uninstall && \
    rustup toolchain list

WORKDIR /workspace
ENTRYPOINT ["cargo-soteria"]
