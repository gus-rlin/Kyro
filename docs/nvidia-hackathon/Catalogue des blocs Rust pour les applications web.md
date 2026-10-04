**Catalogue cible de 180 blocs préconçus, répartis en 18 familles, pour composer une grande variété d’applications web. Chaque bloc que nous construirons doit être écrit en Rust et sécurisé par défaut.** Les 160 premiers couvrent les fonctions applicatives et leurs adaptateurs ; les 20 derniers constituent la fabrique et son exploitation. Tous sont ici **à construire ou à qualifier**, sans présumer de leur disponibilité.

Ce catalogue complète la [page produit](https://chatgpt.com/space/page_c41a658d84b08191b0295f1f1cb36e2d), l’[architecture technique](https://chatgpt.com/space/page_1869350b9e648191966bb8b7cff5b03b) et le [journal de décisions](https://chatgpt.com/space/page_baca1e1d35e481919683b0dc23455045). Il définit notre périmètre de construction initial ; il pourra évoluer par admission de nouvelles capacités. Il ne prétend pas couvrir tout logiciel imaginable.

## Le contrat commun à tous les blocs

Un bloc est une capacité réutilisable avec un contrat stable, implémentée dans une ou plusieurs crates Rust, accompagnées de ressources déclaratives, de tests et de documentation. Les identifiants B001 à B180 servent à suivre les dépendances et la couverture. Une recette métier référence ces identifiants et des versions compatibles ; elle ne constitue pas une implémentation parallèle.

**Rust partout dans les blocs développés.** Les traitements serveur, règles, connecteurs et modules de la fabrique sont natifs Rust. Les blocs d’interface sont écrits en Rust avec Leptos, puis produisent HTML et WebAssembly selon les besoins. CSS, SQL, schémas et liaisons navigateur de l’outillage complètent ces blocs. L’enveloppe de l’IDE peut rester en React et TypeScript. Les moteurs externes, fournisseurs d’identité, prestataires de paiement et sandboxes sont intégrés par des adaptateurs Rust ; leurs logiciels ne sont pas réécrits. Référence pour la cible d’interface : [Leptos](https://book.leptos.dev/).

**Sécurisé par défaut** signifie : accès refusé sans règle, contexte de locataire vérifié, ressources finies, entrées validées, sorties filtrées, secrets hors client et hors modèle, effets explicites, reprises maîtrisées et audit proportionné. Les interfaces, exports, index de recherche et caches respectent les mêmes droits. Un bouton caché ou une vérification WebAssembly ne remplace jamais le contrôle serveur.

Chaque manifeste précise identité, propriétaire, version, empreinte, licence, contrats, invariants, erreurs, dépendances, cibles, permissions, données, destinations réseau, migrations, stratégie de reprise et budget de performance. Chaque opération déclare ses ressources lues ou modifiées, ses effets, ses préconditions et sa sémantique d’annulation. Ces informations permettent au serveur de détecter les conflits de composition. Chaque bloc validé fournit exemples, tests de contrat et d’accès refusé, scénarios de concurrence ou panne applicables et mesures reproductibles.

**La performance est mesurée.** Rust soutient notre objectif de faible latence et de consommation maîtrisée ; la mention « ultra performant » reste une ambition jusqu’aux résultats. CPU, mémoire, débit, latences, coûts externes et taille navigateur sont suivis sur une charge définie. Une crate sûre en mémoire peut encore contenir une erreur de permissions ou de logique métier.

Les invariants des tableaux sont les contrôles particuliers de chaque bloc. Ils s’ajoutent au contrat commun, sans constituer à eux seuls une garantie de sécurité. Aucun bloc expérimental n’est accessible aux agents applicatifs ; une capacité manquante suit le circuit d’enrichissement du catalogue.

## 1 Identité et authentification

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B001 Fédération OpenID Connect | Connexion à un fournisseur d’identité | Émetteur, audience, state, nonce et PKCE contrôlés |
| B002 Comptes locaux optionnels | Identifiants locaux pour les produits qui en ont besoin | Bibliothèques éprouvées, hachage adapté et aucune activation implicite |
| B003 Connexion sans mot de passe | Passkeys ou liens de connexion selon adaptateur validé | Origine vérifiée ou jeton bref à usage unique |
| B004 Authentification renforcée | MFA et nouvelle vérification avant une action sensible | Preuve récente exigée côté serveur |
| B005 Sessions | Création, renouvellement, expiration et déconnexion | Cookie sûr, rotation et révocation vérifiées |
| B006 Récupération de compte | Rétablir un accès perdu | Preuves bornées, anti-énumération et audit |
| B007 Comptes de service et clés API | Authentifier une intégration technique | Portées minimales, secret affiché une fois et rotation |
| B008 Rôles et permissions | Définir des droits par rôle | Refus par défaut et absence d’auto-élévation |
| B009 Autorisations contextuelles | Droits selon objet, relation et situation | Décision serveur sur attributs fiables |
| B010 Révocation des accès | Retirer sessions, clés et droits | Effet sur caches et flux actifs vérifié |

## 2 Organisations et collaboration

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B011 Organisations et locataires | Créer des espaces de données isolés | Contexte de locataire établi depuis l’identité |
| B012 Établissements et espaces | Séparer agences, lieux ou équipes d’un même client | Accès limité aux établissements accordés |
| B013 Adhésions et invitations | Inviter et retirer des membres | Invitation bornée, cible et rôle vérifiés |
| B014 Groupes et équipes | Gérer des ensembles de collaborateurs | Propagation des droits explicitement définie |
| B015 Propriété et délégation | Attribuer ou transférer une ressource | Vérification des deux périmètres et audit |
| B016 Profils et préférences | Conserver les choix d’un utilisateur | Champs modifiables explicitement autorisés |
| B017 Commentaires | Discuter sur un objet métier | Droits de l’objet appliqués et texte échappé |
| B018 Mentions et abonnements | Suivre un objet et désigner un membre | Aucune fuite d’existence vers un tiers |
| B019 Fil d’activité | Présenter les changements utiles | Projection filtrée selon le lecteur |
| B020 Partage temporaire | Accorder un accès limité à un document ou aperçu | Expiration, révocation et portée explicites |

## 3 Sécurité et gouvernance des données

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B021 Moteur de décision d’accès | Appliquer la politique commune aux opérations | Toute absence de règle autorisante entraîne un refus |
| B022 Capacités d’exécution | Limiter outils et actions par tâche | Projet, ressource, environnement et durée liés au droit |
| B023 Références de secrets | Utiliser un coffre sans diffuser ses valeurs | Injection seulement dans l’adaptateur autorisé |
| B024 Limitation de débit | Limiter appels et abus par périmètre | Bornes par acteur et locataire sans croissance mémoire libre |
| B025 Validation des entrées | Valider types, formats et tailles | Profondeur, cardinalité et valeurs inconnues contrôlées |
| B026 Projection et masquage | Exposer seulement les champs utiles | Filtrage serveur avant logs, exports et rendu |
| B027 Journal d’audit de sécurité | Tracer les actions et décisions sensibles | Écriture restreinte, intégrité vérifiable et rétention |
| B028 Rétention et purge | Supprimer ou expirer les données selon politique | Périmètre vérifié et traitement des copies documenté |
| B029 Export des données personnelles | Produire un export autorisé et traçable | Vérification d’identité et téléchargement temporaire |
| B030 Quotas de ressources | Borner stockage, calcul, tâches et volumes | Admission atomique et refus explicite à la limite |

## 4 Données et persistance

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B031 Entités typées | Définir et manipuler les objets métier | Validation serveur et champs accessibles limités |
| B032 Relations et contraintes | Relier les entités et imposer leurs invariants | Clés étrangères et unicité compatibles avec le locataire |
| B033 Transactions métier | Regrouper des écritures cohérentes | Rollback complet et invariants SQL |
| B034 Concurrence optimiste | Modifier une version attendue | Conflit explicite sans écrasement silencieux |
| B035 Requêtes et pagination | Lire, trier et filtrer des données | SQL paramétré, filtres autorisés et taille bornée |
| B036 Migrations de schéma | Faire évoluer les structures persistantes | Verrou, version, reprise et compatibilité testés |
| B037 Historique de versions | Conserver les modifications d’un objet | Accès aux anciennes valeurs aussi contrôlé |
| B038 Brouillons et publication | Séparer édition et état publié | Promotion conditionnelle à une révision |
| B039 Cache applicatif | Réutiliser des résultats coûteux | Clé incluant portée et version, invalidation vérifiée |
| B040 Import de données | Intégrer CSV ou JSON via mapping déclaré | Validation, quotas, prévisualisation et reprise |

## 5 Règles et workflows

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B041 Valeurs exactes | Montants, devises, quantités, unités et identifiants | Pas de flottant pour l’argent, débordements contrôlés |
| B042 Dates et fuseaux | Instants, calendriers et durées | Ambiguïtés de changement d’heure traitées |
| B043 Prédicats métier | Exprimer des conditions combinables | Langage borné sans exécution de code libre |
| B044 Champs calculés | Déduire des valeurs à partir d’autres champs | Dépendances acycliques et accès aux sources contrôlé |
| B045 Machines d’états | Encadrer les transitions d’un objet | Transitions et préconditions validées côté serveur |
| B046 Workflows durables | Enchaîner étapes et attentes persistantes | Version conservée et reprise après interruption |
| B047 Approbations | Organiser validations simples ou multiples | Séparation des rôles et auto-approbation refusée selon la politique |
| B048 Compensations métier | Corriger des effets partiels entre services | Compensation explicite, auditée et elle-même idempotente |
| B049 Activation de fonctionnalités | Ouvrir progressivement une capacité | Valeur sûre initiale, portée et historique |
| B050 Configuration typée | Paramétrer une application ou un bloc | Schéma validé et échec sur paramètre invalide |

## 6 Tâches événements et échanges

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B051 Idempotence | Dédupliquer une commande métier | Clé liée au périmètre et aux paramètres |
| B052 File de travaux | Exécuter des tâches de fond | Baux, générations et concurrence bornée |
| B053 Planification | Déclencher une tâche à une date ou périodiquement | Fuseau, doublons et occurrences manquées traités |
| B054 Outbox transactionnelle | Publier un événement après écriture | État et événement engagés dans la même transaction |
| B055 Inbox et déduplication | Consommer des événements répétés | Effet métier dédupliqué dans sa transaction |
| B056 Reprises et quarantaine | Réessayer ou isoler un travail défaillant | Tentatives finies et résultat inconnu rapproché |
| B057 Webhooks entrants | Recevoir un événement externe | Signature, fenêtre de temps, déduplication et taille |
| B058 Webhooks sortants | Informer un service configuré | Destination autorisée, signature et politique de reprise |
| B059 Client HTTP contrôlé | Appeler des services externes | TLS, prévention SSRF, redirections contrôlées et délais |
| B060 Traitements par lots | Importer, recalculer ou traiter un grand ensemble | Lots bornés, points de reprise et annulation |

## 7 Interface et navigation en Rust

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B061 Structure de page | Composer en-tête, contenu et navigation | HTML sémantique et aucun contenu privé implicite |
| B062 Navigation | Routes, liens, menus et fil d’Ariane | URL validée et aucun droit déduit d’une route visible |
| B063 Thèmes et design tokens | Couleurs, dimensions et identité visuelle | Valeurs autorisées et contrastes contrôlés |
| B064 Textes et actions | Titres, textes, boutons et liens | Échappement, nom accessible et état désactivé cohérent |
| B065 Formulaires | Champs, contraintes et messages d’erreur | Validation serveur identique aux règles affichées |
| B066 Dialogues et panneaux | Confirmation, détails et actions contextuelles | Focus, clavier et retour à l’élément initial |
| B067 Alertes et progression | Informer sur succès, attente et échec | Annonces accessibles et absence de données sensibles |
| B068 Traduction et formats | Langues, nombres, devises et pluriels | Clés validées et contenus échappés |
| B069 Accessibilité commune | Labels, focus, raccourcis et navigation clavier | Tests automatiques et essais humains |
| B070 États vides et erreurs | Afficher indisponibilité ou absence de résultats | Erreur utile sans pile interne ni secret |

## 8 Vues de données en Rust

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B071 Tableaux de données | Afficher, sélectionner, trier et paginer | Pagination serveur et accès contrôlé aux colonnes |
| B072 Recherche et filtres visuels | Composer une requête métier | Filtres issus d’un schéma autorisé |
| B073 Fiches et vues détaillées | Présenter et modifier une ressource | Projection par rôle et commande avec révision |
| B074 Kanban | Déplacer des objets entre états | Transition revalidée côté serveur |
| B075 Calendrier visuel | Présenter dates, créneaux et événements | Fuseau explicite et accès aux événements filtré |
| B076 Graphiques et indicateurs | Afficher séries, mesures et légendes | Agrégats autorisés et alternatives accessibles |
| B077 Arbres et relations | Explorer dossiers, catégories ou dépendances | Profondeur bornée et objets invisibles non révélés |
| B078 Sélecteur de fichiers | Choisir ou déposer un document | Types et tailles contrôlés aussi côté serveur |
| B079 Éditeur de contenu structuré | Composer des documents riches | Schéma restreint et aucun HTML arbitraire |
| B080 Canevas de personnalisation | Éditer les propriétés des composants | Identifiants stables, révisions et opérations autorisées |

## 9 Fichiers documents et contenu

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B081 Téléversement de fichiers | Recevoir des fichiers en flux | Quotas, nom indépendant du chemin et quarantaine |
| B082 Métadonnées de documents | Décrire, classer et étiqueter les fichiers | Droits attachés à chaque objet |
| B083 Téléchargement protégé | Servir un fichier à un lecteur autorisé | Jeton bref, contenu privé et en-têtes adaptés |
| B084 Transformations de médias | Créer vignettes et formats dérivés | Traitement isolé et ressources strictement bornées |
| B085 Modèles de documents | Générer devis, confirmations ou rapports | Données filtrées et modèles approuvés |
| B086 Extraction et OCR | Extraire le texte d’un document | Parseurs ou services isolés et données traitées comme non fiables |
| B087 Contenu éditorial | Gérer pages, articles et révisions | Publication séparée du brouillon |
| B088 Taxonomies | Catégories, tags et classements | Périmètre de locataire et cycles interdits si requis |
| B089 Versions et archives de fichiers | Conserver et restaurer une version | Intégrité, rétention et accès aux anciennes versions |
| B090 Formats d’import et export | Lire et produire formats documentaires validés | Désamorçage des formules CSV et limites de décompression |

## 10 Recherche connaissances et fonctions IA

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B091 Recherche textuelle | Indexer et retrouver des contenus | Index et résultats filtrés par droits |
| B092 Recherche sémantique | Retrouver des contenus par similarité | Embeddings et caches cloisonnés par périmètre |
| B093 Indexation de documents | Découper et indexer des versions | Source, empreinte, rétention et suppression propagées |
| B094 Réponses avec sources | Répondre à partir d’un corpus autorisé | Citations vérifiables et absence de source signalée |
| B095 Extraction structurée par modèle | Extraire des champs dans un schéma | Validation complète et aucune écriture automatique non autorisée |
| B096 Classification assistée | Catégoriser un contenu selon une taxonomie | Classes autorisées, abstention prévue et score calibré s’il pilote une décision |
| B097 Résumé assisté | Résumer un document ou un ensemble | Source conservée et statut de résumé indiqué |
| B098 Validation humaine des résultats IA | Accepter ou corriger une proposition | Décision persistante avant l’effet métier concerné |
| B099 Conversation applicative | Ajouter un assistant à une application | Contexte minimal et outils à capacités limitées |
| B100 Évaluation des fonctions IA | Mesurer qualité, coût et régressions | Jeu de référence versionné et sans secrets |

## 11 Communications et temps réel

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B101 Notifications internes | Informer un utilisateur dans l’application | Destinataire et ressource vérifiés |
| B102 Modèles de messages | Composer un contenu transactionnel | Variables typées et échappement adapté |
| B103 Préférences de notification | Choisir canaux et fréquence | Préférences appliquées avant envoi |
| B104 Courriels transactionnels | Envoyer confirmations et alertes | Connecteur configuré, quotas et déduplication |
| B105 Messages mobiles | Envoyer SMS ou messages via fournisseur | Coût borné, destinataire vérifié et politique d’envoi |
| B106 Notifications push | Informer un appareil inscrit | Abonnement limité à son propriétaire |
| B107 Flux SSE | Diffuser activité et progression | Autorisation, curseur de reprise et files bornées |
| B108 Canaux WebSocket | Interaction bidirectionnelle | Origine, session, autorisation par message et quotas |
| B109 Présence | Afficher une disponibilité temporaire | Durée de vie courte et portée limitée |
| B110 Messagerie d’équipe | Conserver conversations et pièces jointes | Accès aux canaux et documents vérifié à chaque lecture |

## 12 Calendriers réservations et ressources

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B111 Catalogue de ressources | Décrire salles, personnes, équipements ou services | Propriétaire et établissement explicites |
| B112 Règles de disponibilité | Définir horaires et exceptions | Validation des chevauchements et fuseaux |
| B113 Créneaux et capacité | Calculer des places ou unités disponibles | État transactionnel et quantités non négatives |
| B114 Réservations | Créer et confirmer une réservation | Capacité réservée atomiquement et commande idempotente |
| B115 Annulations et reports | Modifier une réservation existante | Règles applicables versionnées et effets compensés |
| B116 Liste d’attente | Proposer une place qui se libère | Ordre explicite et attribution atomique |
| B117 Récurrence | Créer des séries d’événements ou réservations | Expansion bornée et exceptions conservées |
| B118 Affectation de ressources | Associer personnel et matériel à un créneau | Incompatibilités et droits revalidés |
| B119 Accueil et présence | Enregistrer arrivée, départ ou validation d’un billet | Preuve vérifiée et scan rejoué sans double effet |
| B120 Calendriers externes | Synchroniser un calendrier connecté | Portée minimale, conflits et boucle de synchronisation contrôlés |

## 13 Commerce et transactions

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B121 Catalogue de produits | Gérer offres, variantes et descriptions | Publication explicite et champs administratifs privés |
| B122 Tarifs et grilles | Calculer un prix selon des règles | Montants exacts et version du calcul conservée |
| B123 Panier et devis | Préparer une commande | Prix et disponibilité recalculés côté serveur |
| B124 Commandes | Suivre achat, exécution et annulation | Machine d’états et clés de demande uniques |
| B125 Encaissement via prestataire | Initier un paiement hébergé | Aucune donnée de carte traitée par nos blocs |
| B126 Abonnements | Gérer périodes et droits liés à une offre | État rapproché du prestataire et webhook vérifié |
| B127 Documents de facturation | Produire factures ou justificatifs paramétrés | Numérotation contrôlée et règles du territoire à qualifier |
| B128 Remboursements et avoirs | Suivre un remboursement autorisé | Montant borné et rapprochement du résultat externe |
| B129 Promotions | Appliquer coupons et remises | Éligibilité serveur et compteur atomique |
| B130 Stocks et inventaires | Suivre disponibilités et mouvements | Contraintes de stock, journal et concurrence testés |

## 14 Opérations et gestion métier

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B131 Contacts et organisations clientes | Conserver des fiches de relation | Champs privés limités aux rôles autorisés |
| B132 Prospects et opportunités | Suivre un cycle commercial | Transitions configurées et historique des droits |
| B133 Tickets de support | Recevoir et traiter une demande | Visibilité séparée entre demandeur et équipe |
| B134 Projets et tâches | Organiser livrables, responsables et échéances | Affectation et modification autorisées |
| B135 Interventions et ordres de travail | Planifier une prestation ou maintenance | Validation des compétences et accès au site |
| B136 Temps et activité | Saisir et approuver des durées | Corrections historisées et période verrouillable |
| B137 Dépenses et justificatifs | Déclarer et valider une dépense | Montants exacts et validation selon les rôles |
| B138 Achats et fournisseurs | Gérer demandes, commandes et réceptions | Séparation demandeur et approbateur selon politique |
| B139 Matériel et prêts | Suivre possession, prêt et retour | Attribution atomique et historique |
| B140 Dossiers et formulaires métier | Assembler un processus administratif configurable | Schéma versionné, transitions et visibilité des champs |

## 15 Analyse et rapports

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B141 Événements analytiques | Enregistrer des faits d’usage ou métier | Collecte déclarée et données minimisées |
| B142 Agrégations | Calculer totaux et regroupements | Périmètre d’accès maintenu dans la requête |
| B143 Définition des indicateurs | Décrire métriques et périodes | Formule versionnée et unités explicites |
| B144 Tableaux de bord | Composer des vues analytiques | Chaque tuile vérifie sa source et ses droits |
| B145 Rapports planifiés | Produire un rapport à une échéance | Destinataires revalidés au moment de l’envoi |
| B146 Exports volumineux | Générer un export en arrière-plan | Snapshot défini, quotas et téléchargement privé |
| B147 Alertes métier | Détecter un seuil ou une condition | Fenêtre, répétition et déduplication configurées |
| B148 Qualité des données | Détecter doublons et valeurs incohérentes | Corrections séparées de la détection |
| B149 Historique d’indicateurs | Comparer une mesure dans le temps | Version des définitions et comparabilité visibles |
| B150 Suivi de consommation | Présenter quotas, unités et coûts | Registre exhaustif distinct des métriques échantillonnées |

## 16 Adaptateurs de services externes

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B151 Stockage compatible S3 | Lire, écrire et versionner des objets | Préfixe et droits propres, objets privés |
| B152 Messagerie électronique | Brancher un fournisseur d’envoi | Identifiants séparés et statut d’envoi rapproché |
| B153 Prestataires de paiement | Traduire commandes et événements de paiement | Signature, idempotence et aucune clé dans le navigateur |
| B154 Calendriers et agendas | Connecter un fournisseur de calendrier | Scopes limités et jetons chiffrés |
| B155 Cartographie et géocodage | Transformer adresses et coordonnées | Données minimisées, quotas et licence vérifiée |
| B156 OAuth pour connecteurs | Autoriser un compte externe | State, PKCE si applicable, scopes et rotation contrôlés |
| B157 Connecteurs REST typés | Relier une API documentée | Contrat versionné, destinations et effets déclarés |
| B158 Import de bases externes | Importer ou synchroniser un périmètre configuré | Lecture minimale et réseau expressément autorisé |
| B159 API de modèles NVIDIA | Adapter requêtes, réponses et usage d’un endpoint | Clé serveur, capacités vérifiées et aucun repli hors politique |
| B160 Outils MCP | Exposer un ensemble d’outils approuvés | Catalogue d’outils verrouillé et autorisation par appel |

## 17 Fabrique agentique et composition

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B161 Spécification AppSpec | Décrire l’application et ses révisions | Schéma versionné et révision immuable |
| B162 Registre du catalogue | Publier et rechercher les blocs | Version, empreinte, signature et statut d’admission |
| B163 Résolution de composition | Choisir versions et dépendances compatibles | Graphe valide et capacités toutes autorisées |
| B164 Compilateur déterministe | Produire les sources depuis AppSpec | Aucune insertion de code arbitraire |
| B165 Plans et contrats de tâches | Décomposer besoin et critères de réussite | Objectif révisé, lectures, écritures, préconditions et critères protégés |
| B166 Ordonnanceur agentique | Attribuer les tâches aux rôles de modèles | Conflits sémantiques, quotas, équité, générations et arrêt sans progrès |
| B167 Mémoire et compaction | Retrouver décisions, faits et travaux en cours | Contexte ciblé, sources vérifiables, manques signalés et droits rechargés |
| B168 Passerelle de modèles | Appliquer politique et budgets aux appels | Contexte filtré, réservation et réponses validées |
| B169 Passerelle d’outils | Exécuter les intentions des agents | Droits et révision revalidés ; aucun privilège transmis par message |
| B170 Réconciliation du canevas | Intégrer retouches et changements d’agents | Fusion contrôlée, objectif révisé et résultats obsolètes invalidés |

## 18 Construction livraison et exploitation

| Bloc Rust | Capacité préconçue | Invariant particulier à vérifier |
| --- | --- | --- |
| B171 Supervision des sandboxes | Démarrer et arrêter les environnements d’essai | Runtime maintenu, quotas et absence de secrets de production |
| B172 Vérification indépendante | Tester l’image candidate et établir les résultats | Critères protégés, artefact identifié et identité séparée du constructeur |
| B173 Artefacts et provenance | Relier sources, dépendances, image et preuves | Empreintes et authentification vérifiées |
| B174 Promotion et rollback | Déployer puis observer la version servie | Génération, compatibilité des données et reprise |
| B175 Configuration d’environnement | Injecter paramètres et références de secrets | Schéma validé et séparation aperçu et production |
| B176 Observation technique | Produire traces, métriques et journaux | Données expurgées et dimensions bornées |
| B177 Incidents et maintenance | Qualifier, reproduire et corriger un incident | Correction hors production puis même circuit de validation |
| B178 Sauvegarde et restauration | Protéger et récupérer données et objets | Restauration testée et effets externes réconciliés |
| B179 Mises à jour du catalogue | Identifier et migrer les applications concernées | Compatibilité, essais et promotion par application |
| B180 Retrait et export de projet | Exporter puis arrêter proprement un projet | Périmètre confirmé, secrets révoqués et rétention appliquée |

## Recettes métier composables

Ces recettes assemblent les blocs précédents. Elles demandent leurs propres contrats, droits, migrations et tests de bout en bout. Les exemples ne sont pas une promesse de prise en charge immédiate de toute règle sectorielle.

Chaque recette relie ses exigences aux identifiants et versions des blocs, puis à des critères d’acceptation. Distinguer **défini**, **implémenté**, **qualifié isolément** et **qualifié dans cette composition**. « Validé » au catalogue ne signifie pas que toute combinaison de blocs a été testée.

| Recette | Familles mobilisées | Comportement critique |
| --- | --- | --- |
| Réservation de restaurants ou de lieux | Identité, organisations, calendriers, notifications | Une seule attribution de la dernière place |
| Rendez-vous et prestations | Ressources, récurrence, commerce, calendriers externes | Disponibilité et report cohérents |
| Location de matériel | Inventaire, réservation, commandes, documents | Aucun prêt concurrent du même exemplaire |
| Billetterie et événements | Capacité, paiement, notifications, accueil | Paiement et droit d’entrée rapprochés |
| CRM commercial | Contacts, opportunités, tâches, rapports | Cloisonnement des portefeuilles |
| Support client | Tickets, documents, recherche, communications | Un client ne lit jamais un autre dossier |
| Gestion de projets | Tâches, Kanban, fichiers, activité | Droits et changements concurrents préservés |
| Portail d’association | Membres, événements, abonnements, contenu | Adhésion et accès correctement liés |
| Intranet documentaire | Contenu, taxonomies, recherche, partage | Résultats filtrés selon le lecteur |
| Dossiers et demandes administratives | Formulaires, workflows, approbations, documents | Transitions et pièces obligatoires vérifiées |
| Achats et dépenses | Fournisseurs, commandes, justificatifs, approbations | Plafonds et séparation des rôles |
| Suivi d’interventions | Ressources, planning, ordres de travail, fichiers | Affectation et clôture autorisées |
| Stocks et distribution | Produits, inventaire, achats, commandes | Mouvements cohérents sous concurrence |
| Commerce en ligne | Produits, panier, paiement, notifications | Commande et paiement idempotents |
| Abonnement SaaS | Organisations, offres, facturation, quotas | Droit d’usage lié à l’état confirmé |
| Gestion de formation | Membres, contenu, sessions, formulaires | Accès et inscription correctement bornés |
| Portail partenaires | Partage, documents, workflows, intégrations | Accès limité au périmètre convenu |
| Assistant de connaissances | Documents, recherche, conversation, évaluation | Aucune source privée hors des droits |
| Tableau de bord opérationnel | Données, agrégations, indicateurs, alertes | Cohérence des définitions et périodes |
| Outil interne configurable | Entités, règles, formulaires, vues, workflows | Composition validée sans code improvisé |

Les métiers réglementés, écritures comptables officielles, paie, fonctions médicales ou paiements complexes entre vendeurs exigent des extensions, expertises et validations supplémentaires. Un formulaire, un tableau ou un adaptateur de paiement générique ne suffit pas à annoncer leur couverture.

## Organisation de la construction

| Chantier | Périmètre | Preuve attendue |
| --- | --- | --- |
| Fondations communes | Contrats, identité, autorisation, transactions, secrets, quotas et audit | Réutilisation et invariants vérifiés |
| Fonctions transversales | Interfaces, données, fichiers, recherche, notifications, workflows et rapports | Compositions réutilisables entre plusieurs applications |
| Familles métier | Réservation, commerce, opérations, portails et fonctions IA | Recettes complètes avec règles et droits vérifiés |
| Fabrique et cycle de vie | Agents, canevas, mémoire, compaction, publication et maintenance | Création et évolution cohérentes avec mesures de coût |
| Démonstration du hackathon | Scénarios à choisir pour montrer la diversité et la réutilisation | Applications effectivement fonctionnelles et comparatif Codex |

Ces chantiers peuvent avancer selon leurs dépendances ; le tableau n’impose pas un ordre de livraison. Le catalogue de 180 blocs porte l’ambition généraliste du projet. La sélection des capacités à construire et à démontrer au hackathon reste à définir avec Augustin, sans restriction actée à une seule verticale. Chaque bloc conserve son état réel d’avancement et ses exigences de sécurité.

## Validation et maintenance du catalogue

Pour chaque version, suivre propriétaire, état d’admission, preuves, dépendances et applications qui l’utilisent. Les changements incompatibles exigent migration et validation. Une version retirée n’entre plus dans de nouvelles compositions ; une vulnérabilité déclenche l’identification des applications affectées et une correction contrôlée. L’ancienne image peut rester nécessaire à une récupération, sous politique explicite. La qualification porte aussi sur les assemblages : droits effectifs, données transmises, ordre des effets et invariants partagés. Un changement purement compatible au niveau des types peut modifier une règle métier et invalider les preuves concernées.

Le catalogue de capacité B001 à B180 et les recettes constituent notre conception. Les mécanismes s’appuient notamment sur [les garanties et limites de Rust](https://doc.rust-lang.org/book/ch20-01-unsafe-rust.html), [le contrôle d’accès OWASP](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html) et [la sécurité par ligne PostgreSQL](https://www.postgresql.org/docs/current/ddl-rowsecurity.html). Ces références ne certifient pas nos futurs blocs.