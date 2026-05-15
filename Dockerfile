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

# Install all baked state under shared, world-readable locations instead of
# /root. The container runs as a non-root user (see below); /root is mode 700
# and unreachable, so rustup/cargo/soteria state must live somewhere any UID
# can read. These ENVs persist into the runtime stage so cargo-soteria and
# obol resolve the same paths at run time as at build time.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    SOTERIA_HOME=/opt/soteria \
    PATH=/usr/local/cargo/bin:$PATH

# rustup: obol calls it at setup time to install the required Rust nightly
# toolchain. Bootstrap with NO toolchain (`none`) — installing `stable` here
# would bake a ~1.5 GB toolchain into this layer that we don't need and can't
# reclaim from a later layer. obol installs the pinned nightly itself.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none

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

# Run as an unprivileged user so build artifacts written into a bind-mounted
# /workspace are owned by a normal UID, not root. (A user who needs their host
# UID can still override with `docker run --user $(id -u):$(id -g)`.)
#
# Permissions on the baked state — all must be writable for any UID:
#   - /opt/soteria: soteria-rust compiles its plugin crate on first run,
#     writing Cargo.lock and target/ under plugins/, so this is NOT read-only.
#   - rustup/cargo: obol may invoke rustup/cargo at run time and cargo writes
#     its registry/git cache under CARGO_HOME.
# This is a single-purpose container, so world-writable baked dirs are an
# acceptable trade-off (same as the official rust image's `chmod -R a+w`).
RUN useradd --create-home --shell /bin/bash soteria \
 && chmod -R a+rwX /opt/soteria /usr/local/rustup /usr/local/cargo \
 && mkdir -p /workspace && chown soteria:soteria /workspace

ENV HOME=/home/soteria
USER soteria
WORKDIR /workspace
ENTRYPOINT ["cargo-soteria"]
