# Consolidation GitHub — 2026-10-06

## Périmètre

La référence de départ est `main` à `a6f0573`, qui contient déjà les PR #1 à #6 :
socle P1, intégration Nebius, IDE desktop, chat Nano, fabrique P2 et agents P3.
La consolidation ajoute les commits locaux `2d0ba6e` et `0f931cd` : contrôles
desktop réconciliés, pont P3 fermé et authentifié, inventaire des capacités,
plans dans le composeur et contrat OpenAPI mis à jour.

L'historique de la branche P1 est aussi raccordé : son dernier commit
`6d8b1e1` retire les documents internes du suivi Git, politique déjà présente
dans main. Deux conflits sont résolus en préservant le vérificateur P1 actuel
et la preuve publique P3 ; aucun changement de comportement supplémentaire
n'est introduit par ce raccordement d'historique.

Le README présente la vision généraliste, le catalogue cible, l'équipe
d'agents, l'architecture, l'état livré et les étapes P4/P5. L'index documentaire
et le guide de contribution donnent des points d'entrée publics. Les travaux
locaux non commitées, notamment le profil P3 et l'aperçu Leptos en cours,
restent séparés de cette livraison.

## Correction du contrôle CI

La CI de `a6f0573` [échoue à l'audit des dépendances](https://github.com/gus-rlin/Kyro/actions/runs/37403202740)
avec `workspace_roots_mismatch`. Le classifieur attend sept crates ; le
workspace en contient huit après l'ajout de `kyro-agents`.

La reproduction adapte d'abord les fixtures au workspace réel : cinq des
sept tests existants échouent avant la correction. L'ajout de `kyro-agents`
à la liste contrôlée corrige le défaut. Deux régressions supplémentaires
vérifient le refus d'une vulnérabilité active atteinte par P3 et le refus d'un
graphe incomplet. Les neuf tests passent ; la CI les exécute maintenant avant
l'audit réel. Aucun avis n'est supprimé ni transformé en succès par cette correction.

## Vérifications locales

Environnement : Windows x64, Node.js 24.18.0, Electron 44.5.1 ; Rust 1.96.1
sur Linux/Docker, outils retenus, dépendances vendoriées, réseau désactivé et
sources montées en lecture seule pour la suite Rust.

| Contrôle | Résultat observé | Limite |
| --- | --- | --- |
| `npm ci --no-audit --no-fund` | 96 packages installés | Installation du binaire Electron différée |
| `npm run build` | TypeScript et Vite réussis | Téléchargement Electron initial échoué ; même version locale réutilisée, SHA-256 identique |
| `node --test tests/chat-service.test.cjs tests/plans-service.test.cjs` | 13 réussites | Doublures ; aucune inférence réelle |
| `npx playwright test tests/plans.spec.mjs tests/model-picker.spec.mjs tests/composer-tooltips.spec.mjs tests/team-cost.spec.ts` | 7 réussites en 26,5 s | Navigateur et Electron ; P3 simulée, aucun fournisseur réel |
| `cargo test --locked --offline --workspace` | 155 réussites, zéro échec, 111 recettes explicitement ignorées | PostgreSQL, coordinateur et fabrique protégée non requalifiés ici |
| `cargo fmt --all -- --check` | Réussi | Format des sources présentes |
| `node scripts/check-openapi.mjs` | Réussi, 44 chemins / 53 opérations | Contrôle du contrat versionné |
| `node --test scripts/check-dependencies.test.mjs` | 9 réussites après reproduction du défaut | Fixtures d'audit ; l'audit fournisseur d'avis reste une étape CI distincte |
| Syntaxe des scripts, liens locaux des documents actualisés et `git diff --check` | Contrôlés avant publication | Aucun test documentaire artificiel ajouté |

Le téléchargement initial d'Electron échoue avec `fetch failed`. La réutilisation
de son installation locale est vérifiée par empreinte :
`49b61a030a520fc36a4b8fa5cce53fb4e935a7bdbbe4b80e9222f598e49cc7fa`.
Cela ne prouve pas une installation neuve sur un autre réseau. La première
sortie Rust est trop volumineuse pour le retour de console ; la suite est
rejouée avec son cache de compilation pour conserver le journal et compter
les résultats exacts.

## Branches et limites

`main` devient la référence commune. Les branches de livraison distantes sont
retirées uniquement après vérification que leurs commits sont ancêtres de la
version publiée. Les branches et worktrees locaux contenant des travaux
distincts ou non enregistrés sont préservés. Aucun historique n'est réécrit.

La consolidation ne déploie aucun service, ne migre aucune base utilisateur,
ne lance aucun appel modèle payant et ne renouvelle aucune admission de
catalogue. Les preuves NVIDIA restent historiques et liées à leurs sources.
Le runtime Chat seul ne fournit pas une équipe P3 configurée. Le parcours
applicatif complet avec aperçu, publication et maintenance reste à poursuivre.

La [page Actions](https://github.com/gus-rlin/Kyro/actions/workflows/ci.yml)
porte les résultats de CI distante ; les succès locaux de ce rapport ne les
remplacent pas.
