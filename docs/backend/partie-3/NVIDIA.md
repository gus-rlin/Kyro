# P3 NVIDIA acceptance

The 2026-10-06 local campaign exercised real NVIDIA models through Nebius on
synthetic projects. Seven roles passed in **34.18 s**, including four overlapping
executor effects. A minimal covered B031 request then produced an independently
verified candidate in **319.85 s**, with **eleven protected checks**.

These results identify the implementation snapshot before integration of the
later P2 corrections on `main`. Its [49-file inventory](evidence/p3-final-live-source-06.json)
has SHA-256 `a4acc505d8c4ce96ba562ae2be74dcf401da93d6c3ff2f279b9313556a801173`.
Updated integration results must be recorded separately when those bytes change.

## Provider and limits

| Role | Model | USD per million input/output tokens |
| --- | --- | --- |
| Orchestrator | `nvidia/nemotron-3-super-120b-a12b` | 0.30 / 0.90 |
| Other six roles | `nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B` | 0.06 / 0.24 |

[Provider catalogue](evidence/p3-nvidia-catalogue-01.json), consulted 2026-10-06.
Both models offer 262144-token context; available quantization is FP4 and FP8
respectively. Weight versions are unknown. Standard provider retention remains
unknown, explicitly accepted only for planning/generation/review of these
synthetic development projects. Production is not enabled.

Each task has one attempt, at most 8192 output tokens and a 120000 ms deadline.
P1 reserves the full provider context. A phase is claimed once using a persistent
CreateNew file and ledger state. Unknown effects keep their entire reservation
and are never automatically resent. Operator SQL `Seal` disables future sends
before reclaiming capacity that was never engaged.

The user's cumulative ceiling is **1 EUR**. The dated ECB rate is 1.1204 USD/EUR;
a 40% margin gives an internal ceiling of 0.67224 USD. The
[financial summary](evidence/p3-nvidia-budget-final-01.json) includes every failed
trial, wire probe and unknown effect:

- Known tariff estimate: **0.02886972 USD**.
- Seven unknown-effect reserves: **0.19218432 USD**.
- First response without an archive: **0.01769472 USD** provisioned in full.
- Total conservative commitment: **0.23874876 USD**, about **0.2131 EUR**.

These figures are not invoices. Invoice, infrastructure and Codex costs are
unknown. No comparative cost, quality or performance gain is claimed.

## Retained trials

| Trial | Observed result |
| --- | --- |
| First string probe | HTTP 200 but erroneous verifier property; no response archive, full reserve retained |
| Second string probe | Non-JSON inner content rejected; usage measured |
| Native and JSON-object probes | Nano/Super fixed Review wire contracts qualified; no business approval inferred |
| roles01 | TLS failed before emission; no inference sent |
| roles02–05 | Wrong/empty contracts or scopes rejected; unknown responses kept reserved |
| roles06 | Seven known responses and four valid parallel changes; Review refusal correctly blocked integration |
| roles07 | Three responses became unknown; no automatic replay |
| roles08 | Invalid storage node kind rejected |
| [roles09](evidence/p3-nvidia-seven-roles-09.json) | Seven real roles, accepted changes and reviews, four overlapping effects; no build attributed to this phase |
| factory01 | Ten-component plan received, executor response lost; no build and unknown reserve retained |
| [factory02](evidence/p3-nvidia-factory-final-02/report.json) | B031, four real calls, two reviews, protected construction and signed verified artifact |

Numbered supplier logs and all eleven SQL receipts are in `evidence/`. The
[final effects inventory](evidence/p3-nvidia-effects-final-01.json) contains
35 known runtime responses, seven unknown effects and one definitely unsent TLS
failure. Six HTTP-200 wire probes are additional; a billed call count is unknown.

## Verified artifact

The [artifact](evidence/p3-nvidia-factory-final-02/artifact.json),
[public trust](evidence/p3-nvidia-factory-final-02/public-trust.json),
[run](evidence/p3-nvidia-factory-final-02/run.json) and
[export checksums](evidence/p3-nvidia-factory-final-02/sha256.json) preserve the
Evidence/Release signatures and run/job/revision bindings.

- Source: `e5e1115d4498b8563ea747886c9ef4d446eccadc3fdd718a4b15edfa99cac83d`.
- Image: `sha256:a449b618b098ba9157d53b471fe53b2fbf513de4386891ac2d6bd64c760a94eb`.
- Release: `a6586ac6af6536749ecdd3057f428e54054e88a65d05957c18f13d482a653a21`.
- Exact source/OCI archive: 72263680 bytes, SHA-256
  `705deacdd728c56437ea21586c21268850ce2669571f97d70c91cba7e72f047c`.

The large tar is retained by the operator outside Git at
`Kyro_v2-p3-releases/review-20261006/sources-and-candidate.tar`, next to this
project's directory. It is not downloadable from this PR. Request that exact
archive from the operator and check the SHA-256 above before independent
source/OCI inspection. Reproducing the protected recipe in [OPERATIONS](OPERATIONS.md)
creates a new candidate and cannot recreate a historical model response.

Actual Cargo/gVisor/PostgreSQL and an independent attestor are exercised.
Catalogue admission signatures remain fixtures; this is not general P2
requalification, production qualification or deployment.

## Review corrections and archive minimization

The final reviewer found excessively broad qualification archives, an escaped
secret bypass in the raw-response filter, and pricing before response-model
identity was confirmed. All were reproduced and corrected: archive fields are
allowlisted, the decoded projection is checked against the in-memory credential,
and usage cannot release a reserve unless the response model is the exact
expected string. Parsing errors use fixed diagnostics.

Historical provider reasoning was present in one public and two private
qualification archives. Eight archives were reduced; their
[before/after hashes](evidence/p3-qualification-redaction-01.json) retain the
correction history without publishing the removed content. No claim is made
that those fields were never recorded.

[Campaign checks](evidence/p3-campaign-controls-06.txt): **24 PASS** without vault,
network or inference. [SQL guards](evidence/p3-seal-guards-02.json): **8 PASS**
with rollback, foreign project/destination refusal and no persisted change.
[Terminal state](evidence/p3-final-state-01.json): eleven databases fenced,
unknown reserves retained and temporary provider credential removed.
