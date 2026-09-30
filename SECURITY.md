# Security policy

## Status and supported versions

runwell is pre-alpha. Node execution and Docker attribution are under active
development, with no supported production release lines yet. Security fixes
will be developed against the current development branch.

## Threat model

runwell assumes owned, single-tenant Linux hosts and trusted repository authors,
workflow code, actions, dependencies, and pull-request contributors. Cgroups
limit and measure resource consumption. Ephemeral runners and overlay workspaces
reduce state retention; they do not create a hostile-code security boundary.
All jobs share the host kernel.

The daemon runs as root to manage systemd units, cgroups, mounts, and Docker
resources. A daemon compromise can compromise the entire host. Runner processes
should use a dedicated unprivileged account, but workflows with Docker access can
still obtain host-level privileges.

Docker socket access is root-equivalent. The per-job Docker API proxy rewrites
cgroup parents, labels, and socket bind sources to attribute resources. It
attributes rather than isolates. Blocking selected privileged options does not
make the API safe for untrusted workloads. Jobs must not have a direct route to
the host Docker socket; account and filesystem permissions must enforce this.

The in-process proxy adds job/node labels to explicitly created containers,
networks and volumes. Teardown only removes objects carrying both matching
labels, after stopping the proxy; it never prunes shared daemon resources.
Anonymous/implicit volumes without runwell labels and shared image/build caches
are intentionally outside this cleanup. The default per-container memory cap
complements the aggregate slice ceiling; CPU and other user limits are preserved.

`standalone.docker_proxy.deny_host_access` optionally refuses `Privileged=true`
and host PID/network namespaces. It defaults to false for CI compatibility;
these settings are logged either way without logging request bodies. Other
root-equivalent API capabilities remain available, including access to objects
belonging to other jobs. This is an attribution aid, not an authorization layer.

BuildKit upgrade tunnels are passed through as opaque streams. Buildx solves
sent over `/grpc`, and shared BuildKit workers that ignore the `/build` cgroup
option, are not guaranteed to run below the job slice. Do not assume that a
successful proxied build proves full resource attribution. See the
[proxy scope and tests](crates/runwell-dockerproxy/README.md#buildkit-scope).

GitHub App and PAT credentials require administrative runner permissions. At
repository scope, App or fine-grained PAT authentication needs Administration
write permission. Organization runner management needs Self-hosted runners write;
classic PATs use repo or admin:org scopes as appropriate. Enterprise registration
requires an enterprise runner-management PAT; App authentication is not supported
for that scope. A stolen credential can manage runner registrations and may have
broader authority depending on its permissions. Prefer narrowly scoped App
installations and file-based credentials delivered through systemd LoadCredential.

JIT configuration includes private runner credentials. Keep it out of logs, argv,
metrics, reports, and world-readable files. Pass it using
ACTIONS_RUNNER_INPUT_JITCONFIG. Private keys, token caches, and SQLite state need
restricted access, rotation, and backups appropriate to their sensitivity.

**Never use runwell with public repositories that accept fork PRs.** Untrusted
pull requests, actions, or dependency install scripts can compromise the host,
other jobs, caches, and available secrets. A maintainer approval gate does not
turn arbitrary workflow code into a safe workload.

Controller-to-node traffic requires mTLS with authenticated node identities.
Protect the certificate authority, restrict administrative endpoints, and rotate
credentials. mTLS protects the transport; it does not make a compromised node
trustworthy. Warm cache lowers must be immutable while mounted, and cross-job
cache content must be treated as potentially contaminated after a compromised job.

## Reporting vulnerabilities

Use GitHub private vulnerability reporting in this project's repository:
**Security → Advisories → Report a vulnerability**. Include affected versions,
reproduction steps, and impact, without uploading live credentials. Do not post
security vulnerabilities in public issues. Maintainers must enable private
vulnerability reporting on the published repository.
