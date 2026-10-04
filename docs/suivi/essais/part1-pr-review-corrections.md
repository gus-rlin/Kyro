# Corrections des remarques de la PR #1

Date : 2026-10-04 (Europe/Paris). Branche `gus-rlin/p1-delivery`, worktree propre au départ, base `d8fdc9c`, neuf commits fonctionnels ciblés jusqu'à `77105d3`. Skill senior-code-basics appliqué. Checkout racine, serveur de développement et migrations 0001–0016 conservés.

## Changements minimaux

| Remarque | Correction | Commit | Vérification |
| --- | --- | --- | --- |
| Exemples env absents du clone | Exceptions .gitignore et deux exemples sans secret suivis | 2683916 | git ls-files .env* ; plus ignorés |
| Licence Cargo incohérente | SPDX Apache-2.0 au workspace | a6051cf | cargo metadata : cinq packages Apache-2.0 ; LICENSE intacte |
| ChangeSet 64 au lieu de 128 | maxItems 128 et garde du validateur OpenAPI | 2c7a909 | check-openapi, runtime inchangé |
| Resources de grant trop larges | maxItems 1 et garde du validateur | 7c52e90 | check-openapi, runtime inchangé |
| URL PostgreSQL TCP de production non authentifiée | Environnement transmis au parseur ; verify-full et CA explicite non vide, aliases/priorité SQLx, sockets Unix conservés | 87520d4 | régression pour API/worker : absence TLS, modes faibles, CA absente/vide, aliases de downgrade et override TCP refusés |
| Registre de production absent | Même fichier externe contrôlé et read-only pour API/worker | aaa25bb | Compose résolu : même source/cible, read_only, clé seulement au worker |
| CORS navigateur absent | tower-http déjà verrouillé 0.6.11, origine exacte avec credentials, méthodes/en-têtes bornés, ETag exposé | 9e5bca9 | test UI3000 cross-port, prérequêtes, refus origine étrangère ; contrôle HTTP API réelle |
| Migrateur limité par les timeouts runtime | Pool privé à une connexion, conserve les timeouts serveur/opérateur | 6c0b0b6 | pg_sleep(5.1) réussit ; overrides SQLx 10s/7s conservés ; runtime reste 5s/2s |
| Destinations sans clé impossibles | Absence de secret_ref admise si qualified ; Authorization ajouté seulement si secret ; référence déclarée sans valeur reste désactivée | 77105d3 | admission/config keyless ; un vrai envoi loopback, aucun Authorization ni canary ; refus et modes admission existants conservés |

## Preuves et commandes

[Contrôles Linux/Docker](../preuves/part1-pr-review-controls-20261004.json) et [log complet](../preuves/part1-pr-review-ci-c-20261004.txt) : 79 tests passés, aucun échec/ignoré, sous Rust 1.96.1, PostgreSQL 18.6, Docker 29.8.0. Source montée en lecture seule, base dédiée `kyro_p1_test_pr_review_20261004_c`. Les quatre commandes passent :

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo run --locked -p kyro-store --bin kyro-migrate
cargo test --locked --workspace -- --include-ignored
```

Le pool du migrateur conserve les timeouts configurés par l'opérateur dans PostgreSQL ou via les options d'URL SQLx (par exemple options[statement_timeout] et options[lock_timeout]). Il ne remplace plus ces réglages par les limites runtime.

Les URL de test utilisent les rôles synthétiques kyro_admin, kyro_api et kyro_worker, port local 55440, avec les variables KYRO_DATABASE_ADMIN_URL, KYRO_TEST_DATABASE_ADMIN_URL, KYRO_TEST_DATABASE_URL et KYRO_TEST_WORKER_DATABASE_URL sur cette base dédiée. L'image épinglée et les commandes exactes sont dans la preuve des contrôles. Ne pas réutiliser une base métier.

`node scripts/check-openapi.mjs` : 27 chemins/34 opérations. `node --test scripts/check-dependencies.test.mjs` : sept tests passés. Le seul ajout au verrou Cargo est tower-http dans les dépendances de kyro-api ; aucune version de package ne change.

[Configuration Compose](../preuves/part1-pr-review-compose-20261004.json) : `docker compose --env-file <fixture-synthétique> --file compose.p1.production.yaml --profile runtime config --format json` vérifie les deux montages sans démarrer de déploiement. [CORS HTTP réel](../preuves/part1-pr-review-cors-http-20261004.json) : OPTIONS et GET sur /v1/projects, origine autorisée et origine étrangère ; aucun accès sans session (401). Le test Rust couvre l'origine UI3000 distincte du port éphémère API.

[Inventaire source](../preuves/part1-pr-review-source-20261004.json) : empreinte `f96a14525c4e02e6a44f3dbd5e9cf1985eadb0227ba8d71ed9249c44ee29bfc0`. La [recette intégrale finale](../preuves/part1-e2e-20261003T225540-118a0a9e88945932.json) passe P1-01 à P1-13 dans un même run sur cette empreinte, inchangée avant/après. Bases kyro_p1_e2e_118a0a9e88945932 et kyro_restore_118a0a9e88945932 conservées ; 13 appels synthétiques. Restauration : 16 empreintes, sending→unknown, ApplyChanges5→6 et aucune nouvelle émission sous pause. P1-14 est renouvelée : [revue indépendante Sol PASS 9,5/10](../preuves/part1-pr-review-sol-20261004.json), sans constat critique, élevé, moyen ou actionnable non résolu. Le [bilan consolidé](../preuves/part1-pr-review-qualification-20261004.json) relie les quatorze critères, sans modifier le P1-14 partiel du rapport brut.

## Échecs conservés

- [Reproductions avant correction](../preuves/part1-pr-review-before-20261004.txt) : les nouveaux tests refusent l'URL de production sans TLS et l'admission keyless sur les chemins non corrigés.
- [CI-A](../preuves/part1-pr-review-ci-a-20261004.txt) : --locked refuse l'ajout de la dépendance directe déjà présente dans Cargo.lock ; mise à jour hors ligne, une ligne au verrou. Base A conservée.
- [Check intermédiaire](../preuves/part1-pr-review-lock-20261004.txt) : variable RequestBuilder masquant ModelRequest ; renommage http_request, compilé après correction.
- [CI-B](../preuves/part1-pr-review-ci-b-20261004.txt) : garde négative CORS signale une origine constante renvoyée pour toute requête ; liste d'une seule origine adoptée pour ne pas émettre ACAO sur origine étrangère/absente. Base B conservée. CI-C passe ensuite entièrement.
- Un patch documentaire échoue sur son ancre de texte, sans modification partielle ; ancre exacte utilisée ensuite.

## Limites et sources techniques

OIDC, inférence et clés de qualification sont synthétiques ; aucun NVIDIA/Nebius réel, appel payant, merge ni déploiement. Les preuves historiques et leur empreinte demeurent inchangées. La nouvelle version ne réutilise pas implicitement le verdict Sol historique.

Documentation primaire consultée le 2026-10-04 : [parseur SQLx PostgreSQL 0.8.6](https://docs.rs/sqlx-postgres/0.8.6/src/sqlx_postgres/options/parse.rs.html) pour aliases/priorité des options ; [CorsLayer tower-http 0.6.11](https://docs.rs/tower-http/0.6.11/tower_http/cors/struct.CorsLayer.html) pour whitelist et credentials.

## Revue et résultat final

La revue Sol renouvelée vérifie les 94 fichiers et leur correspondance au manifeste E2E, les chemins TLS/pools, CORS/session/CSRF, registre/admission/envoi et Compose/secrets. Note : correction 3/3, validation 1,7/2, sécurité 3/3, compatibilité 1/1, maintenabilité 0,8/1. Les longues suites sont exécutées par le superviseur et leurs preuves relues indépendamment. Aucun constat actionnable non résolu ; vigilance conservée sur l'alignement du parseur d'URL avec SQLx.

**P1 validée dans le périmètre synthétique convenu sur la version corrigée.** Les neuf corrections et leurs preuves sont destinées à la [PR #1](https://github.com/gus-rlin/Kyro/pull/1), sans merge ni déploiement.
