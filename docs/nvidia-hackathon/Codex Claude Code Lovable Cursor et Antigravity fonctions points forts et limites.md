Inventaire des capacités publiques de **Codex**, **Claude Code**, **Lovable**, **Cursor** et **Google Antigravity**, pour comparer notre idée d’IDE fondé sur des composants préconstruits. Sources consultées le **2 octobre 2026**.

Ces cinq produits peuvent produire du logiciel à partir d’une demande. Leur différence tient surtout à l’environnement, à l’autonomie, aux outils de validation et à la prise en charge de la publication. Les fonctions varient selon la surface, le système, le plan et les intégrations. Ce panorama couvre les familles documentées ; les extensions rendent impossible un catalogue fini de toutes les tâches réalisables.

Les **points forts**, **limites** et conclusions pour notre projet sont des analyses de ces caractéristiques, sans classement de qualité ni benchmark réalisé. Un test exécuté ou une démo visuelle ne garantit pas que toutes les exigences d’une application sont satisfaites.

## Comparaison rapide

| Produit | Centre de gravité | Code et environnement | Mise en ligne |
| - | - | - | - |
| Codex | Agent de développement et automatisation de travail | Terminal, extension IDE, bureau et cloud | Via outils, plugins ou infrastructure configurée |
| Claude Code | Agent composable pour travailler dans des projets | Terminal, IDE, bureau, web et cloud | Via commandes, skills et services configurés |
| Lovable | Construction d’applications web par conversation | Projet géré, aperçu visuel, code exportable | Hébergement et backend intégrés |
| Cursor | Éditeur et plateforme d’agents pour développeurs | Éditeur, CLI et agents cloud | Via environnement de développement et services |
| Antigravity | Plateforme de développement organisée autour des agents | Antigravity 2.0, IDE, CLI et SDK | Via outils et services configurés |

**Analyse :** Lovable est le concurrent le plus directement comparable à notre parcours « demande → application publiée ». Les quatre autres sont aussi capables de créer et déployer des applications lorsque leurs environnements et accès le permettent. La différence n’est donc pas une impossibilité technique, mais le degré d’intégration et de configuration.

Sources : [Codex CLI](https://learn.chatgpt.com/docs/codex/cli), [Claude Code](https://code.claude.com/docs/en/overview), [Lovable](https://docs.lovable.dev/introduction/welcome), [Cursor](https://cursor.com/docs), [Antigravity](https://antigravity.google/docs/ide/overview/).

## Codex

Codex désigne ici le produit agentique actuel d’OpenAI, et non le modèle historique de 2021. Il inspecte un projet, modifie des fichiers, utilise des outils et vérifie son travail. Son périmètre s’étend aussi aux tâches de connaissance via skills et plugins.

### Fonctions documentées

| Domaine | Ce qu’il permet |
| - | - |
| Compréhension | Explorer un dépôt, expliquer le code et localiser les parties pertinentes |
| Implémentation | Créer fonctionnalités et applications, modifier plusieurs fichiers, refactoriser |
| Débogage | Examiner erreurs, logs et échecs ; proposer et appliquer des corrections |
| Validation | Exécuter commandes et tests disponibles dans l’environnement |
| Revue | Examiner changements non commités, commits ou différences avec une branche |
| Git et cloud | Soumettre une tâche et récupérer ses changements ; environnements réutilisables, espaces isolés et poursuite ordinateur éteint |
| Parallélisme | Utiliser sous-agents et sessions distinctes ; worktrees pour isoler les modifications |
| Personnalisation | Consignes AGENTS.md, skills, plugins, configuration et règles |
| Intégrations | MCP et outils externes ; capacités selon plugins installés |
| Contextes | Images, recherche web, historique et reprise de sessions |
| Navigateur et écran | Tester des interfaces ; navigateur intégré et contrôle d’applications sur les surfaces compatibles |
| Automatisation | Exécution non interactive, scripts, CI et tâches planifiées selon surface |
| Livraison élargie | Déploiement, documents ou autres livrables via outils et skills adaptés |
| Supervision | Examiner diffs, commandes, permissions et environnement autorisé |

Sources : [CLI et capacités](https://learn.chatgpt.com/docs/codex/cli), [application et worktrees](https://openai.com/index/introducing-the-codex-app/), [skills](https://learn.chatgpt.com/docs/build-skills), [usage au-delà du code](https://openai.com/index/codex-for-knowledge-work/), [cloud actuel](https://learn.chatgpt.com/docs/environments/cloud-environments), [navigateur](https://learn.chatgpt.com/docs/browser), [Computer Use](https://learn.chatgpt.com/docs/computer-use).

**Différence de surface :** le navigateur intégré n’est pas fourni dans le CLI ou l’extension IDE. Computer Use exige le plugin, des permissions et une région compatible. Sur Windows, il utilise le bureau actif ; ce n’est pas un contrôle invisible en arrière-plan.

### Points forts

- **Travail dans un vrai projet** : l’agent utilise les fichiers et outils existants.

- **Délégation et isolation** : sessions et worktrees facilitent plusieurs travaux simultanés.

- **Procédures partageables** : skills et plugins permettent de formaliser nos conventions.

- **Passage entre surfaces** : terminal, IDE et cloud couvrent plusieurs styles de travail.

### Limites

- **Environnement déterminant** : dépendances, accès et tests doivent être exploitables pour vérifier le résultat.

- **Publication à intégrer** : une commande de déploiement ou un plugin doit disposer des bons comptes et configurations.

- **Consommation variable** : exploration, sous-agents et reprises peuvent accroître le budget ; le travail parallèle n’est pas gratuit.

- **Autonomie à vérifier** : permissions et sandboxing encadrent les actions, mais ne prouvent pas la qualité métier du logiciel.

**Pour notre idée — analyse :** « des agents qui écrivent, testent et déploient » existe déjà. L’apport doit se démontrer par la sélection de composants, la réduction des décisions et le coût par application validée.

## Claude Code

Claude Code est l’agent de développement d’Anthropic. Il associe compréhension du dépôt, édition et exécution d’outils, avec des mécanismes de personnalisation et d’automatisation.

### Fonctions documentées

| Domaine | Ce qu’il permet |
| - | - |
| Compréhension | Examiner le dépôt, suivre une erreur et planifier une modification |
| Implémentation | Écrire fonctionnalités, corriger bugs et refactoriser plusieurs fichiers |
| Maintenance | Tests, lint, dépendances, conflits de fusion et notes de version |
| Git | Préparer commits et branches, ouvrir des pull requests |
| Vérification visuelle | Aperçu d’application, navigateur, captures, interactions et appels backend |
| Développement mobile | Contrôle et tests dans le simulateur iOS depuis le bureau, selon environnement |
| Mémoire | CLAUDE.md et mémoire automatique de connaissances de projet |
| Procédures | Skills, plugins et hooks avant ou après les actions |
| Services externes | MCP, connecteurs et workflows GitHub, GitLab ou Slack |
| Plusieurs agents | Sous-agents, coordination et sessions en parallèle |
| Automatisation | CLI non interactive, CI, routines cloud, tâches de bureau et boucles de session |
| Mobilité | Remote Control, accès web/mobile et passage entre sessions selon surface |
| Environnements | Travail local, cloud, SSH ou WSL selon configuration |
| Personnalisation avancée | Agent SDK pour construire un agent et contrôler ses outils |

Sources : [vue d’ensemble](https://code.claude.com/docs/en/overview), [bureau et fonctions par environnement](https://code.claude.com/docs/en/desktop), [skills](https://code.claude.com/docs/en/skills).

### Points forts

- **Composabilité** : terminal, scripts, hooks et SDK s’intègrent à une chaîne de développement.

- **Personnalisation structurée** : consignes de projet, mémoire et skills capitalisent les façons de travailler.

- **Validation proche du produit** : aperçu et navigateur complètent les vérifications de code.

- **Travail continu** : routines cloud et contrôle à distance facilitent la délégation durable.

### Limites

- **Configuration technique** : tests, outils, services et publication doivent être correctement préparés.

- **Différences entre surfaces** : certaines fonctions et permissions ne sont pas identiques dans le terminal, le bureau et le cloud.

- **Budget à suivre** : contexte, longues sessions et plusieurs agents multiplient potentiellement la consommation.

- **Validation toujours nécessaire** : un résultat convaincant en aperçu peut conserver des défauts d’autorisation, de données ou de logique métier.

**Pour notre idée — analyse :** la coordination et les procédures réutilisables sont déjà présentes. Notre hypothèse porte sur la qualité et le coût d’un assemblage généraliste à partir de blocs validés, avec des critères identiques pour le comparer à une construction libre.

## Lovable

Lovable est une plateforme gérée de construction et publication d’applications web par langage naturel. Elle réunit l’agent, l’aperçu, le code, un backend et l’hébergement. Les nouvelles applications utilisent TanStack Start depuis le 13 mai 2026 ; les anciennes applications React et Vite restent documentées séparément.

### Fonctions documentées

| Domaine | Ce qu’il permet |
| - | - |
| Cadrage | Chat pour discuter, Plan pour définir une approche, Build pour réaliser |
| Construction | Générer et modifier frontend, backend et configuration |
| Interface | Aperçu, édition visuelle, styles, pages et composants |
| Débogage | Examiner erreurs de build, logs et requêtes ; corriger |
| Tests | Navigateur, tests frontend et vérification de fonctions backend |
| Données | Base de données, migrations, stockage et temps réel via Cloud |
| Utilisateurs | Authentification et configuration des accès |
| Logique serveur | Fonctions, secrets et tâches backend |
| IA et médias | Capacités IA de l’application et production d’assets via outils disponibles |
| Services | Connecteurs, paiements, email et intégration d’API |
| Comptes connectés | Connexion partagée, personnelle pour le contexte, ou propre à chaque utilisateur final |
| Publication | URL hébergée, HTTPS, domaine personnalisé et mises à jour publiées |
| Sécurité | Scans Quick et Deep ; politiques de publication selon configuration |
| Collaboration | Partage, commentaires, permissions et réutilisation de projets |
| Code et portabilité | Synchronisation GitHub, code possédé par le créateur et hébergement externe |
| Suivi | Diffs, activité, crédits, métadonnées et outils de croissance documentés |

Sources : [présentation](https://docs.lovable.dev/introduction/welcome), [Build mode](https://docs.lovable.dev/features/agent-mode), [Cloud](https://docs.lovable.dev/features/cloud), [connecteurs](https://docs.lovable.dev/integrations/introduction), [publication](https://docs.lovable.dev/features/publish), [sécurité](https://docs.lovable.dev/features/security), [portabilité](https://docs.lovable.dev/tips-tricks/deployment-hosting-ownership).

### Points forts

- **Parcours intégré** : construction, base, auth et publication sont réunies.

- **Accessible sans terminal** : conversation et aperçu permettent d’itérer directement sur le produit.

- **Infrastructure réduite** : hébergement, HTTPS et backend géré diminuent le travail initial.

- **Code récupérable** : Git sync et hébergement externe permettent de poursuivre ailleurs.

### Limites

- **Périmètre centré sur le web** : ce parcours est moins directement adapté à un moteur système ou à un développement natif spécialisé.

- **Coûts distincts** : construction de l’app, backend, IA à l’exécution et services externes doivent être budgétés.

- **Sortie de plateforme à préparer** : exporter le code ne migre pas automatiquement auth, données, stockage et fonctions. L’éditeur et l’agent Lovable ne sont pas auto-hébergeables.

- **Vérification à demander** : la documentation précise que la plupart des outils de test s’exécutent sur demande. Les scans de sécurité ne remplacent pas une revue complète.

Sources pour ces limites : [Build mode et vérification](https://docs.lovable.dev/features/agent-mode), [migration et propriété](https://docs.lovable.dev/tips-tricks/deployment-hosting-ownership), [sécurité](https://docs.lovable.dev/features/security).

**Pour notre idée — analyse :** Lovable constitue une référence pour le parcours de création d’applications web. Comparer plusieurs familles et leurs évolutions permettra de tester la réutilisation de nos primitives ; un seul exemple de restaurant ne suffit pas à établir l’avantage général.

## Cursor

Cursor associe un environnement de programmation, plusieurs modèles et des agents locaux ou cloud. Il conserve un parcours de développeur, tout en permettant de déléguer des tâches plus longues.

### Fonctions documentées

| Domaine | Ce qu’il permet |
| - | - |
| Édition assistée | Tab prédit les prochaines modifications, suggère plusieurs lignes et des passages entre fichiers |
| Compréhension | Recherche de fichiers, lecture de code et analyse du dépôt |
| Planification | Définir une approche ; projets avec coordinateur et délégation |
| Implémentation | Modifier plusieurs fichiers et créer des fonctionnalités |
| Exécution | Commandes terminal, tests et observation des résultats |
| Recherche | Recherche web et ajout de contexte externe |
| Navigateur | Interagir avec l’application, capturer des pages et vérifier des changements |
| Images | Lire des références visuelles et générer des assets |
| Personnalisation | Rules, skills, plugins, hooks, MCP et sous-agents |
| Reprise | Checkpoints de fichiers, recherche de conversations et suivi de tâches |
| Cloud | Agents en VM isolée, tâches en parallèle et environnements multi-dépôts |
| Livraison | Branches et PR avec captures, vidéos ou logs ; bureau distant inspectable |
| Intégrations | Dépôts, Slack, Linear, API et autres outils du workflow |
| Revue et automatisation | Bugbot et automatisations documentés dans l’offre |
| CLI | Usage terminal et workflows non interactifs ou CI |

Sources : [Tab](https://cursor.com/en-US/tab), [documentation](https://cursor.com/docs), [agent et outils](https://cursor.com/docs/agent/overview), [agents cloud](https://cursor.com/docs/cloud-agent).

### Points forts

- **Combinaison manuel et agent** : le développeur peut inspecter et reprendre le code directement.

- **Choix de modèles** : plusieurs familles de modèles, avec orchestration adaptée.

- **Cloud et preuves de travail** : PR, logs, captures et vidéos facilitent la revue.

- **Contexte de plusieurs dépôts** : utile quand frontend, backend et infrastructure sont séparés.

### Limites

- **Préparation du cloud** : dépôts, dépendances, secrets et réseau doivent être disponibles pour fermer la boucle de validation.

- **Facturation variable** : les agents cloud sont facturés au tarif API du modèle choisi ; un contexte plus large peut augmenter la consommation.

- **Différences local et cloud** : modèles sélectionnés, hooks et capacités ne sont pas tous identiques.

- **Reprises limitées aux fichiers** : les checkpoints ne constituent pas une annulation des effets externes ni un remplacement de Git.

Sources : [cloud et facturation](https://cursor.com/docs/cloud-agent), [checkpoints](https://cursor.com/docs/agent/overview).

**Pour notre idée — analyse :** Cursor montre que parallélisme, environnement préconstruit et validation visuelle sont déjà courants. Notre différence devrait être dans les composants métier assemblés et les garanties vérifiables.

## Google Antigravity

Google Antigravity comprend plusieurs surfaces. Il faut distinguer **Antigravity 2.0**, **Antigravity IDE**, le **CLI** et le **SDK** : leurs capacités et leur prise en charge ne sont pas identiques.

### Fonctions documentées

| Domaine | Ce qu’il permet | Surface ou condition |
| - | - | - |
| Développement | Planifier, écrire, déboguer et vérifier du logiciel | Agents et environnement préparé |
| Éditeur | IDE et complétion Tab | Antigravity IDE |
| Plusieurs agents | Agents asynchrones et sous-agents parallèles | Selon surface |
| Projets | Accès à plusieurs dossiers et worktrees isolés | Antigravity 2.0 |
| Navigateur | Lire et interagir avec pages et applications ; tests UI | Intégration navigateur |
| Preuves | Plans, diffs, diagrammes, images et enregistrements de navigateur | Artifacts de l’IDE |
| Terminal et Git | Terminal intégré et panneau de revue Git | Antigravity 2.0 |
| Planification | Déclenchements périodiques de conversations | Tâches planifiées |
| Voix | Dictée et transcription de consignes | Surfaces prises en charge |
| Contrôle distant | Suivre et piloter des sessions depuis un navigateur | Remote Control |
| Personnalisation | Rules, skills, MCP, sous-agents et hooks | Fonctions variant selon surface |
| SDK | Agents personnalisés, outils, politiques, sous-agents et sorties structurées | SDK et configuration |
| Permissions | Accès borné au projet, sandbox et approbations | OS et réglages choisis |

Sources : [Antigravity IDE](https://antigravity.google/docs/ide/overview/), [fonctions Antigravity 2.0](https://www.antigravity.google/docs/features), [documentation et SDK](https://www.antigravity.google/docs/home).

### Points forts

- **Boucle code et navigateur** : l’agent peut vérifier une interface dans son environnement.

- **Livrables inspectables** : plans, diffs et enregistrements aident à suivre les changements.

- **Organisation autour des agents** : projets, worktrees et travail asynchrone facilitent la délégation.

- **Contrôles par projet** : droits et paramètres peuvent être adaptés aux dossiers concernés.

### Limites

- **Offre à distinguer par surface** : l’IDE n’est pas pris en charge pour les clients Enterprise ; Google renvoie vers Antigravity 2.0 ou CLI.

- **Quotas et modèles variables** : disponibilité selon plan, capacité et consommation ; limites susceptibles d’évoluer.

- **Publication à configurer** : l’agent doit disposer des services et comptes nécessaires pour déployer.

- **Preuves partielles** : un enregistrement du navigateur montre un parcours testé, sans démontrer l’absence de défauts sur les autres parcours.

Sources : [prise en charge Enterprise](https://antigravity.google/docs/ide/overview/), [plans et quotas](https://antigravity.google/docs/plans).

**Pour notre idée — analyse :** le parcours « planifier → coder → lancer → tester dans le navigateur » est déjà couvert. Une démonstration d’assemblage fiable et économique apporterait une différence plus précise.

## Ce que nous devons mesurer pour nous distinguer

**Analyse liée à nos premières notes :** aucune des fonctions « multi-agent », « création par prompt », « composants », « tests » ou « déploiement » ne suffit isolément à démontrer une nouveauté. Ces offres sont néanmoins différentes, et leur existence ne réfute pas notre hypothèse d’assemblage.

| Notre hypothèse | Preuve utile |
| - | - |
| Des primitives diminuent le travail du modèle | Tokens et appels par application réussie, face à un agent de référence |
| Des choix bornés augmentent la fiabilité | Même série d’exigences et contrôles exécutés sur chaque application |
| Des modèles légers suffisent sur ce périmètre | Taux de réussite et qualité au même budget, avec reprises incluses |
| Une mise en ligne peut demander peu d’interventions | Temps total et interventions humaines, domaines et comptes compris |
| Les blocs testés sécurisent l’assemblage | Contrôles des permissions et des interfaces après composition |

Une comparaison équitable doit inclure le travail de préparation des primitives, puis distinguer ce coût initial du coût marginal de création. Pour les applications répétitives, le gain peut venir de cette préparation ; pour des demandes nouvelles, elle peut limiter le périmètre pris en charge. Figer la surface, le modèle, les réglages, l’environnement et la date du concurrent testé. Distinguer le duel entre produits complets des ablations entre modèles : ni leurs coûts ni leurs scores ne sont interchangeables.

**Positionnement retenu : une plateforme généraliste qui compose un catalogue extensible de blocs validés et maintient les applications produites.** La diversité vient des compositions et de l’enrichissement contrôlé du catalogue. Une capacité manquante est signalée explicitement. L’avantage revendiqué doit être établi sur qualité, coût complet, vitesse de livraison et réussite des évolutions, sans présumer de la supériorité de notre système.
