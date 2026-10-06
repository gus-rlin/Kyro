# P3 operations

1. Apply P1 migrations with `kyro-migrate`, including additive `0020`–`0023`.
   `kyro_api` and `kyro_worker` remain restricted forced-RLS roles.
2. Configure and qualify the P2 factory and a newly admitted catalogue subject
   for the P3 runtime. Do not reuse historical P2 receipts after source changes.
3. Supply `KYRO_AGENTS_CONFIG_FILE` to the API. The file chooses all seven roles,
   synthetic mode and polling interval. Missing configuration leaves commands
   unavailable; persisted state remains readable.
4. Supply the trusted model registry to API and workers through
   `KYRO_MODEL_REGISTRY_PATH`. Only workers receive
   `KYRO_MODEL_API_KEY_FILE`. The API loads admission metadata and the composition
   signer; the separate attestor holds evidence/release signers.
5. Explicitly configure project data policy, model/write/execute authority,
   financial budget and job limits. P3 does not increase them automatically.
   A protected build may require the project's job TTL to be 900 seconds;
   the local protected recipe sets that limit explicitly.

Operator role file example:

```json
{
  "synthetic": false,
  "poll_ms": 1000,
  "roles": {
    "orchestrator": {"destination_id":"qualified-nvidia","model":"nvidia/retained-highest-price-model"},
    "pixel": {"destination_id":"qualified-nvidia","model":"nvidia/retained-lowest-price-model"},
    "moka": {"destination_id":"qualified-nvidia","model":"nvidia/retained-lowest-price-model"},
    "kiwi": {"destination_id":"qualified-nvidia","model":"nvidia/retained-lowest-price-model"},
    "biscotte": {"destination_id":"qualified-nvidia","model":"nvidia/retained-lowest-price-model"},
    "review": {"destination_id":"qualified-nvidia","model":"nvidia/retained-lowest-price-model"},
    "security": {"destination_id":"qualified-nvidia","model":"nvidia/retained-lowest-price-model"}
  }
}
```

These IDs are placeholders, not a qualified catalogue or provider configuration.
Versions and dated prices must come from the actual retained provider registry.
P1's unknown-tokenizer/context reservation remains authoritative. Set the run's
token cap accordingly; monetary reservations are a separate fence.

Reproduce contracts without external services:

```sh
cargo test --locked -p kyro-domain --test agents
cargo test --locked -p kyro-agents --test config
node scripts/p3/update-openapi.mjs
node scripts/check-openapi.mjs
```

For the coordinator recipe, use a **fresh disposable database** whose name
starts with `kyro_p1_test_`, migrate it, and configure:

```sh
export KYRO_TEST_DATABASE_ADMIN_URL=postgres://kyro_admin@localhost/kyro_p1_test_p3
export KYRO_TEST_DATABASE_URL=postgres://kyro_api@localhost/kyro_p1_test_p3
export KYRO_TEST_WORKER_DATABASE_URL=postgres://kyro_worker@localhost/kyro_p1_test_p3
export KYRO_P3_WORKER_BIN="$PWD/target/debug/kyro-worker"
cargo build --locked -p kyro-worker --bin kyro-worker
cargo test --locked -p kyro-agents --test coordinator -- --ignored --nocapture
```

The fixture starts a local synthetic HTTP provider, generates disposable signing
keys outside the source tree and uses explicitly labelled catalogue signatures.
It kills its own worker; it does not affect other workers or databases. Keep a
failed run's database and log, then use a new database after fixture changes.

The `authority` recipe needs a separate fresh database and the same PostgreSQL
role URLs. Run it with `cargo test --locked -p kyro-agents --test authority --
--ignored --nocapture --test-threads=1`. It checks concurrent transitions with
pool size 1, read/execute grants without write, and refusals for a forged actor,
future event version and worker-role helper access. These are local authority
checks; they do not call a cloud model.

The separate `factory` recipe additionally needs the prepared immutable P2
Docker/gVisor tools root, a controller with Docker supervision access, a mounted
`/workspace` and `/tools-root`, and a compiled `/workspace/target/debug/kyro-attestor`.
Use `KYRO_P3_SANDBOX_CONFIG` for the trusted image/volume/root digest. The tested
controller and workload boundaries are described in P2's operations guide.

```sh
cargo build --locked -p kyro-factory --bins
cargo test --locked -p kyro-agents --test factory -- --ignored --nocapture
```

Use a separate fresh database for that recipe. Optional
`KYRO_P3_FACTORY_PROOF_DIR=/tmp/kyro-p3-factory-proof-NAME` keeps sanitized public
JSON evidence, public verification keys, and a tar archive containing only
the exact generated sources and OCI candidate. Keep the archive checksum and
public trust together with the signed artifact. No private keys or
session/database credentials are exported.

The user authorized at most **1 EUR** for the real-provider campaign. No live
call is allowed before a usable local credential, qualified registry, applicable
retention policy and hard monetary ceiling are in place. The published P1
example registry is unqualified and must stay so until evidence exists.

### Recorded real-provider recipe

The Windows launcher `scripts/p3/nebius-campaign.ps1` is a dated local
acceptance tool. It reuses the existing DPAPI vault through the project's
helper, keeps the registry and cumulative budget outside Git, and transfers
the key through stdin into the dedicated controller's tmpfs. Never pass the
key as a command argument or save it in an example registry.

`Prepare` reads the real catalogue and creates an unqualified private registry;
`Native` installs the closed v2 contract. `Qualify` performs an explicitly
budgeted synthetic Review wire probe for each model. Native strict JSON is the
final observed mode. JSON object mode was also tested, with the trusted schema
in the system prompt and unchanged server validation; its failed trials are
retained. Neither wire probe qualifies an agent's business behavior.

`Reserve -Phase NAME -PhaseCalls 7` reserves at most100000000nanoUSD for a
seven-role phase; use4 for the factory. Create and migrate its fresh database
`kyro_p1_test_p3_live_NAME`, replacing hyphens with underscores. `Deliver`
loads the key into the owned controller; `Run -Phase NAME -Scenario SevenRoles`
or `Factory` executes only the matching ignored acceptance test. All model
requests use synthetic project data, explicit purpose-scoped unknown-retention
consent, one attempt,8192output tokens and120000ms deadlines.

Each phase is claimed once by a persistent CreateNew file and ledger state.
Never relaunch it after a failure or crash. An unknown effect keeps its full
P1 reservation. `Seal -Phase NAME` is an operator-only transaction in that
phase's database: it requires terminal jobs/effects, closes future sends,
lowers the project ceiling to spent+reserved and returns a trusted receipt.
Only unissued capacity can be reclaimed; model reports cannot release funds.
Preserve all failed databases, receipts and the private cumulative ledger.

Qualification archives keep only bounded receipt identifiers, expected-model
matching, numeric usage, and the exact verified fixed Review probe. Arbitrary
provider messages, reasoning, accessory fields and error bodies are excluded.
The in-memory credential is checked against both raw transport and the decoded
receipt before saving; Unicode escapes cannot bypass that check. Parse failures
use fixed diagnostics. `scripts/p3/test-campaign.ps1` exercises budget/claims,
redaction and escaped-secret rejection without vault access or network calls.
The historical excessive archives and their reduction are recorded in the
campaign report; do not describe them as having never contained those fields.
Measured usage releases a qualification reservation only after the response
model is a string equal to the requested model. An unbound identity keeps the
entire reservation and makes the campaign uncertain, preventing further sends.

The existing public TLS root may be supplied only with
`KYRO_MODEL_TLS_ROOT_CERTIFICATE_FILE` when a local TLS inspection chain
requires it. The option adds trust for that single certificate; HTTPS,
hostname, chain and pinned-address checks remain active. Admission mode does
not read the file. Do not disable TLS verification.

The precise controller/toolchain/CA/model identities, budget, failures and
current real-provider evidence are documented in
[the NVIDIA campaign report](NVIDIA.md).
