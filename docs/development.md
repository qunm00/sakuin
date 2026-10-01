# Development environment setup

> How to get a working Rust toolchain for Sakuin on a fresh machine or
> container, so any agent (or human) can build, test, and verify changes
> locally instead of deferring to CI.

## TL;DR

```sh
# 1. Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --no-modify-path --profile default
# 2. C toolchain — REQUIRED, see below
sudo apt-get update
sudo apt-get install -y --no-install-recommends gcc libc6-dev
# 3. Verify
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --check && cargo clippy --all-targets -- -D warnings \
  && cargo build --all-targets && cargo test
```

Expect the first `cargo build --all-targets` to take **~20 minutes** — this
crate compiles the bundled SQLite amalgamation plus criterion/proptest.

## Why a C compiler is mandatory

Sakuin depends on `rusqlite` with the `bundled` feature, which compiles
SQLite's C amalgamation from source. `rustup` alone will *install fine and then
fail at build time* with:

```
error: linker `cc` not found
```

Rust is not self-hosting for linking; `cc` is not optional here. Verify with
`command -v cc` before the first build.

## Why `--profile default` and not `minimal`

`--profile minimal` installs `cargo`/`rustc` only. CI runs `cargo fmt --check`
and `cargo clippy`, and those components are *not* in the minimal profile:

```
error: 'cargo-fmt' is not installed for the toolchain ...
error: 'cargo-clippy' is not installed for the toolchain ...
```

Use the default profile, or add them explicitly:

```sh
rustup component add rustfmt clippy
```

## Notes on this machine

Verified working on Ubuntu 26.04, aarch64, with passwordless `sudo`.

- `apt-get update` prints a 403 for the `deb.nodesource.com` repository. It is
  harmless — Ubuntu's own package lists fetch fine, and the error does not stop
  the Rust packages from installing. If it does block you, comment out the
  nodesource line in `/etc/apt/sources.list.d/`.
- `build-essential` may have no installation candidate; `gcc` + `libc6-dev`
  are the actual minimum and do install.
- The system `python3` `sqlite3` module is a **stub**, not real SQLite: it
  lacks `right()` and 5-argument `snippet()`, and its `bm25()` returns
  garbage. Do **not** use it to check FTS5 behaviour. `sqlite3` is not
  installed as a CLI. For anything FTS5-related, write a throwaway
  `tests/zz_probe.rs` and run it under the real bundled SQLite via
  `cargo test --test zz_probe -- --nocapture`.

## Verifying you actually have real SQLite

The bundled version is what Sakuin runs:

```sh
# in tests/zz_probe.rs
println!("{}", rusqlite::version());   // 3.53.2 as of this writing
```

Pin expectations to that version, not to the system SQLite. FTS5 tokenizer
options are easy to get wrong and the error messages are not specific — see
the case-sensitivity note in
[`docs/features/full-text-search-options.md`](features/full-text-search-options.md)
for an option (`unicode61 case_sensitive 1`) that sounds plausible and does
not exist.

## PATH

`rustup` writes to `~/.cargo/bin`. The `--no-modify-path` flag keeps it out of
your shell profile, so each session needs:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

If you want it permanent, drop that line in `~/.bashrc` / `~/.profile` and drop
`--no-modify-path` from the install command.

## Agent notes

- **Verify, do not assume.** An earlier version of this file's subject claimed
  a Rust toolchain was unavailable on this machine. It was installable in
  about two minutes, and compiling immediately disproved a design assumption
  that had been reasoned about without a compiler. Prefer spending the ~20
  minute build over shipping unverified claims.
- **Grep the vendored source.** For FTS5/SQLite behaviour questions, the
  bundled amalgamation is the ground truth and is on disk:
  ```sh
  grep -n "case_sensitive" \
    ~/.cargo/registry/src/*/libsqlite3-sys-*/sqlite3/sqlite3.c
  ```
  This is faster and more reliable than recalling SQLite internals.
- **Run the full CI gate before reporting done** — it is the same four
  commands CI runs, and it catches things unit tests alone do not (the
  `snippet(fts, N, ...)` column index shifted when `path` was added, and only
  the store tests caught it).

## Checklist before considering work done

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --check                        # must be silent
cargo clippy --all-targets -- -D warnings # must be silent
cargo build --all-targets
cargo test                               # 125 tests as of 2026-09-23
```
