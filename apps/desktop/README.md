# Interface desktop Kyro

Frontend React + TypeScript, Vite et Electron. Sources de l’interface actuelle : `src/`, assets inclus. Le backend Rust est à la racine dans `crates/`.

## Installation et lancement

Windows x64, Node.js 22.12+ et npm. Depuis ce dossier :

```powershell
npm ci
npm run dev
```

`dev` télécharge MinGit depuis la source officielle épinglée dans `git-runtime.json`, contrôle le SHA-256, puis démarre Vite et Electron. Git reste privé à Kyro et ne modifie pas le PATH système. Pour un aperçu navigateur : `npm run prepare:git`, puis `npm run dev:web`. L’adaptateur natif de développement est limité au loopback ; le paquet utilise IPC. Le port 5174 est fixe : arrêter l'ancien aperçu avant d'en lancer un autre.

Electron 44.5.1 télécharge son binaire au premier import. Si ce téléchargement échoue, rétablir l'accès réseau ou réutiliser une installation locale de cette même version ; `npm ci` seul ne garantit pas la présence du binaire.

## Backend et chat

P1, P2 et P3 sont réunies dans la branche `gus-rlin/kyro-integration`, avec les corrections P3 de `3ec918b`. Les contrats et opérations se trouvent dans [P1](../../docs/backend/partie-1/OPERATIONS.md), [P2](../../docs/backend/partie-2/OPERATIONS.md) et [P3](../../docs/backend/partie-3/README.md).

Le chat Nano utilise un runtime local séparé. Depuis la racine, après configuration du compte, de sa politique de données et de son budget :

```powershell
./scripts/nebius-runtime.ps1 -Profile Chat -Action Start
```

Pour utiliser une image locale construite depuis cette branche, ajouter `-RuntimeImage kyro-integration:3ec918b`. Ce nom correspond à l'image locale de développement construite pendant l'intégration ; elle n'est pas publiée. Une installation neuve peut construire sa propre image depuis le Dockerfile du dépôt. Le lancement conserve le projet, les données et le budget existants ; il n'envoie pas de message modèle. L'interface indique un runtime indisponible quand ce service est arrêté.

Le composeur propose Conversation (Nano) et Agents (P3). Le mode Agents choisit un projet backend, consulte son équipe/outils, propose un plan puis permet son exécution, son suivi et son annulation. Le runtime doit configurer l'équipe, la fabrique et admettre le catalogue ; le runtime Chat seul signale cette absence. [Parcours et validation P3](../../docs/backend/partie-3/IDE.md). L'aperçu d'une application générée et la publication restent à raccorder.

## Vérification

```powershell
npm run prepare:git
npm run build
npm test
```

Avec l’aperçu Vite déjà lancé sur le port 5174, `node scripts/check-native-boundary.mjs` vérifie ses refus Host/Origin et les requêtes invalides. Arrêtez cet aperçu avant `npm test`, car la recette de rechargement utilise aussi ce port.

Les tests Electron nécessitent une session graphique Windows. Ils utilisent des profils et dossiers temporaires. Les captures sont produites localement dans `docs/suivi/preuves/` ; elles ne sont pas nécessaires au build. `npm run package` produit le paquet Windows dans `out/` avec le runtime Git embarqué.

## Fonctions présentes et limites

- Interface Kyro, navigation, thèmes clair/sombre, composeur et menus personnalisés.
- Création et ouverture de projets avec confirmation de confiance, explorateur et worktrees locaux.
- Équipe NVIDIA : sélection orchestrateur/sous-agents, effectif, comparaisons tarifaires documentées dans `src/team-models.ts`.
- Préférences d’accès, raisonnement et contexte avec confirmation de l’accès complet.

Les messages du chat sont envoyés à Nano lorsque son runtime et la politique du compte sont configurés. Aucun fichier du dossier choisi n'est automatiquement joint. Les traces et le budget sont persistés côté backend ; l'historique visible du chat disparaît au rechargement, les plans P3 restent lisibles depuis leur projet. La capture microphone et la publication ne sont pas connectées. Le choix « Accès complet » n’accorde pas de nouveaux droits système. Le paquet est non signé. Les binaires, dépendances installées et fichiers de configuration privés ne sont pas versionnés ; le lockfile, les sources et les instructions permettent leur reconstruction.

Les composants tiers et leurs conditions sont documentés dans [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
