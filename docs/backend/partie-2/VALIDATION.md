# P2 delivery and receipt confidentiality

This delivery introduces `kyro-app` and `kyro-factory`, their PostgreSQL schemas,
the closed component catalogue, deterministic assembly, protected build and
attestation tooling, and the P1 API/worker integration. The manifest covers 139
application/adapter capabilities and eight factory capabilities. Agent planning,
application UI, production deployment and live provider qualification are separate.

## Receipt correction

A command response is retained for idempotency. Replaying it must not reveal a
projection that became inaccessible after schema, resource or identity changes.
The existing transaction authority fence and application epoch now also cover:

- B036 schema migrations, including `readable: true` to `false`.
- B031 deletion and B033 batches containing deletion, including cascading deletion.
- B131 contact and organization archival.
- B121 product publication and archival.

The exclusive fence is acquired before business row locks. Fresh commands advance
the epoch within their transaction; rollback and replay never advance it. Older
receipts return `409 idempotency_authority_changed` without returning private fields,
deleting their idempotency key or executing their mutation again. The existing role,
permission, scope and initial MFA context checks remain in force.

Ordinary record updates, batches without deletion and changes in another application
preserve authorized historical receipts. A consumed B022 capability receipt contains
only its historical acknowledgement and remaining count; replaying it after revocation
does not authorize a new operation or consume another use. Fresh consumption still
checks revocation. The exact public purge tombstone remains the existing exception.

`receipt_authority` covers these boundaries with real PostgreSQL, durable history,
receipt, event and outbox counters, a current-reader comparison, and observed advisory
lock contention. The schema, archived-contact and deleted-record reproductions failed
before the fix. No provider or model call is required for these tests.

## Version and installation

The catalogue version is **0.1.2**. Existing 0.1.0 and 0.1.1 attestations belong to
their original source digests and do not admit this source tree. Generated entries
remain pending until a new protected qualification and operator admission.

The application schema contains migrations 0001–0037. Migration 0037 takes the
global transaction fence and invalidates pre-fix responses once, keeping their
durable idempotency keys and business effects. An isolated upgrade test reproduces
an unsafe pre-fix receipt and checks refusal, unchanged effect counters and
idempotent migration replay. Stop the old runtime before migration, then start
the corrected binary; mixed runtime versions cannot preserve this guarantee.

Factory pilotage migration
**0019_factory_artifacts.sql** follows the existing main-branch migrations 0017
(synthetic reconciliation) and 0018 (chat). Those existing migrations are preserved.
`main` chat, provider retention policy, secret filtering and streaming behavior are
retained when adding the embeddings protocol.

## Verification

Local verification uses Rust 1.96.1, PostgreSQL 18.6 and isolated Docker test
databases. The external PostgreSQL fixture uses TLS and SCRAM with synthetic data
and credentials. No paid model or live provider is configured.

| Check | Observed result |
| --- | --- |
| Receipt regressions and upgrade | 10 passed; no failed or ignored tests |
| Complete application suite with `test-support` | 117 passed; no failed or ignored tests |
| Default workspace tests | 136 passed; 100 explicitly ignored integration/protected recipes |
| API/Store PostgreSQL suite with ignored tests enabled | 44 passed; no failed or ignored tests |
| Factory contracts, artifacts and registry with failure injection | 24 passed; no failed or ignored tests |
| Fresh installation, upgrade and migration replay | Application 0037 and pilotage 0019; exact SQLx checksums, restricted roles and forced RLS |
| Format, locked all-targets check, scoped strict Clippy and executable build | Passed |
| OpenAPI and dependency classifier tests | 33 paths / 40 operations; seven classifier tests passed |
| Dependency audit, Linux active graph | 263 active packages, no active advisory; inactive `rsa 0.9.10` / `RUSTSEC-2023-0071` retained (raw audit exit 1, classification exit 0) |

The initial WSL application and workspace runs failed with database availability
and filesystem I/O errors. The following WSL invocation could not read its system
account files. Successful Docker runs use fresh databases; no failed scenario was
skipped or weakened. The first P1 integration run also exposed a chat migration
test fixed at 18 migrations: its count now follows the complete migrator, retaining
the existing checksum, runtime-state and chat privilege checks.

The final receipt run passes after normalizing generated Rust include line endings;
no Rust token changed. A second complete application run and a new protected
worker/build/attestor recipe were interrupted when the Windows disk reached zero
free bytes and Docker became unavailable. They do not count as successful runs.
The previously completed suites above remain distinct from those attempts.

New protected gVisor/OCI qualification, catalogue admission, live provider
qualification, production deployment and a new independent review score remain
unverified. Historical attestations and the previous review score do not qualify
this source tree.

Reproduction commands, after provisioning the constrained test roles and connections
described in [OPERATIONS](OPERATIONS.md):

```sh
cargo test --locked -p kyro-app --test receipt_authority --test receipt_upgrade -- --ignored --nocapture --test-threads=1
cargo test --locked -p kyro-app --features test-support -- --include-ignored --test-threads=1
cargo test --locked --workspace -- --test-threads=1
cargo test --locked -p kyro-store -p kyro-api -- --include-ignored --test-threads=1
cargo test --locked -p kyro-factory --features test-support --lib --test contracts --test artifacts --test registry
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo clippy --locked -p kyro-app -p kyro-factory --all-targets --features kyro-factory/test-support --no-deps -- -D warnings
cargo build --locked --workspace --bins
node scripts/check-openapi.mjs
```

Ignored Docker recipes require the separate protected environment described in the
operations guide. A normal workspace test run does not qualify their behavior.
