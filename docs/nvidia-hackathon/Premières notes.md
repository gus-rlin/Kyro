Nos premières idées, observations et questions pour préparer le hackathon. **Mise en cohérence du 2 octobre 2026 :** cette page conserve l’origine des hypothèses ; les décisions à jour se trouvent dans les pages produit et architecture.

La description détaillée et à jour du produit se trouve dans [Construire et maintenir des applications web avec des agents](https://chatgpt.com/space/page_c41a658d84b08191b0295f1f1cb36e2d). Elle développe ces principes avec un catalogue sémantique, une équipe spécialisée, la maintenance continue, une mémoire persistante et un canevas modifiable depuis un ordinateur ou un téléphone.

## Question à explorer

Peut-on obtenir un résultat de niveau “gros modèle” avec un modèle NVIDIA plus petit parce qu’on a déplacé une partie de l’intelligence du modèle vers l’architecture du système ?

## Règle n°1 Une architecture solide pour encadrer le modèle

Les benchmarks consultés montrent des écarts entre certains modèles NVIDIA et nos références, avec des protocoles et des variantes à distinguer. Ils ne déterminent pas la réussite sur nos tâches de composition. Quelle que soit la capacité du modèle, la fiabilité exige une architecture robuste et des contrôles indépendants. Les valeurs et leurs limites figurent dans la [note comparative des modèles](https://chatgpt.com/space/page_ab48437e0b348191abba31b81471f363).

Nous devons décomposer le travail en tâches précises, limiter les actions autorisées, imposer des formats de sortie vérifiables et valider les résultats avant de les utiliser. Les étapes critiques doivent reposer sur du code déterministe dès que possible, avec des mécanismes de reprise en cas d’échec.

**Principe directeur : la fiabilité du produit doit être construite dans le système qui entoure le modèle.**

---

## Règle n°2 Des opérations bornées et peu coûteuses au service d’applications variées

Notre pari économique est de concentrer les appels coûteux sur les arbitrages et de confier les transformations simples à des modèles NVIDIA peu coûteux ou au logiciel déterministe. L’unité de travail d’un agent peut être étroite tout en contribuant à une plateforme généraliste. La mesure finale porte sur le coût de l’application validée, avec coordination et reprises.

Cette logique exige de donner au modèle quelque chose d’extrêmement borné à faire : une entrée limitée, une seule opération, peu de choix possibles et une sortie courte, structurée et vérifiable. Par exemple, classer chaque événement dans une liste fermée de catégories ou extraire quelques champs définis à l’avance. Le logiciel prépare les données, contrôle la réponse et gère les échecs.

Le budget doit être protégé par des limites explicites sur les tokens, les appels et les reprises. Nous mesurerons le coût pour 1 000 événements correctement traités, avec leur taux d’erreur et leur latence : un appel bon marché perd son intérêt s’il exige de nombreuses corrections. Un prix faible permet davantage de traitements à budget égal ; le débit réel dépend aussi des limites de l’API et devra être testé.

**Principe directeur : réutiliser des opérations simples et vérifiables pour composer une grande diversité d’applications à coût maîtrisé.** La petite taille d’une tâche ne limite pas l’ambition du produit.

### Tarifs des modèles et budget par volume

**Relevé illustratif consigné le 2 octobre 2026**, en dollars par million de tokens. Les montants Nebius ci-dessous n’ont pas pu être reconfirmés pendant le dernier polish : ils restent des hypothèses de calcul, à remplacer par un relevé de l’endpoint réellement accessible avant engagement. Les tarifs OpenAI correspondent au mode Standard, contexte court, hors cache et batch.

| Modèle | Fournisseur de l’API | Entrée par million de tokens | Sortie par million de tokens |
| - | - | -: | -: |
| Nemotron 3 Nano 30B A3B | Nebius | 0,06 $ | 0,24 $ |
| Nemotron 3.5 Lightning | Nebius | 0,06 $ | 0,24 $ |
| Nemotron 3 Super 120B A12B | Nebius | 0,30 $ | 0,90 $ |
| Nemotron 3 Ultra 550B A55B | Nebius | 1,00 $ | 3,00 $ |
| GPT-6.1 Sol | OpenAI | 2,00 $ | 10,00 $ |
| GPT-6 Astra | OpenAI | 10,00 $ | 50,00 $ |

Sources : [catalogue tarifaire officiel Nebius](https://tokenfactory.nebius.com/api/public/models_info) et [tarifs officiels OpenAI](https://developers.openai.com/api/docs/pricing). Les tarifs peuvent évoluer.

**Deux références distinctes.** Ce repère de coût emploie GPT 6.1 Sol et GPT 6 Astra. La [comparaison de benchmarks](https://chatgpt.com/space/page_ab48437e0b348191abba31b81471f363) conserve GPT 5.6 Sol : ne pas associer ses scores au prix d’un autre modèle. Le modèle réellement utilisé par Codex sera figé dans le protocole.

**Exemple calculé :** pour 100 000 événements consommant chacun 500 tokens d’entrée et 50 tokens de sortie, le coût des tokens serait de **4,20 $ avec Nano ou Lightning**, **19,50 $ avec Super**, **65 $ avec Ultra**, **150 $ avec GPT-6.1 Sol** et **750 $ avec GPT-6 Astra**. Ce scénario suppose un seul appel par événement et inclut tous les tokens facturés dans ces plafonds, y compris le raisonnement éventuel ; il exclut les reprises, les outils et les autres coûts du produit. Ce calcul utilise les hypothèses du tableau ; il n’est ni une facture prévisionnelle du projet ni une preuve de qualité équivalente.

À volume de tokens identique, Nano ou Lightning coûteraient donc environ **36 fois moins que GPT-6.1 Sol** dans cet exemple. C’est l’intérêt de borner extrêmement la tâche : garder des entrées courtes et des sorties minimales pour faire passer beaucoup de flux avec un petit budget, sous réserve de valider la qualité sur notre cas d’usage.

L’accès de prototypage aux API NVIDIA NIM est distinct des crédits Nebius et des conditions du hackathon. Un appel direct à un endpoint NVIDIA ne suffit pas à établir l’utilisation de Nebius requise pour la soumission. Vérifier l’accès et ses limites avant usage : [NVIDIA](https://docs.api.nvidia.com/nim/docs/product) et [règles du hackathon](https://nebiusglobalaihackathon.devpost.com/rules).

---

## Première intuition Décomposer un problème complexe entre agents

Nous pourrions partir d’un problème très complexe et le rendre traitable en le décomposant en tâches de plus en plus granulaires. Aucun modèle ne devrait avoir à résoudre seul la tâche complexe entière. La complexité serait prise en charge par l’organisation du travail, les échanges entre agents et la validation de leurs résultats.

L’organisation retenue comprend **un orchestrateur NVIDIA coûteux, quatre exécutants NVIDIA peu coûteux au départ, un agent de revue et un agent de sécurité**. L’effectif est extensible selon les tâches indépendantes et les budgets. La maintenance utilise le même modèle que l’orchestrateur. Les agents applicatifs configurent et composent exclusivement le catalogue validé ; l’assembleur produit le code. Voir la [vision produit](https://chatgpt.com/space/page_c41a658d84b08191b0295f1f1cb36e2d) et l’[architecture](https://chatgpt.com/space/page_1869350b9e648191966bb8b7cff5b03b).

- **L’orchestrateur** traduit le besoin, propose le plan et attribue les tâches. Le logiciel fait respecter dépendances, versions, permissions, budgets et critères de validation avant intégration.

- **La revue et la sécurité** sont confiées à deux agents distincts. Ils examinent les changements et les interactions entre blocs ; leurs analyses complètent les contrôles exécutables.

- **Les exécutants légers** accomplissent chacun une seule tâche très bornée : des données d’entrée limitées, une consigne précise, des actions autorisées restreintes et un format de sortie imposé.

Le principe s’applique aussi à l’orchestrateur, à la revue, à la sécurité et à la maintenance : chaque tâche précise le résultat attendu, les contraintes et le contrôle de réussite. Décomposer une décision de modèle ne la rend pas déterministe ; les opérations calculables et les vérifications prises en charge doivent reposer sur du code.

**Hypothèse à tester : nous pourrions obtenir un résultat complexe en coordonnant beaucoup d’opérations simples, fiables et peu coûteuses.** Il faudra mesurer si le gain des modèles légers compense le coût de la coordination, des échanges et des reprises, et si l’assemblage préserve la cohérence du résultat final. La [revue des études dans le journal, D20 à D23](https://chatgpt.com/space/page_baca1e1d35e481919683b0dc23455045), motive des contrats précis, une coordination adaptée aux dépendances et des preuves indépendantes.

## Un IDE pour créer et déployer des applications à partir de composants préconstruits

**Thèse de l’idée : pré-calculer une partie du développement logiciel sous forme de primitives fiables afin que le modèle ait beaucoup moins de décisions à prendre.**

Cette préparation pourrait permettre à des modèles NVIDIA peu coûteux de produire des applications fonctionnelles avec moins de tokens. Le modèle dispose de choix bornés et d’interfaces explicites ; le logiciel assure l’assemblage répétitif et les vérifications. La qualité et l’économie restent à mesurer face à la référence.

Une piste serait de construire un **IDE avec des agents de programmation, dans l’esprit de Codex d’OpenAI**, permettant de créer et de mettre en ligne une application en visant **quinze minutes**. L’idée reste exploratoire : ce délai est un objectif à tester sur un périmètre d’application défini.

L’agent dispose d’une bibliothèque cible de **180 blocs Rust préconstruits, documentés et sécurisés par défaut**, à construire et qualifier. Il sélectionne les blocs validés, les configure et propose leur composition ; l’assembleur déterministe produit les sources. Il n’écrit aucun code complémentaire improvisé. Une capacité absente est signalée et suit le circuit distinct d’enrichissement du catalogue.

**De l’idée à la mise en ligne :** les agents prennent en charge sélection des capacités, composition de l’interface, configuration, contrôles, publication et maintenance dans le périmètre autorisé. L’objectif est une grande variété d’applications réutilisant les mêmes fondations. Performance et sécurité se vérifient sur les blocs et leurs assemblages.

**Exemple de parcours cible :** Bob demande plusieurs applications pour plusieurs restaurants. Les agents les composent, les vérifient et les publient dans les accès et budgets déjà accordés. Bob n’a pas à manipuler le code ou l’hébergement ; il intervient seulement pour une véritable décision manquante, par exemple l’achat d’un domaine. La restauration est un exemple d’usage, parmi CRM, support, commerce, portails, gestion interne et d’autres familles.

**Ambition du produit : simplifier à l’extrême la création de sites et d’applications, jusqu’à leur publication, pour une personne sans compétences techniques.**

La sécurité doit être vérifiée sur l’application assemblée : la réutilisation de blocs testés ne suffit pas à garantir que leurs connexions, leurs permissions et leur configuration sont correctes.

**Ce que nous voulons démontrer :** partir d’une demande simple et obtenir une application utilisable et accessible en ligne, puis mesurer le temps total jusqu’au déploiement, les tokens consommés, le coût par application validée, le nombre d’interventions de l’utilisateur et la réussite des contrôles fonctionnels et de sécurité. L’objectif est de minimiser les tokens dépensés et les manipulations nécessaires pour atteindre ce résultat.
