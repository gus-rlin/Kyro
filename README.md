# Kyro — backend de pilotage P1

Kyro sépare le contrat des applications Rust de l’API HTTP, de la persistance PostgreSQL, de la passerelle de modèles et du worker. Cette tranche fournit un runtime API versionné, des travaux persistés, un budget par projet, une lecture des effets, des commandes asynchrones de rapprochement et un flux d’événements SSE privé. Le fournisseur OIDC et les fournisseurs de modèles restent configurés séparément; aucun fournisseur NVIDIA réel n’est qualifié par ces contrôles.

## Développement local

Le dépôt utilise le toolchain Rust indiqué par [`rust-toolchain.toml`](rust-toolchain.toml). Une base PostgreSQL réelle est nécessaire pour les parcours d’intégration. Les scripts d’infrastructure et le schéma des rôles sont décrits dans [`docs/backend/partie-1/OPERATIONS.md`](docs/backend/partie-1/OPERATIONS.md); exécuter les migrations avec le rôle admin avant de lancer l’API ou le worker.

Copier [`.env.example`](.env.example) vers `.env`, ajuster le port PostgreSQL retenu par l’infrastructure locale, puis charger les variables dans le terminal. Exemple PowerShell :

```powershell
Get-Content .env | Where-Object { $_ -match '^\s*[A-Z_][A-Z0-9_]*=' } | ForEach-Object {
  $name, $value = $_ -split '=', 2
  Set-Item -Path "Env:$name" -Value $value
}
```

Les noms `local-only` et les endpoints de développement sont des valeurs synthétiques non utilisables en production. Les clés OIDC et modèles ne figurent pas dans le dépôt. Le fournisseur d’identité synthétique doit rester limité au développement.

Après la migration, démarrer séparément les deux processus :

```powershell
cargo run --locked -p kyro-api
cargo run --locked -p kyro-worker
```

Le serveur expose `GET /health/live` sans base de données et `GET /health/ready` avec validation du rôle runtime PostgreSQL. Voir le [contrat HTTP v1](docs/backend/partie-1/HTTP.md) pour les routes, les curseurs de reprise, les préconditions et les erreurs.

## Image runtime

Le [`Dockerfile`](Dockerfile) compile avec Rust 1.96.1 et produit les binaires API, worker et migrateur dans une image Debian minimale non privilégiée. Les images de base sont épinglées à des digests vérifiés le 2026-10-03. L’image démarre l’API par défaut; pour le worker et le migrateur, surcharger l’entrée avec `/usr/local/bin/kyro-worker` ou `/usr/local/bin/kyro-migrate`. Injecter séparément les identifiants runtime `KYRO_DATABASE_URL`, `KYRO_WORKER_DATABASE_URL` et `KYRO_DATABASE_ADMIN_URL` depuis un gestionnaire de secrets; ne pas fournir les credentials admin à l’API ni au worker.

L’API lit le registre de modèles en mode admission sans clé fournisseur. Seul le worker reçoit `KYRO_MODEL_API_KEY` depuis le gestionnaire de secrets; il vérifie la disponibilité réelle avant de réserver un budget ou d’envoyer. Le service API exige en production `KYRO_AUTH_MAX_ACTIVE_SESSIONS` (1–100000), l’origine `KYRO_AUTH_UI_ORIGIN` et les paramètres OIDC. Le worker ne reçoit pas les variables OIDC.

Les URLs de base de données doivent être injectées avec le rôle propre à chaque processus; le migrateur est le seul à recevoir la connexion admin. `KYRO_OIDC_SYNTHETIC_PROVIDER` et la simulation fournisseur loopback restent désactivés en production.

Pour construire une image locale une fois le `Cargo.lock` présent :

```powershell
docker build --tag kyro-p1:local .
```

Le démarrage d’un conteneur ou d’un service de production, la configuration TLS et la restauration restent des procédures d’exploitation distinctes. Aucune infrastructure ni migration n’est lancée implicitement par l’API.
