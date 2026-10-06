# P3 contracts

All routes are under `/v1/projects/{project_id}/plans`. Requests use the existing
P1 session and CSRF rules. Reads require current project visibility; mutations
require the original actor and current execution authority. Model admission and
integration separately recheck model and write authority. Models cannot submit
run status, results, evidence, provider keys or grants through these endpoints.

| Route | Method | Behavior |
| --- | --- | --- |
| collection | GET | Latest 32 actor-visible runs |
| collection | POST | Capture AppSpec revision and queue orchestration |
| `/capabilities` | GET | Actor-visible configured roles, executor count and tools; no authority or budget is granted |
| `/{run_id}` | GET | Current authoritative run |
| `/{run_id}` | DELETE | Cancel active work; keep history and effects |
| `/{run_id}/advance` | POST | One deterministic transition |
| `/{run_id}/execute` | POST | Execute a validated read-only proposal |
| `/{run_id}/contract` | PUT | Replace a proposal through all deterministic checks |
| `/{run_id}/instructions` | POST | New request/epoch; retain compatible completed work |
| `/{run_id}/compaction` | POST | Persist a checkpoint for one named role |
| `/{run_id}/memory?q=…` | GET | Up to 16 ranked sourced observations |
| `/{run_id}/history?before=…` | GET | Up to 16 earlier versions; paginate before the last version |
| `/{run_id}/usage` | GET | Per-call registration, nullable usage and tariff estimates |

Creation requires `Idempotency-Key` and `If-Match: "rev-N"` for the AppSpec
revision. All other mutations use that header format for the **run version**,
which is independent of the AppSpec revision. Replay returns the same run after
fresh authority checks. A changed payload under the same key is a conflict.
Missing preconditions return 428; a changed run version returns 409.

```json
{
  "request": "Build a catalogue-only records application",
  "limits": {
    "max_calls": 32,
    "max_tokens": 2000000,
    "max_output_tokens": 4096,
    "call_timeout_ms": 30000,
    "ttl_seconds": 1800,
    "max_task_attempts": 2,
    "context_bytes": 48000
  },
  "plan_only": true
}
```

`plan_only` permits planning metadata and a controlled model call. It does not
write the application specification, application data or admit a build.

The registry uses structured schema `kyro-agent-contract`. Version `2` places a
native JSON object in `data.contract`, selecting the closed `Plan`, `TaskResult`
or `Review` branch. Version `1` remains readable as a bounded JSON-string for
already admitted jobs; version coercion is refused. The versioned native schema
is in `crates/kyro-agents/src/contract-v2.schema.json`, regenerated with
`node scripts/p3/update-schema.mjs`. The gateway validates bounded flat unions
and maps; Rust then enforces role, scope and catalogue semantics. Unknown
contract fields, secrets, shell/code operations and out-of-scope writes are refused.

The native transport keeps P1's 32 KiB schema, 512-node and 16-level JSON bounds.
Dynamic values allow at most two nested containers before scalar leaves, with
128 object entries and 256 array items. More deeply nested configurations need
a separately qualified transport; the coordinator does not silently truncate
them. Version 1 retains its existing limits. A native contract is capped at
32,000 encoded bytes; the outer provider response has its own lower policy cap.

Task resources are whole nodes, node properties or preferences. Whole-node
writes overlap every property. Absence differs from explicit null; property
observations include node existence/kind. Node additions, removals and version
changes must match declared components. Server-owned capabilities and protected
criteria override supplied values. P2 finally checks declarative configuration,
versions, dependencies and semantic bindings.

Calls, reservations and deadline remain cumulative across new directives,
compaction and restart. A repeated failure or exhausted attempt/call/token cap
blocks the plan. P1 separately fences financial reservations and uncertain
effects. Estimates do not represent invoiced costs; unavailable measurements
remain null. `effect_elapsed_ms` measures the stored effect lifecycle, including
settlement, rather than pure inference latency. `provider_request_id` preserves
the opaque provider receipt when supplied.

Creation currently refuses production. A verified candidate has a signed P2
artifact for the exact integrated revision; it has not been published.
