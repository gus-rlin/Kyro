# P3 validation status

Current results are local. This document is updated after the final recipes and
independent review; pending evidence must not be read as a delivered capability.

| Check | Observed result | Limits |
| --- | --- | --- |
| Domain contracts | 7/7 pass | Pure deterministic contracts, including sequential component replacement |
| Economic role configuration | 1/1 pass | Admission metadata fixtures; no real prices/provider availability |
| PostgreSQL + HTTP coordinator | Pass,15.91s | Synthetic model provider; real roles, four-way barrier, contract retention and forced worker death |
| Authority and single-connection pool | 2/2 pass,5.06s | Concurrent coordination, read/execute without write, restricted metadata helper, protected-artifact read covered separately |
| Protected P3 factory | NVIDIA pass,319.85s, eleven protected checks | Actual gVisor/Cargo/PostgreSQL/attestor and NVIDIA inference, artifact read with pool size1; catalogue admission fixtures; exact public trust/sources/OCI retained |
| Workspace tests | 153 pass,106 explicitly ignored | Service-dependent recipes executed separately below; not all P2 recipes rerun |
| P1 API/Store regressions | 44 pass, zero ignored | Real isolated PostgreSQL, queue/effects/migrations/artifact fences |
| Protected factory contracts | 24 pass, zero ignored | Tampering, signatures, admission, source/OCI/Git and receipt bindings |
| OpenAPI inventory and schema | 43 paths /52 operations, schema validator0.8.4 pass | Purpose-scoped unknown-retention consent added explicitly to DataPolicy; seven agent limit fields required |
| Formatting and agents Clippy | Pass | Strict all-targets --no-deps for agents; pre-existing P2 lints remain |
| Docker build context | Pass | Three required protected drivers included; full production image not rebuilt |
| NVIDIA inference | Seven roles pass34.18s; protected factory pass319.85s | Standard retention unknown, synthetic project data, six wire probes and all failed trials included in cumulative budget |
| Campaign ceilings, claims and archive redaction | 24 controls pass without provider; 8 SQL guard checks with rollback | All finished phase databases closed to sends, unknown reservations retained; invoices unavailable; excessive historical qualification archives reduced with before/after hashes; price estimates require the expected response model |
| Independent review >=9/10 | Current PASS9.5/10 on the predefined P3 rubric; general reviewer rubric PASS9.4/10 | Final 49-file inventory, runtime bytes, signatures, receipts and finances independently checked; no actionable residual finding confirmed; historical scores retained separately |

Coordinator observations include four simultaneous HTTP effects, read-only
planning, strict inputs, semantic ordering, deterministic transformations,
review rejection, source conflicts, harmless rebase and new reviews, atomic
rollback when the build queue is full, in-flight instruction/revocation,
compaction, forced worker termination with an unknown effect and no resend,
actor/RLS/CAS/history boundaries, and fair rotation over 33 active projects.
Compatible completed contracts survive revised instructions; a changed contract
is rerun independently. Cancelled jobs retain known effect usage and estimates.
CI has a separate P3 database and worker-recovery step; remote CI was not run.

Final protected source digest:
`e5e1115d4498b8563ea747886c9ef4d446eccadc3fdd718a4b15edfa99cac83d`.
The exact source/OCI archive, signed artifact and public trust are retained
locally with SHA-256 checksums. Historical factory runs 01/02 remain separate
and do not qualify the new source. Real receipts, token usage and tariff
estimates are retained; invoices and missing cache counts remain unknown.
The public [NVIDIA campaign](NVIDIA.md) records every
failed trial. Its conservative monetary provision is0.23874876USD (about
0.2131EUR at the dated exchange rate), including seven unknown effects and
the first call's missing response archive. Known tariff estimate is0.02886972USD;
neither figure is an invoice. The authorized ceiling is1EUR.

Intermediate failures are retained in the local journal and numbered reports.
They included catalogue IDs outside the backend scope, cancelled-job polling
assumptions, an unsupported preference in a fixture, distinctions between job
and effect states, and the expected NotFound visibility boundary after
revocation. These were corrected in fixtures or implementation and followed by
new runs; they are not erased.
The initial review exposed write-only metadata events, nested transaction reads
with a single-connection pool, and component relabelling before removal. A
restricted metadata-event helper, transaction-local reads and sequential scope
validation fixed the reproduced failures. OpenAPI now requires the same seven
limit fields as the strict Rust request. The final recipes ran after these fixes.

The catalogue version is 0.2.0 because P3 changes the runtime source subject.
Existing admissions do not qualify it. Factory recipe admission signatures are
fixtures and do not replace the P2 component qualification campaign. Local
artifact verification does not establish provider integration or deployment.
