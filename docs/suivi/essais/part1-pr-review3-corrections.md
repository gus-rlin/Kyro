# Cinq nouvelles corrections de la PR P1

Date : 2026-10-04 (Europe/Paris). Base revue : `78e83227aa5c648e97fdbc350f766dc1f4a48363`, worktree propre `p1-delivery-pr`, PR [#1](https://github.com/gus-rlin/Kyro/pull/1). Skill senior-code-basics appliqué ; checkout racine et preuves précédentes préservés.

| Remarque | Correction minimale | Régression |
| --- | --- | --- |
| Cookies HTTPS intersites | Helper commun `SameSite=None; Secure` en production ; Lax en développement ; HttpOnly session/binding, CSRF et Origin inchangés | Création/effacement des trois cookies, guards CSRF/Origin, CORS et sessions |
| SSE après accès DB | Extracteur de permit avant AuthActor ; permit détenu par le flux | Capacité saturée + pool PostgreSQL retenu : refus 429 avec/sans cookie en moins de 1 s ; rejet 401 après libération ne conserve aucun permit |
| IPv4 excessivement refusées | Préfixes exacts 192.0.0/24 et 198.51.100/24 ; TEST-NET-1 192.0.2/24 conservé explicitement | Registre cloud accepte les deux voisins publics et mapped IPv6 ; réservées/private/benchmark refusées |
| Politique secret publiée | Retrait uniquement de DataPolicy.allowed_categories | Contrôle OpenAPI + refus domaine existant ; ModelInput conserve secret pour refus explicite |
| Empreinte d'entrée publique | DTO EffectIntentView distinct, retrait du hash direct et imbriqué ; EffectIntent durable intact | Liste/détail et roundtrip DTO, hash durable, préparation/réconciliation/reprise et recette HTTP |

## Sources officielles consultées

Le [registre IANA IPv4](https://www.iana.org/assignments/iana-ipv4-special-registry/) consulté le 2026-10-04 confirme les deux /24 et TEST-NET-1. La politique conserve le refus des préfixes spéciaux existants ; elle ne prétend pas garantir la routabilité d'une adresse publique.

La [documentation Set-Cookie de MDN](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Set-Cookie), consultée le 2026-10-04, confirme None avec Secure pour l'envoi intersite. Le navigateur peut toujours bloquer les cookies tiers. Aucun test de navigateur réel en topologie HTTPS intersite n'est exécuté ; les attributs produits et les contrôles HTTP sont vérifiés. Le token CSRF est accessible via la route session avec credentials.

La suppression de `fingerprint` des réponses HTTP v1 est une correction de confidentialité : les consommateurs utilisant ces deux propriétés doivent cesser de les demander. Routes, requêtes et persistance restent identiques, sans migration ni dépendance nouvelle. L'empreinte de preuve comptable `evidence_fingerprint`, distincte de celle d'entrée, reste inchangée.

## Reproductions conservées

Snapshot de 78e8322 avec uniquement les nouvelles assertions/fixtures ; source originale conservée hors Git. [Première campagne](../preuves/part1-pr-review3-before-20261004.json), [log](../preuves/part1-pr-review3-before-20261004.log) : IPv4 publique refusée, hash public présent, enum secret annoncé. Base `kyro_p1_test_review3_baseline` conservée.

Les deux tests API initiaux n'ont pas démontré un défaut produit : nouveau test sans qualification de Duration, erreur de compilation corrigée. Une tentative suivante avec bash -lc a perdu le PATH Cargo ([log d'outil](../preuves/part1-pr-review3-before-api-tool-20261004.log)), puis l'exécution directe Cargo a démontré les deux défauts : cookies sans None et SSE 401 au lieu de 429 avant authentification ([log corrigé](../preuves/part1-pr-review3-before-api-20261004.log), 23 passes/2 échecs). Tous les logs sont conservés. Nettoyage des cinq crates avant chaque changement de snapshot pour éviter les résultats Cargo périmés.

## Contrôles finaux

Version fonctionnelle : `961ba2e0fec981cb67698f44560c55b1a2cebeb9`. Cinq commits ciblés : 970e263, db1df5f, c2d9d6b, e06adb6, 961ba2e. Empreinte `24c0277b53e10efd7228fe7849b6f278534dfc676ac5d7eeefed7305b3c882da`, [95 fichiers](../preuves/part1-pr-review3-source-final-20261004.json).

[Contrôles Linux/Docker](../preuves/part1-pr-review3-controls-20261004.json), [log](../preuves/part1-pr-review3-controls-final-20261004.log) : fmt read-only, cargo check --locked workspace/all-targets, migrateur admin et 87 tests avec include-ignored passent, zéro échec/ignoré ; source inchangée. Base `kyro_p1_test_review3_final` conservée. [17 migrations](../preuves/part1-pr-review3-migrations-20261004.json) intactes, checksums installés conformes ; [57 privilèges runtime](../preuves/part1-pr-review3-privileges-20261004.json) inchangés.

[Outils](../preuves/part1-pr-review3-tools-20261004.json), [log](../preuves/part1-pr-review3-tools-20261004.log) : OpenAPI 27 chemins/34 opérations, validateur OpenAPI 0.8.4, syntaxe runner et sept tests du classificateur passent. [Audit classifié](../preuves/part1-pr-review3-audit-summary-20261004.json) : 194 packages Linux actifs sans avis ; RSA 0.9.10 verrouillé inactif RUSTSEC-2023-0071 conservé, [brut](../preuves/part1-pr-review3-audit-raw-20261004.json) exit 1, classification exit 0. Aucune exclusion.

La [recette finale](../preuves/part1-e2e-20261004T010836-26bd77169bfade63.json), [console](../preuves/part1-pr-review3-e2e-console-20261004.log), sort 0 : P1-01 à P1-13 passent dans le même run, sources inchangées. Treize appels synthétiques ; liste/détail sans empreinte d'entrée, persistance intacte, 16 empreintes restaurées, ancienne session 401, nouveau login, ApplyChanges 5→6, rapprochement interne et émissions false/delta fournisseur 0. Bases `kyro_p1_e2e_26bd77169bfade63` et `kyro_restore_26bd77169bfade63` conservées. Le brut garde P1-14 review_pending pour la revue indépendante séparée.

[Revue indépendante Sol P1-14](../preuves/part1-pr-review3-sol-20261004.json) : PASS 9,5/10, aucun constat actionnable restant ; 95 hashes, 17 checksums, 57 privilèges et preuves de recette revérifiés. Les longues suites ont été relues sur preuves, sans nouvelle exécution par le reviewer. [Bilan consolidé des quatorze critères](../preuves/part1-pr-review3-qualification-20261004.json) : **P1 validée dans le périmètre synthétique convenu**. PostgreSQL/API/worker réels, OIDC/inférence synthétiques. NVIDIA/Nebius réels restent non qualifiés. Aucun merge, déploiement ni appel payant.
