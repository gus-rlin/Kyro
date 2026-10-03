# P1 backend foundation: shared contracts and PostgreSQL

This document defines the database and Rust foundation provided by the
`part1-foundation` contribution. The schema is embedded in the versioned SQLx
migrations under `crates/kyro-store/migrations/` (versions `0001`–`0016`).
Versions 0001–0015 remain immutable; later security and
compatibility fixes are forward-only. The runtime API and worker use separate
PostgreSQL roles. They never
own tenant tables and never have `BYPASSRLS`.

## Runtime context and errors

`kyro-domain` exports `Environment::{Development, Production}`, the six
capability actions `read`, `write`, `execute`, `model`, `manage`, and `budget`,
and an `Event` with `project_id: UUID`, `sequence: i64`, `kind`, JSON payload,
optional actor UUID, and UTC creation time. Revisions, sequences, budget units,
and counters use signed 64-bit integers in Rust and `BIGINT` in PostgreSQL.

`Store::connect(url, max_connections)` builds a cloneable pool with a bounded
connection count. `Store::with_environment` selects the runtime environment;
`Store::begin_actor(actor_id)` starts a transaction and sets
`kyro.actor_id` and `kyro.environment` with transaction-local `set_config`.
Missing context denies tenant reads and writes. The settings disappear when the
transaction ends, including when a pooled connection is reused. A queue claim
transaction may additionally set transaction-local
`kyro.queue_claim = 'on'`; only `kyro_worker` queue policies consult it. A
separate worker transaction may set `kyro.accounting_job_id` to one already
loaded job for post-send settlement after rights or leases become stale.

Migration 0016 adds the D34 actor-scoped project metadata lock: it validates
runtime role, actor, environment, membership and current action grants, locks
project → membership → grants, and returns only revision and limits. It does
not grant Write. Model jobs require Execute and Model cumulatively;
reconciliation requires Budget or Manage. Qualified policy correlations keep
command/job references within their actor, project and environment. A worker
may lock only the exact model/reconciliation job selected by its accounting
context after sending, without gaining project mutation privileges.

Reconciliation validates the locked intent against the worker's current model
registry and output schema through a required pure validator, before any
receipt, ledger or hold transition. Target business grants are checked under
the original author's actor context; the Budget operator context is restored
before accounting writes and events. Settlement releases only the original
hold, preserves other holds, and records actual usage once as `settlement`.

The shared error type is `kyro_domain::Error`/`Result<T>`. HTTP callers map
`NotFound` to an invisible resource, `Forbidden` to a visible resource without
that action, `StaleRevision { expected, current }` to a failed precondition,
and `IdempotencyConflict` to a conflict. Database and provider details must
not be copied to client-facing errors or logs.

## Tables and stable names

All tables are in `public`. UUID primary keys use PostgreSQL's
`gen_random_uuid()`; the migrator requires PostgreSQL 13 or later.

| Table | Contract |
| --- | --- |
| `actors` | `id`, `issuer`, `subject`, `created_at`; unique `(issuer, subject)`. |
| `organizations` | `id`, `name`, `created_by`, `created_at`. The creator is retained to constrain initial membership creation. |
| `memberships` | `(organization_id, actor_id)` primary key, `role` (`owner`, `admin`, `member`), `created_by`, `created_at`. |
| `sessions` | `id`, unique 32-byte `token_hash`, `actor_id`, 32-byte `csrf_hash`, `expires_at`, nullable `revoked_at`, `created_at`. No provider token is stored. Partial indexes support bounded cleanup batches for expired and revoked sessions. |
| `login_flows` | `id`, `issuer`, unique 32-byte `state_hash`, 32-byte `nonce_hash`, 32-byte `browser_binding_hash`, nullable ephemeral `pkce_verifier`, expiry/consumption/creation timestamps. Keep the verifier server-side and never return or log it; atomically set it to `NULL` when consuming the flow. |
| `projects` | `id`, `organization_id`, `name`, `current_revision BIGINT`, `event_sequence BIGINT`, JSONB `data_policy` and `limits`, `created_by`, `created_at`, `updated_at`. Both counters start at zero. |
| `capability_grants` | `id`, `actor_id`, `project_id`, `actions TEXT[]`, `resources TEXT[]`, `environment`, strict bounded JSONB `limits`, nullable expiry/revocation timestamps, `created_by`, `created_at`. A resource is exactly `'*'` or the project's canonical UUID. Grant limits may contain only `max_job_attempts` (1–3), `max_job_ttl_secs` (10–1800), `max_model_input_bytes` (1–1048576), `max_model_output_tokens` (1–1000000), and `max_changeset_operations` (1–128); each key is optional and absent limits do not inherit project limits. |
| `app_revisions` | Immutable append-only `(project_id, revision)` primary key, starting at revision zero, JSONB `spec`, `created_by`, `created_at`; specification limit 1 MiB. |
| `change_commands` | `(project_id, idempotency_key)` primary key, 32-byte SHA-256 `fingerprint`, reference-only JSONB `result`, `command_id`, `created_at`. The result is at most 32 KiB. |
| `decisions` | `id`, `project_id`, nonnegative `revision`, `actor_id`, `kind`, JSONB `payload`, `created_at`. |
| `jobs` | `id`, `project_id`, `actor_id`, `environment`, `source_revision` (including initial revision zero; composite FK to that project's revision), JSONB `payload`, `status`, `attempts`, `max_attempts`, `generation`, nullable `lease_owner`/`lease_until`, `deadline`, `cancel_requested`, nullable reference-only JSONB `result`, nullable stable `error_code`, timestamps. |
| `effects` | `id`, unique `job_id` (one durable external effect per job across generations), `project_id`, `generation`, `destination`, 32-byte `fingerprint`, JSONB `intent`, `status`, nullable JSONB `result`, unique non-null `reservation_id`, timestamps. |
| `project_budgets` | One row per project: `limit_units`, `reserved_units`, `spent_units` as `BIGINT`; `currency` (`SYN` initially), positive bounded `unit_scale`, `configuration_version BIGINT`, `updated_at`. Initial amounts and version are zero. |
| `budget_reservations` | `id`, `project_id`, `job_id`, unique `effect_id`, `idempotency_key`, positive `units BIGINT`, status, expiry and timestamps. The deferred scope foreign keys require the effect, job, and project to match. |
| `usage_ledger` | `id`, `project_id`, `job_id`, unique `reservation_id`, nonnegative `units`, `kind`, nullable provider/model labels, bounded JSONB metadata, `recorded_at`. The unique reservation prevents duplicate settlement. |
| `events` | `(project_id, sequence)` primary key, sequence starts at one, `type`, JSONB `payload`, nullable `actor_id`, `created_at`. |
| `outbox_events` | `id`, `project_id`, `event_sequence`, `topic`, JSONB `payload`, availability/delivery timestamps, attempts and creation time; unique event reference and FK to `events`. |
| `runtime_control` | Singleton `id = 1`, `external_sends_enabled`, `updated_at`; initially enabled. API and worker can only read it. |

Database checks bound identifiers, payload sizes, counters, job/effect states,
action names, environment names, budget units, and error codes. Job states are
`pending`, `running`, `succeeded`, `failed`, `cancelled`, `unknown`, and
`stale`; a pending job starts at generation zero and its first claim advances
the generation. Running jobs must have both lease fields. The result is only a
reference such as `{"revision": 3}` or `{"effect_id": "…", "status": "…"}`;
model output and provider error text do not belong in job results, event
payloads, or `error_code`.

Effect states are `prepared`, `sending`, `succeeded`, `failed`, `unknown`, and
`cancelled`. Reservation states are `held`, `settled`, and `released`. An
unknown send keeps its reservation held. Once an external send may have
started, an expired lease must become `unknown`; it is never automatically
resent. An unsent `prepared` effect may be reconsidered only after the worker
proves it was not sent. Effect and reservation links are unique and deferred
within the transaction so the pair can be created atomically.

Budget amounts are integer units. The initial currency is `SYN`, scale one,
and limit zero; paid providers remain disabled until explicit qualification
and an operator-configured budget. Pricing snapshots must use the same
currency and scale as the project budget. Currency or scale cannot be changed
after units have been reserved or spent. No floating-point value is used for
accounting.

`configuration_version` is the compare-and-swap epoch for limit/currency/scale
updates. Every configuration change increments it exactly once; increments
without a configuration change are rejected. Settlement updates to reserved or
spent units never change this version, so two writers cannot silently replace
each other's currency or scale using an unchanged expected version.

## Authorization and RLS

The migration creates `kyro_api` and `kyro_worker` as login roles with no
superuser, role-creation, database-creation, or RLS-bypass privileges. They do
not own any table. The separate `KYRO_DATABASE_ADMIN_URL` is used only by the
migrator and operator procedures. The migration login must be administrative
and must have `BYPASSRLS` or be a superuser because bounded `SECURITY DEFINER`
helpers run under that owner. Runtime credentials are not set by the
migration; provision them outside the repository.

Every tenant and authentication table has both RLS enabled and `FORCE ROW
LEVEL SECURITY`. The identity API may access private actor/session/login-flow
records before an actor context exists; no worker role can read these tables.
Migration 0014 gives only `kyro_api` the DELETE table privilege, narrowed by
restrictive policies: it may delete a login flow only after consumption or
expiry, and a session only after revocation or expiry. Active rows remain
visible to cleanup queries but are filtered from DELETE; `kyro_worker` has no
DELETE privilege on either authentication table.
After authentication, project visibility requires both current membership in
the project's organization and an unexpired, unrevoked grant for the actor
whose environment exactly matches `kyro.environment` and whose resource is
`'*'` or that project UUID. Membership alone does not expose projects. Removing
a member atomically revokes that actor's project grants in every environment;
an organization must retain at least one owner. Missing or invalid
actor/environment context denies access. An invisible project is `404`; a
visible project without the required action is `403`.

Job, effect, reservation, and usage-ledger reads also require the linked job's
environment to match `kyro.environment`, even when the project grant is
otherwise visible. Events and outbox rows remain project-scoped because their
sequence is global: an event with no job/effect/reservation reference is
visible in both environments, while every supplied reference must resolve to
the same project and current environment. Malformed or inconsistent
references, and absent actor/project/environment context, fail closed. A gap
in visible event sequence numbers can therefore be caused by the other
environment and must not be treated as evidence of retention or a purge.
`kyro_event_history_bounds(project_id)` returns the earliest physical event
sequence and the global project counter in one authorized snapshot. The helper
requires the current actor's `read` grant and returns no row without scope; it
returns no earliest sequence when every event was purged while preserving the
nonzero counter. Event-page reads join these bounds and visible rows in one SQL
statement so retention changes cannot race a separate bounds query.

The grant actions are exactly `read`, `write`, `execute`, `model`, `manage`,
and `budget`. The application rechecks the required action and expiry using
the database clock at the point of use. Grant checks that protect a commit
must lock the matching row `FOR SHARE`, so revocation serializes against the
commit. Rights are checked again before incorporating external output.
`kyro_lock_actor_grants(actor_id, project_id, actions)` is the bounded runtime
locking path: it accepts only `kyro_api` or `kyro_worker`, requires the actor
and valid environment from transaction-local context to match, verifies
membership and current project visibility, and validates the requested action
set. It locks the membership row `FOR SHARE` before returning only matching,
active grants scoped to the current environment and `'*'` or the canonical
project UUID, with each returned grant locked `FOR SHARE`. The helper grants no
runtime `UPDATE` privilege on grants; organization-owner policies remain the
only grant-revocation path.
Organization ownership alone can create or revoke grants only for a project
in that owner's actual organization and only for an organization member.
`grant_initial_project_owner(project_id)` is the bounded bootstrap function:
in the creating transaction it verifies the actual project organization, the
creator actor, owner membership, and that no project grant exists, then inserts
the creator's complete grant for the transaction environment. Project
creation must not use `INSERT ... RETURNING` before this grant exists because
the project is not visible under RLS yet.

The `kyro_worker` queue service may read/claim jobs across projects only in a
transaction with `kyro.queue_claim = 'on'`, and only for the configured
environment. Candidates are read without row locks. Before locking a job, the
worker calls `kyro_lock_project_for_job(job_id, skip_locked)`, which locks the
project first and returns only its UUID and current revision; the worker then
locks and revalidates the job. Queue claim context does not expose project
documents. After loading a job, the worker uses the actor context for business
reads/writes and re-evaluates current grants.

Post-send accounting uses a separate transaction with
`kyro.accounting_job_id` set to exactly the loaded job. It can inspect that job
row plus its linked effect, reservation, budget aggregate, and ledger rows in
the configured environment, even after a grant, lease, generation, or
cancellation becomes stale. It cannot enumerate jobs or read project
documents, and it cannot integrate model output. The helper locks the project
before the job so accounting follows the same project → job → effect lock order
as queue work. Settlement remains idempotent; a late invoice must not be lost
because rights were revoked after the provider send.

The API can update only `jobs.cancel_requested`. Its row policy permits the
job's actor with `execute`, or a project manager for another actor's job, only
while status is `pending` or `running`. The flag cannot be cleared. The API does
not change job status, leases, results, generations, or effects; the worker
finishes cancellation and releases an effect only when it proves no send began.
The cancellation event is appended in the same transaction as the flag change.

`kyro_append_event(project_id, kind, payload)` is the only runtime write path
for `events` and `outbox_events`. It atomically locks/increments
`projects.event_sequence`, inserts the event, and inserts its outbox reference.
API calls require a visible project and the corresponding action; cancellation
events allow `execute` for the caller's own job or `manage` for another actor's
job. Worker business events require the actor's current `write` or `manage`
grant and retain that actor id. Service state events are restricted to
`job.*`, `effect.*`, or `budget.*`, accept only bounded reference/status fields,
must reference a job/effect/reservation in the same project and environment,
and record a null actor. They require either the exact queue-claim scope or the
exact `kyro.accounting_job_id`. Direct runtime INSERT/UPDATE on events and
outbox rows is revoked.

The runtime-control row is read-only to both runtime roles. Before preparing
an external call and again immediately before changing an effect to
`sending`, the gateway must verify `external_sends_enabled = true`. A restore
procedure first sets the row to false as the administrative role, then
reconciles `sending` effects to `unknown`, preserves held reservations for
unknown sends, and resets queue leases/generations without replaying output.
Restoring a database never turns external sends back on automatically. An
operator may re-enable them only after restore isolation and reconciliation
are explicitly complete.

## Migration command and local verification

Run `cargo run -p kyro-store --bin kyro-migrate` with
`KYRO_DATABASE_ADMIN_URL` from a secret store. It uses a one-connection pool,
rejects the API/worker roles and roles without administrative migration
privileges, embeds the checked-in SQLx migrations, and emits a generic failure
without printing the URL or SQL diagnostics. The operation is serialized by
SQLx's migration lock; rerunning it reports the schema is current. It does not
drop data or reset the database. `KYRO_DATABASE_URL` and
`KYRO_WORKER_DATABASE_URL` are runtime-only and are never used by this binary.

Migration 0002 preserves already-created revision tables while correcting the
initial revision boundary to zero and granting the worker the row read needed
to calculate a column-limited budget settlement update. Migrations 0003–0010
then add membership/grant invalidation, owner protection, isolated queue and
accounting scopes, the budget configuration CAS epoch, actor-scoped job
cancellation, PostgreSQL-compatible event payload sizing, project-before-job
locking, fail-closed context checks, and the exact accounting-job read policy.
These corrections stay as forward migrations so databases that already
applied earlier versions retain their SQLx checksums and data. Migration 0011
makes `effects.reservation_id` mandatory while keeping its reciprocal foreign
key deferred, so effect and reservation must be created atomically in one
transaction. It fails on pre-existing effects without a reservation rather
than inventing accounting data.
Migration 0012 adds strict per-grant resource limits and environment-scopes
job-linked reads for both actor and service paths. It preserves exact-job
queue/accounting visibility and applies the same reference checks to events
and outbox entries without hiding global project events. Migration 0013 adds
the read-authorized physical event-retention bounds helper without adding a
runtime purge capability. Migration 0014 permits authentication cleanup only
for consumed/expired login flows and revoked/expired sessions, with
restrictive row policies and no worker DELETE grant. Migration 0015 adds the
bounded `kyro_lock_actor_grants` helper so authorized API and worker code can
hold membership and grant rows against concurrent removal without receiving
grant UPDATE privileges.

The isolated qualification database used for P1 verification is PostgreSQL
18.6 at `127.0.0.1:55440`, database `kyro_p1`; its local trust configuration
is bound to loopback and contains synthetic data only. This is not a production
deployment. See `docs/suivi/essais/part1-foundation.md` for exact commands,
failures, and outcomes.
