**Construire le backend en cinq parties successives, avec une preuve de fin pour chacune.** On termine et valide une partie avant d’ouvrir la suivante. Le résultat visé est une chaîne utilisable par API : conserver un projet, assembler une application, faire travailler les agents, publier une version puis la maintenir.

Ce document propose un ordre de réalisation à partir de l’architecture actuelle de Kyro. Il ne constate pas l’avancement du code et ne fixe pas la sélection finale des fonctionnalités du hackathon. Les choix déjà établis restent la base : Rust, PostgreSQL, catalogue obligatoire, modèles NVIDIA par API, quatre exécutants initiaux et maintenance testée hors production.

## Les cinq parties et leur résultat

| Ordre | Partie | Résultat qui permet de passer à la suite |
| --- | --- | --- |
| 1 | Socle durable et accès contrôlés | Une commande autorisée est enregistrée, exécutée et reprise après interruption sans perdre son état. |
| 2 | Catalogue et fabrication des applications | Un AppSpec fourni par API devient une application serveur construite et vérifiée dans un environnement isolé. |
| 3 | Planification et équipe d’agents | Une demande textuelle devient un plan, puis une application vérifiée, avec budgets et reprises contrôlés. |
| 4 | Publication et évolution des versions | L’image vérifiée est publiée, observée et mise à jour en préservant les données. |
| 5 | Maintenance et qualification finale | Un incident déclenche une correction hors production, sa validation et une publication contrôlée ; les résultats et coûts sont mesurés. |

Cinq est le découpage proposé pour garder des points d’arrêt utiles. Chaque partie répond à une question différente : l’état est-il fiable, la fabrication fonctionne-t-elle, les agents savent-ils l’utiliser, la livraison est-elle maîtrisée, puis le service sait-il se maintenir ? Ce sont des étapes de construction, pas cinq microservices ni cinq chantiers parallèles.

La règle « une partie à la fois » concerne le développement du backend. Une fois la partie 3 livrée, le produit pourra exécuter plusieurs tâches d’agents indépendantes en parallèle, comme le prévoit son architecture.

## Le périmètre du backend

Le travail couvre le backend de la plateforme et les capacités serveur des applications qu’elle assemble : API, état persistant, autorisations, tâches, catalogue, compilation, agents, mémoire, sandboxes, preuves, déploiements, données, connecteurs et exploitation. Les essais passent par un client HTTP ou des scripts de recette.

Les écrans de l’IDE, React, le rendu Leptos, le design, les gestes du canevas et les composants visuels ne font pas partie de ce plan. En revanche, leurs besoins côté serveur y figurent : commandes de modification, propriétés persistantes, conflits de révision, permissions, flux d’activité et préparation des aperçus. Une recette HTTP vérifie ces contrats sans construire l’interface.

**Le catalogue demande une borne explicite.** Les 180 blocs comprennent 20 blocs d’interface et de vues, 140 autres blocs applicatifs ou adaptateurs, et 20 blocs de fabrique et d’exploitation. Une capacité mixte, comme un tableau de bord, conserve ici sa partie serveur. Les 140 blocs ne sont pas déclarés terminés parce que l’assembleur fonctionne.

Au début de la partie 1, établir un **manifeste de livraison** : pour chaque capacité backend du catalogue, indiquer sa partie responsable, ses dépendances, ses critères d’acceptation et si elle appartient à la version à livrer. Toute sélection plus courte reste une proposition explicite ; aucune verticale unique ni réduction du hackathon n’est actée ici. Si l’objectif retenu est tout le catalogue backend, la partie 2 doit en livrer toutes les capacités applicatives correspondantes. La fin d’une partie est toujours évaluée contre ce manifeste versionné.

## La règle pour terminer une partie

Une partie est terminée lorsque ses contrats sont documentés et versionnés, son scénario nominal fonctionne sur les composants réels, ses cas de refus et de panne passent, et les recettes des parties précédentes restent valides. Conserver le commit, les versions utilisées et le rapport de recette. Un message d’agent annonçant un succès ne suffit pas.

Les simulateurs servent à provoquer les pannes et à développer les contrats. Ils ne remplacent pas la vérification d’un service réel inclus dans la livraison. Chaque partie ajoute ses tests au même parcours de validation. Un défaut découvert ensuite se corrige avec un test de régression ; « terminé » signifie un résultat accepté, pas une interdiction de faire évoluer le code.

## Partie 1 Construire le socle durable et les accès contrôlés

**Objectif :** disposer d’un serveur fiable que les autres parties pourront appeler. Cette partie ne dépend ni d’un assembleur applicatif ni d’une équipe d’agents.

### Travail à réaliser dans cet ordre

1. Fixer le manifeste de livraison et créer le workspace Rust, les exécutables API et worker, PostgreSQL, les migrations, la configuration typée et la CI. Définir les responsabilités des modules et les dépendances autorisées.

2. Mettre en place l’identité, les sessions, les organisations, les projets et les droits. Ajouter les références de secrets, la politique de données et les limites de ressources. Le contexte de projet est vérifié côté serveur.

3. Persister les révisions immuables d’AppSpec, les opérations de ChangeSet et les décisions du projet. Exiger la révision attendue pour modifier un état ; conserver les identifiants stables et les propriétés non concernées par le changement. La validation sémantique complète du catalogue arrivera en partie 2.

4. Construire la file durable, les baux, les générations de tentatives, l’outbox, la déduplication et les intentions d’effets externes. Prévoir arrêt, échec borné et résultat inconnu à rapprocher avant une éventuelle reprise.

5. Ajouter les adaptateurs d’accès externe nécessaires, dont la passerelle NVIDIA : clé serveur, contexte autorisé, réponses structurées, délais, réservation atomique des budgets et registre de consommation. Qualifier les endpoints retenus et enregistrer leurs capacités et tarifs datés. Un appel de modèle ne porte encore aucun rôle d’orchestrateur.

6. Exposer les commandes et lectures de projet, les erreurs stables et le flux SSE avec reprise. Installer dès maintenant les journaux expurgés, traces, contrôles de santé et sauvegardes de l’état de pilotage.

**Contrats livrés :** projets et révisions, ChangeSet, CapabilityGrant, DataPolicy, travail durable, EffectIntent, événements et consommation. Fournir un contrat HTTP versionné et une recette exécutable sans navigateur.

### Preuve de fin de la partie 1

- [ ] Deux identités de projets distincts ne peuvent lire ni modifier leurs ressources respectives, y compris via le flux d’événements.

- [ ] Une commande persistante survit à l’arrêt du worker ; sa livraison répétée ne produit pas deux mutations internes. Un résultat externe perdu devient inconnu et n’est pas rejoué aveuglément.

- [ ] Une écriture fondée sur une ancienne révision est refusée ; un ancien worker ne peut enregistrer un résultat devenu obsolète.

- [ ] Plusieurs appels concurrents proches du plafond ne réservent pas plus de budget que disponible ; les consommations inconnues restent provisionnées.

- [ ] Un appel réel à l’API de modèle retenue est tracé et rapproché. Un secret factice ou une destination interdite est bloqué sans fuite dans les journaux.

- [ ] Après redémarrage et restauration d’essai du pilotage, projets, révisions, droits et travaux en attente restent cohérents.

**Passage à la partie 2 :** le socle sait conserver et exécuter une commande contrôlée. Les parties suivantes utilisent ses contrats, sans réimplémenter les droits, la file ou la comptabilité.

## Partie 2 Construire le catalogue et la fabrique déterministe

**Objectif :** transformer une spécification fournie par API en application serveur testée. Aucun agent n’est nécessaire pour prouver que cette fabrication fonctionne.

### Travail à réaliser dans cet ordre

1. Créer le registre des composants et leurs manifestes : versions, empreintes, contrats, dépendances, capacités, migrations et preuves. Mettre en place l’admission, la dépréciation et la révocation. Seules les versions validées sont utilisables.

2. Implémenter et qualifier les capacités applicatives du manifeste de livraison, dans l’ordre de leurs dépendances : fondations communes, données et règles, traitements et connecteurs, puis compositions métier. Les fonctions IA utilisent la passerelle de la partie 1. Chaque service externe livré possède un adaptateur Rust et une qualification documentée.

3. Construire le résolveur et le validateur sémantique d’AppSpec : types, références, droits, effets, invariants et compatibilité. Verrouiller les versions de la composition. Une capacité absente produit un diagnostic explicite.

4. Construire l’assembleur Rust : sources et migrations issues uniquement de modèles admis, provenance par composant, sortie reproductible et export Git reprenable. Un paramètre ne devient jamais du code libre.

5. Qualifier les sandboxes et lancer compilation, base applicative de test, procédures d’essai et image OCI candidate sous quotas. Séparer données, identités et réseaux du pilotage, de la construction et des applications.

6. Exécuter les contrôles dans un environnement de vérification indépendant. Produire un EvidenceBundle authentifié lié à l’image, à la configuration, aux migrations et aux tests exécutés. Le constructeur ne peut ni modifier les critères protégés ni authentifier lui-même ses résultats.

**Contrats livrés :** ComponentManifest, verrou de composition, diagnostic de compatibilité, manifeste des sources, image candidate, EvidenceBundle et ReleaseManifest. Entrée : une révision AppSpec et des versions verrouillées. Sortie : un artefact identifié et ses preuves.

### Preuve de fin de la partie 2

- [ ] Les capacités du manifeste sont qualifiées, et plusieurs compositions de familles différentes réutilisent réellement les mêmes blocs. Leurs scénarios API sont fixés avant la recette ; réservation, support et gestion de stock constituent des exemples possibles.

- [ ] Une même entrée verrouillée reproduit les mêmes sources. Cette assertion ne présume pas une reproductibilité binaire de toute l’image.

- [ ] Les cas applicables vérifient accès croisés refusés, concurrence sur une ressource limitée et réception répétée d’un événement sans double effet métier.

- [ ] Un bloc absent, non admis ou révoqué, une composition incompatible et une tentative d’insertion de code libre sont refusés.

- [ ] Les sandboxes respectent leurs limites et n’accèdent pas aux secrets de production ; une image modifiée ou un faux rapport ne conserve pas des preuves valides.

- [ ] Un échec de compilation ou d’export Git conserve l’état de référence et se reprend sans divergence.

**Passage à la partie 3 :** Kyro sait déjà fabriquer les applications du périmètre retenu à partir de spécifications explicites. Les agents pourront ensuite piloter ce mécanisme éprouvé.

## Partie 3 Ajouter la planification et les agents

**Objectif :** convertir une demande en opérations autorisées sur la fabrique, avec mémoire, budgets et reprise.

### Travail à réaliser dans cet ordre

1. Définir Plan et TaskContract : objectif révisable, composants, instantané, lectures, écritures, dépendances, capacités, critères protégés et limites. Implémenter le mode plan en lecture seule sur l’application et ses données.

2. Vérifier par code la couverture du catalogue, les contrats, l’absence de cycles, le budget et les conflits sémantiques. Déterminer les tâches prêtes ; exécuter directement les transformations calculables.

3. Brancher l’orchestrateur NVIDIA, les quatre exécutants initiaux, la revue et la sécurité. Conserver la répartition de modèles prévue dans la page produit et qualifier chaque rôle sur ses vrais contrats. Une seule microtâche active par exécutant ; le parallélisme concerne seulement les travaux indépendants.

4. Relier les sorties structurées aux ChangeSet et aux outils autorisés. L’ordonnanceur intègre les résultats et demande les vérifications de la partie 2. Un modèle ne change ni ses permissions ni les critères qui jugent son travail.

5. Ajouter mémoire sourcée, recherche ciblée et compaction automatique de chaque rôle. Recharger droits, budgets et états critiques depuis le stockage de référence.

6. Traiter annulation, nouvelle consigne, révocation et conflit avec une retouche fournie par API. Invalider les résultats incompatibles, conserver le travail encore valable et arrêter les reprises sans progrès.

**Contrats livrés :** plan versionné, tâche, résultat structuré, blocage, revue et point de reprise. Entrée : demande, révision et politique de projet. Sortie : proposition intégrée et vérifiée, ou blocage explicite avec son diagnostic.

### Preuve de fin de la partie 3

- [ ] Une demande couverte produit, avec les modèles réels, un plan valide puis une application vérifiée par la fabrique. Une demande hors catalogue reste bloquée sans code improvisé.

- [ ] Quatre exécutants traitent des tâches effectivement indépendantes ; deux tâches qui touchent le même invariant sont ordonnées ou refusées, même si leurs fichiers diffèrent.

- [ ] Une compaction et un redémarrage forcés retrouvent objectif, décisions, preuves, budgets et effets en cours sans réexécuter une action à l’aveugle.

- [ ] Une instruction nouvelle ou un droit retiré pendant un appel empêche l’intégration devenue invalide. Une propriété récemment modifiée par le client est préservée ou fait l’objet d’un conflit explicite.

- [ ] Faux succès, preuve manquante, consigne malveillante et erreur répétée sans progrès ne permettent ni validation ni dépassement des limites.

**Passage à la partie 4 :** la demande peut conduire à un artefact candidat réellement vérifié. La publication reste une opération distincte, avec sa propre autorité.

## Partie 4 Publier et faire évoluer les versions

**Objectif :** servir exactement l’image validée et maîtriser ses changements de configuration et de données.

### Travail à réaliser dans cet ordre

1. Préparer l’hébergement de référence et les environnements d’essai et de production, avec bases, rôles, secrets, stockage et configuration distincts. Automatiser leur installation et les contrôles de santé. Les applications possèdent leur runtime et leurs données.

2. Construire le contrôleur de publication durable : version attendue, souhaitée et observée, génération, vérification des preuves, autorisations et une seule promotion active par environnement.

3. Exécuter les migrations par une procédure dédiée, verrouillée et reprenable. Déployer l’image par empreinte, contrôler son état et vérifier le routage effectivement servi.

4. Ajouter le rapprochement après accusé perdu, la mise à jour d’une application existante et le rollback compatible avec le schéma courant. Une restauration du code ne prétend pas annuler les données ou les effets externes.

5. Mettre en service sauvegardes et restauration d’une application isolée avant d’utiliser des données réelles. Conserver ensemble données, objets, versions et références de configuration ; empêcher toute réémission incontrôlée d’effets après restauration.

**Contrats livrés :** Deployment, demande de promotion, état observé, journal de migration, procédure de rollback et procédure de restauration. L’API distingue brouillon conservé, candidat vérifié et version réellement publiée.

### Preuve de fin de la partie 4

- [ ] L’API publique de l’application répond et l’image servie correspond à l’empreinte validée. Une preuve absente, périmée ou attachée à un autre artefact bloque la promotion.

- [ ] Une coupure pendant migration ou bascule est réconciliée ; elle ne déclenche ni double migration ni promotion tardive d’une ancienne génération.

- [ ] Une évolution préserve les données et préférences non concernées. Une migration incompatible bloque la livraison.

- [ ] Un rollback autorisé fonctionne ; une récupération incompatible est refusée. La restauration d’une application laisse les autres applications intactes.

- [ ] L’application conserve ses parcours ordinaires lorsque le pilotage ou le fournisseur de modèles est arrêté. Ses éventuelles fonctions IA signalent leur dépendance indisponible.

**Passage à la partie 5 :** création, publication et évolution fonctionnent avec récupération éprouvée. La maintenance réutilise ce circuit de livraison.

## Partie 5 Assurer la maintenance et qualifier le backend

**Objectif :** exploiter le backend sans session utilisateur ouverte et démontrer ses limites réelles.

### Travail à réaliser dans cet ordre

1. Relier les sondes externes, journaux autorisés et alertes aux incidents. Regrouper les signaux, suivre leur version et réserver une capacité de traitement aux incidents.

2. Brancher l’agent de maintenance avec le même modèle que l’orchestrateur. Il observe, reproduit hors production, prépare un plan et délègue la correction. Il utilise les mêmes contrôles et le même contrôleur de publication que les parties précédentes.

3. Gérer les mises à jour et révocations du catalogue : retrouver les applications affectées, proposer des changements versionnés et les vérifier avant promotion. Un défaut interne d’un bloc rejoint le circuit séparé d’admission du catalogue.

4. Finaliser récupération, rotation des secrets, rétention, export et retrait de projet. Exécuter les recettes de charge, saturation, panne fournisseur, équité entre projets et restauration ; enregistrer les seuils et les résultats.

5. Produire le rapport backend : couverture du manifeste, blocages, performances observées, reprises, consommation de chaque rôle et coût complet. Préparer le protocole reproductible du comparatif Codex et exécuter ses cas backend à conditions identiques.

**Contrats livrés :** incident, diagnostic sourcé, plan de réparation, preuves de correction, export de projet et rapport de qualification. La surveillance fonctionne sur l’hébergement continu même lorsque le poste du client est éteint ; le modèle est appelé lorsqu’un travail est nécessaire.

### Preuve de fin de la partie 5

- [ ] Un défaut injecté dans l’environnement de démonstration est détecté, reproduit hors production, corrigé, testé puis publié. Sans reproduction ou preuves suffisantes, la correction reste bloquée.

- [ ] La correction part de la version publiée, préserve les données et choix du client, et n’embarque pas un brouillon non validé.

- [ ] Une version de bloc révoquée est interdite aux nouvelles constructions ; les applications affectées sont identifiées et traitées par un changement contrôlé.

- [ ] Panne d’API, saturation et redémarrage ne provoquent pas une boucle de reprises ou une dépense sans borne. Les exercices de récupération produisent des temps et pertes de données mesurés.

- [ ] Un export restitue les éléments prévus par son contrat ; le retrait révoque les accès et applique la rétention définie.

- [ ] Le rapport rattache chaque capacité annoncée à ses preuves et compte aussi les échecs, corrections et frais de coordination. Aucun gain de coût ou de qualité n’est annoncé sans mesure.

Le comparatif complet demandé dans la page produit comprend aussi les critères visuels et les parcours avec interface. Ce plan livre son instrumentation et sa partie backend ; il ne permet pas de déclarer ce comparatif complet avant que le chantier d’interface ait fourni ses preuves.

## Où se range chaque chantier existant

| Chantier des pages actuelles | Partie responsable |
| --- | --- |
| Infrastructure du pilotage, identité, projets, droits, révisions, file, budgets et audit | 1 |
| Passerelle de modèles et accès externes contrôlés, dont B159 et B168 | 1 ; qualification des rôles en 3 |
| Capacités applicatives serveur B001 à B060 et B081 à B160 | 2 ; réutilisation des fondations déjà livrées en 1 |
| AppSpec B161 | Persistance et révisions en 1 ; sémantique de composition en 2 |
| Catalogue, résolution et compilateur B162 à B164 | 2 |
| Plans, ordonnanceur, mémoire et compaction B165 à B167 | 3 |
| Passerelle d’outils B169 | Contrôle d’accès commun en 1 ; opérations de fabrique en 2 et délégation en 3 |
| Réconciliation B170 et besoins serveur du canevas | Révisions et propriétés en 1 ; intégration des changements d’agents en 3 |
| Sandboxes, vérification et provenance B171 à B173 | 2 |
| Promotion et configuration B174 et B175 | 4 |
| Observation B176 | Instrumentation dès 1 ; supervision et incidents en 5 |
| Maintenance B177 et évolution du catalogue B179 | 5 |
| Sauvegarde B178 | État du pilotage en 1 ; restauration applicative en 4 ; exercices complets en 5 |
| Retrait et export B180 | 5 |
| Interface B061 à B080 et enveloppe de l’IDE | Chantier d’interface séparé ; contrats serveur couverts ci-dessus |

Lorsqu’un chantier apparaît dans plusieurs parties, chaque livraison ajoute un usage précis à un contrat existant. On n’attend pas la maintenance pour journaliser, ni l’interface pour contrôler les droits. À l’inverse, on ne commence pas les fonctionnalités d’une partie future avant d’avoir validé celle en cours.

## Commencer maintenant par la partie 1

Le premier résultat à rechercher est simple à observer : **créer un projet par API, enregistrer une révision, lancer une opération autorisée, interrompre le worker puis retrouver un état cohérent après reprise.** Préparer d’abord le manifeste de livraison et cette recette. L’interface, la génération d’application et l’orchestration ne sont pas nécessaires à cette première preuve.

## Références du projet

Ce séquencement s’appuie sur la [vision de Kyro](https://chatgpt.com/space/page_c41a658d84b08191b0295f1f1cb36e2d), l’[architecture technique](https://chatgpt.com/space/page_1869350b9e648191966bb8b7cff5b03b), le [catalogue des 180 blocs](https://chatgpt.com/space/page_a34ccccf45d88191a9cf39578c465f43) et le [journal de décisions](https://chatgpt.com/space/page_baca1e1d35e481919683b0dc23455045), relus le 2 octobre 2026. Le découpage en cinq parties et leurs critères de passage constituent la proposition de ce document.