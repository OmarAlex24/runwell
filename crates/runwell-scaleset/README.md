# runwell-scaleset

`ActionsClient::new(Config::new(url, Credentials::Pat(secret))?)` constructs a
lazy client. `Credentials::App` supports an App issuer/client ID, installation ID
and redacted RSA PEM key. `Config` controls REST routing, request deadlines,
conflict retry window, backoff, and an injectable `Clock`.

The public API includes scale-set CRUD/list/lookup, group lookup, runner lookup,
`generate_jit_config`, `remove_runner`, and `open_session`. Runner removal returns
`SafeToKill` or `KeepRunning`; other errors preserve HTTP status, endpoint,
GitHub exception type, and correlation IDs. JIT credentials are `Secret` values.

`Listener::new(session, capacity)` implements `Stream<Item = Result<Message,
Error>>`. Each message contains absolute `Statistics`, ordered typed `Event`s,
and an optional message ID. Startup and recreated-session statistics have no
message ID. For real deliveries, acquire chosen jobs with `acquire_jobs`, finish
durable/idempotent handling, and call `ack(id)`. Calling `next` before that ack
immediately returns `Error::AckRequired` and preserves the pending delivery.
Unknown events are skipped; even an unknown envelope yields an ackable batch.
`set_max_capacity` changes the next poll. Always await `close(self)`, including on
processing errors. Drop schedules cleanup only while a Tokio runtime is alive.

`desired_runners(min, max, assigned)` uses saturating addition and the absolute
assigned-job count. Events may be truncated to 50; counting them is not demand.
`examples/listen.rs` is opt-in through environment variables and is never run by
the test suite. It requires an external supervisor before advertising capacity.

## Retry and recovery decisions

- Idempotent GET/DELETE, absolute scale-set PATCH, session PATCH and acquirejobs
  retry transient transport failures, 429, and 5xx except 501.
- Creating scale sets/sessions, JIT generation, and minting tokens do not retry
  ambiguous transport/429/5xx failures. Retrying acquisition is safe because the
  upstream example explicitly documents repeated acquisition as a no-op.
- An explicit admin/queue 401 refreshes the relevant credential once and replays
  once. Concurrent refreshes share a single exchange/PATCH.
- Registration exchange retries explicit propagation 401/403 only.
- JIT 409 recovery checks ownership and DELETE's busy result before one new POST.
- Session creation retries 409 with jitter within a configurable five-minute
  default window. Queue/PATCH 404 recreates it and resets the message cursor.
- Retry-After seconds and HTTP dates are honored, capped at five minutes.
- A 202 returning in under five seconds triggers jittered exponential backoff
  with a one-second minimum and 30-second cap. A normal long poll resets it.

## Contract-test checklist

Fixtures are synthetic, independently derived from Go HEAD e6daac7 structs and
test shapes. These are HTTP contract tests, not live-service verification.

| Brief edge case | Test |
|---|---|
| BOM | `bom_double_encoded_body_typed_events_and_ack_last`, `list_groups_runner_lookup_and_bom_on_admin_response` |
| Double-encoded body | `bom_double_encoded_body_typed_events_and_ack_last` |
| Unknown messageType | `unknown_message_types_remain_ackable_and_do_not_wedge_queue` |
| 202 empty poll | `empty_202_repolls_with_capacity_and_without_ack` |
| Poll before ack | `next_before_ack_fails_immediately_and_resumes_after_ack` |
| Rapid 202 responses | `rapid_empty_polls_are_bounded_in_a_virtual_minute`, `normal_long_poll_resets_rapid_empty_poll_backoff` |
| Admin 401 refresh/replay | `admin_401_refreshes_and_retries_once_with_new_token`, `repeated_admin_401_stops_after_one_refresh` |
| Queue 401 refresh/replay | `queue_401_refreshes_url_and_token_then_retries_once`, `acquire_and_ack_refresh_queue_token_without_implicit_ack` |
| 404 session gone | `queue_404_recreates_session_and_yields_initial_statistics`, `patch_404_recreates_and_old_message_cannot_be_acked`, `explicit_refresh_recreation_resets_last_message_cursor` |
| 409 session conflict | `session_409_retries_with_backoff_and_preserves_exhausted_status`, `session_conflict_then_success_and_explicit_close` |
| Duplicate JIT / AgentExists | `duplicate_jit_post_recovers_our_runner_and_regenerates_once`, `jit_collision_never_deletes_foreign_or_busy_runner`, `jit_recovery_is_bounded_even_when_collision_repeats` |
| Scale-down / JobStillRunning | `scale_down_race_job_still_running_is_keep_running` |
| 429/5xx retry eligibility | `idempotent_429_and_5xx_retry_with_jitter_and_preserve_final_status`, `non_idempotent_posts_never_retry_429_or_5xx`, `acquire_post_is_explicitly_idempotent_and_removal_retries` |
| Admin expiry | `admin_expiry_refreshes_at_sixty_seconds_and_is_single_flight` |
| Scaling formula | `desired_runners_uses_absolute_assigned_jobs_and_saturates` |

Mapping the research spec's broader §6 table to the **client portion** of each
scenario (process supervision and durable registry behavior require daemon tests):

| §6 row | Client contract test / responsibility boundary |
|---|---|
| 1: crash after premature ack | `processing_failure_closes_without_ack_and_next_session_redelivers` |
| 2: acquired jobs before startup | `bom_double_encoded_body_typed_events_and_ack_last` verifies absolute initial statistics; durable recovery belongs to supervisor |
| 3: runner never acquires | `cancelled_before_assignment_is_typed_completion`; startup deadlines and service requeue count require daemon/live tests |
| 4: scale-down race | `scale_down_race_job_still_running_is_keep_running` |
| 5: lost JIT response/ID | `duplicate_jit_post_recovers_our_runner_and_regenerates_once`; write-ahead name persistence belongs to supervisor |
| 6: repeated JIT POST | `duplicate_jit_post_recovers_our_runner_and_regenerates_once`, `unsafe_post_transport_timeout_is_not_retried` |
| 7: orphaned/conflicting session | `session_conflict_then_success_and_explicit_close`, `session_409_retries_with_backoff_and_preserves_exhausted_status`; local flock belongs to daemon |
| 8: queue token expiry | `queue_401_refreshes_url_and_token_then_retries_once`, `concurrent_queue_401s_share_one_refresh` |
| 9: admin expiry/revocation | `admin_expiry_refreshes_at_sixty_seconds_and_is_single_flight`, `concurrent_admin_401s_share_one_refresh` |
| 10: unknown envelope | `unknown_message_types_remain_ackable_and_do_not_wedge_queue` |
| 11: truncated job messages | `truncated_events_scale_from_absolute_statistics` |
| 12: outdated runner | `empty_202_repolls_with_capacity_and_without_ack` verifies capacity zero; exit detection and upgrades belong to supervisor |
| 13: process dies mid-job | `removal_204_and_404_are_safe_including_exit_without_job` verifies cleanup outcomes; process monitoring belongs to supervisor |
| 14: exit zero without a job | `removal_204_and_404_are_safe_including_exit_without_job` |
| 15: non-409 removal error | `acquire_post_is_explicitly_idempotent_and_removal_retries`, `unrelated_removal_conflict_remains_typed_error`; durable retries belong to supervisor |
| 16: external scale-set deletion | `permanent_statuses_and_501_are_not_retried` preserves 404 for caller decision |
| 17: long Retry-After | `retry_after_seconds_and_http_date_are_capped` |
| 18: restart cursor zero | `bom_double_encoded_body_typed_events_and_ack_last`, `processing_failure_closes_without_ack_and_next_session_redelivers` |
| 19: nil initial statistics | `missing_initial_statistics_deletes_session_then_retries` |

## Spec decisions and live verification

No wire-struct disagreement between the supplied spec and Go HEAD was found.
The brief intentionally changes Go's retry-all-methods behavior, admin 401
handling, conflict/recreation handling, unknown envelopes and omission of cursor
zero. The brief's strict explicit-ack invariant takes precedence over the spec's
suggestion to automatically ack unknown envelopes. Those envelopes are delivered
with empty event arrays for caller acknowledgment. Every request adds
`api-version=6.0-preview`, including GitHub REST and queue calls.

All live behavior remains unverified: auth propagation/lifetimes, redelivery and
queue retention, session orphan expiry, acquisition idempotency, JIT collision
and busy-runner races, GHES labels/routing, polling duration, and rate-limit
headers. The coordinator should exercise these against the live service.
