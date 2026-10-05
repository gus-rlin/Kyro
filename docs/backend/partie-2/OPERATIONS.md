# Exploiter et reproduire la partie 2

Le runtime applicatif est `kyro-app`, la fabrique est `kyro-factory`. Le [contrat](CONTRATS.md), le [manifeste](MANIFESTE.md) et le [rapport de livraison](VALIDATION.md) décrivent leur portée et leurs preuves. Installation locale, déploiement et admission à un fournisseur réel sont des étapes distinctes. P2 est un backend ; les interfaces Leptos appartiennent à la partie suivante.

## Compilation et bases distinctes

Rust 1.96.1, PostgreSQL 18 et un hôte Linux pour l'attestation sont utilisés par la recette. Les exécutables Windows ont été bloqués par le contrôle d'application de l'hôte ; les validations conservées utilisent Linux/Docker. Depuis la racine du dépôt :

```sh
cargo build --locked -p kyro-app -p kyro-factory -p kyro-api -p kyro-worker --bins
cargo fmt --all -- --check
cargo clippy --locked -p kyro-app -p kyro-factory --all-targets --features kyro-factory/test-support --no-deps -- -D warnings
```

Provisionner deux bases séparées avec des credentials issus du gestionnaire de secrets. Le migrateur de pilotage reçoit seul `KYRO_DATABASE_ADMIN_URL` ; `kyro-app-migrate` reçoit seul `KYRO_APP_DATABASE_ADMIN_URL`. Le rôle admin peut créer les rôles de migration ; il ne rejoint ni API, ni worker, ni image générée. Les rôles applicatifs sont `kyro_app_runtime` et `kyro_app_auth_runtime` (NOINHERIT) ; leurs groupes sans LOGIN sont `kyro_app` et `kyro_app_auth`. Leurs mots de passe sont provisionnés par l'opérateur, hors migration et dépôt. L'API vérifie ces propriétés au démarrage. Ne pas utiliser de connexion admin comme raccourci.

```sh
cargo run --locked -p kyro-store --bin kyro-migrate
cargo run --locked -p kyro-app --bin kyro-app-migrate
```

Les migrations sont additives et leurs checksums SQLx sont immuables. `scripts/p2/verify-migrations.mjs application` et `control` créent des bases de recette neuves depuis `KYRO_P2_MIGRATION_TEST_ADMIN_URL`, qui doit nommer une base de test autorisée. Ils vérifient installation neuve, upgrade, réexécution et rôles restreints. Les bases créées restent disponibles pour inspection ; le script ne supprime pas une base existante.

Pour la correction des reçus 0037 : arrêter les anciens runtimes, appliquer `kyro-app-migrate`, puis démarrer les binaires corrigés. La migration attend la clôture globale des transactions et invalide les projections existantes une fois, sans supprimer les intentions ni répéter leurs effets. Ne pas mélanger les versions de runtime pendant cette mise à niveau ; une ancienne version pourrait créer à nouveau des reçus sans clôture de visibilité.

La version de catalogue de livraison est 0.1.2, avec application 0037 et pilotage 0019 (après les migrations existantes 0017 et 0018). Les reçus antérieurs à 0036 sont conservés mais leur projection non liée refuse au rejeu ; la mutation ne recommence pas. Un changement d'autorité de l'application peut également invalider ses reçus, même après ajout de droits. Toute écriture de maintenance SQL sans contexte applicatif prend la fence globale et invalide les reçus du serveur. Après `idempotency_authority_changed`, lire et rapprocher l'état courant ; ne pas inventer automatiquement une nouvelle clé pour retenter l'effet. Les compteurs d'autorité sont sous RLS et ne sont pas modifiables par les rôles runtime. Ne pas remplacer la version 0.1.0 déjà admise avec des sources différentes.

## Identité et runtime applicatif

Provisionner le tenant, l'application, le premier principal et ses adhésions avec la procédure d'administration de l'installation. La route utilisateur ne peut pas fabriquer cet administrateur. Pour OIDC, l'opérateur lie l'issuer et le subject vérifiés dans les tables d'identité ; pour un compte local, il provisionne un hash Argon2id dans `app_local_credentials`, jamais un mot de passe en SQL ou dans les sources. L'auto-inscription OIDC est facultative et son `signup_role` doit rester de moindre privilège. Ne pas assigner `admin` à une auto-inscription publique. Les UUID de l'application doivent correspondre au verrou compilé.

Exemple de bootstrap **OIDC** à exécuter avec le rôle admin applicatif, dans une base neuve. Fournir à `psql -v` les UUID `tenant`, `application`, `principal`, l'`issuer` exact configuré et le `subject` du premier opérateur vérifié auprès du fournisseur. Ce script crée volontairement les lignes une seule fois ; un conflit annule toute la transaction. Les cinq limites sont des choix de l'opérateur ; `0` ferme la consommation correspondante. L'identité externe est liée par issuer/subject, jamais par une correspondance d'adresse courriel.

```sql
BEGIN;
INSERT INTO app_tenants(id) VALUES (:'tenant'::uuid);
INSERT INTO app_applications(tenant_id,id) VALUES (:'tenant'::uuid,:'application'::uuid);
INSERT INTO app_principals(tenant_id,id,display_name)
  VALUES (:'tenant'::uuid,:'principal'::uuid,'Initial operator');
INSERT INTO app_memberships(tenant_id,application_id,principal_id,role)
  VALUES (:'tenant'::uuid,:'application'::uuid,:'principal'::uuid,'admin');
INSERT INTO app_role_permissions(tenant_id,application_id,role,permission)
  VALUES (:'tenant'::uuid,:'application'::uuid,'admin','*');
INSERT INTO app_external_identities(tenant_id,application_id,issuer,subject,principal_id)
  VALUES (:'tenant'::uuid,:'application'::uuid,:'issuer',:'subject',:'principal'::uuid);
INSERT INTO app_quotas(tenant_id,application_id,quota_key,limit_value)
  VALUES (:'tenant'::uuid,:'application'::uuid,'records',:'records_limit'::bigint),
         (:'tenant'::uuid,:'application'::uuid,'storage_bytes',:'storage_limit'::bigint),
         (:'tenant'::uuid,:'application'::uuid,'jobs',:'jobs_limit'::bigint),
         (:'tenant'::uuid,:'application'::uuid,'job_slots',:'job_slots_limit'::bigint),
         (:'tenant'::uuid,:'application'::uuid,'effects',:'effects_limit'::bigint);
COMMIT;
```

Installer ensuite la configuration OIDC et démarrer l'API : le login effectue PKCE, state, nonce et validation RS256 avant de créer la session. Ce bootstrap ne forge aucun JWT. Les recettes d'identité testent cette liaison sur un fournisseur synthétique ; l'installation chez un fournisseur réel reste une étape d'exploitation à qualifier.

Le serveur lit `KYRO_APP_DATABASE_URL`, `KYRO_APP_SESSION_HMAC_KEY` (≥ 32 octets), `KYRO_APP_TOKEN_ISSUER`, `KYRO_APP_TOKEN_AUDIENCE`, `KYRO_APP_LISTEN_ADDRESS` (défaut 127.0.0.1:8081). Un binaire de développement exige `KYRO_APP_COMPONENTS`, tableau JSON explicite d'IDs ; un artefact compilé impose le graphe et ne permet que sa restriction. Exemple de tranche sans fournisseur : `["B021","B030","B031","B032","B033","B034","B035","B036"]`. Les sessions navigateur sont HttpOnly, avec CSRF et droits relus. Les credentials ponctuels ne sont pas conservés dans le cache de réponse.

Pour B001–B010, `KYRO_APP_IDENTITY_FILE` contient l'objet fermé `IdentityConfig` : `tenant_id`, `application_id`, `ui_origin`, `local_enabled`, `oidc_signup`, `signup_role`, `oidc`, `synthetic_loopback`. Fournir aussi `KYRO_APP_AUTH_DATABASE_URL` et `KYRO_APP_CREDENTIAL_KEY` (32 octets aléatoires encodés base64, séparés de la clé de session). OIDC décrit issuer, client, endpoints, redirect et éventuelle référence de secret au coffre. `synthetic_loopback` est exclusivement une configuration de recette. Le mailer d'identité utilise `KYRO_APP_AUTH_DELIVERY_FILE`, avec Resend et budget de fenêtre distinct ; sans reçu valide, une émission ambiguë reste `unknown`.

```sh
cargo run --locked -p kyro-app --bin kyro-app
cargo run --locked -p kyro-app --bin kyro-app-worker
```

Le worker exige `KYRO_APP_WORKER_TOKEN` pour un principal scoped `jobs.worker`. Il ne reçoit aucun credential admin. Il traite B052/B053 et l'outbox B054 indépendamment selon le graphe ; les baux/générations, droits et sources sont revalidés avant chaque effet. Les scopes des clés API de service doivent énumérer exactement les opérations nécessaires. Les tokens de service expirent et doivent être renouvelés par l'exploitation.

## Connecteurs, modèles et documents

`KYRO_APP_CONNECTORS_FILE` décrit les profils opérateur fermés (`Profile` et protocoles dans `crates/kyro-app/src/connectors.rs`). `KYRO_APP_VAULT_FILE` est un tableau de bindings `tenant_id`, `application_id`, `adapter_id`, `reference_id`, `purposes`, `secret_base64`. Permissions Unix 0600 ; secret de 32 à 4096 octets. Les références actives sont liées par B023. Les noms d'hôtes, adresses épinglées, méthodes, chemins, schemas, scopes et quotas appartiennent au profil, pas au payload. B058/B059 utilisent un contrat REST B157 et l'outbox B054 ; B058 signe le corps avec la finalité `webhook.send`. Un timeout après émission possible garde sa réserve ; rapprocher explicitement, sans relancer aveuglément.

Pour l'IA, `KYRO_APP_AI_FILE` décrit `AiConfig` : portée, rôles, politique de données, sélections de modèles et unités de budget. `KYRO_MODEL_REGISTRY_PATH` est le registre de la passerelle P1 ; `KYRO_APP_ENVIRONMENT` est `development` ou `production`. L'API lit ce registre sans clé d'exécution. Le worker seul reçoit `KYRO_MODEL_API_KEY`. Les propositions sont typées, privées et appliquées après accord humain/CAS ; un test synthétique ne qualifie pas NVIDIA/Nebius ou leur rétention. Aucun modèle payant n'est appelé par les recettes décrites ici.

Les originales documentaires restent privées jusqu'à scan et autorisation. La limite est 5 MiB avec morceaux ≤ 32 KiB. B084/B085 utilisent le processeur fixe isolé ; les receipts lient contenu, version, génération, outils et sortie. Le scanner livré est un détecteur borné de signatures, dont EICAR, et ne remplace pas un antivirus commercial. PDF généré : WinAnsi borné ; exports CSV/JSON ; pas de promesse de ZIP ou de rendu universel.

## Outils protégés et contrôleur

Construire le parent public avec `scripts/p2/sandbox/Dockerfile.base`, puis produire le vendor depuis **le même Cargo.lock** avec `cargo vendor --locked`. Le contexte de `scripts/p2/sandbox/Dockerfile` doit contenir uniquement les scripts fixes, `cargo-config.toml` et ce vendor public. Passer `--build-arg TOOLS_IMAGE=<parent épinglé>` ; le parent local utilisé dans la recette historique n'est pas un registre public. Le nouveau Dockerfile du parent permet une reconstruction fonctionnelle ; un rebuild peut avoir d'autres digests et doit être requalifié. Aucun fichier .env, credential Cargo, checkout ou clé privée dans le contexte.

Exporter le rootfs public de l'image dans un volume d'outils neuf, conserver modes/propriétaires et exécuter le [script root-index.sh](../../../scripts/p2/sandbox/root-index.sh) dans le conteneur de préparation où le volume est monté à `/tools-root`. Le SHA de l'inventaire devient `tools_root_digest`. Ne jamais monter le volume d'outils en écriture pendant les essais. Le profil exige Docker/cgroups v2, `runsc` 20260928.0 et systrap ; il refuse l'absence des ressources attendues. D37 documente la variante gVisor et le daemon Docker dans la base de confiance. Le workload n'accède ni au socket, ni aux clés, ni au réseau externe. Les probes atteignent CPU, mémoire, PID et disque ; le chemin de timeout est éprouvé à 3 s, les plafonds de production restent 900/600 s.

`SandboxConfig` contient `tools_image` (sha256 exact), `tools_root_volume`, `tools_root_digest`. `OperatorConfig` ajoute `source_root`, `archive_root`, `tools_root`, `attestor_socket`, `runtime_base_root`, `runtime_base_digest`, `capabilities` et quatre clés publiques distinctes. Tous les chemins doivent être absolus, sans symlinks. Le rootfs de runtime correspond aux cinq fichiers `BASE_FILES` de l'assembleur, liés par hash. Le contrôleur Linux seul peut piloter Docker ; ses credentials ne sont jamais copiés dans le Sentry. Les ressources jetables portent profil, expiration et labels de propriété ; le nettoyeur conserve les ressources étrangères, vivantes et volumes attachés.

## Catalogue, signatures et export

Créer quatre clés RSA distinctes (2048 bits minimum), signatures RS256, stockées 0600 hors dépôt : Catalogue, Composition, Evidence, Release. `KYRO_FACTORY_TRUST_FILE` contient les clés publiques `id`, `purpose`, `public_pem`. `KYRO_FACTORY_CATALOGUE_KEY_FILE`/`KEY_ID` signent le registre. `kyro-catalogue <source-root-absolu> <revision> <pending-absolu.json>` génère les 147 entrées **pending** ; génération n'implique pas qualification.

Après une campagne réussie, `prepare-qualification.mjs` prend quatre chemins relatifs au dépôt : pending, run-index, décisions, matrice. Le run-index donne environnement explicite, source digest et rapports conservés (`app`, `kernel`, `factory`, `factory_booking`, `factory_support`, `factory_stock`, `factory_all`, `isolation`, `media`, `store`), chacun avec chemin, SHA et code de sortie réellement observé, plus `test_inputs` liant les recettes, helpers et fixtures par SHA. Le producteur refuse une source changée, un test absent ou un rapport échoué. L'opérateur examine la matrice et ses limites ; `kyro-catalogue seal <pending> <decisions> <proofs>` avec `KYRO_FACTORY_EVIDENCE_KEY_FILE`/`KEY_ID` authentifie cette décision. Puis `admit-all <pending> <proofs> <admitted>` avec le rôle Catalogue admet le lot atomiquement. `publish <admitted>` reçoit la connexion admin P1 et applique la clôture de publication ; la révocation reste permanente. Les fichiers de sortie utilisent create_new, sans écrasement implicite.

Le pilotage P1 lit `KYRO_FACTORY_CONFIG_FILE`. L'API reçoit seulement la clé Composition pour le verrou (`KYRO_FACTORY_COMPOSITION_KEY_FILE`/`KEY_ID`), le worker uniquement la confiance publique. Le processus séparé `kyro-attestor` reçoit la connexion worker P1 et les clés Evidence/Release (`KYRO_FACTORY_EVIDENCE_KEY_FILE`/`KEY_ID`, `KYRO_FACTORY_RELEASE_KEY_FILE`/`KEY_ID`). Il vérifie réellement l'image dans un PostgreSQL neuf et un autre Sentry, puis la transaction Store clôture révision, lock, bail, droits et digests. Socket Unix opérateur privé avec lock d'inode ; arrêt du processus avec SIGTERM. Aucun attestor natif Windows revendiqué.

Les routes P1 de fabrique et d'artefact sont répertoriées dans l'OpenAPI P1. Les exports authentifiés énumèrent sources, migrations, lock, manifests, Git bundle et blobs OCI, puis servent des morceaux bornés à 64 KiB. Chaque morceau relit les droits et les hashes des blocs ; une révocation interdit aussi un accès mis en cache. Cloner le Git bundle avec `--branch kyro/application` ; il ne fournit pas un HEAD implicite. Les critères protégés et clés privées ne font pas partie de l'export.

Une sauvegarde PostgreSQL préserve les références de fabrique mais **pas** l'archive des sources/OCI, les volumes d'outils ou les clés de signature. Sauvegarder ces objets séparément avec leurs hashes et la procédure de restitution. La restauration désactive émissions externes, sessions et baux avant reprise ; le rapport fournit les contrôles exécutés. Ne pas relancer un effet ambigu après restauration.

## Recettes

Les tests applicatifs ignorés requièrent trois connexions de recette distinctes : `KYRO_P2_TEST_ADMIN_URL`, `KYRO_P2_TEST_RUNTIME_URL`, `KYRO_P2_TEST_AUTH_URL`. B158 ajoute le fournisseur PostgreSQL synthétique TLS/SCRAM lancé par `scripts/p2/fixtures/start-external-postgres.sh` (CA via `KYRO_P2_TEST_EXTERNAL_CA_FILE`, connexion admin synthétique via `KYRO_P2_TEST_EXTERNAL_ADMIN_URL`). Utiliser exclusivement des bases jetables, pas une base contenant des données utilisateur.

```sh
cargo test --locked -p kyro-app --features test-support --tests -- --include-ignored --nocapture --test-threads=1
cargo test --locked -p kyro-factory --features test-support --lib --tests -- --nocapture
```

Les recettes Docker ignorées sont lancées séparément avec le contrôleur préparé : `isolation_real`, `cleanup_real`, `media_real` et `kyro-worker --test factory_real`. La fabrique exige les connexions P1 API/worker/admin et `KYRO_P2_SANDBOX_CONFIG`; `KYRO_P2_FACTORY_COMPOSITION` prend `booking`, `support`, `stock`, puis `all`. `KYRO_P2_FACTORY_EVIDENCE_OUTPUT` est un nouveau répertoire `/tmp/kyro-p2-factory-proof-*`. Les médias exigent `KYRO_P2_MEDIA_CONFIG` contenant sandbox, volume du seul processeur fixe et son SHA. Ne pas modifier Rust/Cargo/migrations/scripts protégés pendant la recette : leurs octets constituent le sujet de la preuve.

Les fixtures de signatures des tests de fabrique servent à vérifier la chaîne, sans qualifier à elles seules les 147 blocs. La campagne et la matrice fournissent cette qualification dans leur environnement documenté. La revue indépendante clôture le travail ; aucun commit, publication, soumission ou déploiement n'est déclenché par ces procédures.
