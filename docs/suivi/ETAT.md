# État courant de Kyro

Dernière mise à jour : 2026-10-04 (Europe/Paris).

**P1 validée dans le périmètre synthétique convenu.** P1-01 à P1-13 passent dans une même recette sur PostgreSQL, API et worker réels sous Linux/Docker ; P1-14 reçoit une revue indépendante Sol PASS 9,5/10 sans constat critique ou élevé non résolu. OIDC et inférence restent synthétiques.

| Élément | État observé | Preuve ou limite |
| --- | --- | --- |
| Socle Rust, API et worker | Implémentés et vérifiés en intégration ; 87 tests Rust/PostgreSQL passent, zéro échec/ignoré | [Contrôle final après les cinq nouvelles remarques](preuves/part1-pr-review3-controls-20261004.json) |
| PostgreSQL et migrations | 0001–0017 ; installation neuve, mise à niveau depuis 0016, concurrence et rejeu vérifiés ; 0001–0016 intactes | [Migration finale](preuves/part1-pr-review2-upgrade-after-20261004.json), [privilèges runtime inchangés](preuves/part1-pr-review2-privileges-after-20261004.json), D34 |
| Identité, sessions et droits | Parcours OIDC synthétique, isolation lecture/mutation/SSE, CSRF, limites et révocation vérifiés | [Recette intégrale finale](preuves/part1-e2e-20261004T010836-26bd77169bfade63.json), P1-02/03/07/12 |
| Projets, révisions et commandes | CAS, idempotence, déduplication concurrente et file durable vérifiés | P1-04 à P1-08 dans la recette finale |
| Budgets, effets et passerelle | Réserves concurrentes, règlement unique, effets inconnus, rapprochement Budget-only et validation de preuve/registre/schéma vérifiés | P1-09 à P1-11 ; modèle synthétique uniquement |
| Sauvegarde et restauration | CLI réels, 16 empreintes conservées, sessions révoquées, sending→unknown, réserves maintenues ; reprise interne et rapprochement sans émission externe | P1-13 ; ApplyChanges révision 5→6, émissions false et delta fournisseur 0 |
| Revue indépendante finale | Sol PASS 9,5/10 ; aucun constat critique/élevé non résolu | [Revue P1-14 renouvelée](preuves/part1-pr-review3-sol-20261004.json) |
| Audit des dépendances | Aucun avis dans les 194 packages Linux actifs ; avis RSA verrouillé inactif conservé | [Audit courant classifié](preuves/part1-pr-review3-audit-summary-20261004.json) ; audit brut sort 1, classification sort 0 |
| Documentation de conception | Onze pages de référence conservées ; manifeste de 180 IDs, dont 160 capacités backend | [Index](../nvidia-hackathon/README.md), [manifeste](../backend/partie-1/MANIFESTE.md) ; les blocs des parties suivantes ne sont pas livrés par P1 |
| Configuration production | Backup/restore réels locaux Docker, certificats et données synthétiques, 16 empreintes conservées ; gardes Windows/Linux vérifiées | [Opérations finales](preuves/part1-pr-review2-operations-20261004.json) ; aucun déploiement réel |
| NVIDIA/Nebius réels | Non qualifiés | Aucun appel d'inférence réel ; cette limite demeure explicite |
| Comparatif Codex/Kyro et dossier du jury | Non réalisés par cette tâche | Aucun gain de coût/qualité/performance, déploiement ou soumission annoncé |

## Version et traçabilité

Version fonctionnelle corrigée : `961ba2e0fec981cb67698f44560c55b1a2cebeb9`, branche `gus-rlin/p1-delivery`. Les cinq nouvelles remarques sont corrigées par cinq commits ciblés ; 87 tests et recette intégrale réussis, revue Sol renouvelée PASS 9,5/10. Empreinte `24c0277b53e10efd7228fe7849b6f278534dfc676ac5d7eeefed7305b3c882da`, 95 fichiers inchangés. [Rapport](essais/part1-pr-review3-corrections.md), [bilan consolidé](preuves/part1-pr-review3-qualification-20261004.json), [inventaire](preuves/part1-pr-review3-source-final-20261004.json). Les deux propriétés publiques fingerprint sont supprimées pour confidentialité ; aucune migration modifiée, hash durable conservé. Les attributs None/Secure sont testés, sans qualification navigateur HTTPS intersite ; blocage possible des cookies tiers explicité.

Historique des huit corrections précédentes : Version fonctionnelle corrigée : `a718deb5a51e33601effa83250b4276636f6811f`, branche `gus-rlin/p1-delivery`. Les huit nouvelles remarques et les compléments de revue sont corrigés ; 86 tests et recette intégrale réussis, revue Sol renouvelée PASS 9,5/10. Empreinte `e2886f97905f2c61349c8ba8c8842a66944a3c4fb73a00aad308b688f2092b89`, 95 fichiers identiques avant/après recette. [Rapport des nouvelles corrections](essais/part1-pr-review2-corrections.md), [bilan consolidé](preuves/part1-pr-review2-qualification-20261004.json) et [inventaire source](preuves/part1-pr-review2-source-final-20261004.json).

Les neuf corrections précédentes restent qualifiées sur `77105d3` / `f96a145...` (79 tests), avec leurs [rapport](essais/part1-pr-review-corrections.md) et [preuves](preuves/part1-pr-review-qualification-20261004.json) conservés. Les refus et erreurs de ces campagnes sont explicités dans SUIVI-0015 et SUIVI-0016 ; aucun échec n'est remplacé par un succès.

Qualification historique conservée :

Base Git `d2d7fbafec5b1e3d50d4b6324ad3f40e836c8cc2`, branche `gus-rlin/backend-p1`, modifications sans commit automatique. L'empreinte des sources testées est `8da46273d2f17bb9abef667e05c7de7af54babdac8c203e089e5464d00a1fd4d`, identique avant/après CI-13 et E2E-12. [Rapport de clôture](essais/part1-cloture.md), [bilan consolidé](preuves/part1-close-qualification-final-20261003.json), [inventaire/empreintes](preuves/part1-close-final-source.json) et [archive des entrées exactes](preuves/part1-close-final-inputs.zip).

Les anciens contrôles de 70 tests et les runs arrêtés avant P1-06 restent historiques. Ils sont remplacés pour le verdict courant par les preuves finales ci-dessus, et restent conservés dans le [journal SUIVI-0012](JOURNAL.md#suivi-0012--boucler-p1-dans-le-périmètre-synthétique-convenu) et la [recette](essais/RECETTE-P1.md). Tous les rapports d'échec et leurs bases sont conservés.

## Suite du projet

La clôture de P1 est achevée dans le périmètre convenu. Aucune partie 2, campagne fournisseur réel/payante, publication, soumission ou déploiement n'a été lancé par cette tâche. Le lancement d'un chantier suivant demande une instruction distincte.


## Livraison en PR

À la demande explicite de l'utilisateur, le socle complet est préparé sur gus-rlin/p1-delivery, issue du main GitHub e277840. La qualification ci-dessus porte sur les sources avant normalisation Git des fins de ligne ; leur archive exacte reste fournie. Voir SUIVI-0013 dans le journal.

[PR #1](https://github.com/gus-rlin/Kyro/pull/1) ouverte sur main, branche gus-rlin/p1-delivery poussée avec les nouvelles corrections. Les deux contrôles GitHub sur 78e8322 ont réussi. Les nouvelles corrections sont qualifiées localement ; les contrôles GitHub après leur push restent distincts. Aucun merge ni déploiement.
