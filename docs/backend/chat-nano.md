# Chat Nano local : connexion et activation

Le chat React utilise `nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B` sur Nebius. Electron et Vite partagent le même pont Node limité à l’état, l’envoi, la lecture du flux et l’arrêt. L’API et le worker Rust réutilisent les autorisations, jobs, effets et réservations PostgreSQL. La version immuable des poids n’est pas fournie par le catalogue ; elle reste inconnue.

Par défaut, les appels réels exigent la confirmation de zéro conservation du compte. Une autorisation humaine explicite permet aussi les essais Nano aux conditions habituelles du compte, exclusivement dans le runtime Chat de développement et sous le même plafond cumulé de 1 €. `store: false` reste envoyé ; il ne constitue pas une preuve de zéro conservation. Les [conditions Nebius, sections 5.1 et 5.4](https://docs.nebius.com/legal/token-factory) décrivent les usages des messages et la demande d’opt-out. Les tests synthétiques restent distincts de la qualification réelle.

## Préparer et démarrer

Prérequis : Windows, PowerShell 7, Docker Desktop Linux, Node et dépendances du desktop installées, coffre DPAPI Nebius existant créé par `scripts/nebius-vault.ps1`. Exécuter depuis le checkout frontend utilisé par l’interface.

```powershell
docker build -t kyro-nebius-chat:local .
docker build -t kyro-nebius-guard:local -f ops/nebius/Dockerfile.guard ops/nebius
docker build -t kyro-nebius-oidc:local -f tests/fixtures/Dockerfile.synthetic-provider .
./scripts/nebius-runtime.ps1 -Profile Chat -Action Prepare
./scripts/nebius-runtime.ps1 -Profile Chat -Action Status
```

`Prepare` consulte uniquement le catalogue des modèles et le taux BCE. Il ne lance aucune inférence. Une deuxième préparation est refusée lorsque le budget existe. L’état est conservé dans `%USERPROFILE%\.kyro\nebius-chat`, distinct de `%USERPROFILE%\.kyro\nebius-p1`. Ne pas supprimer `budget.json`, `client.json` ni le volume PostgreSQL pour redémarrer : leur conservation garantit le plafond cumulé.

Après réception et examen d’une confirmation d’opt-out applicable au compte lié à la clé, préparer **hors dépôt** `qualification.json` dans cet état runtime, à partir de `config/qualification.nebius.chat.example.json` :

- `account_evidence` : référence locale à la confirmation examinée, sans secret ni donnée personnelle copiée dans Git ; une page générique de conditions ne suffit pas.
- `zero_retention_confirmed` : vrai uniquement après cette confirmation.
- `checked_at` : date ISO réelle de la vérification, avec décalage UTC ; la preuve expire après 30 jours et lors d’un changement de coffre.
- `vault_sha256` : SHA-256 du fichier **chiffré** `%LOCALAPPDATA%\Kyro\secrets\nebius.dpapi`, obtenu avec `Get-FileHash`. Le pont ne déchiffre jamais la clé.
- `max_completion_tokens_includes_reasoning` : vrai après vérification de la borne dans le [contrat d’inférence Nebius](https://docs.tokenfactory.nebius.com/api-reference/inference/create-chat-completion).
- `endpoint`, `model`, `source` : valeurs du contrat et URL officielle examinée. Ne pas remplacer la preuve du compte par `store: false`.

Pour l’autre voie, uniquement après autorisation explicite des conditions habituelles, le fichier privé conserve les mêmes contrôles de modèle, endpoint, date, coffre et borne de génération ; il porte `retention_mode: "provider_standard"`, `standard_retention_accepted: true`, `zero_retention_confirmed: false`, `account_evidence: ""` et une `consent_reference` non vide vers l’autorisation examinée. Ne pas inventer une confirmation de zéro conservation. Le registre Chat indique alors une durée de conservation inconnue (`null`) et `provider_standard_retention_accepted: true`. Le projet privé autorise cette durée inconnue uniquement pour `conversation` ; le comportement historique et P1 restent stricts. Ce mode est refusé en production et pour les autres modèles ou modes de registre.

Puis :

```powershell
./scripts/nebius-runtime.ps1 -Profile Chat -Action Start
# Dans apps/desktop :
npm run dev
# Ou utiliser l’aperçu Vite existant sur http://127.0.0.1:5174 et cliquer « Revérifier ».
```

`Start` refuse une qualification absente/invalide avant de lancer les services. Il applique les migrations, qualifie le registre texte et transmet la clé DPAPI au seul worker par son tube privé et son tmpfs. Les sessions OIDC et le jeton CSRF restent dans le pont ; le renderer ne reçoit aucun identifiant de session ni clé fournisseur. L’identité OIDC est synthétique et locale dans ce runtime de développement. Cette connexion ne constitue pas une authentification de production.

Le runtime du chat utilise les ports loopback 58190, 59190 et 59191, le projet Compose `kyro-nebius-chat` et les réseaux 10.248.75/76. Les valeurs par défaut du profil P1 restent séparées. DNS, IPv6 et sorties autres que PostgreSQL et les IP Nebius autorisées restent interdits au worker. Les renouvellements explicites des IP et certificats passent par `RefreshPins` et `RefreshTls` ; ils ne renouvellent pas le budget. Arrêter avec `-Profile Chat -Action Stop`.

## Comportement et comptabilité

Le premier accès qualifié crée un projet backend privé de conversation, sans dossier sélectionné. Un verrou local de provisionnement partagé entre Vite et Electron, puis `client.json`, conservent la référence du même projet après redémarrage. Le plafond de 1 € finance à la fois les essais réels et les premières conversations. Il n’est jamais rechargé automatiquement.

Le budget est exprimé en estimations tarifaires USD, avec conversion BCE datée et marge de 40 %. Pour l’état préparé le 2026-10-04 : 1 EUR = 1,1225 USD au 2026-10-02, plafond utilisable 0,6735 USD, unité 10⁻⁹ USD. Les tarifs sont ceux du catalogue daté ; la facturation réelle est indisponible. Faute de tokenizer fournisseur qualifié, chaque appel réserve de manière conservatrice tout le contexte du catalogue plus 2 048 tokens de sortie, puis règle l’estimation avec l’usage du reçu final. Une réservation inconnue reste comptée et peut bloquer les appels suivants.

Chaque appel est limité à 60 secondes et 2 048 tokens générés, raisonnement compris. Le mode `text_chat` envoie `stream: true`, `store: false`, `stream_options.include_usage`, une instruction système fixe et les seuls messages utilisateur/assistant. Aucun fichier local n’est ajouté. Les champs de raisonnement sont ignorés et les résultats texte sont enveloppés localement dans le contrat d’effet structuré existant. Les registres historiques gardent `structured_json` par défaut et leur empreinte historique.

Le contexte 4k/8k/16k utilise une borne conservatrice en octets et retire les paires complètes les plus anciennes. Le dernier message est conservé ; un message trop grand est refusé avant l’envoi. Une réponse arrêtée/invalide reste visible et son échange est exclu du prochain contexte. Une fin `length`, y compris après raisonnement sans texte visible, est signalée comme tronquée. Les autres modèles, sous-agents, outils et réglages du raisonnement sont indisponibles.

Les fragments sont regroupés sur 100 ms et stockés par job/génération/numéro dans `chat_stream_chunks` (migration 0018). Lecture API toutes les 250 ms via le SSE authentifié `/v1/projects/{project_id}/jobs/{job_id}/stream`. Le curseur `job:génération:numéro` reprend le flux sans créer de job ni refaire d’inférence. Un identifiant d’idempotence stable protège aussi le rejeu d’un envoi incertain. L’accès et le bail sont revérifiés ; un bail périmé ne peut publier. Limites : 1 MiB de SSE fournisseur, 64 KiB par trame, 32 KiB de texte visible et 4 KiB par fragment persisté.

Les vérifications cumulatives et la retenue de suffixe empêchent de transmettre une clé chargée ou les formes de secrets reconnues lorsqu’elles sont réparties entre fragments. Elles ne sont pas un détecteur universel de données sensibles. Le rendu React reste du texte échappé. **Arrêter** annule le job ; si l’envoi a commencé et l’usage final manque, la réservation reste conservée et l’interface le signale.

L’historique React est effacé au rechargement. Les entrées techniques des jobs, fragments et effets restent dans PostgreSQL pour la reprise et la comptabilité, y compris les messages nécessaires à l’inférence. La limitation de l’historique visible ne promet pas leur effacement local.

## Vérification

```powershell
./scripts/verify-chat.ps1
node scripts/check-openapi.mjs
```

La recette utilise des API/worker Rust et PostgreSQL réels avec OIDC et inférence synthétiques, dans des conteneurs temporaires dédiés. Elle exécute les tests Rust/PostgreSQL, l’installation et la mise à niveau 0017→0018, le build TypeScript/Vite et les scénarios Electron/navigateur. Elle ne lit pas DPAPI et n’utilise aucune clé Nebius. Le fournisseur synthétique porte l’identifiant Nano uniquement comme étiquette de contrat ; aucun poids NVIDIA n’est exécuté. Le script refuse les noms de conteneurs/réseau déjà occupés et nettoie seulement ses ressources étiquetées. Les caches de compilation restent disponibles.

Après activation autorisée, vérifier deux échanges avec des messages synthétiques depuis l’interface, par exemple « Retenons le mot Cèdre » puis « Quel mot avons-nous retenu ? ». Vérifier streaming, mémoire, reçu fournisseur, tokens et règlement du même budget. Consigner appels, latence, refus/reprises et montants estimés sans enregistrer clé, cookie, raisonnement interne ou données personnelles.

Le 2026-10-04, après autorisation explicite des conditions habituelles, deux échanges Nebius depuis Vite ont réussi : mémoire de « Cèdre », reçus et usage final présents, réservations réglées. Estimation cumulée : 0,00025908 USD, aucune réservation restante ; la facturation réelle reste inconnue. La recette distincte passe avec 111 tests Rust/PostgreSQL et 11 tests frontend. Le paquet Windows n’a pas été reconstruit.
