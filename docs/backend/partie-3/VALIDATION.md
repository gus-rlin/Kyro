# P3 validation status

All results are local. The PR incorporates P2 fixes from `main`
`a12e1ced89854bc5716b50ca4bbd1c10a8d81ee5`; the original and updated runtime
subjects are distinguished below. See the [historical 61-file inventory](evidence/p3-pr-source-01.json).

## PR #6 review fixes, 2026-10-06

The resource-ID and cumulative-deadline defects reported on `d265d9a151` are
corrected with before/after regressions. Current checks: 155 workspace tests
pass (111 service recipes explicitly ignored), all three PostgreSQL authority
tests pass, and the coordinator/recovery recipe passes in 15.81 s. Agents strict
Clippy, worker build and formatting pass. Domain Clippy completes with existing
warnings; its strict invocation is retained as a failure. See the
[fix report, reproduction commands and retained failures](PR-REVIEW.md).
No real-provider or protected factory campaign was rerun for these changed
runtime bytes; the NVIDIA signatures and independent scores below remain
historical evidence of their original snapshots.

## Checks after integrating main, 2026-10-06

| Check | Observed result | Evidence and limits |
| --- | --- | --- |
| Workspace binaries | PASS, 16.30 s | [Build](evidence/p3-pr-bins-01.txt), Rust offline with retained tools |
| Workspace tests | 153 PASS, 110 explicitly ignored | [Tests](evidence/p3-pr-workspace-01.txt); service recipes below, no general P2 requalification |
| P1 API/Store regressions | 44 PASS, zero ignored | [Regressions](evidence/p3-pr-regressions-01.txt), new isolated PostgreSQL database |
| Authority and pool size one | 2 PASS, 5.13 s | [Authority](evidence/p3-pr-authority-01.txt), separate fresh PostgreSQL database |
| Coordinator and recovery | PASS, 15.43 s | [Coordinator](evidence/p3-pr-coordinator-01.txt), real PostgreSQL/HTTP with synthetic model provider and a killed worker |
| Agents strict Clippy | PASS | [Clippy](evidence/p3-pr-clippy-01.txt), all targets, no dependencies, warnings denied |
| Host formatting and PR diff | PASS | `cargo fmt --all --check`; `git diff --check origin/main`; evidence-only byte preservation attributes |
| New real NVIDIA factory | Factory04 PASS, 406.95 s, eleven protected checks | [Log](evidence/p3-pr-nvidia-factory-04.txt), [signed exports](evidence/p3-pr-factory-04/sha256.json), [SQL seal](evidence/p3-pr-nvidia-factory-04-seal.json); actual Cargo/gVisor/PostgreSQL/attestor, fixture admissions |
| Retained NVIDIA refusal | Factory03 blocked before construction, 36.25 s | [Refusal and measurements](evidence/p3-pr-nvidia-factory-03.json), [SQL seal](evidence/p3-pr-nvidia-factory-03-seal.json); no build attributed to a refusal |
| Independent review | PASS 9.5/10 P3 rubric, 9.4/10 general rubric | [Current review](REVIEW.md), 61 inputs, signatures, runtime bytes, financial provisions and thirteen closures independently checked |

Coordinator coverage includes four simultaneous effects, strict contract
retention, read-only planning, graph/scopes/order, deterministic transformations,
rejection, conflicting edits and harmless rebase with new reviews, atomic
rollback when the build queue is full, instructions/revocation in flight,
compaction, forced worker termination with an unknown effect and no resend,
RLS/CAS/history boundaries, and rotation over 33 active projects. The separate
authority recipe checks read/execute without write and single-connection
transaction reads. Remote CI has not yet run for this PR.

Factory04 source digest:
`a9e3af328fd1fa38d4936b7917dc18427529967cba7e6c2bb85f3662ce5f76f6`.
The [current campaign report](NVIDIA.md) retains exact source/OCI checksums and
both successful and failed additional trials. Current conservative commitment
is 0.24524130 USD (about 0.2189 EUR); known tariff estimate is 0.03536226 USD.
All thirteen databases are fenced and the temporary key is absent, as checked
in the [terminal state](evidence/p3-pr-terminal-state-01.json).

## Original completed P3 campaign, 2026-10-06

The original [49-file inventory](evidence/p3-final-live-source-06.json), SHA-256
`a4acc505d8c4ce96ba562ae2be74dcf401da93d6c3ff2f279b9313556a801173`, passed
153 workspace tests with 106 ignored, 44 API/Store regressions, two authority
tests in 5.06 s and coordinator integration in 15.91 s. Domain/configuration
contracts, 24 protected factory contracts and strict agents Clippy passed.
OpenAPI contains 43 paths / 52 operations; schema validator 0.8.4 passed.

Real NVIDIA seven-role acceptance passed in 34.18 s with four overlapping
executors. Factory02 passed in 319.85 s with eleven protected checks, actual
Cargo/gVisor/PostgreSQL, an independent attestor, signed Evidence/Release and
artifact reads with pool size one. Its source digest is
`e5e1115d4498b8563ea747886c9ef4d446eccadc3fdd718a4b15edfa99cac83d`.
The [NVIDIA report](NVIDIA.md) retains exports, exact archive checksums, every
failed trial and financial provisions. Those historical signatures do not
qualify changed runtime bytes.

Campaign controls passed 24 checks without network or vault access; eight SQL
guard checks passed with rollback. Historical archives were reduced after
excessive provider fields were found; before/after hashes remain. Decoded
secret checks and exact response-model pricing prevent the reproduced bypasses.

The original read-only review reached 9.5/10 on the predefined P3 rubric and
9.4/10 on the reviewer's general rubric. Metadata authority, nested pool reads,
sequential component removal and qualification archive defects were corrected
with reproductions retained. The updated PR review is separate.

## Validation boundaries

Admission signatures for catalogue 0.2.0 are fixtures, not renewal of all P2
admissions. Local verified development candidates do not establish production
qualification or deployment. Provider weight versions and standard retention
are unknown; only synthetic planning/generation/review data is authorized.
Unknown effects stay reserved without automatic resend. Tariff estimates and
conservative provisions include failed trials, but invoices, infrastructure and
Codex costs are unknown. No comparative gain is claimed. The cumulative
authorized provider ceiling remains 1 EUR.
