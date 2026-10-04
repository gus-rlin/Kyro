Ce journal explique les décisions qui font évoluer l’[architecture technique de la plateforme agentique](https://chatgpt.com/space/page_1869350b9e648191966bb8b7cff5b03b). Il conserve les dix décisions de la première revue du 2 octobre 2026, puis documente la révision demandée le même jour : **contrainte entièrement locale retirée, appels API cloud assumés, blocs Rust, modularité et sécurité par défaut**. La dernière revue avant construction est consignée en D20 à D23, avec ses sources et ses critères de validation.

**État courant :** Rust pour le pilotage, l’assembleur et tous les blocs développés ; Leptos pour les interfaces applicatives ; React et TypeScript pour l’enveloppe de l’IDE ; PostgreSQL et API de modèles NVIDIA. Le choix C# et .NET de la première proposition est remplacé par D12. Les preuves D01 à D10 restent requises avec les adaptations ci-dessous. Aucun gain de performance, de sécurité ou de coût n’est encore mesuré.

Les sources primaires établissent des mécanismes et leurs limites. Les arbitrages entre ces mécanismes sont nos choix pour ce produit, avec sa contrainte de faible coût de lancement.

## Vue des décisions

| Décision | Lacune de la première version | Amélioration retenue |
| --- | --- | --- |
| D01 | Autonomie des applications publiées peu explicite | Parcours métier indépendants de l’IA et du pilotage |
| D02 | Répartition d’autorité entre AppSpec et Git ambiguë | Révisions PostgreSQL de référence et export Git rejouable |
| D03 | Reprise après effet externe insuffisamment définie | Intentions durables et état de résultat inconnu |
| D04 | Limites techniques et financières peu articulées | Admission, équité et réservation des budgets |
| D05 | Origine des preuves de test trop abstraite | Vérificateur indépendant lié à l’image candidate |
| D06 | Publication décrite comme une opération simple | Réconciliation entre version souhaitée et version observée |
| D07 | Concurrence du canevas et reconnexion à préciser | Préconditions HTTP, fusion contrôlée et reprise du flux |
| D08 | Isolation exprimée surtout par les composants choisis | Droits, réseau, pools et clés explicitement cloisonnés |
| D09 | Récupération par application non distinguée du cluster | Restauration isolée avec contrôle des effets externes |
| D10 | Risque de coûts cachés et de double comptage | Caches bornés, métriques maîtrisées et allocation unique |
| D11 | Ambiguïté entre IDE installé et inférence locale | Contrainte tout local retirée et API cloud prévues |
| D12 | Rust seulement optionnel et blocs en C# | Tous les blocs développés et le cœur passent en Rust |
| D13 | Modularité insuffisamment vérifiable | Propriété des écritures et dépendances de crates contrôlées |
| D14 | Sécurité décrite sans contrat initial commun | Refus par défaut et admission conditionnée aux preuves |
| D15 | Données envoyées au cloud peu explicites | Politique de données et passerelle de modèles |
| D16 | Étendue du catalogue insuffisamment définie | 180 blocs cibles, recettes et lots de construction |
| D17 | Évolution et retrait des blocs à préciser | Versions verrouillées, migrations et révocation |
| D18 | Risque de confondre langage et performance | Mesures complètes et dépendances IA explicites |
| D19 | Proposition de réduction présentée comme une décision et détails techniques dans la page d’idée | Vision généraliste réaffirmée et séparation des pages |
| D20 | Indépendance des tâches définie surtout par les fichiers | Contrats complets et conflits sémantiques contrôlés |
| D21 | Risque de transmettre des assertions comme des preuves | Messages référencés et validation indépendante |
| D22 | Changement de demande pendant l’exécution à expliciter | Révisions d’objectif et invalidation ciblée |
| D23 | Reprises et score global peu explicatifs | Arrêt sans progrès, qualification et diagnostic des échecs |

## D01 Des applications indépendantes du service de pilotage

**Constat.** La première architecture séparait les machines, sans préciser si l’authentification ou une réservation devait encore consulter le service de pilotage. Une séparation d’hébergement ne suffit pas à éliminer cette dépendance.

**Décision conservée et précisée.** Chaque application publiée conserve ses paramètres, données, sessions et clés nécessaires aux parcours ordinaires. La réservation ne consulte ni le modèle ni la mémoire agentique. Un fournisseur d’identité externe reste une dépendance déclarée. Les fonctions IA ajoutées à une application peuvent dépendre du cloud ; elles ont leur propre mode dégradé. Le cœur demeure un monolithe modulaire, désormais en Rust.

**Pourquoi et compromis.** C’est notre choix pour que les indisponibilités de l’IA affectent la création et la maintenance, sans devenir une panne métier générale. Il impose de versionner et maintenir plusieurs runtimes applicatifs. L’hébergement sur une seule machine applicative reste un point de panne partagé ; aucune haute disponibilité n’est déduite de ce découpage.

**Preuve attendue.** Couper l’API NVIDIA et le pilotage, puis réussir connexion, consultation et réservation sur une application déjà publiée. Réexaminer la topologie quand disponibilité, charge ou coût mesurés l’exigent.

## D02 Une seule référence et un assembleur vérifiable

**Constat.** AppSpec et Git étaient tous deux versionnés, mais la première description ne tranchait pas le cas où une écriture réussit dans PostgreSQL et échoue dans Git. La notion d’assembleur déterministe restait également trop générale.

**Décision.** PostgreSQL accepte les révisions AppSpec immuables. L’outbox pilote génération et export Git ; une livraison exige que les références soient cohérentes. L’assembleur valide structure, références, types, droits, effets et dépendances avant de produire les sources. Il dépend d’entrées entièrement identifiées : révision, catalogue, assembleur et dépendances.

**Pourquoi et compromis.** Notre choix évite une transaction distribuée entre base et dépôt. Git reste consultable et exportable, avec un délai possible avant synchronisation. Une édition externe du code doit être détectée et traitée explicitement. La canonicalisation de la [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785.html) fournit une représentation JSON stable pour les empreintes ; cette RFC est informative. L’assembleur et les règles métier restent à construire et à tester.

**Preuve attendue.** Interrompre l’export, reprendre sans révision divergente, puis générer deux fois les mêmes sources avec les mêmes entrées. Ne pas présenter ce résultat comme une preuve de reproductibilité de toute la chaîne binaire.

## D03 Des reprises qui traitent les résultats inconnus

**Constat.** Un bail expiré ne peut pas annuler une requête déjà partie. Rejouer après un délai dépassé peut répéter un effet externe.

**Décision.** Une action conserve son identifiant et ses paramètres entre tentatives. Le serveur refuse les écritures d’une génération obsolète. Un résultat incertain bloque les actions dépendantes jusqu’à rapprochement ; un connecteur sans idempotence ni moyen de vérification n’autorise pas de reprise automatique.

**Pourquoi et compromis.** [AWS documente](https://aws.amazon.com/builders-library/making-retries-safe-with-idempotent-APIs/) les identifiants de requête et la détection de paramètres incompatibles. Notre état explicite de résultat inconnu accepte un blocage temporaire pour préserver la cohérence. Il ajoute du travail aux adaptateurs de connecteurs.

**Preuve attendue.** Perdre l’accusé après un effet réussi, relancer le worker et vérifier qu’aucun effet n’est répété aveuglément. Tester aussi une même clé avec un contenu différent.

## D04 Des quotas adaptés à chaque ressource

**Constat.** Quatre exécutants ne représentent ni quatre compilations possibles, ni quatre budgets indépendants. Une file peut être durable tout en saturant la machine ou le fournisseur.

**Décision.** Séparer les quotas d’appels LLM, de compilations, de navigateurs et d’aperçus. Organiser une rotation entre projets, réserver de la capacité aux incidents et immobiliser le budget avant chaque appel. Une consommation au résultat inconnu reste provisionnée.

**Pourquoi et compromis.** Notre choix favorise un coût prévisible et limite la propagation des pannes. [AWS recommande](https://docs.aws.amazon.com/wellarchitected/latest/framework/rel_mitigate_interaction_failure_limit_retries.html) des reprises bornées avec attente et aléa. Nous les pilotons à un seul niveau pour éviter leur multiplication. Une file d’attente plus visible et certains délais supplémentaires sont le compromis accepté.

**Preuve attendue.** Lancer plusieurs projets, provoquer des réponses 429 et une panne fournisseur, puis vérifier plafonds, équité, progression et coût. Les valeurs numériques des quotas seront fixées après l’essai sur les machines choisies.

## D05 Des preuves indépendantes du constructeur

**Constat.** La première version exigeait des preuves sans détailler qui les établissait. Un rapport écrit par l’agent ou le processus testé ne suffit pas à autoriser une publication.

**Décision.** Construire l’image candidate, puis la tester dans une sandbox neuve avec un vérificateur protégé. Une identité de confiance authentifie les résultats et les lie à l’image, à la configuration contrôlée et aux versions des tests. Les clés restent hors des étapes exécutant le projet.

**Pourquoi et compromis.** [SLSA sépare les secrets d’authentification de provenance des étapes de construction utilisateur](https://slsa.dev/spec/v1.2/build-requirements). Nous appliquons ce principe au circuit de validation. Il ajoute une frontière de confiance et du calcul. Une signature valide atteste la provenance du résultat, jamais l’absence de défauts ; aucun niveau SLSA n’est revendiqué.

**Preuve attendue.** Soumettre un faux rapport, changer l’image après test et présenter une preuve émise par une identité non autorisée. Les trois promotions doivent être refusées.

## D06 Une publication reprenable

**Constat.** « Déployer l’artefact validé » ne résout pas une coupure après migration ou après bascule du trafic. L’état enregistré peut différer de la version effectivement servie.

**Décision.** Enregistrer version attendue, souhaitée et observée, ainsi qu’une génération et l’étape courante. Un seul contrôleur fait avancer une publication par environnement. Après interruption, il observe l’état réel avant de poursuivre. Les migrations ont un travail dédié, un verrou et un journal de reprise.

**Pourquoi et compromis.** Notre protocole applique le principe de rapprochement des effets externes à la livraison. Il exige un adaptateur d’hébergement capable d’identifier précisément image, routage et migrations. Les opérations non transactionnelles ont leurs propres procédures de récupération ; un retour arrière du code reste conditionné à la compatibilité des données.

**Preuve attendue.** Interrompre successivement migration, démarrage et bascule. À chaque reprise, vérifier l’absence de double migration et l’affichage correct de la version servie. Le nom « publié » n’est utilisé qu’après cette observation.

## D07 Une édition concurrente qui conserve les intentions

**Constat.** Le canevas devait conserver les choix de l’utilisateur, sans protocole suffisamment précis pour une modification simultanée de l’agent. La reconnexion pouvait aussi laisser l’interface avec un historique incomplet.

**Décision.** Employer ETag et If-Match pour refuser les écritures périmées, puis comparer base, proposition et état courant. Réappliquer automatiquement les propriétés indépendantes et revalider la composition ; rendre les incompatibilités explicites. Après expiration du curseur SSE, recharger un état complet.

**Pourquoi et compromis.** La [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.1) définit les préconditions HTTP utiles contre les écrasements concurrents. Le [standard HTML](https://html.spec.whatwg.org/multipage/server-sent-events.html) définit la reprise SSE. Notre résolution métier des conflits complète ces mécanismes. Elle ajoute du code et peut demander une décision lorsqu’aucune fusion ne respecte les deux intentions.

**Preuve attendue.** Modifier une couleur pendant une réparation, modifier simultanément une permission et reconnecter un client après expiration de l’historique. Aucun changement ne doit disparaître silencieusement ; un aperçu conserve des droits distincts de la publication.

## D08 Une isolation définie par les droits effectifs

**Constat.** Choisir gVisor, une base par application et des cookies sécurisés laisse encore ouverts réseau sortant, partage des caches, saturation SQL et persistance des clés.

**Décision conservée et adaptée à Rust.** Imposer quotas et règles réseau depuis l’hôte, séparer construction et vérification, contrôler les actions côté serveur et cloisonner caches et identifiants. Limiter les pools SQLx. Conserver, protéger et renouveler les secrets de sessions et de chiffrement par application et environnement ; le mécanisme Data Protection de la proposition .NET est remplacé.

**Pourquoi et compromis.** [gVisor](https://gvisor.dev/docs/architecture_guide/security/) complète la sécurité réseau et la gestion des ressources. La [gestion des sessions OWASP](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html) guide les protections et la révocation ; [SQLx](https://docs.rs/sqlx/latest/sqlx/) remplace le pilote de données initial. Les interfaces retenues demandent une configuration et des essais concrets, pas seulement un choix de bibliothèques.

**Preuve attendue.** Essayer l’accès aux métadonnées cloud, à l’administration et aux fichiers d’un autre projet ; saturer un pool ; redémarrer les conteneurs. Vérifier le refus des accès, la marge d’administration et la continuité des sessions autorisées.

## D09 Une restauration dont la portée est explicite

**Constat.** La première proposition associait une base par application à une récupération temporelle, sans distinguer leur portée. La [restauration physique PostgreSQL](https://www.postgresql.org/docs/18/continuous-archiving.html) porte sur le cluster entier.

**Décision.** Restaurer un cluster temporaire isolé, en extraire la base concernée puis valider sa réintégration. Conserver aussi objets et clés nécessaires. Suspendre la réémission des événements restaurés jusqu’au rapprochement des effets externes.

**Pourquoi et compromis.** Notre procédure préserve les autres applications. Elle demande du stockage, du calcul temporaire et une vérification de cohérence. Le délai de deux heures reste une cible à mesurer ; une récupération sélective peut prendre davantage de temps. Un archivage présent ne constitue pas une restauration réussie.

**Preuve attendue.** Restaurer une application contenant réservations et objets, tandis qu’une autre continue d’écrire. Vérifier son intégrité, les données de l’application non concernée, les effets externes et le temps réellement nécessaire.

## D10 Des économies mesurables et une comptabilité correcte

**Constat.** Attribuer un coût aux minutes de compilation puis additionner la facture mensuelle de la même VM risque de compter deux fois la dépense. Les caches et la télémétrie peuvent aussi consommer une part importante d’un petit budget.

**Décision.** Séparer dépenses variables et quote-part de coûts fixes, allouée une seule fois. Un cache dépend de toutes les entrées de construction pertinentes et reste cloisonné par projet. Les métriques utilisent des dimensions bornées ; les identifiants détaillés vont dans les traces et la comptabilité.

**Pourquoi et compromis.** [Prometheus rappelle](https://prometheus.io/docs/practices/instrumentation/) que chaque combinaison de labels consomme des ressources. Notre choix limite cette croissance et conserve un registre financier exhaustif. Des caches privés réduisent certaines possibilités de mutualisation ; leur coût doit être comparé au temps réellement économisé. L’enveloppe de 35 à 90 € mensuels reste une estimation d’infrastructure pour la petite bêta, hors taxes, inférence et exploitation humaine.

**Preuve attendue.** Rapprocher le total attribué des factures, publier la capacité inutilisée et mesurer les coûts avec et sans cache. Afficher préparation et entretien du catalogue séparément, puis plusieurs amortissements. Aucun pourcentage d’économie n’est annoncé avant le benchmark.

## Jalons de validation technique

| Jalon | Travail concerné | Preuve attendue |
| --- | --- | --- |
| Composition de bout en bout | AppSpec, assembleur, contrats, file durable et image candidate | Parcours fonctionnels et traçabilité sur les applications retenues |
| Cycle de vie complet | Reprises, canevas, mémoire, maintenance et publication | Continuité des données et comparatif Codex |
| Préparation de la bêta | Isolation, restauration, quotas, clés et observation externe | Essais de panne, de charge et de récupération |
| Évolution de la capacité | Ressources, moteurs spécialisés ou séparation de services | Limite identifiée et amélioration mesurée |

Ces jalons décrivent des dépendances et preuves techniques ; ils ne fixent ni une verticale unique ni une réduction du périmètre du hackathon. La maintenance reste testée hors production, les agents composent le catalogue validé et la configuration NVIDIA exigée est conservée.

Les décisions sont révisables sur preuve. Une proposition d’évolution de langage, de capacité, de permissions, de fournisseur ou de périmètre doit être présentée avec son statut, sa justification et son impact. Un ordre de construction suggéré ne devient pas une décision de réduire l’ambition du produit.

## D11 Le cloud fait partie du fonctionnement normal

**Décision du 2 octobre 2026.** Retirer la contrainte « tout local » issue du cadrage et reconnaître les appels cloud aux modèles NVIDIA comme un fonctionnement prévu. L’IDE peut être installé et le pilotage hébergé chez l’utilisateur ; cela n’impose ni modèle téléchargé ni GPU personnel. Les pages relues décrivaient déjà des API et une maintenance cloud : cette décision supprime l’ambiguïté de cadrage et lève toute interprétation qui interdirait ces échanges.

**Justification et compromis.** Le lieu d’exécution de l’IDE, celui des workers et celui du modèle sont indépendants. Une API implique l’envoi d’un contexte et une dépendance réseau ; la promesse de fonctionnement entièrement hors ligne est donc retirée. Une maintenance continue lorsque l’ordinateur est éteint demande un service hébergé. L’accès API est documenté par [NVIDIA](https://docs.api.nvidia.com/nim/docs/introduction) et par le [fournisseur candidat Nebius](https://docs.tokenfactory.nebius.com/quickstart) ; le fournisseur, le modèle exact et leur admissibilité au hackathon restent à vérifier.

**Ce qui reste exigé et preuve attendue.** Aucune collecte silencieuse du poste et aucun accès général des agents aux données métier de production. Tracer les catégories de données et destinations autorisées, vérifier que la clé reste côté serveur, puis interrompre l’API pour constater la suspension contrôlée des tâches agentiques.

## D12 Tous les blocs développés seront en Rust

**Décision.** Remplacer le socle C# de la première proposition par Rust pour le pilotage, l’assembleur et les blocs. Chaque bloc applicatif développé est implémenté en Rust, y compris les connecteurs et interfaces. Les interfaces applicatives retiennent Leptos avec rendu HTML et WebAssembly. L’enveloppe React et TypeScript de l’IDE reste distincte du catalogue et ne duplique pas les règles métier.

**Justification et compromis.** Cette décision suit l’exigence d’Augustin, permet de partager les contrats Rust et vise une consommation maîtrisée. Elle augmente le besoin de qualification de l’écosystème UI, de maîtrise de Rust et de suivi du temps de compilation. La [possession en Rust](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html) apporte des garanties mémoire ; elle ne prouve ni sécurité applicative complète ni supériorité de performance sur notre charge. [Leptos](https://book.leptos.dev/) fournit la voie retenue pour les interfaces Rust.

**Portée et preuve attendue.** Les ressources CSS, SQL et schémas, l’outillage et les services tiers ne sont pas réécrits. Un simple wrapper Rust autour de logique métier en C# ne respecte pas la décision. Vérifier une tranche complète Axum–SQLx–Leptos, les frontières serveur et navigateur, l’accessibilité, les tests et les mesures ; répercuter la même stack applicative dans le benchmark Codex.

## D13 La modularité est imposée par les contrats

**Décision.** Garder un monolithe modulaire en Rust, avec crates et responsabilités explicites, puis séparer les processus lorsque leurs droits ou leur charge l’exigent. Le domaine ne dépend pas du framework HTTP ou du pilote SQL. Les adaptateurs portent les détails externes ; les modules possèdent leurs écritures.

**Justification et compromis.** Des contrats internes et un graphe de dépendances acyclique rendent les changements vérifiables sans multiplier les transactions distribuées. Cette discipline demande des règles CI et des interfaces entretenues. Une crate ne protège pas contre du code malveillant dans le même processus ; construction, vérification et publication conservent donc leurs frontières de sécurité.

**Preuve attendue.** Vérifier les dépendances interdites, les contrats de modules et l’absence d’écriture SQL depuis un autre propriétaire. Démontrer qu’un adaptateur de modèle peut être remplacé sans modifier les règles métier. Une extraction en microservice doit répondre à une limite mesurée.

## D14 La sécurité par défaut devient un contrat d’admission

**Décision.** Accès et réseau non déclarés refusés, objets privés, paramètres validés, quotas finis, identités distinctes, secrets hors client et prompts, contrôle des permissions à chaque action. Les rôles attribués aux modèles ne confèrent aucun privilège technique implicite. Les propriétés dangereuses restent fermées dans un nouveau projet.

**Justification et compromis.** Les réglages initiaux doivent rendre le comportement sûr reproductible. Ce choix suit les principes de [refus par défaut et de vérification systématique d’OWASP](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html). Il exige parfois de configurer une connexion avant de l’utiliser ; une demande de capacité porte alors sur un besoin concret. Les protections du langage ne remplacent pas les contrôles d’accès, les contraintes transactionnelles ou le cloisonnement réseau.

**Preuve attendue.** Tester accès interclients, suppression d’un rôle pendant une session, SSRF, export interdit, appel d’outil hors contrat, entrée surdimensionnée et fuite dans l’hydratation. Les permissions critiques sont imposées par le serveur et vérifiées indépendamment de l’agent.

## D15 Les données envoyées aux API sont explicites

**Décision.** Ajouter une politique de données par projet et une passerelle de modèles. N’envoyer que le contexte utile autorisé ; exclure secrets et données de production par défaut. Distinguer demande fournie par le client, code et spécification du projet, diagnostics et données des utilisateurs finaux. Les journaux ordinaires ne conservent ni corps de requêtes ni prompts bruts.

**Justification et compromis.** Supprimer la contrainte locale ne justifie pas une collecte indifférenciée. La minimisation et des diagnostics synthétiques réduisent l’exposition mais peuvent rendre certains incidents plus difficiles à reproduire. Les filtres automatiques ne garantissent pas une anonymisation complète. Le choix de fournisseur et de région doit aussi tenir compte de ses conditions réelles de traitement ; aucun repli vers un destinataire non prévu.

**Preuve attendue.** Observer les requêtes de test sortantes, introduire un secret factice dans un contexte et vérifier le blocage sans copie dans les journaux. Valider rétention, purge et droits de diagnostic. La [documentation des endpoints Nebius](https://docs.tokenfactory.nebius.com/public-serverless) illustre pourquoi il faut vérifier la région effective.

## D16 Un catalogue étendu sans code improvisé

**Décision.** Créer un catalogue cible de **180 blocs Rust en 18 familles**, avec capacité et invariant particulier pour chaque bloc. Les recettes composent identité, données, règles, écrans, communications, recherche, réservation, commerce et opérations. Vingt blocs couvrent la fabrique et son exploitation. Le catalogue décrit le travail à construire ; il ne prétend pas être disponible.

**Justification et compromis.** Une couverture large vient de primitives combinables et de recettes testées. Elle demande maintenance, documentation et vérification de compatibilité. Le catalogue de 180 blocs soutient une plateforme généraliste. L’affirmation antérieure imposant un démarrage limité à la réservation est retirée : le périmètre du hackathon et l’ordre des familles à construire restent à définir avec Augustin. Une capacité effectivement manquante demeure signalée.

**Preuve attendue.** Relier chaque exigence d’une recette aux versions des blocs et à des tests de composition. Démontrer la réutilisation sur plusieurs recettes avant d’annoncer une couverture générale. Le coût d’entretien de la bibliothèque reste inclus dans l’analyse économique.

## D17 Versionner et révoquer les blocs de façon contrôlée

**Décision.** Enrichir les manifestes avec données, effets, capacités, cibles, migrations et budgets. Verrouiller la composition par application, conserver les versions d’événements et de workflows et gérer les états expérimental, validé, déprécié et révoqué. Une nouvelle version du catalogue ne met pas automatiquement à jour les applications publiées.

**Justification et compromis.** La modularité n’est utile que si l’on sait quelles applications dépendent d’un changement. L’inventaire permet de cibler une correction ; il demande un registre précis et des migrations explicites. Une signature prouve une provenance, pas une absence de faille. Les versions retirées et les artefacts nécessaires au rollback ont des politiques distinctes.

**Preuve attendue.** Refuser une composition incompatible, retrouver les applications affectées par une version vulnérable, bloquer une nouvelle construction avec un bloc révoqué, puis promouvoir une correction testée. Vérifier aussi qu’un workflow en cours conserve sa sémantique.

## D18 Mesurer la performance et la résilience de bout en bout

**Décision.** Remplacer toute promesse automatique d’ultraperformance par des scénarios de mesure et des budgets explicites. Suivre CPU, mémoire, débit, latences, taille WebAssembly, compilation, inférence et coût complet. Les parcours métier ordinaires restent indépendants de l’IA ; une fonction IA ajoutée à l’application déclare sa dépendance et son comportement en cas de panne.

**Justification et compromis.** Une amélioration de langage peut rester invisible si SQL, le réseau ou le modèle dominent le temps total. Les pools, files et reprises bornés préviennent une surcharge incontrôlée. La première bêta conserve ses limites de disponibilité ; un monolithe modulaire sur quelques machines ne constitue pas une architecture hautement disponible.

**Preuve attendue.** Mesurer avant et après sur les mêmes données et la même charge, provoquer saturation et panne du fournisseur, vérifier les parcours sans IA et la dégradation des fonctions IA. Les objectifs d’interface et le comparatif Codex restent ceux du produit, avec tous les coûts comptés.

Les familles et recettes de D16 sont détaillées dans le [catalogue des 180 blocs Rust](https://chatgpt.com/space/page_a34ccccf45d88191a9cf39578c465f43).

## D19 Préserver la vision généraliste et distinguer propositions et décisions

**Correction du 2 octobre 2026 à la demande d’Augustin.** La page d’idée porte la vision, les usages, l’expérience et le fonctionnement du produit. Les choix de stack, d’hébergement et les justifications techniques restent dans les pages qui leur sont consacrées.

**Périmètre.** Aucune décision de réduire le prototype à une seule verticale n’a été validée. La réservation demeure un exemple. Le projet vise une plateforme généraliste et un catalogue étendu permettant de construire un très grand nombre d’applications ; les scénarios du hackathon doivent être choisis au service de cette ambition.

**Origine de la correction.** L’assistant avait transformé une recommandation de séquencement en décision produit et inscrit cette restriction dans plusieurs pages. Cette prescription est retirée. Une recommandation doit rester identifiée comme telle ; un changement substantiel de périmètre ne doit pas être attribué à Augustin sans son accord.

**Exigence conservée.** Les capacités annoncées comme fonctionnelles doivent être vérifiées. Cette exigence de preuve ne justifie pas, à elle seule, de choisir ou de réduire le périmètre du projet.

## Revue des études et cohérence avant construction

**Polish du 2 octobre 2026, demandé par Augustin.** Les dix pages du projet ont été relues. Les améliorations suivantes précisent les contrats, la coordination, la preuve et la reprise ; elles conservent l’ambition généraliste, les 180 blocs Rust, les rôles de modèles, les API cloud et l’architecture existante. Les sources ci-dessous ont été recoupées, avec des publications jusqu’en septembre 2026. Leurs résultats, nos adaptations et les mesures futures sont distingués.

| Source primaire et version consultée | Résultat ou apport utile | Limite pour notre projet |
| --- | --- | --- |
| [Why Do Multi-Agent LLM Systems Fail? — v3, 26 octobre 2025](https://arxiv.org/abs/2503.13657v3) | MAST distingue défauts de conception, désalignements entre agents et problèmes de vérification ; les boucles et faux succès demandent un diagnostic explicite. | Taxonomie issue des systèmes étudiés, pas preuve que notre configuration fonctionne. |
| [Towards a Science of Scaling Agent Systems — v3, 8 avril 2026](https://arxiv.org/abs/2512.08296v3) | Sur 260 configurations et six benchmarks, le bénéfice de coordination dépend de la structure de la tâche ; les coûts de coordination et la propagation d’erreurs comptent. | Les effets et seuils mesurés ne se transposent pas automatiquement à nos blocs Rust ou modèles NVIDIA. |
| [Multi-Agent LLMs Fail to Explore Each Other — 13 juillet 2026](https://arxiv.org/abs/2607.11250v1) | Étudie la difficulté des agents à découvrir les capacités utiles de leurs partenaires. | Prépublication et environnements particuliers ; elle motive une qualification des rôles, pas l’adoption automatique de MACE. |
| [Developing LLM-based Multi-Agent Systems in Software Engineering — 12 août 2026](https://arxiv.org/abs/2608.11965v1) | Le retour d’expérience met en évidence les besoins de coordination et de télémétrie dans les frameworks. | La comparaison empirique porte sur des résumés de README ; elle ne départage pas les plateformes pour construire des applications. |
| [Agent-Integrated Software: Interaction Contracts and Continuous Assurance — 10 septembre 2026](https://arxiv.org/abs/2609.11381v1) | Formalise le lien entre objectif révisable, autorité, état applicatif et preuve du résultat. | Cadre conceptuel et propositions conditionnelles ; aucun gain de production démontré pour notre système. |
| [Anthropic — retour d’ingénierie, 13 juin 2025](https://www.anthropic.com/engineering/multi-agent-research-system) | Délégations explicites et contextes séparés servent les recherches parallélisables ; leur coût doit être mesuré. | Expérience de recherche documentaire interne, pas benchmark de notre fabrique logicielle. |

Les versions des articles sont fixées dans les liens : la révision d’avril 2026 de l’étude sur le passage à l’échelle ne doit pas être confondue avec ses premiers chiffres de décembre 2025. Les choix D20 à D23 sont nos adaptations de conception, pas des conclusions expérimentales sur notre produit.

## D20 Déléguer par contrat et paralléliser les travaux indépendants

**Décision précisée.** Chaque tâche relie objectif, version de départ, propriétaire, lectures, écritures, dépendances, capacités, budget et critères d’acceptation. L’ordonnanceur vérifie l’indépendance des changements sur les objets et invariants, au-delà des fichiers. Les quatre exécutants forment le pool initial ; l’activité effective suit les tâches prêtes et les ressources.

**Pourquoi et compromis.** Des contrats précis rendent les responsabilités inspectables ; des tâches trop petites peuvent pourtant coûter plus en coordination qu’elles n’économisent. Le serveur effectue directement les transformations calculables. Nous conservons une coordination centrale, sans ajouter de nouveau framework ou de rôle d’agent.

**Preuve attendue.** Faire traiter des tâches indépendantes en parallèle, puis injecter un conflit métier entre deux changements de fichiers distincts. Vérifier qu’il est détecté avant intégration ; comparer qualité, coût et durée à concurrence différente sur les mêmes cas.

## D21 Relier les messages à des preuves indépendantes

**Décision précisée.** Les messages contiennent changement proposé, preuves référencées, hypothèses, limites et diagnostic utile. La mémoire sert des contextes ciblés ; les éléments absents sont signalés. La revue et la sécurité examinent contrat et observations séparément du verdict constructeur. Le logiciel reste seul responsable de l’état validé.

**Pourquoi et compromis.** Cela réduit le risque qu’une assertion soit répétée jusqu’à être considérée comme un fait. Les références doivent rester accessibles et versionnées ; résumer trop agressivement peut supprimer une information décisive. Les messages entre agents restent des données non fiables et ne transmettent aucun privilège.

**Preuve attendue.** Soumettre un faux succès, une preuve absente, un contexte incomplet et une instruction malveillante transmise par un autre agent. Aucun de ces cas ne doit permettre une publication ou un élargissement de droits.

## D22 Respecter les changements de demande pendant l’exécution

**Décision précisée.** Relier tâches et résultats à une révision de l’objectif et de la politique. Une retouche ou une révocation suspend les admissions concernées et invalide les résultats incompatibles. Préserver les travaux toujours valables, replanifier localement et suivre les effets déjà engagés.

**Pourquoi et compromis.** Conversation et canevas pilotent le même projet. Une vérification correcte hier peut ne plus autoriser l’action aujourd’hui. Le coût vient du suivi des dépendances et des validations à refaire ; l’annulation ne garantit pas le retour en arrière d’une action externe.

**Preuve attendue.** Modifier une exigence et retirer un droit pendant un appel en cours ; refuser une intégration obsolète et distinguer clairement arrêt demandé, nouvelles actions bloquées et résultat externe encore inconnu.

## D23 Mesurer les échecs et arrêter les reprises sans progrès

**Décision précisée.** Qualifier chaque rôle–modèle–endpoint sur les contrats réels avant la campagne. Séparer ce réglage des briefs d’évaluation. Tracer les échecs de contrat, pertes de contexte, doublons, conflits, erreurs de contrôle et faux succès. Une reprise identique sans élément nouveau est suspendue pour un arbitrage borné.

**Pourquoi et compromis.** Un taux de réussite seul explique mal un échec. La classification aide à cibler les corrections, mais un diagnostic produit par un modèle reste une hypothèse. Les métriques n’imposent aucun changement de modèle ou de nombre d’agents : la répartition demandée reste la configuration principale.

**Preuve attendue.** Rejouer des erreurs connues, observer leur cause et leur coût total, puis comparer les mêmes cas avant et après correction. Inclure les blocages et toutes les répétitions ; publier la dispersion et les frais de coordination, sans retenir seulement les meilleurs essais.

**Corrections documentaires associées.** Les anciennes mentions de deux exécutants, de code complémentaire improvisé et de générateur limité à une spécialité sont remplacées dans les pages de référence. Le calendrier et le règlement du hackathon concordent désormais sur un jugement du 1er au 15 décembre 2026. Les valeurs Nebius non reconfirmées pendant cette revue restent étiquetées comme relevés antérieurs, sans servir de devis ni de choix automatique. D19 continue de gouverner la distinction entre vision, proposition et décision.