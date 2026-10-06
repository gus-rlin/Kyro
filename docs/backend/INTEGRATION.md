# Version locale intégrée — 2026-10-06

Mise à jour du composeur : modes Conversation/Agents et commandes P3 reliées
au backend. [Parcours, configuration et validation](partie-3/IDE.md).
Le runtime local récent utilise `kyro-composeur:local` ; il signale explicitement
l'absence d'équipe P3 au lieu de présenter des sous-agents simulés comme actifs.
Les contrôles ci-dessous restent ceux de la consolidation initiale.

La branche `gus-rlin/kyro-integration` réunit `main` a12e1ce (frontend,
chat corrigé et P2) et P3 avec ses corrections de revue 3ec918b. Les worktrees
d'origine sont préservés. Les modifications historiques de desktop-glass ne
sont pas recopiées : elles précèdent les menus, l'aperçu et le chat livrés.

## Démarrage

Depuis `apps/desktop`, installer les dépendances puis lancer l'aperçu :

```powershell
npm ci
npm run prepare:git
npm run dev:web
```

Pour Electron, utiliser `npm run dev` à la place de `dev:web`. Un seul aperçu
peut écouter sur `127.0.0.1:5174`. Electron télécharge son binaire au premier
import ; ce téléchargement nécessite un accès réseau fonctionnel.

Le chat utilise le compte et le budget locaux déjà configurés, sans joindre les
fichiers du dossier ouvert. À la racine :

```powershell
./scripts/nebius-runtime.ps1 -Profile Chat -Action Start -RuntimeImage kyro-integration:3ec918b
```

Cette image de développement locale, non publiée, contient les binaires API,
worker et migrateur compilés depuis 3ec918b avec Rust 1.96.1 sous Linux.
Elle est construite sur la base locale `kyro-nebius-chat:local`, en remplaçant
les trois binaires. Son digest local est
`sha256:1c1e30ae68b383deb3aa8cfb2ad95a6431776a10d49ae1d4ca9ddd829552be27`.
Pour une installation indépendante, construire une image à partir du
Dockerfile du dépôt et fournir son nom à `-RuntimeImage` ; la construction
complète depuis ce Dockerfile n'a pas été requalifiée pendant cette tâche.

`Start` exécute les migrations, démarre les services locaux et conserve les
données et le budget existants. Il n'envoie pas de demande d'inférence. Le
backend écoute sur `127.0.0.1:58190`, l'identité synthétique locale sur 59190.

## Contrôles exécutés

- Build TypeScript/Vite réussi.
- Suite frontend complète : 22 réussites, zéro échec, zéro scénario ignoré,
  89,37 s. Les essais de chat utilisent PostgreSQL/API/worker réels et un
  fournisseur synthétique ; ils couvrent streaming, mémoire de conversation,
  reprise, arrêt, erreurs, budget et frontières locales.
- Après stabilisation du chargement des fixtures de fenêtres avec le runtime
  utilisateur actif : six scénarios desktop réussis à nouveau en 17,9 s.
- Service de chat : huit tests réussis avec doublures, dont renouvellement
  de session concurrent et maintien des clés d'idempotence.
- Rust sous Linux/Docker : 155 réussites, zéro échec, 111 recettes ignorées
  nécessitant des environnements distincts ; aucun résultat ne leur est attribué.
  Les binaires API, worker et migrateur sont construits depuis cette version.
- Aperçu HTTP 200 ; `/health/live` et `/health/ready` HTTP 200.
  API et worker actifs avec l'image récente ; statut du chat `ready: true`.
  Aucun nouvel échange Nebius envoyé pendant cette tâche.
- Format du diff et parsing PowerShell réussis.

## Corrections et limites

Les anciens tests desktop visaient des écrans et boutons retirés. Ils sont
adaptés au frontend livré, en conservant les contrôles de sandbox, d'opacité,
de navigation, de worktrees Git réels, de refus et de rechargement à chaud.
Les recettes de fenêtres bloquent explicitement les inférences payantes.
Les textes du chat, du modèle et de l'en-tête compact respectent désormais
le minimum de 14 px vérifié par les recettes existantes.

Windows a bloqué les build scripts Rust (erreur 4551) ; aucune politique
système modifiée. Les contrôles ont été exécutés sous Linux. Les réseaux
Docker automatiques étaient épuisés et Windows réservait le port de test
58690 ; une base jetable sur un sous-réseau explicite et un mapping API
18690 ont permis les essais. Ces services de test ont été retirés ensuite.

P3 est présente côté backend. La configuration de l'équipe, de la fabrique et
l'admission du catalogue restent des opérations distinctes décrites dans
[P3](partie-3/OPERATIONS.md). Le composeur peut désormais proposer, suivre,
exécuter et annuler les plans P3 lorsque ce runtime est configuré. Le service
Chat actif reste sans équipe P3. L'aperçu d'une application générée et sa
publication restent à raccorder. Aucune qualification générale de production,
nouvelle preuve NVIDIA ou nouvelle attestation de catalogue n'est revendiquée.

L'outil de contrôle de l'onglet utilisateur a refusé l'accès au titre de sa
politique d'URL. Le rechargement de cet onglet reste manuel ; les contrôles
navigateur/Electron ci-dessus proviennent des recettes isolées.
