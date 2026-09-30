# Contributing to runwell

runwell is pre-alpha. Start with the [architecture](docs/architecture.md),
[ADRs](docs/adr/0001-rust-single-binary.md), and research in `docs/research/`.
Discuss substantial protocol or lifecycle changes before implementation.

## Development setup

Install Rust through rustup. `rust-toolchain.toml` selects stable Rust and the
rustfmt and clippy components. The workspace uses edition 2024 and resolver 3.
Build and check on Linux or macOS; systemd, PSI triggers, and overlay mounts are
Linux-only and must stay behind `#[cfg(target_os = "linux")]`. Running the future
node requires systemd, cgroup v2, PSI, overlayfs, and Docker with its systemd
cgroup driver. KVM is not required.

Build artifacts can be large. Set `CARGO_TARGET_DIR` before every Cargo command
when the system disk has limited space:

```sh
export CARGO_TARGET_DIR=/path/to/external-volume/cargo-targets/runwell
cargo run -p runwell -- --help
```

The example TOML config uses placeholder identities and credential file paths.
Never commit credentials, JIT configs, host details, or private repository names.

## Required checks

With `CARGO_TARGET_DIR` exported, run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace --all-targets
cargo run -p runwell -- --help
```

CI runs formatting, clippy, and tests on Linux, a workspace check on macOS, and
cargo-deny for dependency licenses, advisories, bans, and sources. If cargo-deny
is installed, use `cargo deny check` locally. Keep `Cargo.lock` in version control
because this workspace ships an application. Direct dependency versions are
pinned in the root manifest; review both that manifest and the lockfile when
updating them.

## Code and review

- Write code, docs, errors, and examples in English with generic identities.
- Keep files focused and under 400 lines; split modules by responsibility.
- Document each crate's responsibility and main invariants.
- No unsafe code and no unwrap or expect outside tests.
- Return typed errors for unfinished operations; never use todo or unimplemented
  panic macros as placeholders.
- Keep admission and scheduling pure, and test actual policy behavior when added.
- Preserve idempotency, process-before-ack ordering, DELETE-first runner removal,
  and final-stat sampling before cgroup teardown.
- Explain the change, motivation, relevant tests, and any remaining limitations
  in a pull request. Infrastructure integration tests must be explicit and must
  not contact real GitHub accounts or privileged host services by default.

By submitting a contribution, you agree it may be distributed under either
MIT or Apache-2.0. Participation follows the [code of conduct](CODE_OF_CONDUCT.md).
