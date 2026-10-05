# Contrats serveur de la partie 2

État du document : 2026-10-05, implémentation et qualification en cours. Ce document décrit les frontières livrées ; le [rapport de livraison](VALIDATION.md) conserve les résultats et limites. La présence d'un contrat ou d'un bloc routé ne vaut pas admission au catalogue.

## Portée et séparation

P2 contient les 147 IDs du [manifeste](MANIFESTE.md) : 139 blocs applicatifs/adaptateurs et huit contrats de fabrique. B061–B080, B159, les interfaces Leptos et les agents de P3 restent dans leur partie respective. Le cœur, les blocs et l'assembleur sont en Rust. PostgreSQL porte les données applicatives ; les adaptateurs et modèles passent par des API contrôlées. Les données du pilotage P1 et celles des applications n'utilisent pas les mêmes identités ni bases.

Une capacité est successivement prévue, implémentée, vérifiée isolément, vérifiée en intégration puis éventuellement déployée et vérifiée. Les essais P2 actuels sont locaux et synthétiques ; aucun tarif, coût facturé, modèle réel ou déploiement n'en est déduit.

## Commandes applicatives

`POST /v1/apps/{application_id}/operations` accepte un objet fermé `component_id`, `action`, `payload`, `expected_version`, `idempotency_key`. `POST /v1/apps/{application_id}/nodes/{node_id}/operations` obtient le composant depuis le graphe compilé. Une session est vérifiée avant chaque opération et ses droits sont relus sous la clôture d'autorité PostgreSQL ; l'objet Actor en mémoire ne maintient pas un droit retiré.

Les actions disponibles sont fermées dans le dispatcher. La configuration compilée peut restreindre les actions et imposer des valeurs par défaut. Les champs fixes connus, UUID non nil, noms d'entité/record et devises de trois lettres majuscules sont validés avant fabrication avec les validateurs du runtime. Une référence de port invalide ou contradictoire avec un default refuse. L'existence de la ressource et ses droits restent vérifiés en transaction, lors de l'opération. Un appel direct au composant traverse les mêmes contraintes ; plusieurs instances d'un composant imposent l'identification du nœud. Les bindings doivent viser un port de type compatible et une dépendance présente dans le verrou.

Les écritures exigent une clé d'idempotence. Sa portée comprend tenant, application, acteur, composant et action ; une même clé avec un corps différent est refusée. Un rejeu restitue le reçu uniquement sous le même contexte d'autorité : générations globale/applicative et digest des rôles, permissions, scopes et état MFA initial de la transaction. Sinon HTTP 409 `idempotency_authority_changed`, sans champs privés ni réexécution ; la clé reste occupée. La migration 0036 conserve les anciens reçus non liés et en refuse la restitution. Une purge ne peut restituer que son tombstone exact `{"purged":true,"repeat_execution":false}`, qui ne contient aucune projection privée. B083 revalide aussi l'accès au document. L'admission de débit reste comptée par requête, y compris pour un rejeu refusé. Les changements versionnés comparent `expected_version` dans la transaction. RLS forcé et requêtes paramétrées portent l'isolation ; les permissions sont filtrées avant pagination ou agrégation.

Le transport utilise des erreurs à codes fermés et des réponses privées sans cache. Les secrets ponctuels ne rejoignent pas le journal d'idempotence. Pour une session navigateur, rotation et déconnexion écrivent les cookies HttpOnly et vérifient le CSRF ; le secret de session ne se retrouve pas dans la réponse JSON.

## Effets externes et travaux

La migration 0037 invalide les projections préexistantes sous la clôture globale : un ancien changement de visibilité pouvait ne pas avoir avancé sa génération. Les clés d'idempotence et effets métier sont conservés. Arrêter les anciens binaires avant migration, puis démarrer les binaires corrigés.

Les changements de visibilité prennent la même clôture exclusive que les révocations : migration B036, suppression B031, lot B033 contenant une suppression (y compris cascade), archivage CRM B131 et publication/archivage produit B121. Ils avancent la génération applicative seulement lors d'une exécution initiale réussie. Un lot composé uniquement de créations ou de mises à jour conserve les reçus autorisés. Le reçu historique B022 de consommation n'accorde aucun nouveau droit : une nouvelle consommation revalide toujours la révocation. Voir les scénarios de [confidentialité des reçus](VALIDATION.md).

Une transaction prépare un effet, ses références de source, le connecteur et sa réserve. Une autre étape ne peut émettre qu'après commit de la prise en charge, avec bail/génération, droits, source et configuration encore valides. Aucun paramètre fourni par un utilisateur ne choisit une commande shell, un hôte libre ou une clé du coffre.

Les destinations HTTP sont des noms exacts autorisés, résolus puis épinglés à des adresses publiques. TLS reste vérifié ; redirections et proxy sont désactivés. Les limites de corps, durée et réponse sont vérifiées. Les credentials sont chiffrés ou restent dans un coffre opérateur lié au tenant, à l'application, à l'adaptateur et à une finalité.

Une panne après ouverture d'une requête est conservativement `unknown`. Elle garde la réserve nécessaire au rapprochement et interdit un nouvel appel aveugle. Un événement ou un résultat fabriqué par une commande générique ne remplace pas un reçu fournisseur. Jobs, imports, évaluations et exports reprennent par checkpoints ; une nouvelle génération ne peut acquitter un ancien bail.

Le fournisseur public Nominatim est refusé pour l'usage de plateforme générique selon sa politique officielle consultée le 2026-10-05. Une instance propre ou tierce explicitement configurée reste utilisable ; l'attribution OpenStreetMap et la licence ODbL sont fournies au consommateur.

## Catalogue et résolution

Chaque manifeste versionné décrit ID, hashes des sources et migrations, schema fermé de configuration, dépendances/version exactes, capacités, ports typés, effets et critères. Les états sont `pending`, `admitted`, `deprecated`, `revoked`. La révocation d'une version est permanente ; une autre version demande sa propre qualification. La révision du catalogue progresse strictement.

L'admission exige une preuve signée Evidence distincte du rôle Catalogue. Elle lie le manifeste, les sources, les critères et quatre observations : nominal, refus, panne et invariant. Le registre généré de 147 contrats reste pending tant que ces observations ne sont pas qualifiées. Les catalogues de fixtures des tests servent à éprouver la chaîne cryptographique et ne constituent pas une qualification des blocs.

Le resolver prend un AppSpec P1 immuable et une permission fournie par le serveur : projet, application, révision, environnement et capacités. Il refuse propriétés inconnues, versions absentes/révoquées, dépendances manquantes, bindings incompatibles, cycles et capacités non autorisées. Le tri topologique est déterministe, sans version latest implicite. Le verrou signé contient le graphe entier, les digests et la toolchain ; une admission le compare à une nouvelle résolution des sources protégées.

## Fabrication et provenance

Le bundle contient uniquement les sources Rust/Cargo/migrations et scripts fixes répertoriés. Les valeurs AppSpec restent du JSON, jamais du Rust, SQL ou shell interpolé. Le SourceManifest lie les octets, leur propriétaire/template, le verrou signé complet, la toolchain et les migrations. Secrets, checkout de l'opérateur et fichiers non répertoriés ne sont pas des inputs de construction.

La construction utilise Cargo frozen hors ligne dans un Sentry gVisor distinct. L'enveloppe Docker supervise CPU, mémoire, PID, stockage et échéance ; aucun socket Docker ou clé d'attestation ne pénètre le workload. Variante locale D37 : runsc 20260928.0 systrap, UID extérieur 1000 avec un seul UID invité mappé, capacités vides. Le contrôleur du daemon est privilégié. Ce profil ne prouve pas une VM KVM ou un daemon entièrement rootless.

L'OCI est une structure fermée : index, manifest, configuration et couche exacte de trois binaires, bibliothèques/CA et configuration publique attendues. Digests, types, modes, noms et volumes sont vérifiés avant emploi. Une image ou un chemin arbitraire fourni par le job n'est pas lancé. L'export Git utilise les octets de ce bundle, arguments fixes, environnement nettoyé, date fixe et reprise d'une référence contrôlée.

## Vérification et publication atomique

Le vérificateur obtient les critères et migrations de sources protégées. Il démarre une base neuve et un candidat dans un autre Sentry. Les identifiants administrateur de recette et le programme des assertions sont absents du candidat. Le lock détermine les nœuds et les familles à vérifier ; un PASS déclaré par le constructeur n'est pas une observation.

Les observations communes contrôlent installation, rôle runtime, santé HTTP, tenant/application, révocation et réseau. Les blocs de données ajoutent nominal, validation, rejeu et CAS ; les compositions réservation/support/stock ajoutent concurrence, accès privés et comportements propres à ces familles. Une composition comportant d'autres fonctions demande aussi leur preuve de bloc : les critères communs seuls ne prouvent pas ces fonctions.

Evidence et Release utilisent des clés distinctes de Catalogue et Composition. Les preuves lient le run, l'image exacte, le profil, les sources, migrations, critères et observations. Le worker transmet à l'attestor seulement les identifiants/bail/génération/digest d'image ; il ne possède pas les clés Evidence/Release. L'attestor redérive les inputs depuis le Store et recontrôle l'autorité pendant et après la recette.

L'enregistrement de l'artefact, le résultat succeeded du job et l'événement final se font dans une transaction qui relit bail, génération, deadline, annulation, révision, catalogue et droits. Une signature seule ne force pas cette transaction. Après perte de réponse, le worker recherche le résultat durable vérifiable avant toute reprise.

## API de fabrique P1

| Route | Préconditions | Réponse |
| --- | --- | --- |
| POST `/v1/projects/{id}/factory/composition` | Read et Execute frais, If-Match de révision, service configuré | Verrou signé complet |
| POST `/v1/projects/{id}/factory/builds` | mêmes droits, If-Match, Idempotency-Key, TTL/attempts bornés | Job BuildApplication 202 |
| GET `/v1/projects/{id}/factory/artifacts/{artifact_id}` | Read frais et environnement autorisé | Métadonnées et preuves signées, aucun chemin hôte |
| GET `/v1/projects/{id}/factory/artifacts/{artifact_id}/exports/{format}` | mêmes droits, Release et OCI vérifiés | Index privé `sources`, `git` ou `oci`, tailles et hashes |
| GET `/v1/projects/{id}/factory/artifacts/{artifact_id}/exports/{format}/chunk` | droits et session revalidés, chemin de l'index, offset/limit bornés | Base64 de ≤ 64 KiB, SHA du fichier et prochain offset |

La migration P1 0019 rend les artefacts immuables et leur insertion dépendante du job et de son bail. Le catalogue est modifié uniquement par l'opérateur. L'export vérifie les signatures à chaque requête ; un cache borné conserve les descripteurs et hashes de blocs, jamais les droits. Chaque morceau relit ses blocs et refuse modification, symlink ou traversée. Les sources comprennent manifests, verrou et migrations ; le Git bundle fournit la branche `kyro/application` ; OCI fournit ses descriptors/blobs exacts. La route revalide la session et le droit après la lecture disque. Le [guide d'exploitation](OPERATIONS.md) et le [rapport de livraison](VALIDATION.md) donnent commandes et limites.
