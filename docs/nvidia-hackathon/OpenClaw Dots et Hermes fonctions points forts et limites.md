Inventaire des principales capacités publiques de **OpenClaw**, **Dots d’OpenAI** et **Hermes Agent de Nous Research**, pour situer notre projet de hackathon. Sources consultées le **2 octobre 2026**.

Ces produits associent un modèle, des outils, de la mémoire et une organisation du travail. Leur périmètre est extensible : « tout ce qu’ils font » désigne ici les familles de fonctions documentées, sans prétendre énumérer chaque plugin ni garantir la réussite de toute tâche. Une fonction disponible exige parfois un fournisseur, un compte connecté, une permission ou une configuration.

Les rubriques **Points forts** et **Limites** sont notre analyse des caractéristiques documentées, sans benchmark comparatif réalisé. « Apprendre » désigne généralement mémoriser et réutiliser des procédures ; cela ne signifie pas réentraîner automatiquement les poids du modèle.

## Comparaison rapide

| Produit | Fonction centrale | Exécution | Modèles et personnalisation |
| - | - | - | - |
| OpenClaw | Assistant connecté à de nombreux canaux, avec une passerelle qui organise outils et agents | Machine ou serveur choisi par l’utilisateur | Plusieurs fournisseurs, modèles locaux et plugins |
| Dots | Assistant personnel persistant qui suit des objectifs et coordonne du travail | Ordinateur cloud géré par OpenAI ; accès local facultatif | GPT 6 Astra ; règles et plugins dans ChatGPT |
| Hermes Agent | Assistant généraliste avec mémoire et procédures réutilisables | Installation personnelle ; offre gérée via Nous Portal | Plusieurs fournisseurs, outils et plugins |

Sources : [OpenClaw](https://docs.openclaw.ai/concepts/features), [Dots](https://openai.com/index/introducing-dots/), [Hermes](https://hermes-agent.nousresearch.com/docs/user-guide/features/overview/).

## OpenClaw

OpenClaw est une infrastructure d’assistant open source : une passerelle relie conversations, modèles, outils et environnements d’exécution. Elle peut être installée sur une machine personnelle ou un serveur. Ce n’est pas un modèle d’IA en soi.

### Fonctions documentées

| Domaine | Ce qu’il permet | Conditions et portée |
| - | - | - |
| Conversation | Dialoguer en direct ou dans des groupes ; transmettre réponses et pièces jointes | Canaux connectés et droits configurés |
| Messageries | Telegram, WebChat, Discord, Slack, Teams, WhatsApp, Signal, iMessage et autres | Plusieurs canaux passent par des plugins officiels |
| Recherche | Recherche web et consultation de sources | Fournisseur de recherche configuré |
| Navigateur | Naviguer, cliquer, remplir des formulaires, capturer des pages | Profil contrôlé et permissions ; restrictions propres aux sites |
| Code et fichiers | Exécuter des commandes, modifier des fichiers, lancer des scripts | Outils autorisés et environnement d’exécution |
| Mémoire | Conserver du contexte, rechercher des souvenirs et personnaliser l’assistant | Moteur de mémoire et règles de conservation |
| Plusieurs agents | Router des conversations vers différents agents avec sessions isolées | Configuration des agents et espaces |
| Automatisation | Tâches planifiées, vérifications périodiques et workflows | Passerelle active ; déclencheurs configurés |
| Extensions | Ajouter skills, plugins, hooks et pipelines de travail | Installation et configuration nécessaires |
| Multimédia | Images, audio, vidéo, documents, transcription et synthèse vocale ; génération d’images ou de vidéos | Modèles et fournisseurs adaptés |
| Appareils | Interfaces web, compagnon macOS et nœuds mobiles | Appairage ; caméra, écran ou localisation selon permissions |

Sources : [catalogue fonctionnel](https://docs.openclaw.ai/concepts/features), [navigateur](https://docs.openclaw.ai/tools/browser), [mémoire](https://docs.openclaw.ai/concepts/memory), [documentation](https://docs.openclaw.ai/).

### Points forts

- **Contrôle du système** : code ouvert, hébergement choisi et logique modifiable.

- **Souplesse des modèles** : possibilité d’utiliser différents fournisseurs ou un endpoint compatible auto-hébergé.

- **Nombreux points d’entrée** : adapté à un assistant accessible depuis les outils de communication existants.

- **Architecture réutilisable** : mémoire, routage, automatisations et extensions évitent de reconstruire une passerelle complète.

### Limites

- **Exploitation à assurer** : installation, disponibilité, mises à jour et connexions demandent du travail.

- **Coût réel variable** : logiciel ouvert ne signifie pas inférence, serveur et outils externes gratuits.

- **Qualité dépendante du modèle** : l’infrastructure seule ne rend pas un petit modèle fiable sur une tâche complexe.

- **Confinement à configurer** : la documentation indique que le sandboxing est désactivé par défaut ; les plugins s’exécutent dans le processus et ne constituent pas une frontière d’isolation.

Les deux derniers points de configuration sont décrits dans la [comparaison d’architecture publiée par OpenClaw](https://docs.openclaw.ai/start/why-openclaw/openclaw-and-hermes-agent). Cette source décrit aussi un concurrent : elle n’est pas un benchmark indépendant.

**Lecture pour notre projet — analyse :** une passerelle multi-modèles seule serait peu distinctive. Notre hypothèse porte sur la création et la maintenance de nombreuses familles d’applications par composition, avec des résultats vérifiés et un coût complet mesuré.

## Dots

Dots est le service d’agents personnels persistants d’OpenAI, annoncé le 29 septembre 2026. Un dot suit des objectifs dans la durée, garde du contexte et dispose de son propre ordinateur cloud.

### Fonctions documentées

| Domaine | Ce qu’il permet | Conditions et portée |
| - | - | - |
| Suivi de projets | Continuer des tâches entre les conversations et suivre plusieurs projets | Travail autorisé, limites du plan |
| Personnalisation | Retenir préférences, retours et contexte de travail | Mémoire et données accessibles |
| Proactivité | Examiner les sources connectées pour repérer des évolutions utiles | Recherche proactive limitée à la lecture |
| Ordinateur cloud | Navigateur, code, outils, analyses et création de fichiers | Environnement géré par OpenAI |
| Applications | Lire ou agir dans des services via plugins | Connexion et droits accordés ; catalogue ne signifie pas accès universel |
| Développement | Suivre des retours, préparer correctifs, tests et PR ; créer des tâches Codex | Dépôts et environnements configurés |
| Travail documentaire | Préparer ou actualiser analyses, propositions, contenus et supports | Outils et données nécessaires |
| Planification | Rappels et vérifications récurrentes | Tâches programmées et notifications |
| Communication | Échanges dans ChatGPT, Slack et Teams ; conversation vocale | Disponibilité selon canal et compte |
| Accès local | Utiliser fichiers, outils, skills ou navigateur de la machine connectée | Facultatif, désactivé initialement |
| Supervision | Voir l’activité, ouvrir son ordinateur, corriger ou arrêter le travail | Interfaces de suivi |
| Autorisations | Custom Rules et vérification Auto-review des actions | Protections obligatoires non désactivables |

Sources : [présentation officielle](https://openai.com/index/introducing-dots/), [démarrage et fonctions](https://help.openai.com/en/articles/20001530-getting-started-with-your-dot), [contrôles et sécurité](https://openai.com/index/how-we-build-safety-security-and-privacy-into-dots/).

### Points forts

- **Continuité** : une responsabilité peut se poursuivre sans ouvrir une nouvelle conversation pour chaque étape.

- **Infrastructure gérée** : ordinateur cloud et coordination sont fournis par le service.

- **Intégration** : accès aux plugins, à Codex et à ChatGPT Work.

- **Contrôle visible** : activité, règles et examen séparé des actions facilitent la supervision.

### Limites

- **Disponibilité en France** : au 2 octobre, le lancement Pro exclut l’EEE, la Suisse et le Royaume-Uni. Business Premium est annoncé dans les régions ChatGPT prises en charge ; Enterprise, Edu et Healthcare passent par une bêta activée par l’administrateur. Le déploiement reste progressif.

- **Autonomie encadrée** : la recherche proactive ne peut directement envoyer de messages, modifier les applications ou contrôler un navigateur. Certaines actions restent soumises à approbation ou à une intervention humaine.

- **Mémoire peu granulaire** : le centre d’aide indique qu’on ne peut actuellement consulter, modifier ou supprimer individuellement les souvenirs propres du dot. Déconnecter une application n’efface pas son contexte déjà acquis.

- **Budget et dépendance** : service géré par OpenAI ; les tâches déléguées à Codex ou Work consomment leurs limites habituelles. Le premier dot inclus ne signifie pas travail profond illimité.

Sources : [accès et limites](https://help.openai.com/en/articles/20001530-getting-started-with-your-dot), [mémoire et autorisations](https://help.openai.com/en/articles/20001529-dots-privacy-security-and-safety-faqs).

### Fonctions annoncées ou limitées au lancement

Les équipes de plusieurs dots sont présentées comme une évolution future. Les dots spécialistes d’entreprise sont en pilotes ciblés. Le SMS est décrit par l’aide comme une bêta limitée à certains comptes Pro aux États-Unis. Un dot ne peut pas initier un appel vocal vers l’utilisateur au lancement, ni recevoir sa propre adresse email autonome.

**Lecture pour notre projet — analyse :** la continuité d’un assistant n’est pas une nouveauté suffisante. Nous voulons l’appliquer à tout le cycle de vie d’applications variées, avec une mémoire traçable, un canevas et des critères de publication vérifiables.

## Hermes Agent

Hermes Agent est l’assistant open source de Nous Research. Il met l’accent sur la mémoire persistante et les skills que l’agent peut créer ou améliorer à partir de son expérience. Ces skills sont des procédures et ressources réutilisables, pas un nouvel entraînement du modèle.

### Fonctions documentées

| Domaine | Ce qu’il permet | Conditions et portée |
| - | - | - |
| Interfaces | Terminal, interface de bureau, messageries et intégration dans des éditeurs | Surface et intégration configurées |
| Recherche et web | Chercher, consulter des pages et automatiser un navigateur | Fournisseur ou backend local/cloud |
| Code et fichiers | Commandes terminal, édition, scripts et exécution Python appelant les outils | Backend et outils autorisés |
| Mémoire | Retenir préférences, environnement et projets ; utiliser l’historique | Mémoire bornée et backends facultatifs |
| Skills | Charger, créer, réutiliser et améliorer des procédures | Documents et scripts ; validation de leur qualité nécessaire |
| Contexte de projet | Charger fichiers de consignes et références de fichiers, dossiers, URLs ou diffs | Fichiers présents et accessibles |
| Délégation | Créer des sous-agents avec contexte et outils restreints | Concurrence configurable ; l’isolation Git par worktree est une option |
| Planification | Tâches cron, résultats livrés sur un canal, pause et reprise | Agent actif ou hébergement géré |
| Traitement en lot | Exécuter de nombreux prompts ; capturer des trajectoires pour évaluation | Ressources et budget disponibles |
| Multimédia | Vision, génération d’images, voix, transcription et synthèse vocale | Modèles et fournisseurs compatibles |
| Intégrations | MCP, API compatible OpenAI et intégration IDE via ACP | Serveurs et clients configurés |
| Fournisseurs | Routage, repli, pools de clés et cache selon fournisseur | Options dépendantes du service utilisé |
| Extensions | Plugins, hooks, mémoire externe, personnalité et thèmes | Installation et configuration |
| Reprise | Checkpoints de fichiers et rollback | Ne remplace pas l’annulation d’une action externe |

Sources : [fonctions](https://hermes-agent.nousresearch.com/docs/user-guide/features/overview/), [outils](https://hermes-agent.nousresearch.com/docs/user-guide/features/tools/), [skills](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills), [délégation](https://hermes-agent.nousresearch.com/docs/user-guide/features/delegation).

### Points forts

- **Procédures capitalisées** : une tâche répétée peut devenir un skill plutôt qu’être redécouverte à chaque fois.

- **Contrôle et extensibilité** : code ouvert, fournisseurs multiples, plugins et mémoire configurable.

- **Bon socle d’expérimentation** : délégation, exécution Python et traitement en lot conviennent à des workflows évaluables.

- **Plusieurs modes d’exploitation** : installation personnelle ou services Nous Portal pour simplifier certains outils.

### Limites

- **Apprentissage à contrôler** : mémoriser une mauvaise procédure peut la reproduire ; « self-improving » n’est pas une preuve de progrès.

- **Configuration nécessaire** : disponibilité permanente, credentials, outils et coûts restent à organiser selon le mode choisi.

- **Services séparés** : modèles, recherche, navigateur et médias peuvent nécessiter des abonnements ou des clés ; le Tool Gateway de Nous Portal est payant.

- **Mémoire bornée et reprises partielles** : conservation de contexte et snapshots de fichiers n’équivalent pas à une mémoire parfaite ou à l’annulation des changements effectués dans des services tiers. Les sous-agents partagent le dossier de travail par défaut ; l’[isolation par worktree](https://hermes-agent.nousresearch.com/docs/user-guide/features/delegation#worktree-isolation) doit être activée pour séparer les modifications. Des contextes de conversation distincts ne constituent pas une isolation des fichiers ou des droits.

Sources : [site officiel et modes d’usage](https://hermes-agent.nousresearch.com/), [Tool Gateway](https://hermes-agent.nousresearch.com/docs/user-guide/features/tool-gateway).

**Lecture pour notre projet — analyse :** Hermes fournit déjà mémoire, skills et sous-agents. Notre hypothèse doit porter sur une organisation qui obtient un résultat plus fiable ou moins coûteux, démonstration à l’appui.

## Ce que cette comparaison implique pour nos premières notes

**Analyse pour le hackathon :** l’orchestration, la mémoire, la délégation et les outils ne suffisent plus à définir l’originalité d’un projet. La piste de notre page « Premières notes » devient plus précise si nous montrons :

1. Des tâches locales bien délimitées qui, une fois composées, réalisent des applications de familles différentes.

2. Des exécutants légers et un contrôle déterministe aux étapes critiques.

3. Une coordination mesurée : coût total, doublons, conflits, pertes de contexte et reprises.

4. Un avantage observé face à un agent généraliste sur le même jeu de tâches.

Aucun classement de performance n’est établi ici. Tester mêmes objectifs, mêmes ressources et mêmes critères sera plus instructif qu’un compte de fonctionnalités. Cette page compare des produits ; les études sur la coordination et les adaptations retenues sont documentées dans le [journal D20 à D23](https://chatgpt.com/space/page_baca1e1d35e481919683b0dc23455045).
