# P3 acceptance

The following criteria were established before the acceptance runs. Local
synthetic integration, actual protected construction, real NVIDIA inference and
deployment are separate evidence levels.

| ID | Required behavior | Relevant recipe |
| --- | --- | --- |
| P3-01 | Versioned typed plan, read-only proposal, catalogue gap | domain contracts; coordinator |
| P3-02 | Graph, contracts, scope, component versions, semantic ordering, limits | domain; config; coordinator |
| P3-03 | Four independent effects, one active microtask per executor | coordinator with real PostgreSQL + synthetic HTTP |
| P3-04 | Model results become scoped ChangeSets; reviews bind exact candidate; build admission atomic | coordinator and protected factory |
| P3-05 | Sourced memory, targeted search, compaction, forced restart without blind replay | coordinator, killed worker process |
| P3-06 | New instruction/revocation during an emitted call, compatible retention, conflicting and harmless edits | coordinator |
| P3-07 | False success, missing proof, rejection, malicious fields and repeated failure cannot validate or exceed caps | HTTP/domain/coordinator; P2 evidence verification |
| P3-08 | Covered request produces a verified artifact with real NVIDIA models within authorized cost | PASS: seven roles34.18s; NVIDIA protected B031 factory319.85s, eleven checks, signed artifact |

Independent review rubric: functional coverage 2 points, correctness and atomic
state 2, authority/evidence boundaries 2, concurrency/recovery 2, tests and
documentation 2. At least 9/10 is required by the user. The reviewer must state
its own score, actionable findings and validation limits. A review score cannot
substitute for a missing real-provider acceptance run.

No P4 deployment, P5 maintenance, global cost/performance comparison, complete
P2 requalification or production eligibility is claimed by P3's local recipes.
