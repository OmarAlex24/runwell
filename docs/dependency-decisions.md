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

## M7a workflow analysis

`yaml-rust2` is pinned to `=0.13.0` with default encoding features disabled:
workflow inputs are already UTF-8. This maintained YAML 1.2 implementation exposes
[marked parser events](https://docs.rs/yaml-rust2/0.13.0/yaml_rust2/parser/index.html)
for mappings, sequences, scalars, anchors, aliases, and multiple documents. The
analyzer retains the original text and uses the event locations for diagnostics
and local edits; it never serializes the workflow back through a YAML emitter.
Line/column markers are converted to UTF-8 byte offsets against the original
text, avoiding assumptions about scanner index units across multiline Unicode
scalars.
Scalars stay strings, so the `on` key and Actions expressions are not coerced by
YAML 1.1 boolean rules. Merge keys are resolved for analysis; shared structures
are refused for automatic editing. Flow mappings remain analyzable, with block
insertions refused where locality cannot be established.

The parser is MIT/Apache-2.0 licensed. Cargo-deny checks the pinned parser and its
transitive graph under the workspace's existing advisory/license/source policy;
no parser exceptions are needed. The existing `dirs` dependency of host discovery
pulls `option-ext 0.2.0`, licensed MPL-2.0; a package/version-scoped license
allowance is recorded in deny.toml so the existing workspace graph passes without
broadening the license policy for other packages. `similar =2.7.0` supplies unified textual diffs
without normalizing original line endings. It is Apache-2.0 licensed.

Automatic runner-label inference uses the standard image labels in the
[GitHub-hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
Dynamic expressions and custom larger-runner labels can use the explicit runner
override; this list should be updated when GitHub publishes new image labels.

GitHub's context-availability table excludes `runner` from job-level `env`.
Cache insertions therefore use `${{ github.workspace }}/../.runwell-cache/${{ github.run_id }}-${{ github.run_attempt }}-${{ github.job }}-${{ strategy.job-index || 0 }}`
with a tool-specific suffix. This isolates runs, attempts, jobs, and matrix
indices outside the checkout, using contexts permitted at that location.
It avoids relying on shell expansion inside YAML environment values.
Cache retention on persistent runners remains an operator decision.
