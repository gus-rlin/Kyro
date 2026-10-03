# Portée de livraison - Backend P1

## Référence de version

- Manifeste : [MANIFESTE.md, version 1.0](MANIFESTE.md), daté du 2026-10-03.
- Identifiant de portée : Kyro Backend P1-v1.
- Nature : baseline documentaire des contrats et preuves prévues, ni version logicielle ni tag de release.
- Révision de départ : HEAD 7b5f3e60dbde2fb32525ffe92ec421bcca62cc49 de la branche gus-rlin/backend-p1.
- État du périmètre : prévu. Ce document ne déduit aucun comportement fonctionnel du plan ou du manifeste.
- Recette indépendante : [ACCEPTATION.md, version 1](ACCEPTATION.md), scénarios P1-01 à P1-14.
- Rapport de contrôle documentaire : [part1-manifest.md](../../suivi/essais/part1-manifest.md).

Une modification de capacité, de partie responsable, de dépendance de livraison ou de critère nécessite une nouvelle version du manifeste et de cette portée. Le commit livré et les versions de dépendances seront liés aux résultats de recette acceptés, sans créer ici une release logicielle.

## Résultat P1 visé

P1 fournit un socle de pilotage durable en Rust utilisable par une API et un worker séparés, avec PostgreSQL réel, migrations et contrat HTTP versionné. La recette s’exécute par HTTP sans navigateur. La séparation API/worker conserve un monolithe modulaire et les contrats communs.

Le socle P1 prévu couvre :

- identité fédérée OIDC, sessions opaques côté serveur, expiration, rotation et révocation ;
- organisations/projets, décisions d’accès côté serveur, portée de locataire et isolation renforcée par RLS PostgreSQL ;
- projets, AppSpec versionné, révisions immuables et ChangeSet typé avec comparaison sur révision attendue ;
- travaux persistants, baux et générations, admission bornée, idempotence, outbox transactionnelle et intentions d’effets externes ;
- passerelle de modèles avec budget réservé atomiquement, registre de consommation et configuration/version de prix conservée ; destination, catégorie, format, délai, taille et référence de secret contrôlés ;
- événements de projet persistants et flux SSE reprenable par Last-Event-ID, avec autorisation à la connexion et coupure après révocation ;
- sauvegarde/restauration isolée de l’état de pilotage, en préservant les travaux et effets en attente sans réémission automatique ;
- configuration typée, erreurs stables, santé, traces expurgées et dimensions métriques bornées.

Les scénarios P1-01 à P1-14 d’ACCEPTATION.md fixent les preuves attendues, les composants réels ou simulés et les limites. Ils couvrent migrations et démarrage, identité, séparation des projets, révisions, idempotence, reprise des travaux, résultats obsolètes, limites, budgets, effets inconnus, passerelle, SSE, restauration et documentation/revue. Leur énoncé ne prouve pas que ces comportements sont déjà implémentés.

## Frontière de catalogue

La [baseline MANIFESTE.md](MANIFESTE.md) suit les 180 IDs du catalogue dans les 18 familles et garde la vision généraliste. La réservation et les autres recettes sont des exemples ; aucune verticale unique n’est actée.

Les 160 capacités backend B001-B060 et B081-B180 restent des cibles complètes des parties 2 à 5. P1 prépare et qualifie seulement les socles communs explicitement listés par ID dans le manifeste. Une ligne « socle P1 prévu » n’annonce pas que le bloc complet est livré : les 180 états de ce manifeste restent « prévu - non qualifié » tant que la recette correspondante ne fournit pas de preuve.

La répartition suit le plan des cinq parties : P2 construit les capacités applicatives backend, le registre, la résolution, l’assembleur déterministe, les sandboxes et les preuves ; P3 ajoute plans, ordonnanceur, mémoire et délégation ; P4 couvre publication, configuration et restauration des applications ; P5 couvre exploitation, incidents, maintenance, évolution du catalogue, qualification complète de récupération, export et retrait. Les blocs B159 et B168 utilisent le contrat de passerelle/budget exercé en P1 au simulateur ; la qualification du fournisseur et des rôles reste future.

B061-B080 sont les 20 composants d’interface Rust/Leptos du chantier d’interface séparé. Ils ne font pas partie de la livraison backend P1-P5. Le backend livre dans ses parties les contrats dont l’interface dépend : identité, autorisations, données filtrées, révisions, événements et préparation d’aperçus. React/TypeScript reste l’enveloppe IDE ; il ne remplace pas les blocs d’interface Leptos ou le backend Rust.

## Services simulés et limites

L’API HTTP du fournisseur de modèles est simulée avec un serveur de test contrôlé selon ACCEPTATION v1. PostgreSQL, l’API Kyro et le worker sont les composants réels visés par cette recette. Le simulateur vérifie les contrats de passerelle, de budget, de réponse et de refus, mais ne qualifie pas l’intégration réelle NVIDIA ou Nebius. Aucun secret fournisseur réel n’est enregistré dans le dépôt.

Ce manifeste et cette portée définissent un plan versionné. Ils ne déclarent aucun bloc implémenté, aucune recette réussie, aucun déploiement, SLA, gain mesuré ou fournisseur externe qualifié.
