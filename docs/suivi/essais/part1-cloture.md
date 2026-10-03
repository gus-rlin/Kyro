# Clôture de P1 — SUIVI-0012

- Date : 2026-10-03 (Europe/Paris).
- Périmètre : PostgreSQL, API et worker réels sous Linux/Docker ; identité OIDC et inférence synthétiques. Aucun fournisseur réel, appel payant, déploiement, publication ou commit automatique.
- État : **P1 validée dans le périmètre synthétique convenu**. P1-01 à P1-13 passent dans une recette intégrale ; P1-14 reçoit le verdict indépendant Sol PASS, 9,5/10, sans constat critique ou élevé non résolu. Aucun succès intermédiaire n'est utilisé pour remplacer ce run final.

## Version et preuves finales

- Base Git : `d2d7fbafec5b1e3d50d4b6324ad3f40e836c8cc2`, branche `gus-rlin/backend-p1`, changements livrés sans commit automatique.
- Empreinte des 94 entrées de construction/test : `8da46273d2f17bb9abef667e05c7de7af54babdac8c203e089e5464d00a1fd4d`, identique avant/après recette et contrôle complet. [Inventaire des sources](../preuves/part1-close-final-source.json).
- [Recette intégrale E2E-12](../preuves/part1-e2e-20261003T204435-4ef16a288963cbbf.json) : P1-01 à P1-13 PASS sur PostgreSQL 18.6, Node 24.18.0 et Docker 29.8.0 ; 13 requêtes d'inférence synthétiques. Le brut conserve honnêtement P1-14 `partial`, puisque la revue a lieu séparément.
- [Contrôle Linux final CI-13](../preuves/part1-close-controls-final-20261003.json) et [log intégral](../preuves/part1-close-ci-13-20261003.txt) : format en lecture seule, check verrouillé de toutes les cibles, installation neuve et 74 tests Rust/PostgreSQL réussis, zéro échec et zéro ignoré. Rust/Cargo 1.96.1, image Rust épinglée dans le rapport ; Compose v5.5.1.
- [Revue P1-14](part1-revue-finale.md) et [bilan consolidé](../preuves/part1-close-qualification-final-20261003.json) relient les quatorze critères au même code.
- [Archive exacte des entrées](../preuves/part1-close-final-inputs.zip), 326696 octets, SHA-256 `06c004e1ce6198fbfe27ed9530b3a1364fd01275c5da5a2450e256f9ccb3d12a`. Pour reconstituer le code sans commit, repartir du commit de base et remplacer les entrées par celles de cette archive ; son `SOURCE-MANIFEST.json` permet de contrôler chaque fichier, y compris les fins de ligne. Le [patch de revue](../preuves/part1-close-final.patch), SHA-256 `5a239c00a209e345364f14134c770bf5869a55fa9464c30339fbfac2b931c4b5`, inclut 0016 ; `git apply --reverse --check` réussit sur la version livrée.

| Critère | Résultat vérifié dans le run final |
| --- | --- |
| P1-01 | Binaires, migrations concurrentes/rejeu, rôles runtime et démarrage API/worker réels |
| P1-02 | OIDC synthétique, refus des claims invalides, session opaque, CSRF/Origin, révocation et expiration |
| P1-03 | Six surfaces de lecture isolées, mutations interprojet refusées, absence de modification persistée |
| P1-04 | Révisions, CAS, préconditions et limites des opérations |
| P1-05 | Huit requêtes simultanées dédupliquées, conflit d'empreinte refusé |
| P1-06 | File durable, redémarrage et SIGKILL avec rollback/reprise de génération |
| P1-07 | Source obsolète et révocation suppriment le résultat métier, comptabilité conservée |
| P1-08 | Bornes, saturation, annulation pending/running et deadline |
| P1-09 | Six jobs/quatre workers : deux appels, quatre refus, deux lignes ledger, réserve finale 0, dépense réelle 56 SYN |
| P1-10 | Effet connu repris sans renvoi, sending interrompu devient unknown, rapprochement Budget-only par un autre acteur, preuve Processed invalide refusée puis valide réglée une fois |
| P1-11 | Destination/catégorie/référence de secret refusées, réponses malformées/volumineuses/503 sans retry, sentinelle absente des réponses/logs/événements/outbox |
| P1-12 | Reprise SSE, trous d'environnement masqués, préfixe physiquement purgé et révocation du lecteur |
| P1-13 | Sauvegarde/restauration gérée, 16 empreintes, reprise interne et rapprochement sous pause |
| P1-14 | Revue indépendante Sol PASS 9,5/10, aucun constat critique/élevé non résolu |

La restauration compare les 16 empreintes canoniques, y compris les limites de grants, et conserve les réserves prepared/unknown. `sending → unknown`, génération préparée 1→2, aucune session active ni flow conservé ; l'ancien cookie reçoit 401. Après nouveau login, un véritable ApplyChanges passe de révision 5→6 avec préférence persistée ; le worker `kyro_worker` exécute un rapprochement sans nouvel appel. `external_sends_enabled_after_worker_start=false`, delta fournisseur 0. Les bases finale et restaurée sont conservées : `kyro_p1_e2e_4ef16a288963cbbf` et `kyro_restore_4ef16a288963cbbf`.

Les [mesures disponibles](../preuves/part1-close-model-measurements-20261003.json) distinguent six usages déclarés par le fournisseur synthétique (102 tokens d'entrée, 66 de sortie, cache inconnu) et une preuve manuelle de rapprochement (17/11, cache déclaré 0). Les usages des pannes/inconnus, latences fournisseur et consommation des agents non mesurés restent inconnus. Le modèle synthétique n'annonce aucune version ; schéma 1, tarif `synthetic-2026-10-03`, données synthétiques et plafonds finis. Ces nombres ne mesurent ni un coût facturé ni un gain.

## Changements

La migration additive 0016 réalise D34 : verrou de métadonnées borné sans Write, admission Execute+Model et rapprochement Budget|Manage, corrélations explicites acteur/projet/environnement des jobs et commandes, événements d'admission/annulation et accès comptable limité au job chargé. Les migrations 0001–0015 restent intactes.

La préparation d'effet crée son intention et sa réserve dans la transaction avant le verrou comptable du budget ; tout refus annule ces lignes. Le règlement libère seulement la réserve concernée et inscrit le coût réel une seule fois dans le ledger `settlement`. La revalidation du rapprochement examine les droits de l'auteur cible puis restaure l'opérateur. Le worker valide la preuve contre le registre et le schéma sous les verrous, avant toute mutation.

Le banc conserve le contrat HTTP v1 et identifie les sources non commitées par SHA-256 de chaque entrée de construction/test, agrégé et vérifié avant/après. Le port 18080 préserve l'API de développement existante sur 8080. Les mutations interprojet et l'admission/exécution interne après restauration sont exercées réellement.

## Reproduction

Depuis la racine, obtenir l'empreinte avec `node scripts/verify-p1.mjs --fingerprint`, puis utiliser son champ `sha256` :

```powershell
node scripts/verify-p1.mjs --run --execution docker --db-container kyro-p1-ops-postgres-1 --db-port 55440 --api-port 18080 --source-sha256 <empreinte> --keep-on-failure
```

Ce runner construit les binaires verrouillés, crée une base dédiée neuve et utilise uniquement des clés éphémères synthétiques. Les bases d'échec sont conservées. Les autres services locaux restent hors cible.

Contrôle complet dans l'image Linux Rust 1.96.1 Bookworm, source montée en lecture seule et base neuve `kyro_p1_test_*` :

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo run --locked -p kyro-store --bin kyro-migrate
cargo test --locked --workspace -- --include-ignored
```

Variables : `KYRO_DATABASE_ADMIN_URL` et `KYRO_TEST_DATABASE_ADMIN_URL` désignent le rôle administrateur de la base dédiée ; `KYRO_TEST_DATABASE_URL` désigne `kyro_api` ; `KYRO_TEST_WORKER_DATABASE_URL` désigne `kyro_worker`. PostgreSQL synthétique local utilise le port 55440. Aucun mot de passe n'est enregistré.

Contrôles JavaScript : `node --check scripts/verify-p1.mjs`, `node scripts/check-openapi.mjs`, `node --test scripts/check-dependencies.test.mjs`, les deux scripts `tests/fixtures/synthetic-provider*-smoke.mjs`.

## Migrations et dépendances

La [preuve de mise à niveau](../preuves/part1-close-upgrade-20261003.json) part de 15 migrations, applique 0016 en concurrence puis rejoue le migrateur. Les deux exécutions concurrentes et le rejeu sortent 0. Les checksums antérieurs et les empreintes des projets/grants/limites sont conservés. L'installation neuve est également exercée par les contrôles complets et la recette.

Le checksum SHA-384 de 0016 dans cette preuve correspond au fichier SQL final. Les [57 privilèges de table runtime](../preuves/part1-close-runtime-table-privileges-20261003.json) sont identiques entre 0015 et 0016 ; le test Execute+Model vérifie aussi qu'il ne peut pas modifier le projet. Les nouvelles policies comptables restent bornées au rôle worker et au job/projet/environnement chargé.

L'[audit classifié](../preuves/part1-close-audit-summary-20261003.json) examine 194 packages Linux actifs : aucun avis actif. L'[audit brut](../preuves/part1-close-audit-raw-20261003.json) sort 1 pour RSA 0.9.10/RUSTSEC-2023-0071 verrouillé mais inactif ; il n'est pas présenté comme un audit brut réussi. Base RustSec rafraîchie, commit `ef6173cbc5c50ec8166f9a5b28f07834144373ee`, daté `2026-10-03T10:14:03+02:00`. Cargo.lock n'a pas changé pendant cette tâche.

## Échecs conservés

Le [journal SUIVI-0012](../JOURNAL.md#suivi-0012--boucler-p1-dans-le-périmètre-synthétique-convenu) détaille les refus du nom de base protégé, la variable worker absente, la réutilisation incorrecte d'une base avec anciens jobs, les fixtures erronées, les verrous comptables, les défauts de règlement et les assertions incorrectes du banc. Les logs CI-01 à CI-13, les reproductions avant correction et les rapports E2E restent dans `docs/suivi/preuves/` ; leurs bases dédiées restent dans PostgreSQL. L'[historique des douze runs](../preuves/part1-close-run-history-20261003.json) vérifie la présence des douze bases source. Aucun échec n'est remplacé par un succès ultérieur.

## Limites

La qualification porte exclusivement sur le périmètre synthétique convenu. Elle ne qualifie ni NVIDIA/Nebius réels, ni un déploiement, ni les capacités du catalogue prévues dans les parties suivantes. Aucun coût facturé ou gain de performance n'est annoncé.
