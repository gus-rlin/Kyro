# Interface desktop Kyro

Frontend React + TypeScript, Vite et Electron. Sources de l’interface actuelle : `src/`, assets inclus. Le backend Rust est à la racine dans `crates/`.

## Installation et lancement

Windows x64, Node.js 22.12+ et npm. Depuis ce dossier :

```powershell
npm ci
npm run dev
```

`dev` télécharge MinGit depuis la source officielle épinglée dans `git-runtime.json`, contrôle le SHA-256, puis démarre Vite et Electron. Git reste privé à Kyro et ne modifie pas le PATH système. Pour un aperçu navigateur : `npx vite --host 127.0.0.1`. L’adaptateur natif de développement est limité au loopback ; le paquet utilise IPC.

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

Les messages et réglages d’agents restent locaux : aucune inférence, publication ou capture microphone n’est connectée. Le choix « Accès complet » n’accorde pas de nouveaux droits système. Le paquet est non signé. Les binaires, dépendances installées et fichiers de configuration privés ne sont pas versionnés ; le lockfile, les sources et les instructions permettent leur reconstruction.

Les composants tiers et leurs conditions sont documentés dans [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
