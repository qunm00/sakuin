# Container dev environment for Sakuin.
#
# `rust:1-bookworm` (not `slim`) — it already has gcc/libc6-dev, which is
# needed to link the bundled SQLite that `rusqlite` compiles.
FROM rust:1-bookworm

# rustfmt and clippy are NOT in this image's toolchain any more, and CI runs
# both, so add them explicitly. Verified 2026-09-23: without this,
# `cargo fmt`/`cargo clippy` fail with "not installed for the toolchain".
RUN rustup component add rustfmt clippy

# Dependency layer: invalidated only when the manifests change, so editing
# a .rs file never re-downloads the crate graph.
WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
# Cargo refuses to even parse a manifest with no targets, so drop in
# placeholder sources. They are overwritten by the COPY . . below.
RUN mkdir -p src benches \
    && : > src/lib.rs \
    && : > benches/bench_index.rs \
    && cargo fetch --locked

COPY . .

ENV CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=0

CMD ["bash"]
