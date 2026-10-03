# Contrat HTTP P1 — API v1

État : en cours d’implémentation et de vérification locale. Ce document décrit le contrat visé par l’API de pilotage P1; il ne qualifie ni le fournisseur de modèles ni un déploiement.

Le schéma OpenAPI versionné est [`openapi.v1.json`](openapi.v1.json). Le workflow valide son JSON, ses références et la présence des routes documentées dans les sources Axum.

## Origine et authentification

Les routes applicatives sont sous `/v1`. Le serveur n’exécute pas les migrations à son démarrage. `GET /health/live` indique que le processus répond; `GET /health/ready` vérifie la base et le rôle d’exécution sans exposer les détails de connexion. Un échec de readiness renvoie `503`.

`GET /metrics` expose au format Prometheus un compteur HTTP aux étiquettes bornées `method` (GET, HEAD, POST, PUT, DELETE, PATCH, OPTIONS, OTHER) et `status_class` (1xx–5xx, other), ainsi qu’un histogramme sans étiquette du temps jusqu’à la production des en-têtes de réponse. Les métriques ne comportent pas de label route, d’identifiant, de query string ou de contenu client. Dans la trace JSON, `route` désigne séparément le template de route Axum obtenu par `MatchedPath`, ou `unmatched` si aucune route ne correspond; il s’agit d’une valeur tirée des routes déclarées, jamais de l’URL concrète. Le middleware mesure l’arrivée de la réponse avec ses en-têtes, pas la consommation complète de son corps (notamment pour SSE). L’endpoint agrégé ne requiert pas de session; le réseau de déploiement doit limiter son accès aux collecteurs autorisés.

Les routes privées utilisent une session serveur dans un cookie opaque. Les opérations `POST`, `PUT`, `PATCH` et `DELETE` exigent `X-CSRF-Token`; lorsqu’un en-tête `Origin` est présent, sa valeur doit correspondre exactement à l’origine d’interface configurée. Les droits sont revérifiés côté serveur sur chaque ressource. La configuration production exige aussi `KYRO_AUTH_MAX_ACTIVE_SESSIONS` entre 1 et 100000 pour borner les sessions actives; le fournisseur OIDC synthétique est réservé au développement.

Les réponses et erreurs portent `Cache-Control: no-store` et un `X-Request-Id` généré côté serveur. La taille maximale du corps HTTP est 256 KiB. Les traces JSON contiennent méthode normalisée, template `MatchedPath` (ou `unmatched`), statut, durée et ID de requête généré; elles omettent les identifiants de ressource, query strings, headers client, cookies, corps et messages de fournisseur. Le processus API charge les modèles en mode admission sans clé fournisseur; seul le worker reçoit les credentials et revérifie la disponibilité au moment de l’exécution. Les requêtes ordinaires ont une concurrence bornée et un délai de 30 s. Les flux SSE ont 64 places distinctes, sans délai sur le corps, des lots d’au plus 100 événements et une revalidation de session/droit avant chaque émission ou cycle de polling.

## Routes

| Méthode et chemin | Contrat |
| --- | --- |
| `GET /health/live` | Sonde processus sans accès à la base. |
| `GET /health/ready` | Vérifie la base et le rôle courant non privilégié; réponse générique en cas d’indisponibilité. |
| `/v1/auth/...` | Flux d’identité et de session défini par `identity::routes()`. |
| `/v1/projects...` | Projets, révisions, changements et décisions définis par `projects::routes()`. Les mutations de révision exigent `If-Match: "rev-N"`. |
| `GET /v1/projects/{project_id}/jobs` | Liste paginée, limite 1–100; `before` est un curseur opaque de tri et ne confère aucun droit. |
| `POST /v1/projects/{project_id}/jobs` | Soumet `{ "payload": <JobPayload>, "max_attempts"?: u8, "ttl_seconds"?: u32 }`; exige `Idempotency-Key` et la révision source dans `If-Match: "rev-N"`. La clé est limitée à 200 caractères ASCII alphanumériques, `-`, `_`, `.` et `:`. Pour `ModelCall`, le serveur exige aussi l’action `model` et valide registre, schéma, politique, taille, délais et marqueurs de secret avant d’écrire le travail. Répéter la même clé et le même corps renvoie le travail admis; réutiliser la clé avec un corps différent renvoie `409`. |
| `GET /v1/projects/{project_id}/jobs/{job_id}` | Lit une vue typée du travail sans son contenu d’entrée, bail propriétaire ni texte fournisseur. Le résultat d’un appel modèle reste dans la ressource effet autorisée; le travail expose au plus son identifiant d’effet et son statut. Pour suivre sa propre commande de rapprochement après le `202`, son créateur peut lire ce seul job avec `budget` ou `manage`, sans droit `read`; `Store` vérifie cette exception atomiquement. Les autres jobs restent soumis à l’autorisation `read`. |
| `DELETE /v1/projects/{project_id}/jobs/{job_id}` | Enregistre `cancel_requested` et renvoie `202`; le propriétaire du travail doit avoir `execute`, un autre acteur `manage`. La demande est idempotente. Le worker terminalise le travail et libère les réservations lorsqu’il peut le faire sans risque; un envoi déjà commencé ou incertain reste à rapprocher et n’est jamais rejoué automatiquement. Un travail déjà terminal renvoie `409`. |
| `GET /v1/projects/{project_id}/budget` | Lit les plafonds, réservations et consommations du projet et renvoie l’ETag fort `"budget-N"` correspondant à la version de configuration. |
| `PUT /v1/projects/{project_id}/budget` | Exige `If-Match: "budget-N"`; body `{ "limit_units": i64, "currency": "SYN", "unit_scale": 1 }`. La version protège atomiquement les changements de plafond, devise et échelle contre les mises à jour perdues et les retours en arrière ABA. Le currency/scale ne changent que si rien n’est réservé ou dépensé. Un refus de plafond renvoie `429`; version périmée `412`. `SYN` est une unité synthétique, pas une devise ni un tarif réel. |
| `GET /v1/projects/{project_id}/effects` | Liste les intentions d’effets et leur comptabilité, limite 1–100 et curseur UUID `before`; le résultat structuré est consultable uniquement avec l’autorité `read`. Aucun prompt, secret ni message fournisseur n’est renvoyé. |
| `GET /v1/projects/{project_id}/effects/{effect_id}` | Lit le détail de l’effet dans le projet et son résultat structuré autorisé; ne retourne ni prompt ni secret. |
| `POST /v1/projects/{project_id}/effects/{effect_id}/reconcile` | Met en file une preuve synthétique faisant autorité; exige l’action `budget` ou `manage` et `Idempotency-Key`. Body `ReconcileEffectRequest` (`evidence_id`, décision `processed` avec réponse structurée ou `not_processed`). Répond `202` avec le résumé sûr du job, sans preuve ni réponse structurée. Le worker applique la décision financière de façon atomique; cette commande ne réémet jamais la requête fournisseur. La clé répétée avec le même corps renvoie le job existant; avec un corps différent, `409`. |
| `GET /v1/projects/{project_id}/events` | Flux privé `text/event-stream`, reprenable avec `Last-Event-ID`. |

Une précondition `If-Match` absente sur une mutation qui en exige une donne `428 Precondition Required`. Une révision obsolète donne `412 Precondition Failed`; les ETags de révision sont forts et sérialisés exactement sous la forme `"rev-N"`.

## Exemples synthétiques

Les exemples utilisent des UUID factices et des placeholders de session/CSRF; ils ne contiennent aucun credential réutilisable. Une soumission déclarative utilise la révision lue dans le snapshot et une clé d’idempotence propre à la commande :

```sh
curl --include --request POST \
  'http://127.0.0.1:8080/v1/projects/00000000-0000-4000-8000-000000000001/jobs' \
  --header 'Content-Type: application/json' \
  --header 'Origin: http://127.0.0.1:3000' \
  --header 'X-CSRF-Token: <csrf-token-from-session>' \
  --header 'If-Match: "rev-0"' \
  --header 'Idempotency-Key: example-command-001' \
  --cookie 'kyro_session=<opaque-session-cookie>' \
  --data '{"payload":{"kind":"apply_changes","changes":{"operations":[{"op":"set_preference","key":"theme","value":"dark"}]}},"max_attempts":2,"ttl_seconds":60}'
```

La réponse d’acceptation est `202 Accepted`; le client lit ensuite le job ou les événements du projet. Pour modifier le budget, récupérer d’abord son ETag puis présenter cette version inchangée dans `If-Match` :

```sh
curl --include 'http://127.0.0.1:8080/v1/projects/00000000-0000-4000-8000-000000000001/budget' \
  --cookie 'kyro_session=<opaque-session-cookie>'

curl --include --request PUT \
  'http://127.0.0.1:8080/v1/projects/00000000-0000-4000-8000-000000000001/budget' \
  --header 'Content-Type: application/json' \
  --header 'Origin: http://127.0.0.1:3000' \
  --header 'X-CSRF-Token: <csrf-token-from-session>' \
  --header 'If-Match: "budget-0"' \
  --cookie 'kyro_session=<opaque-session-cookie>' \
  --data '{"limit_units":10000,"currency":"SYN","unit_scale":1}'
```

Un client SSE browser peut reprendre un snapshot avec `?after=<project-uuid>:<event_sequence>`; après connexion, l’`id:` de chaque événement devient automatiquement `Last-Event-ID` à la reconnexion :

```sh
curl --no-buffer \
  'http://127.0.0.1:8080/v1/projects/00000000-0000-4000-8000-000000000001/events?after=00000000-0000-4000-8000-000000000001:0' \
  --cookie 'kyro_session=<opaque-session-cookie>'
```

## Flux d’événements

L’identifiant SSE est `<project-uuid>:<sequence>`. Le séquenceur est global au projet; l’UUID dans l’identifiant permet de refuser un curseur copié d’un autre projet. Les droits d’environnement peuvent masquer des lignes sans rendre leurs séquences contiguës : le client doit accepter les sauts de séquence. Pour l’abonnement initial, le client peut passer `?after=<project-uuid>:<event_sequence>` obtenu dans le snapshot du projet; à défaut, le flux repart de la séquence zéro. À la reconnexion, `Last-Event-ID` est repris. Si les deux curseurs sont présents, ils doivent être identiques.

Le serveur lit au plus 100 événements par cycle et conserve au plus un lot de 100 en mémoire par connexion. Il ne précharge pas une file en mémoire sans borne. Un client lent est soumis aux limites de connexions SSE.

À l’ouverture puis avant chaque événement ou polling, le serveur relit la session et le droit `read`. Une session expirée/révoquée ou un droit retiré termine le flux. Bornes physiques de rétention et lignes visibles sont lues dans une même transaction. Un identifiant mal formé renvoie `400`; un curseur d’un autre projet ou futur renvoie `409`; un historique dont le préfixe a réellement été purgé renvoie `410`. Des événements encore conservés mais masqués par le périmètre d’environnement, y compris un historique visible vide, ne déclenchent pas `410`. Ces refus sont des réponses JSON avant l’établissement SSE : le client recharge un snapshot du projet, puis ouvre un nouveau flux avec son `event_sequence`. Si une purge apparaît après l’établissement, le serveur envoie `reset-required` puis ferme le flux. Aucun corps de réponse de modèle ni secret fournisseur ne figure dans les événements.

## Erreurs

Les erreurs suivent une enveloppe JSON stable et générique; elles ne contiennent jamais le SQL, le message brut d’un fournisseur, une URL sensible, un cookie ou un corps de requête. `X-Request-Id` est généré par le serveur et corrèle la réponse et sa trace.

| Statut | Signification |
| --- | --- |
| `400` | Corps, en-tête ou curseur mal formé. |
| `401` | Session absente, expirée ou révoquée. |
| `403` | Acteur connu sans la capacité requise sur un projet qu’il peut voir. |
| `404` | Ressource absente ou invisible au regard de l’isolation du tenant. |
| `409` | Conflit d’idempotence ou curseur SSE d’un autre projet/futur. |
| `410` | Historique demandé non conservé; le client doit charger un snapshot. |
| `412` | Précondition `If-Match` obsolète. |
| `428` | Précondition obligatoire absente. |
| `413` | Corps au-delà de la limite configurée (au plus 256 KiB). |
| `429` | Budget, quota ou capacité bornée épuisé. |
| `503` | Dépendance nécessaire indisponible; aucune donnée interne n’est divulguée. |
| `504` | Échéance de traitement ordinaire dépassée. |
| `500` | Échec interne générique, sans détail serveur. |

Les codes JSON précis sont ceux du module API `error`; ils sont versionnés avec ce contrat et couverts par les tests HTTP.
