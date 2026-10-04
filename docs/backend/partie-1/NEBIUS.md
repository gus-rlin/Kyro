# Nebius P1 local integration

P1 now has a separate Nebius registry, in `config/models.nebius.example.json`. The default registry remains empty, and the synthetic test provider remains separate. This backend change does not connect the desktop chat to the Rust HTTP API.

The candidate is `nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B`, selected from the authenticated catalogue on 2026-10-04 by the lowest price for 2,048 input and 512 output tokens, then ordinal model ID. Its listed region is Finland; the weight revision and actual serving region are unknown. Selection remains provisional because the catalogue did not establish strict JSON Schema support for this exact model. The example is deliberately unqualified and cannot emit inference requests.

## Prerequisites and preparation

Use Windows, PowerShell 7, Node.js 24 and Docker Desktop with Linux containers, from this worktree's root. Ports 58090, 59090 and 59091 and subnets 10.248.73.0/24 and 10.248.74.0/24 must be available. OIDC is a local test fixture.

```powershell
docker build --tag kyro-nebius-p1:local .
docker build --tag kyro-nebius-guard:local --file ops/nebius/Dockerfile.guard ops/nebius
docker build --tag kyro-nebius-oidc:local --file tests/fixtures/Dockerfile.synthetic-provider .
pwsh -NoProfile -File scripts/nebius-vault.ps1 -Action ImportStdin
pwsh -NoProfile -File scripts/nebius-runtime.ps1 -Action Prepare
pwsh -NoProfile -File scripts/nebius-runtime.ps1 -Action Status
```

Import uses masked interactive input. Never put the key in an argument, `.env`, Docker environment variable or image. Re-import replaces the local copy; `-Action Remove` deletes it without revoking the provider key. The vault is DPAPI CurrentUser encrypted, with owner-only permissions, at `%LOCALAPPDATA%\Kyro\secrets\nebius.dpapi`.

Private runtime state lives at `%USERPROFILE%\.kyro\nebius-p1`, accessible to the owner and SYSTEM for Docker Desktop. It contains database credentials and TLS material, but no Nebius API key. `Prepare` freezes catalogue metadata, pricing, IP pins and a dated ECB exchange rate. It refuses to reset an existing campaign. Preserve its PostgreSQL volume and campaign files together.

## Qualification gate and budget

`Start` requires a private `qualification.json` with the exact endpoint and model, SHA-256 of the encrypted vault, an ISO `checked_at` within 30 days, an official HTTPS `source`, and these established facts: `zero_retention_confirmed`, `json_schema_supported`, and `max_completion_tokens_includes_reasoning`. None is established merely by setting a boolean. Evidence must apply to this account, model and endpoint.

Example, intentionally disabled:

```json
{
  "endpoint": "https://api.tokenfactory.nebius.com/v1/",
  "model": "nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B",
  "vault_sha256": "SHA256_OF_ENCRYPTED_VAULT",
  "checked_at": "ISO_DATE_WITH_TIMEZONE",
  "zero_retention_confirmed": false,
  "json_schema_supported": false,
  "max_completion_tokens_includes_reasoning": false,
  "source": "https://docs.nebius.com/legal/token-factory"
}
```

`store: false` alone does not establish zero retention. Consult the [endpoint contract](https://docs.tokenfactory.nebius.com/api-reference/inference/create-chat-completion) and [applicable provider terms](https://docs.nebius.com/legal/token-factory), and retain account-specific evidence. If this candidate fails qualification, select again among confirmed compatible models before activation.

The qualification uses one persistent project, at most three short requests, and a EUR 1 ceiling converted to nano-USD at the dated ECB rate. A 40% margin remains inside that ceiling. The 2026-10-02 rate produced a USD 0.6735 admission limit. Failed attempts count; any uncertainty stops the campaign. Because the tokenizer revision is unknown, the initial reservation covers the full advertised input context plus bounded output. Provider formatting has an additional 1,024-token allowance in the request preparation. Billing amounts remain unknown until supported by billing evidence. Other uses of the account are outside this campaign ceiling.

After qualification is established:

```powershell
pwsh -NoProfile -File scripts/nebius-runtime.ps1 -Action Start
node scripts/verify-nebius-p1.mjs --run
```

Each run admits one fictitious request through HTTP and the PostgreSQL queue, checks structured output, durable effect, provider receipt, usage and settlement, then replays admission with the same idempotency key. The runner stops the qualification services in its cleanup. An uncertain result retains its reservation and forbids automatic re-emission. Synthetic reconciliation cannot release a Nebius reserve. Do not remove campaign locks or uncertainty markers to retry.

## Isolation checks and shutdown

Without enabling inference, after preparation:

```powershell
$env:KYRO_NEBIUS_STATE = "$env:USERPROFILE/.kyro/nebius-p1"
docker compose --file compose.p1.nebius.yaml up -d --wait postgres egress
docker compose --file compose.p1.nebius.yaml --profile migration run --rm migrate
docker compose --file compose.p1.nebius.yaml up -d api oidc
node scripts/verify-nebius-network.mjs
node scripts/verify-nebius-p1.mjs --preflight
pwsh -NoProfile -File ops/nebius/verify-worker.ps1
pwsh -NoProfile -File scripts/nebius-runtime.ps1 -Action Stop
```

Network checks use curl/openssl without a key, independently of Rust. The worker check uses a fake key and refuses an enabled registry. Neither proves live NVIDIA inference.

Only the secret-free guard installer receives NET_ADMIN; its long-lived process drops capabilities. The worker shares its filtered network namespace, starts only after successful rule installation, runs without capabilities as UID 10001, and has a read-only filesystem, resource quotas and no Docker socket. Its real key travels through stdin into owner-only tmpfs. Egress permits dedicated PostgreSQL and explicitly validated Nebius IPv4 pins on port 443; runtime DNS, metadata endpoints, other destinations and IPv6 are denied. HTTPS verifies certificates, refuses redirects and ignores proxies.

Always stop after qualification or interruption:

```powershell
pwsh -NoProfile -File scripts/nebius-runtime.ps1 -Action Stop
```

This removes the worker and its secret tmpfs, stops the dedicated services and preserves durable budgets. `-Action RefreshPins` explicitly validates new addresses after stopping/removing worker and guard; it does not restart them.

Recognizable secrets and forbidden categories are rejected before queue persistence and checked again by the worker. This is a limited denylist, not exhaustive DLP. The vault protects local copies; it cannot remove a key already shared in a conversation or protect against a compromised host administrator. Automated provider tests are synthetic. Live activation remains blocked until the missing qualification evidence exists.
