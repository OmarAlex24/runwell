# GitHub Actions Runner Scale Set Protocol — Implementation Spec (for a Rust re-implementation)

Status: research spec, derived from source on 2026-09-30. Target: a single-binary Rust daemon on one Linux VPS
that replaces `actions/scaleset` (client + listener) **and** the parts of ARC that manage ephemeral runners.

## 0. Sources and pinned revisions

All `file:line` references are relative to these checkouts under `scratchpad/refs/`:

| Tag in this doc | Repo | Revision |
|---|---|---|
| `SS:` | `actions/scaleset` | `e6daac7` (2026-09-16, HEAD of main) |
| `SS@9b28:` | `actions/scaleset` | `9b28032` (2026-07-06) — **the version ARC currently pins** (`actions-runner-controller/go.mod:9`) |
| `ARC:` | `actions/actions-runner-controller` | `e14c5a0` (2026-09-30, HEAD of master) |
| `ARC012:` | ARC tag `gha-runner-scale-set-0.12.1` (extracted to `arc-0.12.1/`) — last release with ARC's *own* client/listener, before it moved to `actions/scaleset` |
| `RUN:` | `actions/runner` | `ca43437` (runner version `2.337.0`, `runner/src/runnerversion`) |
| `RHTTP:` | `hashicorp/go-retryablehttp` | `v0.7.8` (the version in `SS: go.mod`) — governs *all* retry behaviour of the Go client |

Important history (from `git log` of scaleset):
- 2026-02-05 `63a0a32` "It's alive!" — public preview. **No `JobAvailable`, no `acquirejobs`.** Assignment was fully server-side.
- 2026-04-14 `22bae12` "Restore job acquisition flow (#90)" — `JobAvailable` messages + `POST .../acquirejobs` came back. They are required now (see §4).
- 2026-09-15 `fb56300` (#113) — listener API changed: ack moved to **after** processing. ARC has not picked this up yet (ARC pins `9b28032`, which acks **before** processing).

---

## 1. Authentication

### 1.1 Config URL → scope
`SS: config.go:32-73`. Trim `/`, split path:
- 1 segment → **organization** (`https://github.com/<org>`)
- 2 segments, first == `enterprises` (case-insensitive) → **enterprise**
- 2 segments otherwise → **repository** (`https://github.com/<owner>/<repo>`)
- anything else → error.

GitHub REST API base (`SS: config.go:75-109`):
- Hosted (`github.com`, `www.github.com`, `github.localhost`, `*.ghe.com`): `https://api.<host>` (and `www.github.com` → `api.github.com`).
- Otherwise (GHES): `https://<host>/api/v3`. Env var `GITHUB_ACTIONS_FORCE_GHES` (presence) forces GHES mode.

### 1.2 Three-step token exchange (credential → registration token → Actions-service admin token)

The Actions service ("pipelines", `*.actions.githubusercontent.com`) is **not** called with the GitHub credential.
The client performs a chain (`SS: client.go:1061-1109 updateTokenIfNeeded`):

**Step A (GitHub App only) — installation access token**
`SS: client.go:899-932`, JWT: `SS: jwt_provider.go:93-102`
```
POST {api}/app/installations/{installation_id}/access_tokens
Authorization: Bearer <app JWT>
Content-Type: application/vnd.github+json
→ 201 {"token":"ghs_...","expires_at":"2026-..Z", ...}
```
App JWT: RS256, claims `iss=<client_id or app_id>`, `iat=now-60s`, `exp=iat+9min` (i.e. now+8min).
Note: the Go client fetches a **new** installation token every time it refreshes the admin token (no caching); that is fine
because it only happens ~hourly.

**Step B — runner registration token** (`SS: client.go:845-891`, path `SS: client.go:1020-1037`)
```
POST {api}/repos/{owner}/{repo}/actions/runners/registration-token   (repo scope)
POST {api}/orgs/{org}/actions/runners/registration-token             (org scope)
POST {api}/enterprises/{ent}/actions/runners/registration-token      (enterprise scope)
Authorization: Bearer <PAT or installation token>
Content-Type: application/vnd.github.v3+json
(empty body)
→ 201 {"token":"AABC...","expires_at":"..."}
```
Any status other than 201 is an error.

**Step C — Actions service admin connection** (`SS: client.go:934-1018`)
```
POST {api}/actions/runner-registration
Authorization: RemoteAuth <registration token>      <-- literal scheme "RemoteAuth"
Content-Type: application/json
{"url":"<the config URL exactly as given, e.g. https://github.com/my-org>","runner_event":"register"}
→ 2xx {"url":"https://pipelinesghubeus<N>.actions.githubusercontent.com/<tenant-id>/","token":"<JWT>"}
```
- JSON must be encoded without HTML escaping (`SetEscapeHTML(false)`, `client.go:951-952`).
- Both `url` and `token` must be non-empty (`client.go:1010-1015`).
- **This call is retried on 401 and 403** in addition to the default retry policy (`client.go:982-989`),
  because a freshly minted registration token can take a moment to propagate. ARC 0.12.1 did the same with up to 5
  retries and exponential backoff (`ARC012: github/actions/client.go:1136-1160`).

**Admin token lifetime.** The `token` is a JWT; its `exp` claim is read **without signature verification**
(`SS: client.go:1045-1059`). The token is considered valid until `now + 60s > exp` (`SS: client.go:1120`).
Refresh is lazy: every Actions-service request calls `updateTokenIfNeeded` first (`client.go:313-334`), double-checked
under a mutex so concurrent callers trigger one refresh (`client.go:1061-1071`).

**Actions-service request shape** (`SS: client.go:313-368`):
```
{admin.url joined with path}?api-version=6.0-preview      (added unless already present)
Authorization: Bearer <admin token>
Content-Type: application/json
User-Agent: <JSON blob, see 1.4>
```
`joinURLPath` trims trailing `/` from base and ensures one `/` (`client.go:370-387`). Query params from the path and the
explicit query are merged (`client.go:343-368`).

> **Gap in upstream (fix in Rust):** a 401 from an Actions-service admin endpoint does **not** trigger a token refresh;
> the client keeps using the cached token until 60s before `exp`. If the admin token is revoked early, every call fails
> until natural expiry. The Rust client should invalidate the cached admin token on 401 and retry once.

### 1.3 Required permissions

From GitHub docs ("Authenticating ARC to the GitHub API", fetched 2026-09-30), confirmed consistent with the endpoints above:

| Scope | GitHub App | Classic PAT | Fine-grained PAT |
|---|---|---|---|
| Repository | Repo perms: **Administration: RW**, **Metadata: R** ("Administration RW is only required ... at the repository scope") | `repo` | **Administration: RW** |
| Organization | Repo perm **Metadata: R**; Org perm **Self-hosted runners: RW** | `admin:org` | Org **Self-hosted runners: RW** + **Administration: R** |
| Enterprise | **Not supported** ("You cannot authenticate using a GitHub App for runners at the enterprise level") | `manage_runners:enterprise` (docs are thin here; this is the scope the registration-token endpoint requires) | n/a |

The permissions are needed for Step B (registration-token). Everything after Step C is authorized by the admin token /
message-queue token, not by the GitHub credential.

### 1.4 User-Agent (telemetry the service sees)
`SS: common_client.go:165-173`, `client.go:290-295`. The User-Agent header is a **JSON object**:
```json
{"system":"my-engine","version":"0.1.0","commit_sha":"abc","scale_set_id":42,"subsystem":"listener",
 "build_version":"<lib version>","build_commit_sha":"<lib sha>","kind":"scaleset"}
```
ARC sets `system:"actions-runner-controller"`, `subsystem:"ghalistener"` (`ARC: cmd/ghalistener/config/config.go:144-151`).
Not functionally required, but copy the shape (GitHub uses it for support/diagnostics).

---

## 2. Scale set CRUD

Endpoint constants: `SS: client.go:23-26`
```
scaleSetEndpoint = "_apis/runtime/runnerscalesets"
runnerEndpoint   = "_apis/distributedtask/pools/0/agents"
```
All below are Actions-service requests (admin token, `api-version=6.0-preview`).

| Operation | Method + path | Success | Source |
|---|---|---|---|
| Get by name in group | `GET _apis/runtime/runnerscalesets?runnerGroupId={gid}&name={name}` | 200 list; `count==0` → none; `>1` → error | `SS: client.go:395-429` |
| List in group | `GET _apis/runtime/runnerscalesets?runnerGroupId={gid}` | 200 list | `client.go:431-457` |
| Get by id | `GET _apis/runtime/runnerscalesets/{id}` | 200 object; 404 → gone | `client.go:459-482`; ARC treats `NotFoundError` as "scale set deleted" (`ARC: controllers/actions.github.com/autoscalingrunnerset_controller.go:1337-1356`) |
| Create | `POST _apis/runtime/runnerscalesets` | **200** (not 201) | `client.go:540-572` |
| Update | `PATCH _apis/runtime/runnerscalesets/{id}` | 200 | `client.go:574-604` |
| Delete | `DELETE _apis/runtime/runnerscalesets/{id}` | **204** | `client.go:606-625` |
| Runner group by name | `GET _apis/runtime/runnergroups/?groupName={name}` | 200 list, must be exactly 1 | `client.go:484-514` |

List envelope (`SS: types.go:114-117`, `76-79`): `{"count": N, "value": [ ... ]}`.

**RunnerScaleSet JSON** (`SS: types.go:81-91`):
```json
{
  "id": 42,                               // omit on create
  "name": "my-scale-set",
  "runnerGroupId": 1,
  "runnerGroupName": "Default",           // response only
  "labels": [{"type":"System","name":"my-scale-set"}, {"type":"System","name":"linux-x64"}],
  "RunnerSetting": {"disableUpdate": true},   // NOTE capital "R" in the key — legacy quirk, copy exactly
  "createdOn": "0001-01-01T00:00:00Z",        // Go always serializes this (no omitempty)
  "runnerJitConfigUrl": "...",                // response only
  "statistics": { ...RunnerScaleSetStatistic... }  // response only
}
```
Rules:
- If `labels` is empty on create, the client injects `[{name: <scale set name>, type:"System"}]`; name or ≥1 label is
  required (`SS: client.go:527-538`). Any label with empty `type` gets `"System"` (`client.go:516-525`), on create **and** update.
- ARC always puts the scale-set name as the first label, de-duplicates extra labels, and **always sets
  `disableUpdate: true`** (`ARC: autoscalingrunnerset_controller.go:1043-1080`). Workflows target the scale set by
  `runs-on: <any label>`.
- GHES: multi-label requires GHES ≥3.18 and feature flag `DistributedTask.AllowRunnerScaleSetCustomLabels`
  (default on from 3.21); otherwise extra labels are **silently dropped** (`SS: README.md:187-200`).
- Names are unique **within a runner group** (`client.go:540`). Default group: ARC and the example hard-code
  `runnerGroupID = 1` for the default group without a lookup (`ARC: autoscalingrunnerset_controller.go:1022`,
  `SS: examples/dockerscaleset/main.go:65-75`).
- Idempotent create pattern used by ARC: `GET by name` → if nil `POST create` → persist ID
  (`ARC: autoscalingrunnerset_controller.go:1033-1111`).
- Update is partial: ARC PATCHes `{"runnerGroupId": N}` or `{"name": "..."}` alone
  (`ARC: autoscalingrunnerset_controller.go:1141, 1178`). Beware: Go omits zero-valued `id/name/runnerGroupId/labels`
  (`omitempty`) but always sends `RunnerSetting` and `createdOn`; mirror this with `#[serde(skip_serializing_if)]`.
- Delete: ARC treats 404 as success (`ARC: autoscalingrunnerset_controller.go:1239-1248`).

**Runner group JSON** (`SS: types.go:69-74`): `{"id":1,"name":"Default","size":0,"isDefaultGroup":true}`.

---

## 3. Message session and polling

### 3.1 Create session
`SS: session_client.go:44-71`, invoked by `Client.MessageSessionClient` (`SS: client.go:698-723`).
```
POST _apis/runtime/runnerscalesets/{scaleSetId}/sessions?api-version=6.0-preview
Authorization: Bearer <admin token>
{"ownerName":"<hostname>"}
→ 200
{
  "sessionId": "uuid",
  "ownerName": "vps-01",
  "runnerScaleSet": { ...RunnerScaleSet... },
  "messageQueueUrl": "https://.../<queue path>",
  "messageQueueAccessToken": "<opaque/JWT>",
  "statistics": { ...RunnerScaleSetStatistic... }
}
```
(`SS: types.go:124-131`). `ownerName` is the host name (fallback: random UUID) in both ARC and the example
(`ARC: cmd/ghalistener/main.go:68-72`).

**The session's `statistics` is the initial scaling signal.** Both listeners run a scaling pass with it before the
first poll and fail if it is nil (`SS: listener/listener.go:127-149`; `SS@9b28: listener/listener.go:146-169`).
New listener passes it as a synthetic message with `MessageID = -1` (`SS: listener/listener.go:96-99`).

### 3.2 Session conflict (409) — only one listener per scale set

Only one active session per scale set is allowed. A second `POST .../sessions` while a session is alive returns
**409 Conflict** (the runner's analogous exception is `TaskAgentSessionConflictException`,
`RUN: src/Sdk/WebApi/WebApi/BrokerHttpClient.cs:162-165`).

- **Current `actions/scaleset`: no retry.** 409 surfaces as `ConflictError` (`SS: errors.go:27, 105-117`) and
  `MessageSessionClient` returns an error. ARC's listener process then exits 1 (`ARC: cmd/ghalistener/main.go:36-39`)
  and the controller deletes/recreates the dead pod (`ARC: controllers/actions.github.com/autoscalinglistener_controller.go:528-531, 979-986`),
  i.e. retry-by-restart.
- **ARC ≤0.12.1 explicitly retried 409: up to 10 attempts, 30 s apart** (`ARC012: cmd/ghalistener/listener/listener.go:18-20, 225-257`).
- The runner itself retries session conflicts every 30 s for up to 4 min (`RUN: src/Runner.Listener/MessageListener.cs:52-53`).

The typical cause is your own previous process crashing without `DELETE session`; the server expires the orphan
after an unspecified time (not visible in any client source — **empirically measure**; the 4-min/5-min figures above
are the budgets clients use).

**Rust recommendation:** on 409, retry every 30 s with jitter for up to ~5 min, then crash-loop via systemd. Always
`DELETE` the session on graceful shutdown (SIGTERM) with a 30 s deadline (ARC012 used 30 s: `listener.go:441-451`).
Hold a local lockfile/flock so two daemons on the same host never race for the session.

### 3.3 Delete session
```
DELETE _apis/runtime/runnerscalesets/{scaleSetId}/sessions/{sessionId}?api-version=6.0-preview   (admin token)
→ 204
```
`SS: session_client.go:73-77`, `Close()` at `38-42`.

### 3.4 Refresh session (message-queue token expired)
```
PATCH _apis/runtime/runnerscalesets/{scaleSetId}/sessions/{sessionId}?api-version=6.0-preview   (admin token, no body)
→ 200 <full RunnerScaleSetSession, with new messageQueueUrl + messageQueueAccessToken>
```
`SS: session_client.go:79-98`. Trigger: **any 401** from GetMessage / DeleteMessage / AcquireJobs is mapped to
`MessageQueueTokenExpiredError` (`session_client.go:170-171, 226-230, 283-285`). The client refreshes **once** and
retries the call **once** (`session_client.go:103-129, 181-197, 244-260`). Concurrent refreshes are de-duplicated: if the
stored session already differs from the one that failed, skip the PATCH (`session_client.go:82-88`).
Replace the whole session (URL can change too).

### 3.5 Get message (long poll)
`SS: session_client.go:131-176`
```
GET {messageQueueUrl}[?lastMessageId={id}]            (only added when id > 0)
Accept: application/json; api-version=6.0-preview
Authorization: Bearer <messageQueueAccessToken>       <-- NOT the admin token
User-Agent: <json>
X-ScaleSetMaxCapacity: <int>                          <-- required: max runners you can produce right now
```
Responses:
| Status | Meaning |
|---|---|
| **202 Accepted** | Long poll timed out (~50 s server-side) with nothing to deliver → `None`. Poll again immediately. (`SS: README.md:91-99`) |
| **200 OK** | A message (below) |
| **401** | Queue token expired → refresh session (3.4) and retry once |
| anything else | error (after transport-level retries, §3.9) |

HTTP client timeout is 5 min (`SS: common_client.go:105-107`) — must be > the ~50 s long poll. Use a per-request
timeout of ~2–5 min for this call specifically.

**Message envelope** (`SS: types.go:98-103`):
```json
{
  "messageId": 1234,
  "messageType": "RunnerScaleSetJobMessages",
  "body": "[{\"messageType\":\"JobAvailable\", ...}, {...}]",   // a JSON *string* containing a JSON array
  "statistics": { ...RunnerScaleSetStatistic... }
}
```
- `messageType` other than `RunnerScaleSetJobMessages` is a hard error in the lib (`SS: client.go:633-635`).
  **Rust:** log + ack + continue rather than crash, so an unknown future message type cannot wedge the queue.
- `body` is double-encoded; may be empty (`client.go:642-647`).
- A body carries **at most 50** job messages; backlogs are truncated (`SS: README.md:44-47`). Hence never derive
  demand by counting messages.
- The response body may start with a UTF-8 BOM; strip `EF BB BF` before JSON parsing (`SS: common_client.go:72`, `client.go:1039-1043`). This applies to **every** Actions-service response.
- Message IDs start at 0 on the server (`SS: listener/listener.go:96-99`); the `lastMessageId` param is only sent when > 0.

**Statistics** (`SS: types.go:133-141`):
```json
{"totalAvailableJobs":0,"totalAcquiredJobs":0,"totalAssignedJobs":3,"totalRunningJobs":1,
 "totalRegisteredRunners":4,"totalBusyRunners":1,"totalIdleRunners":3}
```
**Scaling signal = `totalAssignedJobs`** (jobs waiting for a runner + jobs running; `>= totalRunningJobs`)
(`SS: README.md:40-49`). Desired runners = `min(maxRunners, minRunners + totalAssignedJobs)`
(`ARC: cmd/ghalistener/scaler/scaler.go:278`; `SS: examples/dockerscaleset/scaler.go:70`).

**Job messages** — common base (`SS: types.go:47-62`):
```json
{
  "messageType": "JobAvailable|JobAssigned|JobStarted|JobCompleted",
  "runnerRequestId": 987654321,        // int64 — the key used for acquirejobs
  "repositoryName": "repo",
  "ownerName": "org",
  "jobId": "uuid-string",
  "jobWorkflowRef": "org/repo/.github/workflows/ci.yml@refs/heads/main",
  "jobDisplayName": "build (ubuntu)",
  "workflowRunId": 123,
  "eventName": "push",
  "requestLabels": ["my-scale-set"],
  "queueTime": "...", "scaleSetAssignTime": "...", "runnerAssignTime": "...", "finishTime": "..."
}
```
Type-specific extra fields (`SS: types.go:21-41`):
- `JobAvailable`: `acquireJobUrl` (string). **Must be acquired** (§4).
- `JobAssigned`: none. Informational; ARC just logs it (`ARC012: listener.go:367-373`).
- `JobStarted`: `runnerId` (int), `runnerName` (string). Use to mark that runner busy (ARC patches the EphemeralRunner
  status so it is not scaled down: `ARC: cmd/ghalistener/scaler/scaler.go:127-195`).
- `JobCompleted`: `result` (string: e.g. `succeeded`, `failed`, `canceled`), `runnerId`, `runnerName`.
  `runnerId`/`runnerName` can be empty/0 when the job was cancelled before a runner took it.
- Unknown `messageType` values inside the body are ignored (`SS: client.go:688`).
- Timestamps are RFC3339; zero values may appear as `0001-01-01T00:00:00` — parse leniently (`Option<DateTime>`).

### 3.6 Ack / delete message
`SS: session_client.go:199-231`
```
DELETE {messageQueueUrl}/{messageId}                  (path append, query preserved)
Authorization: Bearer <messageQueueAccessToken>
Content-Type: application/json
→ 204 ; 401 → refresh+retry once
```
Unacked messages are redelivered on the next poll (`SS: README.md:101-110`).

**Ack ordering differs between versions — this matters for lost jobs:**
- `SS@9b28` (what ARC runs today): **ack first**, then acquire, then handle JobStarted/Completed, then scale
  (`SS@9b28: listener/listener.go:207-240`). ARC012 did: parse → acquire → ack → handle (`ARC012: listener.go:184-223`).
  A crash after ack loses the job messages; ARC tolerates this because it scales on `statistics` only.
- `SS` HEAD: **process first, ack only after `Scale()` succeeds**; ack uses a non-cancellable context
  (`SS: listener/listener.go:101-124, 169-180`). A failure stops the listener without ack → redelivery → handler must be
  idempotent.

**Rust recommendation:** process → ack (the HEAD semantics), with every step idempotent: acquirejobs is safe to repeat
("acquiring an already acquired job is a no-op", `SS: examples/dockerscaleset/scaler.go:33-35`), JobStarted/Completed
handlers keyed by `runnerRequestId`/`runnerName`, and scaling driven by absolute `totalAssignedJobs`.

### 3.7 Poll loop (reference behaviour)
`SS: listener/listener.go:126-182`:
```
scale(session.statistics)                       # initial pass
last = 0
loop:
  msg = get_message(last, max_capacity)         # 202 → None
  scale(msg)                                    # on None: re-converge using cached last statistics
  if msg: last = msg.id ; delete_message(msg.id)
```
The pinned version re-runs `HandleDesiredRunnerCount(latestStatistics.TotalAssignedJobs)` on every 202 so the
scaler converges even when idle (`SS@9b28: listener/listener.go:189-196`). `X-ScaleSetMaxCapacity` is re-read every poll
and can be changed at runtime (`SS: listener/listener.go:70-74`) — lower it when the VPS is resource-constrained so the
backend stops over-assigning to you (`SS: README.md:49`).

### 3.8 Error body format and typed errors
`SS: errors.go:30-118`. Actions-service error bodies are either `text/plain` or
```json
{"typeName":"GitHub.DistributedTask.WebApi.AgentExistsException, ...","message":"..."}
```
Match by **substring** on `typeName`:
| substring | meaning | observed status (tests) |
|---|---|---|
| `AgentExistsException` | runner name already registered (JIT) | 409 (`SS: errors_test.go:159-162`) |
| `AgentNotFoundException` | runner id unknown | 404 (`errors_test.go:176-182`) |
| `JobStillRunningException` | cannot remove: runner is executing a job | 409 (`errors_test.go:198-201`) |

Status → sentinel: 400 BadRequest, 401 Unauthorized, 404 NotFound, 409 Conflict (`errors.go:105-117`).
Always log `ActivityId` (Actions service) and `X-GitHub-Request-Id` (GitHub API) response headers (`common_client.go:17-20`, `errors.go:57-63`) — GitHub support needs them.

### 3.9 Transport retries (applies to every call above)
The Go client wraps everything in `go-retryablehttp` with `RetryMax=4`, `RetryWaitMin=1s`, `RetryWaitMax=30s`
(`SS: client.go:246-253`, `common_client.go:95-108`, `RHTTP: client.go:49-51`):
- Retries on: any transport error except TLS-cert/redirect/scheme/header errors (`RHTTP: client.go:495-522`);
  **429**; any **5xx except 501**; status 0 (`RHTTP: client.go:524-540`). Does **not** retry other 4xx.
- Retries **all methods, including POST** (generatejitconfig, create scale set, acquirejobs) — see §4.3 for the
  consequence.
- Backoff: `min * 2^attempt` capped at 30 s; for **429 and 503** honour `Retry-After` (seconds or HTTP-date), **uncapped**
  (`RHTTP: client.go:551-566, 578-600`).
- After exhausting retries the default handler **drops the response** and returns
  `"<METHOD> <url> giving up after N attempt(s)"` (`RHTTP: client.go:815-842`) — the status code is lost. Only the
  runner-registration call overrides this to keep the response (`SS: client.go:990-993`).
- Context cancellation stops retries (`RHTTP: client.go:472-481`).

Summary of status handling for the Rust engine:

| Status | Admin endpoints | Message queue (get/delete/acquire) | Session create |
|---|---|---|---|
| 401 | (upstream: fail) → **Rust: drop admin token, re-exchange, retry once** | refresh session (PATCH), retry once | re-exchange admin token, retry once |
| 403 | fail (permissions) — except runner-registration: retry | fail | fail |
| 404 | NotFound (scale set / runner / session gone) | session gone → recreate session | scale set deleted → recreate or stop |
| 409 | Conflict: AgentExists / JobStillRunning | — | session conflict → retry 30 s up to ~5 min |
| 429 | retry with Retry-After | same | same |
| 5xx (≠501), network | exp backoff 1→30 s, 4 retries, then bubble up | same; then restart loop with backoff | same |

Upstream has no handling for "session deleted server-side" (404 on get message). Rust: on 404 from the queue or
PATCH, create a fresh session.

---

## 4. Job acquisition, JIT config, runner removal

### 4.1 `acquirejobs` (present and required since 2026-04)
`SS: session_client.go:242-297`
```
POST {admin.url}/_apis/runtime/runnerscalesets/{scaleSetId}/acquirejobs?api-version=6.0-preview
Authorization: Bearer <messageQueueAccessToken>       <-- queue token, NOT admin token (session_client.go:275)
Content-Type: application/json
[987654321, 987654322]                                <-- bare JSON array of runnerRequestId (int64)
→ 200 {"count":2,"value":[987654321,987654322]}       <-- ids actually acquired (types.go:119-122)
401 → refresh session, retry once
```
Semantics (`SS: listener/listener.go:111-113`): *"Every JobAvailable the implementation wants must be passed to
Client.AcquireJobs, or the job stays unassigned."* Acquiring an already-acquired job is a no-op
(`SS: examples/dockerscaleset/scaler.go:33-35`). The returned list may be a subset (another scale set with the same
label may win; or the job was cancelled). Acquisition moves the job into this scale set's `totalAssignedJobs`.

How assignment works end-to-end:
1. Job queued with `runs-on` matching one of your labels + runner-group policy allows the repo.
2. Service emits `JobAvailable` to matching scale sets' sessions (bounded by `X-ScaleSetMaxCapacity`).
3. Listener calls `acquirejobs` → job is now assigned to **the scale set** (emits `JobAssigned`, `totalAssignedJobs++`).
4. Any **idle registered runner** of that scale set gets the job (the runner process, via its own broker session,
   receives it). You don't choose which runner.
5. `JobStarted` (runnerId/runnerName) → … → `JobCompleted` (result).
6. If no runner of the scale set picks it up in time, the service cancels the assignment and requeues:
   `JobAssigned` followed by `JobCompleted{result:"canceled"}`, up to **3 times with increasing delays**
   (`SS: README.md:112-116`). A job is only really lost if all attempts time out.

Historical endpoint (ARC ≤0.12.1, not in the current lib; unverified whether still served):
`GET _apis/runtime/runnerscalesets/{id}/acquirablejobs` (admin token) → 200 `{"count":N,"value":[{acquireJobUrl,messageType,runnerRequestId,repositoryName,ownerName,jobWorkflowRef,eventName,requestLabels}]}` or 204 none
(`ARC012: github/actions/client.go:809-842`, `types.go:9-23`). Useful as a recovery probe if you suspect an acked-but-unacquired `JobAvailable` — test before relying on it.

### 4.2 Generate JIT runner config
`SS: client.go:725-756`
```
POST _apis/runtime/runnerscalesets/{scaleSetId}/generatejitconfig?api-version=6.0-preview   (admin token)
{"name":"runner-7f3a9c21","workFolder":"_work"}      // types.go:93-96; workFolder "" = default
→ 200
{
  "runner": {"id": 1234, "name": "runner-7f3a9c21", "runnerScaleSetId": 42},
  "encodedJITConfig": "<base64>"
}
```
(`SS: types.go:152-161`). The runner is **registered server-side at this moment** (it counts in
`totalRegisteredRunners` and is eligible for jobs as soon as its process connects). Persist `runner.id` + `name`
**before** starting the process (ARC stores `jitToken`, `runnerName`, `runnerId` in a Secret: `ARC: controllers/actions.github.com/resourcebuilder.go:955-963`).
Treat `encodedJITConfig` as a secret (it contains the runner's RSA private key).

`encodedJITConfig` = base64(JSON map `{ "<file name>": "<base64 file content>" }`). The runner writes each entry into
its **root directory** (`.runner`, `.credentials`, `.credentials_rsaparams`) (`RUN: src/Runner.Listener/Runner.cs:234-266`).
`.runner` deserializes to `RunnerSettings` with `AgentId, AgentName, PoolId, PoolName, DisableUpdate, Ephemeral,
ServerUrl, GitHubUrl, WorkFolder, UseV2Flow, UseRunnerAdminFlow, ServerUrlV2` (`RUN: src/Runner.Common/ConfigurationStore.cs:21-60`).
Consequence: **each concurrently running runner needs its own copy of the runner install directory** (or at least its
own root with `bin/` and `externals/` symlinked); two runners cannot share a root.

### 4.3 Name collisions / POST retry duplicates (`AgentExistsException`)
Because transport retries re-POST `generatejitconfig`, a request that succeeded server-side but timed out client-side
comes back as `409 AgentExistsException` on retry. ARC's recovery (`ARC: controllers/actions.github.com/ephemeralrunner_controller.go:831-891`):
1. `GET _apis/distributedtask/pools/0/agents?agentName={name}` (`SS: client.go:785-816`) → `{"count":N,"value":[RunnerReference]}`.
2. If none → retry generation.
3. If found and `runnerScaleSetId == ours` → `DELETE .../agents/{id}` then retry generation.
4. If it belongs to another scale set → fatal for that name; pick a new name.
**Rust:** use unique random names (e.g. `<scaleset>-<8 hex>`), so path 4 never happens, and implement 1–3.

### 4.4 Runner lookup / removal
```
GET    _apis/distributedtask/pools/0/agents/{runnerId}             → 200 RunnerReference   (SS: client.go:758-783)
GET    _apis/distributedtask/pools/0/agents?agentName={name}       → 200 list               (client.go:785-816)
DELETE _apis/distributedtask/pools/0/agents/{runnerId}             → 204                    (client.go:818-838)
```
`RunnerReference` = `{"id":1234,"name":"...","runnerScaleSetId":42}` (`SS: types.go:152-156`). Pool id is literally `0`
(the service resolves it).

`DELETE agent` outcomes (as ARC interprets them):
- **204** — removed. The runner can no longer be assigned a job; its process will exit on its own (it loses its
  session/credentials; the runner exits 0 on `TaskAgentAccessTokenExpiredException`, `RUN: Runner.cs:890-897`).
- **409 `JobStillRunningException`** — the runner is **executing a job**; do not kill it. Retry later
  (ARC: 30 s, `ARC: controllers/actions.github.com/runner_unregistration.go:33-35, 309-312`).
- **404 / `AgentNotFoundException`** — already gone; treat as success (`runner_unregistration.go:305-307`).
- Other errors — ARC logs and **leaves it for the service** to garbage-collect (`runner_unregistration.go:268-278, 316-318`).

### 4.5 The safe scale-down / kill protocol (the core invariant)
ARC never kills a live runner process first. It asks the service to remove the registration and uses the response as
an atomic "is it busy?" check (`ARC: controllers/actions.github.com/ephemeralrunnerset_controller.go:1075-1180`,
`ephemeralrunner_controller.go:1181-1221`):
```
for runner in idle_candidates (not busy per JobStarted, has runnerId):
    match DELETE agent/{id}:
        204 | 404        -> kill process, cleanup dir            # it can no longer receive a job
        409 JobStillRunning -> skip (it just got a job)          # race lost, keep it
        other error       -> skip, retry next reconcile
```
Runners that have not yet recorded their ID are never touched by scale-down (`ephemeralrunnerset_controller.go:1093-1096`).
Scale-down only happens when the listener publishes the "idle at minimum" state (PatchID 0 in ARC,
`ephemeralrunnerset_controller.go:362-381`); otherwise runners are left to exit after their single job
(`SS: examples/dockerscaleset/scaler.go:93-99` explains why scale-down is unnecessary in the JIT-per-job model).

### 4.6 Stale / offline runner detection and cleanup
- Service-side GC (GitHub docs, "Removing self-hosted runners"): a runner not connected for **14 days** is auto-removed;
  an **ephemeral** runner (all JIT scale-set runners are ephemeral) is auto-removed after **1 day** not connected.
- ARC's unregistration queue relies on this GC for anything it fails to delete (`runner_unregistration.go:268-278`).
- The scale-set API exposes **no list-runners endpoint** in the lib; you can only look up by id or by name. So the Rust
  engine must keep a **durable local registry** `{runnerId, name, pid, dir, state, createdAt, jobRequestId}` (SQLite or
  a JSON file fsynced on each transition) and reconcile it at startup:
  1. For each registry entry whose process is not alive: `DELETE agent/{id}` (404 ok, 409 → it is still running
     somewhere?! log loudly), then remove its dir.
  2. For each live process not in the registry: kill it (it can't be accounted for).
- Optionally cross-check with the public REST API (`GET /orgs/{org}/actions/runners?per_page=100` — lists scale-set
  runners with `status: online|offline` and `busy`) to find orphans whose IDs you lost (match by name prefix).
- Startup ordering: reconcile registry → create session (initial statistics) → poll. `statistics.totalRegisteredRunners`
  vs your registry count is a cheap drift alarm.

---

## 5. Ephemeral runner process lifecycle

### 5.1 Starting the runner with the JIT config
Two equivalent ways (`RUN: src/Runner.Listener/CommandSettings.cs:112-123` maps env `ACTIONS_RUNNER_INPUT_<ARG>` → `--<arg>`):
```
<runner_root>/bin/Runner.Listener run --jitconfig <encodedJITConfig>
# or
ACTIONS_RUNNER_INPUT_JITCONFIG=<encodedJITConfig> <runner_root>/run.sh
```
ARC injects (`ARC: controllers/actions.github.com/constants.go:13-17`, `resourcebuilder.go:905-930`):
- `ACTIONS_RUNNER_INPUT_JITCONFIG` (from the secret),
- `GITHUB_ACTIONS_RUNNER_EXTRA_USER_AGENT=actions-runner-controller/<ver>`,
- `ACTIONS_RUNNER_RETURN_VERSION_DEPRECATED_EXIT_CODE=1` (so a too-old runner exits **7** instead of 1/0).
The docker example uses `ACTIONS_RUNNER_INPUT_JITCONFIG` + `/home/runner/run.sh` (`SS: examples/dockerscaleset/scaler.go:138-147`).
Prefer passing via env (not argv) so the secret does not appear in `ps`. Run as a non-root user
(`run-helper.sh` refuses root unless `RUNNER_ALLOW_RUNASROOT`, `RUN: src/Misc/layoutroot/run-helper.sh.template:3-8`).

With a JIT config the runner is ephemeral (`settings.Ephemeral`), runs exactly one job, then exits
(`RUN: Runner.cs:331, 877-880`).

### 5.2 Exit codes
`RUN: src/Runner.Common/Constants.cs:150-163`:
| Code | Name | Meaning for the engine |
|---|---|---|
| 0 | Success | Job done (or runner was removed/deprovisioned/token revoked — **0 does not prove a job ran**, see `Runner.cs:699-703, 890-897`). Service already deregistered an ephemeral runner that completed its job. |
| 1 | TerminatedError | Non-retryable (bad/used JIT config, runner not found, NonRetryableException) (`RUN: src/Runner.Listener/Program.cs:133-152`) |
| 2 | RetryableError | Generic exception (`Program.cs:153-158`) |
| 3 | RunnerUpdating | Self-update in progress (non-ephemeral) |
| 4 | RunOnceRunnerUpdating | Self-update for ephemeral runner (`Runner.cs:550-555`) — should not happen with `disableUpdate:true` |
| 5 | SessionConflict | Another process holds this runner's session |
| 6 | RunnerConfigurationRefreshed | Config migrated, restart required |
| 7 | RunnerVersionDeprecated | Runner too old; only returned if `ACTIONS_RUNNER_RETURN_VERSION_DEPRECATED_EXIT_CODE=1`, otherwise it's **1** (`Program.cs:142-168`). Source: broker `RunnerVersionTooOld` → `AccessDeniedException{ErrorCode=1}` (`RUN: src/Sdk/WebApi/WebApi/BrokerHttpClient.cs:117-126`) |

**Wrapper scripts rewrite exit codes — do not use them if you need the real code.**
`run-helper.sh` maps 1→0, 5→0, unknown→0, 2/3/4→2 (restart loop), 7→7 only with the env var
(`RUN: src/Misc/layoutroot/run-helper.sh.template:36-81`); `run.sh` loops on 2 and exits 0 otherwise
(`RUN: src/Misc/layoutroot/run.sh:13-30`). **Rust:** spawn `bin/Runner.Listener run` directly, set the env var, and
map codes yourself.

ARC's mapping (`ARC: controllers/actions.github.com/ephemeralrunner_controller.go:447-536`):
- 0 → Succeeded; treated as self-deregistered, no DELETE call (`runner_unregistration.go:45-65`).
- 7 → Outdated: mark, queue DELETE, and **stop the whole scale set's listener** until the runner image is fixed
  (`autoscalingrunnerset_controller.go:797-870`).
- other non-zero → failed: if it had a job, delete it (and DELETE registration); else recreate the process **with the
  same JIT config**, backoff `0,5,10,20,40,80 s`, after >5 failures discard and generate a new runner
  (`ephemeralrunner_controller.go:91-102, 361-389, 539-564, 805-829`).

**Rust recommended mapping:**
| Exit | Action |
|---|---|
| 0 | done; DELETE agent anyway (cheap, 404 ok — protects against the "0 without job" cases) ; remove dir |
| 7 (or 1 with "version" in stderr) | mark scale set **Outdated**: stop generating runners, lower `X-ScaleSetMaxCapacity` to 0, alert, trigger runner-binary upgrade |
| 1, 5 | JIT config unusable → DELETE agent, discard, create a fresh runner if still needed |
| 2, 6, crash/signal | restart same JIT config up to N times with backoff, then discard + DELETE |
| 3, 4 | should not occur with `disableUpdate:true`; treat as 2 |

### 5.3 Completion detection
Two independent signals; use both:
1. Process exit (authoritative for resource cleanup).
2. `JobCompleted{runnerName}` message (authoritative for "the job finished"). The docker example removes the container
   on `JobCompleted` (`SS: examples/dockerscaleset/scaler.go:113-122`); ARC instead waits for the pod exit and only uses
   `JobStarted` to protect busy runners (`ARC: cmd/ghalistener/scaler/scaler.go:122-200`).
If `JobCompleted` arrives but the process has not exited within ~60–120 s, SIGTERM (graceful, lets it post logs), then
SIGKILL, then DELETE agent.

### 5.4 Runner version / auto-update (30-day rule)
- Scale sets are created with `disableUpdate: true` (ARC always; `autoscalingrunnerset_controller.go:1073-1075`), so
  runners never self-update. GitHub docs: with updates disabled *"you will be required to update your runner version
  within 30 days of a new version being made available. If you do not ... the GitHub Actions service will not queue
  jobs to your runner."* In the broker flow this surfaces as `RunnerVersionTooOld` → exit 7.
- **Rust engine responsibility:** poll `https://api.github.com/repos/actions/runner/releases/latest` daily, download +
  verify (sha256 from release notes) the `actions-runner-linux-x64-<ver>.tar.gz`, stage it as the new template dir,
  and use it for all **new** runners (running ones finish on the old version). Alert if the in-use version is >~20 days
  behind latest.
- Alternative (`disableUpdate:false`): the runner self-updates on exit code 3/4 via `run.sh` — incompatible with
  "one fresh dir per job" and wasteful; not recommended.

### 5.5 Runner created but never picks up a job
- It stays **registered + idle**, counts toward `totalRegisteredRunners/totalIdleRunners`, and is eligible for any future
  assigned job. Not a problem per se when `minRunners > 0` (warm pool).
- If the **process never connected** (crashed on start, bad network): the registration is dangling. Service GC removes it
  after 1 day; ARC deletes pods that fail before start and retries (`ephemeralrunner_controller.go:456-461, 494-499`),
  and for quota failures it re-creates the runner after 10 min (`ephemeralrunner_controller.go:414-429`).
- Jobs assigned to the scale set while all runners are broken get **cancelled + requeued up to 3×** (§4.1 step 6) and
  then fail. So the engine must detect "runner process up but not listening" — watch its stdout for
  `Listening for Jobs` (`RUN: Runner.cs:491`) within a timeout (e.g. 120 s); if missing, kill + DELETE agent + replace.
- Surplus idle runners (desired < current, no jobs): use the §4.5 DELETE-first protocol.

---

## 6. Edge cases that lose or stick jobs, and how ARC/scaleset handle them

| # | Scenario | Upstream handling | Rust engine requirement |
|---|---|---|---|
| 1 | Crash after ack, before acquire (pinned `SS@9b28` acks first) | Not handled; job may stay unassigned (`SS: listener/listener.go:111-113`) | Ack **after** acquire+handle (HEAD semantics). Idempotent handlers. |
| 2 | Crash after acquire, before runners started | Redelivery (HEAD) or statistics on next session (`totalAssignedJobs` includes it) | Scale from absolute statistics at startup (initial session stats). |
| 3 | Assigned but no runner acquires it in time | Service cancels + requeues ≤3× (`SS: README.md:112-116`) | Keep runner startup < ~1–2 min; pre-warm with `minRunners`; health-check `Listening for Jobs`. |
| 4 | Scale-down kills a runner that just got a job | ARC: DELETE-first, `JobStillRunning` → keep (`ephemeralrunnerset_controller.go:1143-1147`) | Same. Never SIGKILL an unregistered-but-live runner. |
| 5 | JIT generated, engine crashes before recording id | ARC recovers id from secret or `GET agents?agentName=` (`ephemeralrunner_controller.go:1118-1168`) | Write-ahead: persist `{name}` **before** POST, `{id}` right after; on restart look up by name. |
| 6 | `generatejitconfig` POST retried after server-side success | `AgentExistsException` → lookup, DELETE if ours, regenerate (`ephemeralrunner_controller.go:856-890`) | Same (§4.3). |
| 7 | Second listener / orphaned session | 409; ARC restarts pod; ARC012 retried 10×30 s (`ARC012: listener.go:225-257`) | Retry 30 s ≤5 min; flock; DELETE session on shutdown. |
| 8 | Queue token expiry | 401 → PATCH session, retry once (`SS: session_client.go:103-129`) | Same; single-flight refresh. |
| 9 | Admin token expiry/revocation | Proactive refresh 60 s before `exp`; no 401 handling (`SS: client.go:1111-1125`) | Proactive + refresh-on-401. |
| 10 | Unknown top-level `messageType` | Hard error, listener dies (`SS: client.go:633-635`) → poison message | Log, ack, continue. |
| 11 | Response truncated to 50 job messages | Scale by `statistics` (`SS: README.md:44-47`) | Same; JobStarted may be missed → also track busy via process/`DELETE`→409. |
| 12 | Runner outdated (exit 7) | Mark Outdated, stop listener, scale to 0 until fixed (`autoscalingrunnerset_controller.go:797-870`) | Auto-upgrade runner template; alert; set capacity 0 meanwhile. |
| 13 | Runner process dies mid-job | Registration still held → DELETE queued (`ephemeralrunner_controller.go:539-556`); GitHub fails the job after runner heartbeat loss | DELETE agent; don't reuse config; the job itself is lost (GitHub marks it failed) — nothing to recover. |
| 14 | Exit 0 without having run a job (removed / token revoked / job-not-found on ack) | ARC assumes self-deregistered (`runner_unregistration.go:45-65`) | Always DELETE agent (404 ok). |
| 15 | DELETE agent fails with non-409 | ARC drops it; service GC after 1 day (`runner_unregistration.go:268-278`) | Retry with backoff from durable registry; GC is the backstop. |
| 16 | Scale set deleted externally | ARC detects 404 via `GetRunnerScaleSetByID` (`autoscalingrunnerset_controller.go:1337-1356`) | Treat 404 on session/scale-set calls as "recreate scale set" (by name) or stop. |
| 17 | Retry-After on 429 very long | Honoured uncapped (`RHTTP: client.go:551-557`) | Honour, but cap at e.g. 5 min and surface metrics. |
| 18 | `lastMessageId` omitted/0 on restart | Re-reads from first available (`SS: README.md:108-110`) | Harmless if idempotent; persist `lastMessageId` optionally. |
| 19 | Session statistics nil | Listener refuses to start (`SS: listener/listener.go:134-136`) | Retry session creation. |

---

## 7. Is this documented / existing Rust implementations

**Official documentation.** There is **no public HTTP API reference** for `_apis/runtime/runnerscalesets*`, the
message queue, or `actions/runner-registration`. The scaleset README explicitly says non-Go users should
"treat this repo as reference documentation" (`SS: README.md:132-139`). GitHub Docs cover only concepts (runner scale
sets, ARC), required permissions, the 30-day update rule, and the 14-day/1-day runner GC. The API is labelled
**public preview**: "While the API is stable, interfaces and examples ... may change" (`SS: README.md:3`); the April
2026 acquire-flow flip-flop shows the wire contract itself can still change. The only documented *public REST*
alternative is the non-scale-set JIT endpoint `POST /{repos|orgs}/.../actions/runners/generate-jitconfig`, which gives
no demand signal (you would have to poll queued jobs or use `workflow_job` webhooks).

**Rust implementations found (2026-09-30):**
| Project | What | Maturity |
|---|---|---|
| `cpeters/actions-scaleset` (GitHub, MIT, cloned to `refs/actions-scaleset/`) | Direct port of `actions/scaleset`: auth (App+PAT), CRUD, sessions, get/delete message, acquire jobs, JIT, listener; acks after processing; reqwest 0.12 + rustls + tokio | 0 stars, ~36 commits, last push 2026-08-28, **not on crates.io** (`actions-scaleset` crate does not exist). No custom CA/mTLS/proxy. Worth reading/vendoring, not depending on. |
| `tailrocks/velnor` (Apache-2.0) | Rust self-hosted runner + node-local control plane with Docker/Firecracker; has `crates/velnor-runner/src/scaleset/{client,daemon,listener,converge,scale}.rs` and protocol tests | 3 stars, active (pushed 2026-09-30). Closest to the target architecture; good second reference. |
| `5aaee9/shaula` | `crates/shaula-scaleset` (wire types, fixtures) | 1 star, no license → cannot reuse code. |
| `runner-manager` / `runner-manager-github` (crates.io 0.4.28) | Autoscaler TUI using public REST JIT + inventory, not the scale-set protocol | Different approach. |
| `octocrab` | Covers public REST runner endpoints (registration token, generate-jitconfig) only | Useful for Step A/B only. |

Recommendation: implement the ~15 endpoints yourself (small surface, see §8) using `cpeters/actions-scaleset` and
`velnor` as cross-checks, and build a wiremock contract-test suite from the fixtures in `SS: *_test.go`.

---

## 8. Endpoint cheat-sheet

| # | Call | Host / auth | Method + path | OK |
|---|---|---|---|---|
| A | App installation token | GitHub API / App JWT | `POST /app/installations/{iid}/access_tokens` | 201 |
| B | Registration token | GitHub API / PAT or inst. token | `POST /{repos/o/r \| orgs/o \| enterprises/e}/actions/runners/registration-token` | 201 |
| C | Admin connection | GitHub API / `RemoteAuth <regtoken>` | `POST /actions/runner-registration` `{url,runner_event:"register"}` | 2xx |
| 1 | Runner group | Actions / admin | `GET _apis/runtime/runnergroups/?groupName=` | 200 |
| 2 | Scale set by name | Actions / admin | `GET _apis/runtime/runnerscalesets?runnerGroupId=&name=` | 200 |
| 3 | List scale sets | Actions / admin | `GET _apis/runtime/runnerscalesets?runnerGroupId=` | 200 |
| 4 | Scale set by id | Actions / admin | `GET _apis/runtime/runnerscalesets/{id}` | 200 |
| 5 | Create scale set | Actions / admin | `POST _apis/runtime/runnerscalesets` | 200 |
| 6 | Update scale set | Actions / admin | `PATCH _apis/runtime/runnerscalesets/{id}` | 200 |
| 7 | Delete scale set | Actions / admin | `DELETE _apis/runtime/runnerscalesets/{id}` | 204 |
| 8 | Create session | Actions / admin | `POST .../runnerscalesets/{id}/sessions` `{ownerName}` | 200 |
| 9 | Refresh session | Actions / admin | `PATCH .../runnerscalesets/{id}/sessions/{sid}` | 200 |
| 10 | Delete session | Actions / admin | `DELETE .../runnerscalesets/{id}/sessions/{sid}` | 204 |
| 11 | Get message | messageQueueUrl / queue token | `GET {mq}?lastMessageId=` + `X-ScaleSetMaxCapacity` | 200 / 202 |
| 12 | Ack message | messageQueueUrl / queue token | `DELETE {mq}/{messageId}` | 204 |
| 13 | Acquire jobs | Actions / **queue token** | `POST .../runnerscalesets/{id}/acquirejobs` `[ids]` | 200 |
| 14 | Generate JIT | Actions / admin | `POST .../runnerscalesets/{id}/generatejitconfig` `{name,workFolder}` | 200 |
| 15 | Get runner | Actions / admin | `GET _apis/distributedtask/pools/0/agents/{rid}` | 200 |
| 16 | Runner by name | Actions / admin | `GET _apis/distributedtask/pools/0/agents?agentName=` | 200 |
| 17 | Remove runner | Actions / admin | `DELETE _apis/distributedtask/pools/0/agents/{rid}` | 204 |

All Actions-service calls: `?api-version=6.0-preview`, `Content-Type: application/json`, JSON `User-Agent`, strip BOM.

## 9. Suggested Rust daemon structure (informative)

```
auth::TokenManager        // A→B→C chain, cached admin {url, token, exp}, single-flight refresh, refresh-on-401
api::ActionsClient        // endpoints 1–17; typed errors (AgentExists, AgentNotFound, JobStillRunning, Conflict, NotFound)
api::Session              // create w/ 409 retry, refresh single-flight, delete on drop/shutdown
listener::Loop            // initial stats → poll → acquire → handle → ack; capacity from supervisor
supervisor::Registry      // durable {runnerId,name,dir,pid,state}; startup reconcile; DELETE-first scale-down
runner::Process           // copy template dir, spawn bin/Runner.Listener with ACTIONS_RUNNER_INPUT_JITCONFIG +
                          // ACTIONS_RUNNER_RETURN_VERSION_DEPRECATED_EXIT_CODE=1, watch "Listening for Jobs", map exit codes
runner::Updater           // track actions/runner releases, stage new template, 30-day alarm
```
HTTP: `reqwest` + rustls, own retry layer replicating §3.9 (4 retries, 1→30 s exp + jitter, Retry-After for 429/503,
retry 5xx≠501 and connect errors), per-call timeout (long poll ≥ 2 min, others 30–60 s).
