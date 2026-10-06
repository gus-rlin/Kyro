# Part 3 — Durable planning and controlled agents

P3 turns a project request into a versioned catalogue plan, bounded microtasks,
structured changes, two reviews and an independently verified candidate. The
Rust coordinator uses P1 for jobs, effects and accounting, and P2 for catalogue
admission, composition, isolated construction and signed verification.

Pixel, Moka, Kiwi and Biscotte each run at most one microtask at a time. The
orchestrator uses the most expensive compatible NVIDIA model in the operator's
retained registry; executors, review and security use its cheapest compatible
NVIDIA model. The comparison uses a declared 1:1 input/output price profile,
equal currency and unit scale. This is a selection rule, not evidence of cost
or quality gains. Synthetic roles are allowed only in development.

Implementation lives in `kyro-domain::agents`, `kyro-agents`, `kyro-store::agents`
and `kyro-api::agents`. There is no model-generated executable code. Models
select admitted versions and propose declarative `ChangeSet` operations.
Permissions, provider configuration, source code, verification criteria and
release evidence are server-owned.

- [Contracts and HTTP commands](CONTRATS.md)
- [Architecture and recovery](ARCHITECTURE.md)
- [Operator setup and reproduction](OPERATIONS.md)
- [Acceptance criteria and evidence boundaries](ACCEPTATION.md)
- [Executed validation and limitations](VALIDATION.md)
- [Real NVIDIA campaign and retained failures](NVIDIA.md)
- [Independent review and source bindings](REVIEW.md)
- [Versioned OpenAPI](../partie-1/openapi.v1.json)

P3 currently creates development candidates. Publication and production
promotion belong to P4. A `verified` plan is not a deployment.

The isolated implementation starts from P2 commit
`30f36318a9b2812d17f381ced739dd8b8a565484`. Before PR publication it incorporates
the later P2 corrections from `main` at
`a12e1ced89854bc5716b50ca4bbd1c10a8d81ee5`, including additive application
migration 0038 and all three protected Docker drivers. Validation before and
after that integration is recorded separately. P3 runtime changes require new
catalogue subjects: version **0.2.0** starts pending. Historical P2 admissions
do not qualify these source bytes.
