# P3 dans le composeur de Kyro

Le menu **Votre équipe** propose **Conversation** (Nano) et **Agents** (P3).
Le mode Agents affiche les rôles configurés par l'opérateur : orchestrateur,
Pixel, Moka, Kiwi, Biscotte, revue et sécurité. Il ne remplace pas les modèles
et ne dépasse pas les quatre exécutants. L'ancien choix jusqu'à 32 était une maquette.

## Parcours

1. Choisir Agents puis un **projet de construction** parmi les 32 derniers
   projets visibles de la session backend. Le dossier Git local et ce projet
   sont deux sélections distinctes ; aucun fichier local n'est envoyé.
2. Consulter les modèles, outils configurés et budget au dernier contrôle.
   `GET /v1/projects/{id}/plans/capabilities` est authentifié et exige le droit
   de lecture. Un outil configuré ne confère aucune permission.
3. Envoyer le brief avec **Proposer un plan**. Cela sollicite l'orchestrateur
   et peut consommer le budget ; `plan_only: true` ne modifie pas l'application
   et ne lance pas la fabrication.
4. Examiner tâches, blocs, dépendances et diagnostics. **Exécuter le plan**
   appelle P3 avec la version du plan affichée.
5. Suivre les transitions, annuler ou retrouver les plans persistés en
   rechoisissant le projet après rechargement. Un candidat vérifié n'est pas
   publié ; l'aperçu Leptos de ce candidat reste à connecter à l'IDE.

Le pont réutilise session/CSRF P1 et des commandes fermées. Il ne crée/finance
pas de projet, ne modifie pas permissions/politique et n'ouvre pas les campagnes
scellées. P3 recontrôle accès, politique, modèle, budget, catalogue et versions.
Un envoi incertain garde sa clé et sa révision source pour une reprise explicite.
Les mutations ne sont pas répétées automatiquement, sauf le renouvellement
P1 existant après un refus d'authentification confirmé.

Bornes initiales : 32 appels, deux millions de tokens cumulés, 4096 tokens de
sortie/appel, 30 secondes/appel, 1800 secondes/plan et contexte encodé 4/8/16 Ko.
Les limites et réservations financières du projet s'appliquent indépendamment.

## Activation

Le runtime Chat actif n'a ni équipe ni fabrique P3 : **agents_unconfigured**.
Le brouillon est conservé et aucun plan n'est envoyé. P3 existe dans les sources
mais ce service ne l'active pas. Configurer selon [OPERATIONS](OPERATIONS.md) :
registre structuré qualifié, sept rôles, fabrique/signataire, catalogue signé
admis sur les sources concernées et projet avec droits, politique et budget.
Le registre texte Nano ne qualifie pas les contrats d'agents. Le consentement
Conversation ne modifie pas `accepted_unknown_retention_purposes` pour
planning/generation/review. Le frontend ne contourne pas ces contrôles.

L'image locale non publiée `kyro-composeur:local` contient l'API de ce
raccordement, sur la base `kyro-integration:3ec918b`. Elle conserve les autres
binaires. Depuis la racine :

```powershell
./scripts/nebius-runtime.ps1 -Profile Chat -Action Start -RuntimeImage kyro-composeur:local
```

Cela conserve les données/budgets Chat ; aucune configuration P3 ou inférence
n'est effectuée automatiquement. Le Dockerfile complet neuf n'est pas requalifié.

## Validation — 2026-10-06

- Build TypeScript/Vite et API Linux hors réseau réussis.
- Suite frontend : 21 réussites, quatre intégrations Chat ignorées, 48,7 s.
  Test Electron P3 ajouté ensuite : une réussite, 2,0 s.
- Trois recettes navigateur P3 avec backend simulé : plan, exécution, candidat,
  rechargement, annulation, mobile, refus, envoi incertain. Test Electron avec
  handlers IPC synthétiques. Aucune génération contre coordinateur réel prouvée ici.
- Treize tests Node Chat/P3 réussis : session/CSRF, idempotence, préconditions,
  champs fermés, UTF-8 et révision source zéro.
- Rust API/agents : 33 réussites, neuf recettes ignorées. Les recettes
  coordinateur/fabrique/NVIDIA ne sont pas réexécutées dans cette tâche.
- API locale réelle récente : inventaire Chat non configuré HTTP 200,
  accès sans session HTTP 401, projet invisible refusé (`not_found`).
  Budget relevé inchangé, aucune mutation de plan ni nouvelle inférence réelle.
- OpenAPI : 44 chemins, 53 opérations ; format du diff contrôlé.

Échecs et corrections conservés dans SUIVI-0067 : premier conteneur sans PATH
Rust, port 5178 occupé (recettes ensuite sur ports éphémères), premier patch
documentaire refusé. Anciennes preuves NVIDIA toujours liées à leurs sources.
