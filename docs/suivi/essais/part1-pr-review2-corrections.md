# Huit nouvelles corrections de la PR P1

Date : 2026-10-04 (Europe/Paris). Base examinée : `0573e258260aa5c565bc7732770272e0c193815f`, PR [#1](https://github.com/gus-rlin/Kyro/pull/1). Worktree `p1-delivery-pr` propre au départ ; checkout racine et autres services préservés. Skill `senior-code-basics` appliqué. Aucun merge, fournisseur réel ou déploiement.

## Corrections ciblées

| Remarque | Correction et preuve pertinente | Commit |
| --- | --- | --- |
| Sauvegarde/restauration production bloquées | Retirer l'exigence de CA du conteneur PostgreSQL, où les CLI utilisent le socket local. Gardes SCRAM, TLS, mounts read-only et réseau privé conservées ; inspection des ports compatible avec un objet Docker vide sous PowerShell strict. Backup/restore réels avec Compose de production et données synthétiques : 16 empreintes concordantes. | `d116b4e` |
| Sauvegarde dans le dépôt sous Unix | Séparateurs natifs et comparaison Ordinal sous Unix / OrdinalIgnoreCase sous Windows ; chemins normalisés et dépôt lui-même refusés, répertoire frère autorisé jusqu'à Docker. | `b6c8bc7` |
| Liveness à 429 sous saturation | `/health/live` sort du sémaphore ordinaire ; en-têtes, CORS et observation conservés. HTTP réel : 128 permits occupés, ready/metrics/projects à 429, live à 200, ready à 200 après libération. | `891b1f9` |
| Timeouts PostgreSQL à 500 | Codes 57014 et 55P03 → Unavailable dans store/budget/queue, autres classifications conservées. Régressions avec pg_sleep et verrou advisory réellement interrompus sur PostgreSQL. | `6fea992` |
| Arrondi des bornes entières | Comparaison exacte i128 pour les nombres JSON i64/u64 et floats intégraux dans leur voisinage. Même validateur pour réponse live/rapprochée ; tests autour de 2^53, i64::MIN, u64::MAX, bornes flottantes et bornes inversées. | `11bb645` |
| Appel dépassant la deadline du job | Timeout limité au minimum du délai de requête et du TTL restant ; aucun socket à deadline expirée, réponse trop tardive unknown sans règlement. Chrono déjà verrouillé passe de dev-dependency à dependency, sans changement du lock. | `655add0` |
| Destination synthétique renommée impossible à rapprocher | Query et policy additive 0017 sélectionnent `intent.registration.provider_kind = synthetic`. Corrélations de projet, environnement, job et réserve held conservées. Admission/exécution Budget-only avec destination alternate, idempotence et refus cloud/droits/projet/environnement. | `5795767` |
| Projets tronqués à 1000 | Pagination de 1 à 1000, curseur opaque `(updated_at, id)` et en-tête X-Next-Cursor exposé par CORS. Tableau et tri existants conservés ; chaque page revalide les droits. Test PostgreSQL à 1006 projets, révocation et isolation ; recette HTTP à deux pages et paramètres invalides. | `4ae2cf1` |

Le [contrat HTTP](../../backend/partie-1/HTTP.md) et [OpenAPI v1](../../backend/partie-1/openapi.v1.json) documentent les paramètres additifs. Le parcours de pagination n'est pas un snapshot lors de modifications concurrentes des projets. Les migrations historiques 0001–0016 restent intactes ; une installation existante doit appliquer 0017 avec le migrateur admin.

## Reproductions et échecs conservés

Un snapshot Git de la base a reçu uniquement les nouvelles fixtures/tests de régression, dans un dossier temporaire extérieur au worktree. Les contrôles qui nécessitaient d'accéder aux mappers ont seulement rendu leur visibilité `pub(crate)` dans ce snapshot ; leur comportement est resté celui de la base.

- [Baseline API](../preuves/part1-pr-review2-baseline-20261004.log) : live 429 au lieu de 200.
- [Baseline gateway/store](../preuves/part1-pr-review2-baseline-other-20261004.log) : entier hors borne accepté ; deadline expirée ouvre un appel ; réponse tardive Succeeded au lieu de Unknown ; timeout SQL Internal ; 1000 projets au lieu de 1006 ; rapprochement alternate NotFound. L'échec queue suivant est une conséquence de la fixture pending conservée après le rapprochement échoué, pas un nouveau diagnostic indépendant.
- Baseline des scripts : [backup production refusé](../preuves/part1-pr-review2-baseline-production-backup-20261004.log), [restore bloqué par le champ de port absent](../preuves/part1-pr-review2-baseline-production-restore-20261004.log), [chemin interne Unix atteint Docker](../preuves/part1-pr-review2-baseline-linux-paths-20261004.log).
- [Premier contrôle final](../preuves/part1-pr-review2-controls-final-20261004.log) : artefacts Cargo du snapshot baseline réutilisés après remontage au même `/workspace` dans le cache partagé. Ancienne signature list_projects visible au compilateur, et [première upgrade](../preuves/part1-pr-review2-upgrade-after-cached-baseline-20261004.json) restée à 16 avec le binaire baseline. Nettoyage des cinq crates Kyro, sans modifier le produit, puis reconstruction complète sur une nouvelle base ; l'échec et sa base restent conservés.
- [Erreur de préparation C](../preuves/part1-pr-review2-controls-final-c-fixture-failure-20261004.log) : deux CREATE DATABASE dans un seul psql -c sont refusés par PostgreSQL comme transaction implicite ; le migrateur échoue ensuite faute de base. Préparation corrigée en deux commandes indépendantes, puis format/check/migrations/86 tests réussis sur C. Une matrice de chemins Windows avait aussi mal parenthésé une concaténation dans un tableau ; matrice corrigée, sans changement produit.
- Premier staging par hunks : patch rejeté à cause de la conversion des fins de ligne du pipe texte Windows ; aucun changement partiel. Passage d'octets bruts à git apply réussi. Deux commandes exploratoires visaient des noms d'outils inexistants ; les vrais outils sont check-dependencies.mjs et check-dependencies.test.mjs.

## Contrôles et reproduction

[Contrôle Linux/Docker final](../preuves/part1-pr-review2-controls-20261004.json) et [log](../preuves/part1-pr-review2-controls-final-c-20261004.log) : 86 tests passent, zéro échec/ignoré. Source montée read-only ; image Rust et versions exactes dans le JSON. Base dédiée `kyro_p1_test_review2_final_c`, rôles API/worker non propriétaires. Commandes :

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo run --locked -p kyro-store --bin kyro-migrate
cargo test --locked --workspace -- --include-ignored
node scripts/check-openapi.mjs
node --test scripts/check-dependencies.test.mjs
```

[Mise à niveau](../preuves/part1-pr-review2-upgrade-20261004.log) depuis 0016 : deux migrateurs concurrents et rejeu réussis ; [avant](../preuves/part1-pr-review2-upgrade-final-before-20261004.json)/[après](../preuves/part1-pr-review2-upgrade-after-20261004.json), 17 versions, les 16 anciens checksums identiques. [Privilèges avant](../preuves/part1-pr-review2-privileges-before-20261004.json)/[après](../preuves/part1-pr-review2-privileges-after-20261004.json) identiques.

[OpenAPI](../preuves/part1-pr-review2-openapi-20261004.json) : 27 chemins/34 opérations. [Classificateur](../preuves/part1-pr-review2-dependency-tests-20261004.log) : sept tests. [Audit courant](../preuves/part1-pr-review2-audit-summary-20261004.json) : aucun avis dans les 194 packages Linux actifs ; avis RSA RUSTSEC-2023-0071 verrouillé inactif conservé. [Audit brut](../preuves/part1-pr-review2-audit-raw-20261004.json) sort 1 ; classification sort 0.

[Opérations production locales synthétiques](../preuves/part1-pr-review2-operations-20261004.json), [backup final](../preuves/part1-pr-review2-production-final-backup-20261004.log), [restore final](../preuves/part1-pr-review2-production-final-restore-20261004.log), [gardes Linux](../preuves/part1-pr-review2-linux-paths-final-20261004.log) et [Windows](../preuves/part1-pr-review2-windows-paths-final-20261004.log). Les secrets/certificats temporaires et le dump restent hors Git. La source de backup finale clone la base de test C dans une instance PostgreSQL séparée du Compose production ; aucun service API/worker n'est lancé dans ce projet. Les sessions de cette fixture sont vides ; cette preuve seule ne démontre pas leur révocation. La recette intégrale couvre ce cas séparément.

Pour reproduire l'opération production, provisionner une CA et un leaf CA:FALSE/SAN postgres, des fichiers secrets synthétiques hors dépôt et le Compose de production sur un projet/volume absent au précontrôle. Installer les migrations et données synthétiques, puis exécuter :

```powershell
./scripts/backup-p1.ps1 -TargetEnvironment Production -ProductionEnvFile <env-hors-depot> -OutputFile <archive-hors-depot>
./scripts/restore-p1.ps1 -TargetEnvironment Production -ProductionEnvFile <env-hors-depot> -BackupFile <archive-hors-depot> -ExpectedSha256 <sha> -TargetDatabase kyro_restore_<nouveau> -SourceDatabase <source>
```

## Recette et revue finales

Version finale : `a718deb5a51e33601effa83250b4276636f6811f`, [95 fichiers / empreinte source](../preuves/part1-pr-review2-source-final-20261004.json) `e2886f97905f2c61349c8ba8c8842a66944a3c4fb73a00aad308b688f2092b89`.

Les deux constats Sol (garde Unix de restore et ligne vide finale de la nouvelle 0017) sont corrigés dans `a718deb`, relus et vérifiés. 0017, encore non publiée, reçoit son checksum final sur installation neuve et clone 0016 distinct ; les anciennes fixtures et preuves restent conservées. Les 86 tests passent à nouveau et le backup/restore production local conserve les 16 empreintes. Un défaut d'encodage du rapport dû à la locale Python Windows est corrigé en UTF-8 explicite ; le nom de la base finale C et les paragraphes périmés sont rectifiés.

La première recette sur `4ae2cf1` est [réussie et conservée](../preuves/part1-e2e-20261004T000556-81cdfe5985947d33.json). La [recette finale sur a718deb](../preuves/part1-e2e-20261004T001514-1c895e888d1d0961.json) passe P1-01 à P1-13, 13 appels synthétiques, source inchangée et 16 empreintes de restauration ; reprise interne, ancienne session refusée, ApplyChanges 5→6 et delta fournisseur zéro. Le brut conserve P1-14 review_pending car la revue indépendante est produite séparément. [Revue Sol finale](../preuves/part1-pr-review2-sol-20261004.json) : PASS 9,5/10, aucun constat actionnable restant. Les longues suites sont contrôlées sur leurs preuves, sans rejeu indépendant par le reviewer. [Bilan consolidé P1-01 à P1-14](../preuves/part1-pr-review2-qualification-20261004.json) : **P1 validée dans le périmètre synthétique convenu**. Aucun verdict historique n'est étendu implicitement au nouveau code. L'inférence NVIDIA/Nebius réelle reste non qualifiée.

Les sorties brutes PostgreSQL/Docker et les logs peuvent contenir les espaces finaux de leurs producteurs. Elles restent conservées telles quelles ; la liste exacte des seules exclusions du contrôle whitespace figure dans SUIVI-0015. Le diff fonctionnel et les autres documents passent le contrôle.
