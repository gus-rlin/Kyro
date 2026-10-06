# PR #6 review fixes — 2026-10-06

Base: `d265d9a15147633c9df48d34e6caaac568ea3dc2`. Rust 1.96.1,
Linux/Docker and dedicated newly migrated PostgreSQL test databases; catalogue
admissions and model responses are synthetic fixtures. No real provider calls.

Resource scopes now reuse AppSpec's node-ID validator: nonempty, at most 128
UTF-8 bytes and no control characters. Task IDs retain their 64-byte ASCII
alphanumeric / dot / underscore / hyphen rule. Tests cover node and property
reads/writes, colon and Unicode IDs, 65/128-byte limits, whitespace accepted by
AppSpec, invalid control characters, and authorized edits to existing nodes.

Model admission recalculates the remaining whole seconds after request
preparation and rejects values below the queue's ten-second minimum. It caps
the TTL at 300 seconds without raising it. After SQL admission it also checks
the actual job deadline against the run deadline. An overrun aborts before
commit; the existing transition savepoint rolls back jobs and accounting, and
the coordinator persists the blocked state. Creation/directive transactions
also commit only after successful admission. No new grants or migration.

## Reproductions and checks

| Check | Observed result | Evidence |
| --- | --- | --- |
| Original resource-ID defect | Valid `customer:profile` rejected as `invalid_contract_id` | [Before](evidence/p3-pr6-review-ids-before-01.txt) |
| Original deadline defect | With nine seconds remaining, execution continued instead of blocking | [Before](evidence/p3-pr6-review-deadline-before-01.txt) |
| Domain/agents units and contracts | 45 PASS; includes both corrected boundaries | [After](evidence/p3-pr6-review-units-after-01.txt) |
| Authority and deadlines | 3 PASS, 7.18 s, no ignored tests | [After](evidence/p3-pr6-review-authority-after-03.txt) |
| Workspace | 155 PASS, zero failures, 111 explicitly ignored recipes | [Tests](evidence/p3-pr6-review-workspace-01.txt) |
| Coordinator/recovery | PASS, 15.81 s, synthetic HTTP and real PostgreSQL/worker | [Recipe](evidence/p3-pr6-review-coordinator-01.txt) |
| Worker build | PASS, 17.95 s | [Build](evidence/p3-pr6-review-worker-build-01.txt) |
| Agents strict Clippy | PASS, all targets, no dependencies | [Check](evidence/p3-pr6-review-clippy-agents-02.txt) |
| Domain Clippy | Completes with existing warnings | [Check](evidence/p3-pr6-review-clippy-domain-03.txt) |
| Format / whitespace | PASS | `cargo fmt --all -- --check`; `git diff --check` |

The database regression observes a blocked run at nine seconds, unchanged
call/token counters, only the original planning job/effect, and no new provider
request. A thirty-second run still admits four executors whose persisted
deadlines are no later than the run deadline. Unit cases cover expired time,
9.999 seconds, exactly ten seconds, rounding down, the 300-second cap, and time
consumed during preparation. No induced database-clock skew or SQL admission
delay scenario was run; the post-admission guard was manually reviewed.

## Retained failed checks and corrections

The first authority rerun reused the reproduction database, which still had
unprocessed jobs from the deliberately failing trial. Those jobs affected
the fixture's global worker claims: [attempt 1](evidence/p3-pr6-review-authority-after-01.txt).
On a fresh database, the two existing tests passed, then the new scenario
encountered a cancelled pending job left by the metadata test:
[attempt 2](evidence/p3-pr6-review-authority-after-02.txt).
`claim_next_job` can settle a cancelled job and return no lease. The metadata
test now settles its final cancellation before the next scenario, matching its
existing setup cleanup. A third fresh database passes all three scenarios;
production queue logic and assertions were not weakened.

The strict combined domain/agents Clippy invocation failed on four existing
library warnings in unchanged `model.rs` and `task.rs`:
[failure](evidence/p3-pr6-review-clippy-01.txt). Domain-only Clippy additionally
reports an existing test initializer warning outside the added regression.
The changed agents crate passes with warnings denied. No unrelated lint
refactor was included.

Reproduction after migrations, with distinct dedicated databases for authority
and coordinator recipes and API/worker/admin URLs configured:

```sh
cargo test --locked --offline -p kyro-domain -p kyro-agents --lib --test agents
cargo test --locked --offline --workspace
cargo test --locked --offline -p kyro-agents --test authority -- --ignored --nocapture --test-threads=1
cargo build --locked --offline -p kyro-worker --bin kyro-worker
KYRO_P3_WORKER_BIN=/workspace/target/debug/kyro-worker cargo test --locked --offline -p kyro-agents --test coordinator -- --ignored --nocapture
cargo clippy --locked --offline -p kyro-agents --all-targets --no-deps -- -D warnings
cargo clippy --locked --offline -p kyro-domain --all-targets --no-deps
cargo fmt --all -- --check
git diff --check
```

Manual review covered validation compatibility, UTF-8 byte limits, scope
authorization, queue minimum/maximum TTL, all admission callers, savepoint
rollback and worker deadline consumption. No new independent review score,
NVIDIA qualification, catalogue admission, protected factory artifact or
deployment is claimed for these runtime changes. Earlier evidence remains
bound to its archived source digests.
