# M0 dependency pins

The root workspace dependency catalog follows the block in
[the stack research](research/RUST_STACK.md). Direct registry dependencies use
exact version requirements; abbreviated research versions are expanded to patch
zero. Cargo.lock records the resolved transitive graph.

No dependency is moved outside its researched version range. The research leaves
serde_json at major version 1; M0 pins it to 1.0.151. The TOML requirement =1.1.0
resolves to the published 1.1.0+spec-1.1.0 release; the suffix is build metadata.

Two feature selections make the researched stack explicit and keep CI passing:

- jsonwebtoken remains 11.1.0, but uses aws_lc_rs instead of rust_crypto. The
  rust_crypto feature pulls rsa 0.9.10, which fails cargo-deny's advisory check
  due to [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html).
  The alternative backend is supported by this jsonwebtoken release and already
  used by reqwest's rustls transport. No advisory is suppressed.
- zbus_systemd remains 0.26200.0, with systemd1 and zbus-async-tokio enabled. Its
  zbus dependency has no default backend. The extra feature explicitly selects
  the same Tokio backend as the workspace zbus 5.19.0 dependency.

M0 imports only dependencies needed by its public interfaces or immediate
subsystem scaffolds. The remaining researched entries stay in the shared catalog
for later implementation; Cargo.lock includes dependencies referenced by members.
Internal path dependencies carry an exact 0.1.0 version for cargo-deny's wildcard
policy and future publication. Duplicate transitive versions are warnings as
specified by the brief; advisories, licenses, and unknown sources remain checked.
