# Independent P3 review

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
