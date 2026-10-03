# Intégration et vérifications du superviseur P1

## INT-P1-01 — Verrou et domaine intégrés

- Date : 2026-10-03 ; cible Linux x86_64 dans Docker, Rust 1.96.1, fournisseur absent.
- Objectif préalable : métadonnées Cargo entièrement résolues avec `--locked`, puis tests des contrats du domaine intégrés, sans modifier le verrou pendant l'essai.
- Départ : branche `gus-rlin/backend-p1`, modules spec/model/task intégrés ; premier verrou commun incomplet pour les dépendances directes worker.
- Échec : `cargo metadata --locked --format-version 1` et le conteneur `kyro-part1-root-domain-v2` (sortie 101) exigent une modification de Cargo.lock. Le mode `--no-deps` avait seulement validé les manifests et ne prouvait pas le verrou complet.
- Correction : commit common `879306d` intégré en `089032e`; metadata complète réussie. Aucun assouplissement de `--locked`.
- Essai : conteneur conservé `kyro-part1-root-domain-v3`, image `rust:1.96.1-slim-bookworm@sha256:39decc9f9f4a87e03db5069b2424c0ee13c3d7e4116010980d4f96849e823bc4`, workspace monté en lecture seule, caches Cargo et cible distincts. Commande `cargo test --locked -p kyro-domain`, état des sources à `9459ace` (les changements locaux ne concernent alors que le suivi).
- Résultat observé récupéré après interruption : Docker State ExitCode 0, terminé `2026-10-03T04:08:41.555+02:00`, 24 tests réussis, aucun échec/ignoré, doc-tests sans test. Logs conservés dans le conteneur d'essai ; résultat isolé du domaine, sans preuve API/worker/PG.

## INT-P1-02 — Premier check de persistance assemblée

- Date : 2026-10-03 ; PostgreSQL n'est pas sollicité par ce check.
- Départ : migrations, identité et projets intégrés ; exports communs intégrés en `0fb41ca`, branche à `90ecf8e`. Le module queue est présent mais n'est pas encore exporté faute de couche budget intégrée.
- Essai natif : `cargo check --locked -p kyro-store`, échec sur build-script `zerocopy`, OS 4551 Windows. Contrôle d'application non modifié ; cette voie ne valide pas le Store.
- Essai Linux : conteneur conservé `kyro-part1-root-store-v1`, même image et montage que ci-dessus, `cargo check --locked -p kyro-store`. Sortie 101 : 17 erreurs, issues de l'import invalide `sqlx::PgRow` et de `Store` non importé dans `projects.rs`. Correction confiée au propriétaire du module ; nouvel essai requis.
- Limite : ce check porte sur les modules déjà exportés et ne vaut pas compilation du workspace complet. Les modules restants seront exportés puis vérifiés ensemble.
- Correction et nouvel essai : imports corrigés en `1bb50c2`; conteneur `kyro-part1-root-store-v2`, sortie 0, terminé `2026-10-03T14:30:40+02:00`. Les modules alors exportés sont compilés ; queue et budget n'y étaient pas encore inclus.

## INT-P1-03 — Contrôles du banc et de l'audit

- Date : 2026-10-03 ; Node 24.18.0 sur Windows.
- Commandes : `node --check scripts/verify-p1.mjs`, `node --check scripts/check-dependencies.mjs`, `node --test scripts/check-dependencies.test.mjs`.
- Résultat : syntaxe valide ; sept tests du classificateur réussis (avis actif refusé, avis inactif distingué du scanner brut, erreurs et graphe manquant refusés, absence de chemins locaux dans le résumé).
- Limites : ces contrôles prouvent le classificateur et la syntaxe du banc, pas la recette P1. Le contrôle réel Linux du graphe, l'exécution E2E, la sauvegarde/restauration métier et la revue Sol restent à fournir.

## INT-P1-04 — Compilation du workspace complet et contrats entre modules

- Date : 2026-10-03 ; Rust 1.96.1/Linux, même image épinglée et montage en lecture seule.
- Objectif préalable : compiler tous les exécutables et cibles de test avec `cargo check --locked --workspace --all-targets`, après export effectif de queue/budget et intégration de la passerelle.
- Premier essai : `kyro-part1-root-workspace-v1`, sortie 101, import `sqlx::PgRow` invalide dans budget ; correction intégrée `8b3fb7f`.
- Deuxième essai : `kyro-part1-root-workspace-v2`, sources à `0668a83`, sortie 101, terminé `2026-10-03T15:06:15+02:00`. Quatre erreurs observées : deadline absente de ModelEffectContext dans le worker, branche StaleBudgetVersion manquante, fonctionnalité Axum matched-path absente et type u32 au lieu de i64 pour list_effects.
- Corrections : les propriétaires corrigent les appels et le manifeste sans assouplir le verrou. Les résultats de revalidation seront ajoutés ; ces deux essais ne constituent pas une compilation réussie du logiciel.
- Revue d'intégration : limites de grants, périmètre d'environnement, lecture atomique des événements et suivi de la commande de rapprochement intégrés. Un modèle suspendu en tête de projet pouvait empêcher une commande locale de passer ; priorité des commandes sans HTTP demandée au propriétaire de la file, avec reproduction dans la recette.
- Nouvel essai `kyro-part1-root-workspace-v3` à `28e06da` : runtime compilé, mais sortie 101 sur 13 erreurs des fixtures OIDC PostgreSQL (tableaux de 16 octets au lieu des empreintes de 32 octets). Correction dédiée `76522d5`, sans réduire le contrat persisté.
- Build réel : `kyro-part1-root-bins-v1`, `cargo build --locked --bins --workspace`, sortie 0 à `28e06da`, terminé `2026-10-03T15:59:43.562+02:00`. Les warnings HTTP observés ont été corrigés ensuite ; il s'agit d'un build local Linux, pas d'un démarrage ou déploiement.
- Format : `kyro-part1-root-format-v1` échoue uniquement sur budget.rs ; mise en forme intégrée `9c63f26`, puis `kyro-part1-root-format-v2` passe à `49c3fe6`, sortie 0, terminé `2026-10-03T16:04:28.995+02:00`. Aucune migration déjà appliquée n'est réécrite.
- Check et tests intégrés : `kyro-part1-root-workspace-tests-v1`, sources `76522d5`, commandes `cargo check --locked --workspace --all-targets && cargo test --locked --workspace`, sortie 0, terminé `2026-10-03T16:05:14.472+02:00`. 67 tests réussis (23 API, 4 OIDC sans PG, 29 domaine, 11 passerelle). Les trois scénarios PostgreSQL sont explicitement ignorés dans cet essai sans URLs de test et doivent être exécutés séparément en intégration.
- Le test HTTP de réutilisation d'un résultat connu sans clé, auparavant bloqué par OS4551 sous Windows, passe effectivement dans cette suite Linux. Les bornes de réponse, timeout, absence de retry et redirection sont aussi exercés contre un serveur HTTP synthétique local.

## INT-P1-05 — Autorité du rôle applicatif en PostgreSQL réel

- Date : 2026-10-03 ; PostgreSQL 18.6 ops, nouvelle base dédiée `kyro_p1_test_81536e37d46d`, migrée 1–13 par le banc indépendant. Aucun accès admin utilisé par le Store.
- Objectif préalable : exécuter les trois tests ignorés dans la suite sans URLs et prouver l'identité, les mutations non propriétaires et la reprise d'événements sous le rôle kyro_api.
- Échec OIDC : `postgres_persists_flows_sessions_and_organization_membership` échoue sur `persist one-use OIDC flow: Forbidden`. Vérification du superviseur : `has_table_privilege` retourne false pour DELETE login_flows et sessions. Le nettoyage exécuté avant admission n'a pas son privilège ; cause confirmée par le code et les droits effectivement chargés.
- Correction : migration additive 0014 intégrée en `3f91273`, droit de nettoyage API avec politiques DELETE restrictives aux lignes expirées, consommées ou révoquées. Sur clone séparé, le propriétaire SQL vérifie DELETE actif zéro ligne, nettoyage admissible et refus worker. Le banc doit rejouer la reproduction Rust ; aucune migration antérieure n'est réécrite.
- Résultats Store séparés : `postgres_event_feed_tracks_global_bounds_with_hidden_events_and_purge` passe. `postgres_projects_enforce_isolation_cas_and_idempotency` échoue sur le membre Write borné, ligne 333, `Forbidden`.
- Reproduction supplémentaire du superviseur sous kyro_api, transaction annulée : contexte du membre synthétique, action Write true, SELECT de son grant count=1, même SELECT FOR SHARE count=0. Le banc reproduit indépendamment ce filtrage ; le worker n'a par ailleurs aucun privilège UPDATE de grants. [D33](../DECISIONS.md#d33--verrouillage-des-autorisations-sans-droit-de-modification) fixe une correction étroite, sans droit de modifier ses propres autorisations.
- Conclusion : un scénario PG vérifié ; les deux autres restent en échec jusqu'au nouvel essai après correction. Les 67 tests sans PG ne démontrent pas ces comportements d'intégration.
- Correction intégrée : migration 0015 au commit `5f5fbc0`, appel Rust du helper `e8d37d7`. Le [rapport SQL](part1-foundation.md) vérifie API et worker, refus hors périmètre, UPDATE toujours refusé et révocation concurrente sérialisée. L'échec initial du fixture concurrent est conservé. Ces contrôles isolés ne remplacent pas les trois scénarios Rust/PG et la recette HTTP.
- Revalidation intégrée du superviseur : sources `6f3ca3a`, conteneur conservé `kyro-part1-root-ci-tests-v2`, PostgreSQL 18.6 sur la base dédiée `kyro_p1_test_e2e15_a901` contenant les 15 migrations. Commandes `cargo fmt --all -- --check`, `cargo check --locked --workspace --all-targets`, `cargo test --locked --workspace -- --include-ignored`. Sortie 0, terminé `2026-10-03T17:40:14.535+02:00` ; 70 tests réussis, zéro échec et zéro ignoré. Les reproductions OIDC, mutations du membre borné et feed/purge passent réellement sous kyro_api ; seul le fixture de purge utilise l'URL admin de cette base isolée. [Logs](../preuves/part1-root-ci-tests-20261003.txt) et [résumé versionné](../preuves/part1-root-linux-20261003.json). La suite n'est pas une preuve de crash/reprise du processus worker ou de restauration.

## INT-P1-06 — Corriger les faux statuts du banc

- Date : 2026-10-03 ; Node 24.18.0/Windows, PostgreSQL 18.6, source runtime `2060211`.
- Premier parcours complet : migrations concurrentes (0/0) et rejeu (0) réussis ; échec avant authentification sur « role connection retained actor context or exposed unscoped project rows ». Base de test `kyro_p1_e2e_a7aac2328f79abe4` conservée pour diagnostic.
- Reproduction du superviseur, même requête sous kyro_api : stdout `BEGIN`, `SET`, `SET`, `COMMIT`, puis `|0`, sortie 0. Le parseur prenait les tags psql pour un contexte acteur ; le résultat SQL prouve au contraire contexte vide et zéro ligne. L'ajout de quiet doit supprimer seulement ces tags ; la requête et l'assertion restent identiques. Le rapport indépendant conservera les deux sorties et le nouvel essai.
- Autre défaut du banc constaté par lecture : la fin du parcours annonçait inconditionnellement « partial_pending_execution » et code 1 même si tous les scénarios avaient été réellement exécutés. Correction demandée : succès runtime avec revue indépendante en attente et code 0 pour le check CI ; P1-14 reste partiel jusqu'à la revue Sol, chaque véritable échec reste en code 1 avec sa base conservée.
- Résultat : diagnostic confirmé ; correctifs du banc en préparation dans son worktree. Aucun succès E2E complet annoncé à ce point.
- Revue de couverture au checkpoint `de35290` : P1-01 était marqué passé après build/migrations/RLS, avant la readiness API et la connexion du worker. Le banc doit désormais attendre ces deux processus ; les anciens JSON restent des preuves de ce sous-ensemble. Une fixture modèle dépassait aussi la borne domaine de 48 000 octets et le lancement Docker transmettait le PATH Windows ; corrections et démarrage ciblé à intégrer depuis le worktree E2E.
- Restauration : la comparaison du banc couvre actuellement dix tables et ne démarre pas les services restaurés. La qualification finale doit comparer les identités, memberships, décisions et commandes en plus des données métier, conserver les limites des grants, puis contrôler session ancienne refusée, nouvelle connexion et suspension persistante des ModelCall au redémarrage. Le cas `sending` doit être restauré comme inconnu avec réserve conservée. Le propriétaire du banc couvre ces branches ; le propriétaire opérations ajoute la colonne `limits` omise par son manifeste de comparaison. Aucun succès de ces nouveaux contrôles n'est annoncé avant leur exécution.
