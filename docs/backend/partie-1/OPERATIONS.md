# Exploitation PostgreSQL de la partie 1

## Environnement local

Le service local utilise l’image officielle PostgreSQL `18.6-alpine`, épinglée par l’index OCI `sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873`. L’inspection du registre le 2026-10-03 a confirmé un manifeste `linux/amd64` `sha256:d8703cd7fba306b9fec9268ecedfa8a966846c053036a60e3635791957eb2f66`. Pour PostgreSQL 18, l’image officielle place `PGDATA` sous `/var/lib/postgresql/18/docker` et définit `/var/lib/postgresql` comme volume ; les deux Compose montent donc leur volume à `/var/lib/postgresql` ([documentation de l’image officielle](https://hub.docker.com/_/postgres)).

Depuis la racine du dépôt, démarrer et vérifier la base réelle :

```powershell
.\scripts\infra-p1.ps1 -Action Start
.\scripts\infra-p1.ps1 -Action Status
```

Le nom de projet Compose `kyro-p1-ops`, le conteneur `kyro-p1-ops-postgres-1` et le volume `kyro-p1-ops_postgres-data` sont réservés à cette instance. Le port publié est uniquement `127.0.0.1`. La valeur par défaut est `55432` (`KYRO_P1_BIND_PORT` dans [`.env.p1.local.example`](../../../.env.p1.local.example)). Si elle est occupée et qu’aucun port n’a été choisi explicitement, le script cherche automatiquement un port libre entre `55440` et `55539`, puis affiche le port retenu et les trois URL de connexion. Si `KYRO_P1_BIND_PORT` est fixé par l’opérateur et indisponible, le script échoue sans modifier le service qui l’utilise.

Le Compose local active `trust` pour faciliter les essais avec des données synthétiques ; l’écoute limitée à la boucle locale et cette authentification ne conviennent pas à la production. Les rôles `kyro_api` et `kyro_worker` sont créés par les migrations. Les URL runtime affichées par le script servent après cette création. Ne pas insérer de données réelles dans cette instance de qualification.

Pour conserver un port différent dans un shell PowerShell :

```powershell
$env:KYRO_P1_BIND_PORT = '55440'
.\scripts\infra-p1.ps1 -Action Start
```

Si le conteneur existe déjà, son port est repris automatiquement. Une modification explicite du port de ce conteneur existant est refusée pour éviter de recréer silencieusement son service ; sa base et son volume sont conservés.

## Production

Le fichier [`.env.p1.production.example`](../../../.env.p1.production.example) ne contient que des noms de variables : les URL, certificats, chemins de secrets et référence d’image doivent être fournis par le déploiement. Compose refuse de résoudre la configuration tant qu’une valeur obligatoire manque. L’image applicative `KYRO_P1_RUNTIME_IMAGE` doit être une référence construite et publiée, épinglée par digest.

Le Compose de production lance PostgreSQL, une API et un worker distincts. PostgreSQL n’a aucun port publié sur l’hôte. L’API expose le port 8080 uniquement au réseau Compose, où un reverse proxy TLS peut lui accéder ; le worker n’expose aucun port. Les deux processus sont configurés en production et reçoivent seulement leur URL DB par fichier secret : l’API utilise le rôle `kyro_api`, le worker `kyro_worker`, et seul le migrateur reçoit `KYRO_DATABASE_ADMIN_URL`. Les variables OIDC obligatoires sont limitées au service API. `KYRO_MODEL_API_KEY` est lue depuis un fichier secret uniquement dans le worker ; si la clé n’est pas requise par les modèles activés, le fichier peut être vide. Le secret OIDC client peut aussi être un fichier vide seulement si le fournisseur n’en exige pas.

PostgreSQL utilise SCRAM, active TLS avec `server.crt` et `server.key`, et refuse les connexions TCP sans TLS via `pg_hba.production.conf`. Le certificat présenté par PostgreSQL doit être un certificat serveur leaf (`CA:FALSE`), autorisé pour `serverAuth`, avec un SAN correspondant au nom DNS utilisé dans les URL DB. `KYRO_POSTGRES_CA_FILE` contient la CA qui signe ce leaf et est monté en lecture seule dans les clients ; ne présentez pas un certificat CA `CA:TRUE` comme certificat serveur. Un contrôle local avec le client runtime SQLx/Rustls a refusé ce dernier, alors que libpq acceptait le même certificat auto-signé comme ancre de confiance. Le chemin `KYRO_POSTGRES_TLS_DIRECTORY` doit être monté depuis un emplacement contrôlé par l’exploitant ; le propriétaire PostgreSQL dans l’image doit pouvoir lire la clé, et PostgreSQL exige des permissions de clé privée restrictives (par exemple `0600`). Les trois fichiers URL (admin, API, worker) doivent employer le nom DNS couvert par le certificat et inclure `sslmode=verify-full&sslrootcert=/run/secrets/postgres_ca`. L’URL et les mots de passe sont lus par le shell sans être affichés ; seuls les services explicitement listés ont accès à chaque secret.

Créer d’abord le service PostgreSQL, puis appliquer les migrations avec le profil ponctuel du migrateur ; ne lancer API et worker qu’après le provisionnement des mots de passe des rôles runtime :

```powershell
$compose = @('--project-name', 'kyro-p1-production', '--env-file', '<fichier-env-deploiement>', '--file', 'compose.p1.production.yaml')
docker compose @compose up --detach postgres
docker compose @compose --profile migration run --rm migrate
docker compose @compose exec postgres psql --username kyro_admin --dbname postgres
```

Dans la session `psql`, utiliser `\password kyro_api` puis `\password kyro_worker` pour saisir des mots de passe sans les placer dans l’historique de commandes. Mettre à jour les deux fichiers URL runtime et les fichiers secrets de façon cohérente, puis démarrer `api` et `worker`. Aucun worker ne doit démarrer avant une migration réussie et l’installation des deux secrets runtime. Le Compose n’active jamais `trust`.

Les services API et worker portent le profil Compose `runtime`; une commande `up` générique ne lance donc que PostgreSQL. Après le provisionnement des rôles et secrets : `docker compose @compose --profile runtime up --detach api worker`. L’API répond en HTTP sur `/health/ready` à l’intérieur du réseau Compose (200 si prête, 503 générique sinon) ; un reverse proxy doit réaliser et journaliser son propre contrôle de readiness.

`POSTGRES_PASSWORD_FILE` n’initialise le mot de passe admin que lors de la création du cluster. Changer le fichier secret ne tourne pas à lui seul le mot de passe d’un cluster existant ; la rotation doit modifier le rôle dans PostgreSQL, mettre à jour les fichiers URL qui en dépendent, puis redémarrer les services concernés. Pour un service géré ou une terminaison TLS réseau externe, conserver `sslmode=verify-full`, fournir sa CA au processus et appliquer les règles réseau du fournisseur ; aucun port DB n’est publié par Compose.

## Migrations, sauvegarde et restauration

Le migrateur se lance depuis la racine du dépôt avec `scripts/infra-p1.ps1 -Action Migrate`. Le script contrôle PostgreSQL local, fournit uniquement `KYRO_DATABASE_ADMIN_URL` pour le rôle `kyro_admin`, puis appelle `cargo run --locked -p kyro-store --bin kyro-migrate`. Le migrateur applique les migrations embarquées dans l’ordre et refuse les rôles runtime. Après migration, le script vérifie que `kyro_api` et `kyro_worker` existent et ne sont ni superutilisateurs, ni propriétaires de la base, ni dotés de `BYPASSRLS`.

Pour une qualification sur une base vide isolée de la base locale par défaut, créer explicitement une base `kyro_p1_ops_*`, puis appeler `scripts/infra-p1.ps1 -Action Migrate -MigrationRunner Docker -Database kyro_p1_ops_<suffixe>`. Le wrapper refuse de créer ou supprimer cette base ; `-Database` est limité à l’action `Migrate` et au préfixe `kyro_p1_ops_`. La base `kyro_p1` reste la cible par défaut.

Si Cargo natif est indisponible ou bloqué par la politique Windows, utiliser `scripts/infra-p1.ps1 -Action Migrate -MigrationRunner Docker`. Cette variante monte le dépôt en lecture seule dans un conteneur Linux fondé sur l’image officielle `rust:1.96.1-slim-bookworm`, épinglée par l’index OCI `sha256:e18a79fc84dfcfc3ab5ba72290398a644c135c97eaa881447fddc354ee4701a3` ([image officielle Rust](https://hub.docker.com/_/rust)). Le cache de compilation reste éphémère dans le conteneur ; l’URL admin synthétique passe par `host.docker.internal` et n’a pas de mot de passe. Le migrateur cible uniquement la base choisie sur l’instance locale dédiée : `kyro_p1` par défaut, ou une base existante `kyro_p1_ops_*` fournie explicitement pour les essais ; il ne cible pas la production.

Le script `backup-p1.ps1` crée une archive personnalisée `pg_dump` dans un emplacement hors dépôt. Par défaut il sauvegarde la base initialisée `kyro_p1`; `-Database` permet de choisir une base isolée de fixtures sur ce même conteneur. En production, fournir `-TargetEnvironment Production -ProductionEnvFile <fichier-env-deploiement>`.

Pour restaurer, choisir un nom de base nouveau commençant par `kyro_restore_`, puis fournir l’empreinte de l’archive et, si la source est disponible et quiescente, son nom pour comparer les empreintes internes expurgées :

```powershell
.\scripts\backup-p1.ps1 -Database kyro_p1 -OutputFile C:\temp\kyro-p1.dump
$sha = (Get-FileHash C:\temp\kyro-p1.dump -Algorithm SHA256).Hash
.\scripts\restore-p1.ps1 -BackupFile C:\temp\kyro-p1.dump -ExpectedSha256 $sha -TargetDatabase kyro_restore_20261003 -SourceDatabase kyro_p1
```

La restauration refuse toute cible existante, n’utilise jamais `--clean` et ne démarre aucun worker. Avant récupération, elle exige les rôles runtime globaux attendus et vérifie les tables/colonnes de la cible réellement restaurée. Elle désactive d’abord `runtime_control.external_sends_enabled`, puis transforme les effets `sending` en `unknown`, marque les jobs liés `unknown`, reprend les jobs sûrs avec une génération avancée et conserve les budgets/réservations/ledger. Les résultats d’effets `succeeded` restent connus pour permettre au worker de revalider puis réutiliser le résultat sans appel HTTP. Les réservations inconnues ou préparées ne sont jamais libérées. Les sessions restaurées sont révoquées et les flux de connexion éphémères supprimés. Les lignes `events` et `outbox_events` sont conservées ; les clients doivent charger un snapshot complet et réinitialiser le curseur d’événements avant reprise. Si une archive échoue après la création de sa nouvelle cible, celle-ci est conservée pour inspection et ne peut pas être réutilisée par le script ; choisir ensuite un autre nom `kyro_restore_*` après diagnostic.

Le latch des envois demeure désactivé après restauration. La restauration n’offre aucune réactivation publique et ne tente aucun rapprochement. L’opérateur doit d’abord réconcilier explicitement chaque effet inconnu avec le fournisseur et remettre son résultat en état connu via la procédure d’exploitation autorisée. Seulement après vérification qu’il ne reste aucun effet `sending` ou `unknown`, un administrateur peut exécuter le contrôle transactionnel suivant dans la session `psql` ouverte ci-dessus. Il échoue fermé si un effet est encore incertain et ne démarre aucun service :

```sql
BEGIN;
DO $resume$
DECLARE changed_rows INTEGER;
BEGIN
    IF EXISTS (SELECT 1 FROM public.effects WHERE status IN ('sending', 'unknown')) THEN
        RAISE EXCEPTION 'cannot resume while external effects are unresolved';
    END IF;

    UPDATE public.runtime_control
       SET external_sends_enabled = TRUE, updated_at = clock_timestamp()
     WHERE id = 1;
    GET DIAGNOSTICS changed_rows = ROW_COUNT;
    IF changed_rows <> 1 THEN
        RAISE EXCEPTION 'runtime control singleton missing or duplicated';
    END IF;
END
$resume$;
COMMIT;
```

Une restauration avec `-SourceDatabase` compare les empreintes des principales données conservées ; elle exige que les écrivains source soient arrêtés pendant la comparaison. Les commandes et résultats, y compris les échecs et limites, figurent dans [le rapport d’essai](../../suivi/essais/part1-ops.md). La cible est toujours une nouvelle base isolée, jamais une base existante.
