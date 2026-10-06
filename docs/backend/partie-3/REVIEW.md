# Independent P3 review

This independent score covers the sources published in `d265d9a151`, before
the two subsequent [PR #6 review fixes](PR-REVIEW.md). Those fixes have targeted
regressions and a manual diff review; no new independent score is claimed.

## PR integration review

The same read-only review agent is checking the final PR against `main`
`a12e1ced89854bc5716b50ca4bbd1c10a8d81ee5`. Its current
[61-file inventory](evidence/p3-pr-source-01.json) has SHA-256
`63552cb5bfe220bcea60b08d83d498d50d02f2f0a440955fe8f332d8978334be`.
This includes the earlier P3 inputs, the integrated upstream code changes and
the evidence Git attributes; documentation is excluded.

**PASS: 9.5/10 on the P3 rubric, 9.4/10 on the reviewer's general rubric.**
No actionable residual finding, critical defect or high defect was confirmed
in the reviewed scope. The new rating applies to the indexed PR changes and
the integrated main snapshot above, separately from the original review below.

The reviewer confirmed upstream application files and migration bytes are
preserved, protected drivers are present, and the evidence bytes still match
their published checksums. It also checked the new integration test results
and retained Factory03 refusal. Factory04 passed in 406.95 s. It independently
verified five export checksums, RS256 Evidence/Release signatures, exact
run/job/artifact/revision/candidate bindings and eleven protected checks.
The durable archive contains 197 source files including 191 runtime files
equal to the worktree, 38 migrations and three verified OCI blobs.

P3 scores: coverage 1.8/2, correctness/atomicity 2/2, authority/evidence 2/2,
concurrency/recovery 1.9/2, tests/documentation 1.8/2. General scores:
correctness 3/3, validation 1.7/2, security 2.9/3, compatibility 0.9/1,
maintainability 0.9/1. Remaining limits are local validation, fixture
admissions, remote CI and general P2 requalification. The retained worker
`lease_conflict` warning has no established precise cause; acceptance and
terminal bindings passed.

The reviewer recouped [financial totals](evidence/p3-pr-budget-01.json) against
thirteen closure receipts and terminal states: conservative provision
0.24524130 USD, about 0.2189 EUR, with all unknown reserves held. It did not
access the key or call the provider. The supervisor's
[publication check](evidence/p3-pr-publication-check-01.json) records the actual
credential/decoded JSON/complete private-key scan without retaining the key.
The [terminal check](evidence/p3-pr-terminal-state-01.json) confirms its removal.

## Original completed review

Review recorded on 2026-10-06, following the local NVIDIA campaign. The single
review agent worked read-only; implementation and fixes were made by the main
agent. **PASS: 9.5/10 on the predefined P3 rubric**, with no confirmed actionable
residual finding and no critical/high defect identified in the reviewed scope.

| Category | Score |
| --- | --- |
| Functional coverage | 1.8/2 |
| Correctness and atomicity | 2/2 |
| Authority and evidence | 2/2 |
| Concurrency and recovery | 1.9/2 |
| Tests and documentation | 1.8/2 |
| **Total** | **9.5/10** |

The review agent's general rubric also passed at 9.4/10. Reservations concern
local validation scope, bounded native JSON transport, the dated campaign
launcher, remote CI and the absence of general P2 requalification.

Reviewed [source inventory](evidence/p3-final-live-source-06.json): **49/49 files**,
SHA-256 `a4acc505d8c4ce96ba562ae2be74dcf401da93d6c3ff2f279b9313556a801173`,
initial P2 base `30f36318a9b2812d17f381ced739dd8b8a565484`.
This review precedes the later integration of upstream P2 corrections; it does
not automatically attest changed runtime bytes.

The reviewer independently checked the five historical Factory02 exports,
RS256 Evidence/Release signatures, eleven protected criteria and bindings,
196 bundle files including 190 runtime sources, three OCI blobs, supplier
receipts, executor overlaps and financial totals. It used only read operations
and in-memory counterexamples, without a provider call or access to the key.

Confirmed findings and corrections:

- Metadata events incorrectly required write; restricted server-owned metadata
  events now preserve read/execute authority. Reproduction retained.
- Nested transaction reads failed with pool size one; transition reads use the
  active connection. Sequential replacement/removal scopes were also corrected.
- Qualification archives retained arbitrary provider fields; allowlist
  projection and historical redaction preserve measurements without those fields.
- Unicode-escaped credential echoes could bypass raw filtering; decoded receipts
  are also checked. An absent secret guard is rejected.
- Unbound response models could release a priced reservation; exact string
  identity is now required before recording an estimate. Mismatch retains funds.
- The factory Seal guard was restored and tested with rollback; an earlier
  unexpected source reversion has no demonstrated cause and is retained in the
  local journal. Final hashes were checked repeatedly.

[NVIDIA evidence and retained failures](NVIDIA.md), [validation](VALIDATION.md),
[24 campaign controls](evidence/p3-campaign-controls-06.txt) and
[eight SQL guard checks](evidence/p3-seal-guards-02.json) provide the boundaries
of this review. Invoices, model weight versions and retention remain unknown.
