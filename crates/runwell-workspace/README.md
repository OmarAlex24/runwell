# Warm isolated workspaces

The node prepares a HOME under `<run_dir>/jobs/<id>/home` before registering the
runner. In standalone mode `run_dir` is `workspaces` beside `runners_dir`; cache
storage defaults to the sibling `caches` directory. The runner installation and
its `_work` directory remain M3's independent per-job copies. This milestone
warms HOME, including Go, npm/bun and Playwright's usual caches. Additional
absolute cache mount points are not configured in this implementation.

`Cache` owns generation selection, durable references, promotion and collection.
`Workspace` separates Linux `OverlayWorkspace` from portable `CopyWorkspace`.
One filesystem lock per cache/run root prevents concurrent managers; node
operations run on Tokio's blocking pool; HOME lookup computes the path without
acquiring the cache lock. The daemon must own both roots, stop
all runner processes and containers before harvest, and preserve live jobs
through startup reconciliation. These are trusted-workload caches, not a
security boundary against privileged jobs or root-equivalent Docker access.

## Generations and isolation

Keys are the case-insensitive repository plus an optional class. SHA-256 of the
unambiguous identity forms the directory name. Each key has `gen-N/` trees and
an atomically replaced relative `current` symlink. An overlay always names the
resolved generation, never the symlink. The cache parent is daemon-private;
generation contents retain the job UID and writable permission bits needed by
overlay copy-up, but are immutable by API and inaccessible through their backing
path to the runner. Neither ownership nor file contents of an existing lower
are changed during publication. On a runner UID/GID change, private seeds are
re-copied and re-owned; publication breaks hardlinks before changing ownership.

Upper/work and all backing mounts live under `<run_dir>/upper/<id>`, behind a
root-owned 0700 parent. Overlay upper/work roots stay root-owned. A private
`base/home` tree hardlinks the selected immutable generation (overlayfs does not
traverse nested lower bind mounts). This costs directory-entry I/O rather than
copying warm file data. Those links remain daemon-private; UID changes break
links before re-owning files. A root-owned overlay merges the base and upper,
then only its user-owned `home` child is bound to the public
HOME mountpoint. Thus ownership inside HOME is correct without exposing upper
paths. The node's systemd unit masks the workspace root with a root-owned,
read-only `TemporaryFileSystem` and uses `BindPaths` to reveal only its own HOME.
HOME keeps its original host pathname for Docker bind-source resolution.
`PrivateUsers=true` preserves root/runner UID mappings while separating process
capabilities, so same-UID `/proc/<peer>/root` aliases cannot bypass the mount mask.
Linux hosts must permit systemd to create these per-unit user namespaces.
Systemd cannot nest binds beneath `InaccessiblePaths`; the tmpfs mask provides
that isolation for both overlay and copy backends
([systemd execution namespaces](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html#TemporaryFileSystem=)).

Linux probes an actual mount on the workspace filesystem once on startup.
`redirect_dir=on` permits directory renames, `index=off` avoids lower file-handle
bindings, and `metacopy=off` ensures upper files contain their data. Harvest
consumes device/xattr whiteouts, independent opaque/redirect flags, and relative
redirects resolved against each directory's original lower path.
The semantics follow the [kernel overlayfs documentation](https://docs.kernel.org/filesystems/overlayfs.html).
A rename is built from the job's original lower before applying its upper,
while ordinary changes apply to the newest generation. Generation publication
fsyncs contents and metadata, then renames a new `current` symlink.

The portable fallback reports itself clearly and copies files into an ordinary
writable HOME. It deliberately cannot seed writable files with hardlinks:
a write or chmod on such a link would mutate the shared generation. There is no
portable hardlink copy-on-write primitive on ext4. Hardlinks are used only
between immutable generations; changed files always replace the destination
inode. Fallback harvest computes a delta against its original generation so
concurrent completed jobs retain each other's disjoint cache additions.

## Promotion policy

Defaults are success on a default-branch push, at most once per six hours per
key, at most 20 GiB of logical regular-file data, 1,000,000 entries, depth 64,
and two generations retained. Entry and depth caps also bound empty directory
trees; input is scanned iteratively before recursive copying. The depth setting
accepts 1–256. Final merged candidates are checked before publication.
Failed, cancelled, unknown, fork, and wrong-repository results never promote.
The interval survives restart in generation metadata; clock rollback delays
promotion. A byte, entry-count, or depth-cap violation discards the candidate, leaves `current`
unchanged and warns. Cache verification/build failures warn and skip harvest;
mount teardown failures retain cleanup for retry. When `allow_pull_requests=true`,
successful same-repository PRs publish exclusively into a separate `-pr` key,
with an independent interval and generations. Default-branch jobs never read
that partition. Node jobs seed the default-branch generation because the actual
workflow event is only authenticated after assignment; enabling PR promotion
cannot feed PR changes back into the default-branch cache.

The node requires the journaled JobCompleted success and queries authenticated
GitHub REST workflow-run and repository metadata to establish the actual event,
head repository, branch and default branch. A runner's zero exit and
`job_workflow_ref` are insufficient (reusable workflows have their own refs).
App/PAT credentials need repository metadata and Actions read access in addition
to existing runner permissions ([workflow-run API](https://docs.github.com/en/rest/actions/workflow-runs#get-a-workflow-run),
[repository API](https://docs.github.com/en/rest/repos/repos#get-a-repository)).
Verification failures or incomplete actual-assignment events skip promotion.
Assignment evidence is journaled before acknowledging the event; metadata from
the request that provisioned capacity cannot authorize another job’s harvest.

Repository-scoped GitHub configurations select their repository automatically.
Organization-scoped scale sets cannot force an acquired request onto a specific
runner. Configure `repositories` by class and restrict the GitHub runner group's
repository access accordingly. Otherwise jobs get a cold, non-promoting HOME;
the node never guesses a shared key from an unbound acquisition request.

```toml
[standalone.workspace]
cache_root = "/var/lib/runwell/caches"
per_class = false
promotion_interval_seconds = 21600
max_generation_bytes = 21474836480
max_generation_entries = 1000000
max_generation_depth = 64
keep_generations = 2
allow_pull_requests = false
excludes = ["company/private/*"]

[standalone.workspace.repositories]
runwell-small = "owner/repository"
runwell-large = "owner/repository"
```

## Exclusions

Administrator globs extend mandatory case-insensitive exclusions at every depth;
`*` and `?` match any characters. Defaults exclude `.ssh`, `.gnupg`, `.aws`,
`.azure`, `.kube`, all of `.docker` (including `config.json`), all of `.config`,
Git metadata/config/credentials, `.netrc`, npm/yarn/pip/bun configuration, `.env*`,
`.dockercfg`, `.pgpass`, `.my.cnf`, Terraform configuration/credentials, `.oci`,
`.ansible`, `.mc`, `.vault-token`, `.gem/credentials`, `.config/gh`, `.config/gcloud`,
Cargo credentials/config, Maven settings, Gradle properties, keyrings, shell
startup/history, `*.jks`, `*.keystore`, `*.p8`, `*.p12`, `*.pfx`, `*.kdbx`,
`*.gpg`, `*.asc`, `*.ovpn`, other private-key formats and names containing `token`, `credential`,
`secret`, `password`, `auth`, or `keyring`. The authoritative complete list is
`DEFAULT_EXCLUDES` in `src/excludes.rs`.

Symlinks, devices, sockets, FIFOs, non-UTF8 names and job files with multiple
hardlinks are omitted. This prevents aliases of an excluded credential from
entering a generation. ACLs, capabilities and overlay xattrs are not copied.
Root opens job files with `O_NOFOLLOW|O_NONBLOCK`, verifies regular-file type via
the opened descriptor, and copies from that descriptor. This rejects symlink/FIFO
substitutions without following them or blocking. Tools may regenerate excluded metadata. Path filtering cannot recognize a
secret deliberately copied into an innocently named regular cache file; jobs
must never embed credentials in cache data. No file-content secret scanner is
claimed.

## Restart and collection

A root-private lease is fsynced before mounting or seeding. It records the
backend and resolved lower, so restarted daemons preserve leases even after a
backend/configuration change. GC keeps the newest N generations, current and
all journal/mount-table references. Unknown mounts under this manager's jobs
root are also reconciled. Orphan job directories (including partial upper/work
trees) are removed only when not retained by the node's durable registry.
Completed jobs with pending cleanup are retained long enough to harvest.
GC after publication is best-effort. Damaged keys and undecodable mountinfo lines
warn and are skipped. Corrupt leases warn and are skipped too; private lower
wrapper references continue pinning generations even after a lazy detach. Reconciliation
continues after individual teardown failures, then reports all failed job IDs.

Unmount uses a normal unmount first. EBUSY triggers a warning and lazy detach.
Because detached mounts disappear from mountinfo while references can survive,
the lease, upper/work and generation remain pinned until a different boot ID is
observed. Crash uncertainty during unmount follows the same conservative rule.

Portable tests exercise isolation, switching, restart, exclusions/properties,
whiteouts/opacity/redirects, size limits and reference-counted GC. Ignored Linux
root tests exercise real overlay mounts, unprivileged writes, deletion whiteouts,
directory redirects and stale-mount recovery; CI's `linux-privileged` job includes
this crate. macOS alone cannot validate overlayfs, kernel xattrs, root ownership,
host-namespace mount cleanup, or systemd/GitHub production integration.
