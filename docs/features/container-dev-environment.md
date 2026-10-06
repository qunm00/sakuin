# Plan — Container Dev Environment

> Plan for a Dockerfile that gives a Rust toolchain for Sakuin without
> installing anything on the host, so the toolchain survives across
> sessions (including ones run by agents) and matches what CI runs.
>
> Scope: **a `Dockerfile` only.** No compose file, no Makefile, no named
> volumes. Everything is inside the image.

---

## Goal

- One command produces an image with a working Rust toolchain
  (`cargo`, `rustc`, `rustfmt`, `clippy`) that can build, test, lint,
  and benchmark Sakuin.
- Nothing is installed on or changed about the host machine.
- The image is long-lived: rebuild only when the Dockerfile itself
  changes; otherwise reuse the tagged image between sessions.
- Source code always lives on the host, never only inside the container.

## Non-goals

- A runtime/production image. `sakuin-md` is a **library** crate — it has
  no server or daemon to ship. The only runnable entry point is
  `examples/basic-index.rs`, which is a dev-time thing.
- Reproducing the host's `sqlite3` CLI. The container uses the
  **bundled** SQLite compiled into the crate; a distro `sqlite3` binary
  would be a *different* SQLite version and can mislead (see the
  FTS5 warnings in [full-text-search-options.md](full-text-search-options.md)).
- CI migration. `.github/workflows/ci.yml` stays as-is.

---

## Design

### Base image

`rust:1-bookworm`.

Chosen because it needs **no `apt-get` at all**:

- `rustup` installs the `default` profile, which includes `rustfmt` and
  `clippy`. CI runs both, so they must be present — this is the same trap
  as `--profile minimal` in [development.md](../development.md).
  **Correction (verified 2026-09-23):** the current `rust:1-bookworm`
  image no longer ships those components, so the `Dockerfile` runs
  `rustup component add rustfmt clippy` explicitly. Without it,
  `cargo fmt` and `cargo clippy` both fail with
  "not installed for the toolchain".
- `gcc`/`libc6-dev` are already installed in the `rust` image.
  `rusqlite` with the `bundled` feature compiles SQLite's C amalgamation
  and will fail with `linker cc not found` without them.

`bookworm` over `slim`: `slim` drops the C toolchain needed to link the
bundled SQLite.

### Files in the image

| Path | Contents |
|---|---|
| `/workspace` | `WORKDIR`; the host working copy is mounted here at run time |
| `/usr/local/cargo` | Toolchain + cargo registry (`CARGO_HOME`, from the base image) |

### Layer / cache strategy

1. Copy `Cargo.toml` + `Cargo.lock` **before** the source, then run
   `cargo fetch --locked`.
2. Then copy the rest of the source.

Rationale: dependency downloads land in a layer that is only invalidated
when the manifests change, so editing a `.rs` file never re-downloads the
crate graph. `Cargo.lock` is committed in this repo, so `--locked` works
and the layer stays deterministic.

`cargo fetch` alone (no `cargo build`) is deliberate: it avoids compiling
SQLite before the source is present, and the *compiled* artifacts do not
survive container removal anyway — only the image layers do.

### `ENV`

- `CARGO_TERM_COLOR=always` — matches `ci.yml`.
- `CARGO_INCREMENTAL=0` — incremental artifacts are large, gain little on
  a ~125-test crate, and bloat the bind-mounted `target/`.
- No `PATH` fiddling: the base image already puts `cargo` on `PATH`.

### Entrypoint

`CMD ["bash"]`, plus a `tty`/`stdin_open` note in the docs so `-it` shells
behave normally. No `ENTRYPOINT` — a wrapper script would only get in the
way of running arbitrary commands.

**Use `bash -c`, never `bash -lc`.** A login shell sources
`/etc/profile`, which overwrites `PATH` and hides the toolchain in
`/usr/local/cargo/bin`, so `rustc` comes back as "command not found".

---

## Implementation steps

1. Write `Dockerfile` at the repo root.
2. Write `.dockerignore`:
   ```
   /target
   /.git
   /.pi
   *.db
   ```
   Keeps the build context small and prevents a host `target/` (wrong
   architecture or stale) from shadowing container-built artifacts.
3. Build and tag the image so it persists between sessions:
   ```sh
   docker build -t sakuin-dev .
   ```
4. Verify the toolchain and the full CI gate inside the container:
   ```sh
   docker run --rm -v "$PWD:/workspace" sakuin-dev \
     bash -c 'cargo fmt --all --check \
       && cargo clippy --all-targets -- -D warnings \
       && cargo build --all-targets \
       && cargo test'
   ```
   Use `-v "$PWD:/workspace"` so the bind mount shadows the image's
   `/workspace` copy — otherwise the container would test the snapshot
   baked into the image, not the working copy.
5. Add a "Container workflow" section to
   [development.md](../development.md), documenting: build, run a shell,
   run the CI gate, and run the example. Note that the example takes no
   path argument (it indexes its own cwd) and never exits, so the docs
   must show a `cd` into the mounted vault and tell the reader to stop it
   with Ctrl-C.

---

## Notes and trade-offs

- **First build is still slow** (~20 min) because bundled SQLite is
  compiled from source. A later `docker build` with an unchanged
  Dockerfile is instant (layer cache); only `docker build --no-cache`
  pays it again.
- **Optional mount for build artifacts.** Mounting
  `-v "$PWD/target:/workspace/target"` reuses compiled artifacts across
  container recreation. Left out of the Dockerfile itself because it is a
  run-time flag, but documented as optional.
- **Persistence without a compose file:** the *image* is the cache
  (keep the `sakuin-dev` tag), and the *source* lives on the host via a
  bind mount. Removing the container loses nothing.
- **Host architecture must match the image** (`linux/amd64` vs
  `linux/arm64`), otherwise mounted `target/` artifacts are rejected.
  Docker Desktop's default `--platform` handling covers this; on Apple
  Silicon, prefer not to mount `target/` at all.

## Risks

| Risk | Mitigation |
|---|---|
| Rust version drifts from host/CI | Pin the major tag (`rust:1-bookworm`) and rely on Cargo.lock for dep versions; note that the exact `rustc` patch will differ from the host's. |
| `cargo fetch --locked` fails if `Cargo.lock` is stale | It fails loudly at build time, which is the intent. |
| Volume-mounted `target/` has root-owned files | Container runs as root by default; document `docker run --user "$(id -u):$(id -g)"` if it bites. |

## Status

- [x] Plan written.
- [x] `Dockerfile` implemented.
- [x] `.dockerignore` implemented.
- [x] Verified 2026-09-23 on `linux/arm64`: CI gate green inside the
  container — `cargo fmt --check`, `cargo clippy --all-targets -D warnings`,
  `cargo build --all-targets` (23m), `cargo test` (122 passed, 0 failed,
  plus 1 doctest). Two defects were found and fixed during that run: the
  missing `rustfmt`/`clippy` components, the `bash -lc` `PATH` bug, and
  the example command that passed a path argument it does not accept.
- [x] `docs/development.md` updated.
