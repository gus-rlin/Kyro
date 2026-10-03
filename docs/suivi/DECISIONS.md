# Décisions de réalisation

Les [décisions D01 à D23](../nvidia-hackathon/D%C3%A9cisions%20de%20conception%20et%20justification%20des%20am%C3%A9liorations.md) restent la référence de conception. Les décisions ci-dessous précisent leur réalisation ; elles ne réduisent pas le catalogue généraliste.

## D24 — Qualification synthétique des services externes en partie 1

- Date : 2026-10-03. Statut : retenu pour ce chantier à la demande de l'utilisateur.
- Contexte : aucun compte ni clé fournisseur n'est disponible ; le plan demande une preuve d'appel réel avant de qualifier le fournisseur.
- Choix : PostgreSQL, API et worker réels ; fournisseurs d'identité et d'inférence synthétiques pour vérifier les contrats et provoquer les pannes. Aucun appel payant ni transmission de données réelles. L'adaptateur NVIDIA reste désactivé sans configuration, référence de secret et tarif qualifiés.
- Alternatives examinées : attendre une clé (empêche le développement demandé) ; présenter la simulation comme preuve NVIDIA (preuve invalide). Ni l'une ni l'autre n'est retenue.
- Justification : l'instruction explicite de développer avec données synthétiques permet la livraison du socle, tout en conservant la limite du critère fournisseur réel.
- Conséquences : la recette peut accepter le socle dans son périmètre synthétique ; l'intégration réelle, les tarifs du fournisseur et un déploiement de production ne sont pas déclarés vérifiés. Références : D11, D15, D18, D23.
- Réexamen : configuration d'un endpoint, clé serveur, budget autorisé et recette d'usage réel tracé/rapproché disponibles.

## D25 — Frontières de persistance et intégration isolée

- Date : 2026-10-03. Statut : retenu pour la partie 1.
- Contexte : dépôt initial sans commit ; chaque sous-agent doit créer et utiliser son worktree et sa branche.
- Choix : branche d'intégration `gus-rlin/backend-p1`, baseline Git des références, branches dédiées `gus-rlin/p1-*`. Domaine Rust sans Axum/SQLx, stockage PostgreSQL, passerelle HTTP, exécutables API et worker distincts. Les rôles PostgreSQL applicatifs sont distincts du migrateur, non propriétaires et sans privilège de contournement RLS ; le contexte d'acteur est local à chaque transaction.
- Alternatives examinées : édition simultanée dans le checkout principal (ne respecte pas l'isolation demandée) ; base en mémoire remplaçant PostgreSQL (ne démontre pas durabilité ni concurrence). Non retenues.
- Justification : dépendances acycliques et composition transactionnelle explicite, conformément à D13 ; vrais contrôles et baux conformément à D02, D03, D14.
- Conséquences : chaque contribution est intégrée après examen ; l'API ne migre pas au démarrage avec ses droits applicatifs. Une restauration d'essai se fait dans une base distincte, sans sorties externes automatiques, avec invalidation des anciennes tentatives avant reprise.
- Réexamen : limite mesurée du monolithe modulaire ou évolution justifiée de la frontière de confiance.

## D26 — Dépendances et validation d'identité du socle P1

- Date : 2026-10-03. Statut : révisé après audit ; backend AWS-LC intégré, graphe actif Linux vérifié après synchronisation RustSec, revue finale attendue.
- Contexte : le poste possède Rust 1.96.1 ; P1 doit vérifier OIDC sur un fournisseur synthétique signé et permettre la configuration d'un fournisseur réel.
- Choix : toolchain 1.96.1 épinglée, Axum 0.8.9, SQLx 0.8.6 et Reqwest 0.12.28 avec Rustls ; vérification JWT par `jsonwebtoken` 11.1.0, backend `rust_crypto`, fonctionnalités par défaut désactivées. Les signatures acceptées sont configurées explicitement ; issuer, audience, expiration, state, nonce et PKCE S256 ne sont pas délégués aux valeurs par défaut de la bibliothèque. Cargo.lock et l'image de compilation épinglée figeront les versions résolues.
- Alternatives examinées : passer à la toolchain stable la plus récente (inutile pour les MSRV vérifiés) ; backend JWT AWS-LC (alternative maintenue, ajoutant du code natif à compiler) ; anciennes versions JWT (correctif de validation absent avant 10.3.0). Le choix réduit les variations de compilation et conserve une bibliothèque de cryptographie maintenue ; aucune cryptographie n'est réimplémentée.
- Sources primaires consultées par le sous-agent de recherche le 2026-10-03 : [manifest du mainteneur JWT](https://github.com/Keats/jsonwebtoken/blob/master/Cargo.toml), [avis GHSA-h395-gr6q-cpjc](https://github.com/advisories/GHSA-h395-gr6q-cpjc), [OIDC Core](https://openid.net/specs/openid-connect-core-1_0.html), [RFC 7636](https://www.rfc-editor.org/rfc/rfc7636.html), [manifest Reqwest 0.12.28](https://docs.rs/crate/reqwest/0.12.28/source/Cargo.toml.orig).
- Conséquences : les tests doivent utiliser de vraies signatures synthétiques et vérifier les refus ; l'audit des dépendances et les builds restent nécessaires. Une recherche documentaire n'est pas une preuve d'intégration.
- Réexamen : avis de sécurité, incompatibilité du graphe résolu ou évolution du contrat OIDC réellement requise.
- Réexamen effectif le 2026-10-03T03:14:57+02:00 : `cargo audit --json` échoue sur `rsa` 0.9.10, introduit uniquement par `jsonwebtoken` et son backend `rust_crypto` (`cargo tree --locked -i rsa`). [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html), consulté à cette date, ne fournit pas de version corrigée. Le service utilise des clés publiques de vérification ; l'exploitabilité du chemin privé décrit par l'avis n'est pas démontrée ici. Le graphe signalé reste néanmoins refusé pour la livraison. Choix de correction : backend `aws_lc_rs` de la même bibliothèque JWT, avec compilation dans la cible Linux déjà retenue. La motivation initiale d'éviter une compilation native ne suffit plus à conserver le choix initial. Aucun avis n'est ignoré ; la compilation et un nouvel audit restent à réussir avant acceptation.
- État de la correction : `jsonwebtoken` 11.1.0 utilise désormais `aws_lc_rs`. Le verrou garde `rsa` via la dépendance optionnelle inactive `sqlx-mysql` ; l'audit brut continue donc à retourner 1. Le [vérificateur de dépendances](../../scripts/check-dependencies.mjs) confronte cet audit au graphe actif produit par Cargo pour les cinq crates, sans fichier d'exclusion ni réimplémentation de la résolution de fonctionnalités. Les erreurs d'audit ou de parsing et tout avis actif font échouer ce contrôle. Après le premier contrôle Windows, la passe Linux sur `c35ec2f` confirme cinq racines et 194 paquets actifs sans vulnérabilité active ; un fetch hôte réussi corrobore la base RustSec au commit `ef6173cbc5c50ec8166f9a5b28f07834144373ee`. Le fetch du conteneur avait échoué, la base fraîche a donc été montée en lecture seule et auditée avec `--no-fetch`. Le [rapport du socle](essais/part1-common.md) conserve les deux résultats et leur provenance. La revue indépendante reste requise ; le scanner brut n'est pas annoncé propre.

## D27 — Identité durable des effets et restauration sans réémission

- Date : 2026-10-03. Statut : retenu pour P1 ; réalisation et recette en cours.
- Contexte : un arrêt peut survenir après l'effet externe et avant la clôture du job ; une restauration peut retrouver une intention préparée ou envoyée avec une ancienne génération.
- Choix : une intention et sa réservation ont une identité stable par job modèle, toutes générations confondues. Un résultat connu est réutilisé sans appel ; `sending` ou `unknown` conserve la provision et bloque toute reprise automatique. La comptabilisation d'une réponse tardive est distincte de l'autorisation d'intégrer son résultat. Le worker possède une autorité de service limitée à la file et à cette comptabilité ; les mutations AppSpec restent soumises aux droits et à la révision courants.
- Alternatives examinées : unicité de l'effet par job et génération (peut réémettre après un crash entre rapprochement et clôture) ; bloquer la comptabilité sur le bail ou le grant devenu obsolète (peut perdre une consommation réelle). Ces alternatives sont rejetées.
- Restauration : une base cible nouvelle conserve les révisions, droits, événements, intentions et réserves. Un contrôle persistant `runtime_control.external_sends_enabled` passe à `false` dans la base restaurée ; le démarrage ne le réactive pas. Les baux sont invalidés et les envois non confirmés deviennent inconnus. La reprise des sorties externes exige un rapprochement et une action d'exploitation explicite.
- Conséquences : génération et révocation empêchent une intégration tardive, sans prétendre annuler une requête envoyée ni son coût. Les tests doivent interrompre les deux fenêtres critiques et vérifier le compteur du fournisseur synthétique. Références : D02, D03, D14, D15 et D24.
- Réexamen : nouveau type d'effet ou fournisseur avec un contrat d'idempotence et de rapprochement qualifié.

## D28 — Retrait d'un membre et révocation des accès

- Date : 2026-10-03. Statut : retenu pour P1 ; réalisation et essais en cours.
- Contexte : un grant donne accès à un projet, tandis qu'une adhésion seule ne donne aucune visibilité. Supprimer une adhésion sans traiter les grants pourrait laisser un accès actif ou le réactiver lors d'une réadhésion.
- Choix : l'accès exige une adhésion courante et un grant actif adapté à l'action, à l'environnement et à la ressource. Le retrait d'un membre non propriétaire révoque dans la même transaction tous ses grants sur les projets de l'organisation, y compris les projets invisibles au demandeur. Un trigger de persistance à privilèges limités réalise cette révocation. La création d'un grant verrouille l'adhésion cible pour se sérialiser avec son retrait.
- Alternatives examinées : supprimer seulement l'adhésion (laisse des grants réutilisables) ; révoquer seulement les grants visibles au demandeur (révocation incomplète). Rejetées.
- Justification : le retrait doit fermer l'autorité effective et empêcher une renaissance implicite des droits. Les mutations en cours verrouillant un grant se terminent avant sa révocation ; les admissions suivantes, intégrations de résultats et lectures SSE revérifient les droits.
- Conséquences : la réadhésion exige de nouveaux grants ; l'API P1 ne transfère ni ne supprime le propriétaire. Les tests doivent couvrir retrait, réadhésion, accès HTTP, SSE et résultat tardif. Références : D03, D14 et D25.
- Réexamen : introduction d'invitations, de transfert de propriété ou d'une autorité d'organisation différente, avec contrat explicite et recette de concurrence.

## D29 — Versions des métadonnées et du plafond de budget

- Date : 2026-10-03. Statut : retenu pour P1 ; vérification en intégration attendue.
- Contexte : une modification de DataPolicy ou des limites peut invalider un travail déjà admis. Le plafond de budget possède une concurrence différente des réservations et consommations.
- Choix : les mutations de DataPolicy et des limites exigent la révision attendue, verrouillent le projet et créent une nouvelle révision AppSpec immuable, même si son contenu reste identique. Métadonnées, décision et événement changent dans la même transaction. Les anciens jobs métiers deviennent obsolètes. La configuration du budget a sa propre `configuration_version` et son ETag `budget-N` ; réserver ou rapprocher un usage ne change pas cette version.
- Alternatives examinées : modifier les métadonnées sans révision (ne clôt pas les travaux devenus inadmissibles) ; utiliser les variations de consommation comme version de configuration (provoque des conflits sans mutation du plafond). Rejetées.
- Conséquences : verrouillage commun projet avant jobs, effets, réserve/budget et grants ; tests CAS simultanés et refus des résultats anciens nécessaires. Références : D02, D03, D14, D15.
- Réexamen : séparation future des versions métier et de politique, avec contrat de compatibilité explicite.

## D30 — Réconciliation par une commande durable du worker

- Date : 2026-10-03. Statut : retenu pour P1 ; réalisation en cours.
- Contexte : le rôle PostgreSQL de l'API lit les effets et modifie le plafond, mais ne peut écrire leurs résultats, le journal de consommation ou les montants réservés/dépensés. Le premier handler synchrone demandait ces écritures sous le mauvais rôle.
- Choix : `POST .../effects/{effect_id}/reconcile` admet une commande durable `ReconcileEffect` avec clé d'idempotence et répond `202` avec une référence de job. Le serveur exige Budget ou Manage courant, sans Read, Execute ou révision métier attendue. Le worker vérifie le bail, la génération, le délai, l'annulation et cette autorité de la commande ; il rapproche l'effet synthétique et clôt la commande dans la même transaction, sans appel réseau ni clé fournisseur.
- Alternatives examinées : donner les droits financiers du worker à l'API (affaiblit la séparation) ; dupliquer toute la validation Rust dans une fonction SQL privilégiée (multiplie les sources de logique). Non retenues.
- Conséquences : verrou projet puis les deux jobs triés par UUID, puis effet/réserve/budget et grants. L'opérateur peut différer de l'auteur du job cible. Une correction financière ne rend son résultat métier intégrable que si les droits, source, délai et annulation propres au job cible le permettent encore. Les réserves d'effets inconnus restent retenues jusqu'à cette décision explicite. API et worker conservent leurs identifiants PostgreSQL distincts. Références : D14, D15, D25, D27.
- Réexamen : rapprochement qualifié d'un fournisseur réel, avec preuve et protocole propres au fournisseur ; les décisions manuelles P1 restent limitées au synthétique.

## D31 — Limites portées par les autorisations

- Date : 2026-10-03. Statut : retenu pour P1 ; implémentation intégrée, recette globale attendue.
- Contexte : la conception demande des CapabilityGrant bornés par les ressources, l'environnement, la durée et les limites. Les limites du projet seules ne suffisent pas à restreindre une délégation.
- Choix : cinq plafonds optionnels, validés en Rust et PostgreSQL : tentatives de job, durée totale du job, octets d'entrée modèle, tokens de sortie modèle et opérations ChangeSet. L'absence d'un plafond hérite des bornes finies du projet et du fournisseur. Pour chaque action exigée, une autorisation courante doit couvrir toutes les dimensions de la demande ; des autorisations distinctes peuvent couvrir des actions distinctes.
- Alternatives examinées : additionner les plafonds de plusieurs grants (élargissement implicite) ; vérifier uniquement à l'admission (laisse intégrer un résultat après réduction ou révocation). Rejetées.
- Conséquences : contrôle des faits persistés à l'admission, au rejeu, avant préparation/envoi et à l'intégration. L'entrée modèle est mesurée par sa représentation JSON complète. Durée et création du job proviennent du même timestamp SQL ; la revalidation ne gonfle pas artificiellement la durée. Références : D03, D14, D25, D29.
- Réexamen : nouvelle dimension mesurable de délégation, avec migration et preuve des refus correspondants.

## D32 — Reprise des événements et rétention physique

- Date : 2026-10-03. Statut : retenu pour P1 ; implémentation intégrée, recette globale attendue.
- Contexte : le compteur d'événements est global au projet, tandis que les références de jobs, effets et consommation sont filtrées par environnement. Un trou visible peut donc exister sans purge.
- Choix : une fonction SQL à privilèges bornés expose seulement le premier numéro physiquement conservé et le compteur global à un acteur ayant Read courant sur le projet. Le Store lit ces bornes et les événements autorisés en une seule requête/snapshot. SSE accepte les sauts croissants ; un curseur futur est refusé, un préfixe réellement purgé impose un snapshot.
- Alternatives examinées : exiger des séquences visibles contiguës (faux refus pour un autre environnement) ; lire séparément les bornes et le batch (course avec la rétention). Rejetées.
- Conséquences : aucune charge utile d'un autre environnement n'est exposée. La rétention supportée supprime un préfixe global ou tout l'historique ; la révocation est revérifiée pendant le flux. Un snapshot utilise le compteur global. Références : D03, D14, D25.
- Réexamen : introduction d'une rétention non préfixe ou de compteurs distincts par scope, avec protocole de reprise explicite.

## D33 — Verrouillage des autorisations sans droit de modification

- Date : 2026-10-03. Statut : retenu ; correction 0015 intégrée, contrôles SQL et reproductions Rust/PG vérifiés ; recette HTTP globale en cours.
- Contexte : le test PostgreSQL d'un membre Write valide montre une ligne en SELECT simple et zéro en SELECT FOR SHARE ; ce dernier applique la politique UPDATE réservée au propriétaire. Le worker ne possède aucun privilège UPDATE sur les autorisations. Donner ce privilège aux acteurs permettrait un élargissement de leurs propres droits.
- Choix : une fonction SQL à privilèges bornés vérifie rôle runtime, identité exacte du contexte, environnement, projet, adhésion et actions reconnues. Elle verrouille l'adhésion puis les seules autorisations courantes de l'acteur pour ce projet, et en retourne les champs nécessaires à la validation des plafonds Rust. Les rôles runtime ne reçoivent aucun droit UPDATE supplémentaire sur CapabilityGrant.
- Alternatives examinées : supprimer les verrous (réintroduit une course avec la révocation) ; rendre les propres grants modifiables (permet l'escalade). Rejetées.
- Conséquences : admission et intégration conservent la sérialisation avec retrait/révocation. Les appels dont acteur, projet, environnement ou actions ne correspondent pas ne retournent aucun grant. Les tests doivent vérifier les non propriétaires, le worker, les refus et le blocage concurrent d'une révocation. Références : D03, D14, D25, D28, D31.
- Réexamen : changement de la frontière d'identité du rôle PostgreSQL ou de l'ordre des verrous, accompagné d'une preuve de concurrence.

## D34 — Verrouiller un projet et enregistrer un job sans exiger Write

- Qualification finale (2026-10-03, SUIVI-0012) : 0016 intégrée et vérifiée dans le [run final](preuves/part1-e2e-20261003T204435-4ef16a288963cbbf.json), CI-13 et la [revue Sol](essais/part1-revue-finale.md). Installation neuve, mise à niveau depuis 0015, concurrence/rejeu, checksums et privilèges runtime inchangés vérifiés. Les événements et verrous comptables restent bornés ; le callback de registre/schéma du worker valide les preuves avant mutation. Aucun droit Write ajouté aux acteurs Execute+Model ou Budget-only ; contrat HTTP v1 conservé.

- Date : 2026-10-03. Statut : retenu et implémenté pour P1 ; vérification intégrale finale réussie dans le périmètre synthétique convenu.
- Contexte : la [reproduction SQL](essais/part1-foundation.md#suivi-0007--diagnostic-du-verrou-de-projet-à-ladmission) sous API et worker voit un projet avec Execute+Model, mais `FOR UPDATE` le masque par la policy Write|Manage. Budget-only rencontre le même verrou, puis les policies d’insertion du job et de sa commande. Ces droits doivent rester indépendants selon D30–D31.
- Choix : une fonction `kyro_lock_project_for_actor(project, actions)` vérifie rôle runtime, acteur du contexte, environnement, membre, projet et grants valides ; elle verrouille le projet puis relocke/revalide les grants avec D33. Elle retourne uniquement révision courante et limites. Les actions passées sont des alternatives, comme `authorize_demand_in` ; les exigences cumulatives restent vérifiées séparément dans Rust. Admission, annulation et exécution d’un job demandent Execute ; le rapprochement demande Budget ou Manage.
- Choix d’insertion : un job de rapprochement Budget|Manage est borné à un effet inconnu avec réserve maintenue du même projet/environnement. Une commande contenant `result.job_id` doit désigner un job persisté du même acteur/projet/environnement, avec l’autorité adaptée à son type. La voie des commandes directes sans référence de job conserve Write|Manage. Les entrées mal formées sont refusées ; aucune permission UPDATE projet ou grant n’est ajoutée.
- Alternatives examinées : donner Write aux appelants Model/Budget ou élargir la policy UPDATE projets (confond les droits) ; retirer le verrou (réintroduit les courses de quotas/CAS/idempotence). Rejetées au profit d’un verrou de métadonnées borné et de voies d’INSERT précises.
- Conséquences : migrations additives, appels de file adaptés et régressions avec Model sans Write, Budget sans Execute/Read/Write, mauvais scope et références de jobs étrangères. L’ordre projet→membership→grants et la sérialisation de révocation doivent être conservés. Aucun appel fournisseur réel n’est nécessaire. Références : D03, D14, D25, D30, D31, D33.
- Réexamen : nouveau type de job, nouvelle autorité ou changement d’ordre des verrous, avec recette de concurrence et isolation adaptée.
