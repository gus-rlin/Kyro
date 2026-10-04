État documentaire au **2 octobre 2026**. Cette note conserve la comparaison NVIDIA–GPT 5.6 Sol et ses protocoles. Le dernier recoupement confirme les repères NVIDIA Lightning/Ultra et Sol cités ci-dessous ; les prix, contextes et configurations Nebius du relevé antérieur restent **à reconfirmer pour notre endpoint**. L’accès du compte, les quotas et les crédits restent à vérifier.

## NVIDIA face à GPT 5.6 sol — comparaison directe

### Évaluation indépendante : Artificial Analysis

Même Intelligence Index v4.3.2, relevé le 2 octobre 2026. Modèles avec raisonnement ; Sol est testé à l’effort max. Scores arrondis affichés par l’évaluateur, plus élevé = meilleur résultat.

| Modèle NVIDIA | NVIDIA — AA v4.3.2 | GPT 5.6 sol (max) — AA v4.3.2 | Écart NVIDIA − Sol |
| - | - | - | - |
| Nemotron 3.5 Lightning | 13 | 47 | −34 points |
| Nemotron 3 Super 120B A12B | 13 | 47 | −34 points |
| Nemotron 3 Ultra 550B A55B | 23 | 47 | −24 points |

Sources indépendantes : [Lightning](https://artificialanalysis.ai/models/nemotron-3-5-lightning), [Super](https://artificialanalysis.ai/models/nvidia-nemotron-3-super-120b-a12b), [Ultra](https://artificialanalysis.ai/models/nvidia-nemotron-3-ultra-550b-a55b), [Sol max](https://artificialanalysis.ai/models/gpt-5-6-sol).

L’indice combine dix évaluations de travail avec agents, code, connaissances et contexte long. Ce sont des points, pas des pourcentages ni un rapport de puissance. Le 58,9 précédemment indiqué pour Sol concernait v4.1 : il ne se compare pas aux scores v4.3.2. Les résultats individuels des dix tests ne sont pas accessibles dans les données consultées ; aucune valeur n’est déduite de l’indice. Nano est absent faute de score vérifié ici. Les configurations évaluées ne certifient pas celles de notre endpoint Nebius.

### Benchmarks précis : chaque modèle NVIDIA à côté de Sol

| Benchmark | Modèle NVIDIA évalué | Score NVIDIA | GPT 5.6 sol | Écart NVIDIA − Sol |
| - | - | - | - | - |
| GPQA Diamond | Lightning BF16 | 75,44 % | 94,6 % | −19,16 points |
| Terminal-Bench 2.1 | Lightning BF16 | 24,58 % | 88,8 % | −64,22 points |
| Terminal-Bench 2.1 | Ultra NVFP4 | 53,9 % | 88,8 % | −34,9 points |
| BrowseComp | Lightning BF16 | 36,97 % | 90,4 % | −53,43 points |
| BrowseComp | Ultra NVFP4 | 41,4 % | 90,4 % | −49,0 points |

Sources : [OpenAI — évaluations GPT 5.6](https://openai.com/index/gpt-5-6/), [NVIDIA Lightning](https://build.nvidia.com/nvidia/nemotron-3.5-lightning-30b-a3b/modelcard), [NVIDIA Ultra](https://build.nvidia.com/nvidia/nemotron-3-ultra-550b-a55b/modelcard). Ces lignes rapprochent des scores fournisseurs sur les mêmes intitulés ; prompts, outils, agent et budget peuvent différer. Elles sont moins contrôlées que l’évaluation indépendante commune ci-dessus.

SWE-bench Pro et SWE-bench Verified restent séparés : le score Pro de Sol ne peut pas être placé face au score Verified de Nemotron comme s’il s’agissait du même test. MMMU Pro est multimodal, alors que les quatre Nemotron de cette note sont des modèles de texte. GPQA n’est assimilé à GPQA Diamond que lorsque la source établit cette version.

Lecture : Sol devance les trois Nemotron sur l’indice indépendant commun ; Ultra est le plus proche parmi les NVIDIA comparés. Les résultats fournisseurs de Terminal-Bench 2.1 et BrowseComp vont dans le même sens. Les tarifs plus faibles des Nemotron permettent d’évaluer ce compromis sur nos tâches réelles ; les écarts en points ne mesurent pas un facteur de puissance.

## Accès pour le hackathon

Le projet doit tourner sur Nebius Token Factory ou Nebius AI Cloud et utiliser au moins un modèle NVIDIA open source. GPT 5.6 sol est ici une référence de comparaison ; il ne remplace pas cette exigence. Les ressources annoncent 25 $ de crédits Token Factory, puis 25 $ supplémentaires via le Builders Program, sous réserve d’activation. Ces crédits ne constituent pas une enveloppe API OpenAI. [Conditions du hackathon](https://nebiusglobalaihackathon.devpost.com/) · [Crédits](https://nebiusglobalaihackathon.devpost.com/resources)

## Modèles et prix

Prix en dollars par million de tokens, entrée non mise en cache et sortie. L’entrée correspond au prompt, à l’historique et aux données envoyées ; la sortie correspond aux tokens générés et facturés, y compris le raisonnement lorsqu’il est comptabilisé. Pour Nebius, il s’agit des endpoints publics facturés au token, pas d’une location de GPU. Le contexte indiqué est celui annoncé par Nebius, qui peut être inférieur à la capacité théorique du modèle. **Statut du relevé Nebius :** valeurs consignées précédemment, non reconfirmées pendant ce polish ; les sources publiques n’ont pas pu être relues. Les calculs restent illustratifs et ne servent pas à arrêter le budget. Les prix GPT 5.6 Sol ont été recoupés avec sa fiche officielle.

| Modèle | Paramètres totaux / actifs | Contexte annoncé | Configuration Nebius | Entrée / 1 M | Sortie / 1 M |
| - | - | - | - | - | - |
| Nemotron 3.5 Lightning | 30 Md / 3 Md | 1 024 K | BF16 | 0,06 $ | 0,24 $ |
| Nemotron 3 Super | 120 Md / 12 Md | 256 K | FP4 | 0,30 $ | 0,90 $ |
| Nemotron 3 Ultra | 550 Md / 55 Md | 1 024 K | FP4 | 1,00 $ | 3,00 $ |
| Nemotron 3 Nano | 30 Md / 3 Md | 262 K | FP8 | 0,06 $ | 0,24 $ |
| GPT 5.6 sol | Non publiés dans la fiche API | 1 050 K | Service propriétaire OpenAI | 4,00 $ | 20,00 $ |

[Source Nebius : catalogue, configurations et tarifs](https://nebius.com/services/token-factory/models/nvidia-nemotron-models-inference) · [Source OpenAI : GPT 5.6 sol](https://developers.openai.com/api/docs/models/gpt-5.6-sol). Le cookbook Nebius met surtout en avant Lightning, Super et Ultra. Nano figure encore sur la page officielle Nebius, mais n’est plus mis en avant dans ce cookbook : vérifier sa présence dans notre catalogue avant de l’intégrer. [Cookbook](https://github.com/nebius/token-factory-cookbook/blob/main/models/nemotron/README.md) Archiver à la qualification l’identifiant exact servi, la configuration exposée, le contexte accepté, la région et le tarif du compte. Le tarif ou le contexte affiché chez un évaluateur ou un autre hébergeur ne remplace pas celui de Nebius.

Pour GPT 5.6 sol, les tarifs promotionnels sont annoncés au moins jusqu’au 21 novembre 2026. Au-delà de 272 K tokens d’entrée, la requête entière passe à 8 $ en entrée et 30 $ en sortie par million. Le cache et les outils peuvent modifier la facture. Les prix API ne correspondent pas au prix de notre abonnement ChatGPT.

## Performances publiées par NVIDIA

Scores en pourcentage publiés par NVIDIA, sans outils pour GPQA et HLE. Ce sont des évaluations du fournisseur, pas des tests effectués par nous. Les variantes et les protocoles sont indiqués pour éviter de confondre un modèle et son déploiement.

| Modèle et variante évaluée | GPQA sans outils | HLE sans outils | Code | Source |
| - | - | - | - | - |
| Lightning BF16 | 75,44 % — Diamond explicite | 11,72 % — texte seul | SWE-bench Verified : 51,56 % | [Fiche NVIDIA](https://build.nvidia.com/nvidia/nemotron-3.5-lightning-30b-a3b/modelcard) |
| Super NVFP4 | 79,42 % — libellé GPQA | 17,42 % | LiveCodeBench v6 : 78,44 % | [Fiche NVIDIA NVFP4](https://huggingface.co/nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-NVFP4) |
| Ultra NVFP4 | 87,9 % — libellé GPQA | 26,1 % | SWE-bench Verified : 69,7 % | [Fiche NVIDIA](https://build.nvidia.com/nvidia/nemotron-3-ultra-550b-a55b/modelcard) |
| Nano BF16 — différent du FP8 servi | 73,0 % — libellé GPQA | 10,6 % | SWE-bench avec OpenHands : 38,8 % | [Fiche NVIDIA BF16](https://huggingface.co/nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B-BF16) |

GPQA mesure le raisonnement scientifique ; HLE rassemble des questions difficiles dans plusieurs disciplines. SWE-bench évalue la résolution de problèmes de dépôts logiciels avec un agent ; LiveCodeBench évalue une autre forme de programmation. Leurs scores ne se comparent pas entre eux. Les outils, le budget de raisonnement et le logiciel qui pilote l’agent influencent aussi les résultats.

Les scores de Nano BF16 ne garantissent pas ceux du FP8 de Nebius. De même, une configuration FP4 annoncée par Nebius ne suffit pas à certifier l’identité exacte du checkpoint et des paramètres du benchmark NVIDIA. Les résultats GPT 5.6 sol publiés par OpenAI sont maintenant présentés ci-dessous ; les différences de protocole restent explicites.

### Connaissances et raisonnement

Les colonnes conservent les variantes de la synthèse : Lightning BF16, Super NVFP4, Ultra NVFP4 et Nano BF16. « — » signifie non renseigné dans les sources retenues, pas zéro. Tous les scores ci-dessous sont en pourcentage, sauf indication contraire.

| Benchmark | Lightning BF16 | Super NVFP4 | Ultra NVFP4 | Nano BF16 |
| - | - | - | - | - |
| MMLU-Pro — connaissances | 81,94 | 83,33 | — | 78,3 |
| SciCode — programmation scientifique | 32,60 | 40,83 — sous-tâches | 43,5 — sous-tâches | 33,3 — sous-tâches |
| HMMT février 2025 avec outils — mathématiques | — | 95,36 | — | — |
| AIME 2025 sans outils — mathématiques | — | — | — | 89,1 |
| AIME 2025 avec outils | — | — | — | 99,2 |
| MiniF2F pass\@1 — preuves formelles | — | — | — | 50,0 |
| MiniF2F pass\@32 | — | — | — | 79,9 |
| CritPt sans outils — physique | — | — | 3,4 | — |

### Programmation agents et recherche

| Benchmark | Lightning BF16 | Super NVFP4 | Ultra NVFP4 | Nano BF16 |
| - | - | - | - | - |
| SWE-bench Multilingual | 39,33 | — | 65,8 | — |
| Terminal-Bench 2.1 | 24,58 | — | 53,9 | — |
| Terminal Bench sous-ensemble difficile — autre protocole | — | 24,48 | — | 8,5 |
| PinchBench | 85,37 | — | 89,8 | — |
| BrowseComp | 36,97 | — | 41,4 | — |
| τ³-bench banque | 9,28 | — | 19,2 | — |
| TauBench V3 moyenne | — | — | 70,3 | — |
| TauBench V2 moyenne — version différente | — | 60,46 | — | 49,0 |
| BFCL v4 — appels de fonctions | — | — | — | 53,8 |
| ProfBench avec recherche | — | — | 56,4 | — |

### Instructions contexte long et langues

| Benchmark | Lightning BF16 | Super NVFP4 | Ultra NVFP4 | Nano BF16 |
| - | - | - | - | - |
| IFBench loose | 71,88 | — | — | — |
| IFBench prompt — autre mesure | — | 73,30 | 82,3 | 71,5 |
| AA-LCR — raisonnement en contexte long | 52,00 | 58,06 | 65,5 | 35,9 |
| RULER-500 à 256 K | — | 96,52 | — | — |
| RULER-100 à 256 K — autre protocole | — | — | — | 92,9 |
| RULER à 1 M — libellé de la fiche Ultra | — | — | 94,0 | — |
| RULER-100 à 1 M | — | — | — | 86,3 |
| MMLU-ProX moyenne multilingue | — | 79,37 | — | 59,5 |
| Scale AI Multi-Challenge | — | 52,8 | — | 38,5 |

Sources des trois tableaux : [Lightning](https://build.nvidia.com/nvidia/nemotron-3.5-lightning-30b-a3b/modelcard), [Super NVFP4](https://huggingface.co/nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-NVFP4), [Ultra NVFP4](https://build.nvidia.com/nvidia/nemotron-3-ultra-550b-a55b/modelcard) et [Nano BF16](https://huggingface.co/nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B-BF16). Les résultats proviennent de NVIDIA et ne constituent pas une évaluation indépendante commune.

Les variantes de Terminal-Bench, TauBench, IFBench et RULER restent sur des lignes séparées. Le nombre de tentatives compte : pass\@32 autorise davantage d’essais que pass\@1. Un score obtenu à 1 M tokens sur un checkpoint ne donne pas accès à ce contexte sur l’endpoint Nebius. Les protocoles SciCode, PinchBench et BrowseComp doivent également être vérifiés avant un classement direct. Aucun score global de « puissance » n’est calculé à partir de ces tables.

## Comparaison pratique avec GPT 5.6 sol

| Critère | NVIDIA sur Nebius | GPT 5.6 sol |
| - | - | - |
| Rôle dans le projet | Satisfait l’exigence NVIDIA lorsque réellement utilisé sur Nebius | Référence externe pour comparer les résultats |
| Entrées et sorties | Les quatre modèles ci-dessus traitent du texte | Entrées texte et images, sortie texte |
| Agents | Raisonnement et appels de fonctions annoncés ; comportement à tester | Appels de fonctions et sorties structurées documentés ; outils intégrés via Responses |
| Contrôle du modèle | Poids ouverts ; possibilité de déploiement selon licence et ressources | Service propriétaire via OpenAI |
| Qualité sur notre tâche | À mesurer avec nos données et nos outils | À mesurer avec les mêmes données et outils |
| Rapidité réelle | Pas de débit universel garanti par les benchmarks NVIDIA | Pas de mesure comparable établie dans cette note |

Sources : [Nebius](https://nebius.com/services/token-factory/models/nvidia-nemotron-models-inference) et [documentation officielle OpenAI](https://developers.openai.com/api/docs/models/gpt-5.6-sol). Les outils intégrés d’une plateforme sont une capacité du système complet, pas uniquement du modèle.

## Coût pour une charge identique

Calcul illustratif à partir du relevé ci-dessus, dont les montants Nebius restent à reconfirmer : 10 000 tokens d’entrée + 2 000 tokens de sortie par requête, sans cache, outils payants ni relances. Inclure les tokens de raisonnement lorsqu’ils sont facturés dans la sortie. Formule : coût = tokens d’entrée × prix d’entrée / 1 000 000 + tokens de sortie × prix de sortie / 1 000 000.

| Modèle | Coût entrée — 10 000 tokens | Coût sortie — 2 000 tokens | Total par requête | Total de 1 000 requêtes | Ratio Sol / modèle |
| - | - | - | - | - | - |
| Lightning ou Nano | 0,00060 $ | 0,00048 $ | 0,00108 $ | 1,08 $ | × 74,1 |
| Super | 0,00300 $ | 0,00180 $ | 0,00480 $ | 4,80 $ | × 16,7 |
| Ultra | 0,01000 $ | 0,00600 $ | 0,01600 $ | 16,00 $ | × 5 |
| GPT 5.6 sol | 0,04000 $ | 0,04000 $ | 0,08000 $ | 80,00 $ | Référence |

Ces ratios sont des calculs de prix à volume identique, pas des rapports de qualité. Un modèle qui produit davantage de raisonnement ou multiplie les tentatives peut coûter plus cher par tâche réussie.

**Comparabilité des références.** Les [premières notes](https://chatgpt.com/space/page_1ef5d98198008191831651e980886b2d) donnent aussi des exemples de prix GPT 6.1 Sol et GPT 6 Astra. Leurs tarifs ne doivent pas être associés aux scores GPT 5.6 Sol de cette page. Modèle, version, effort de raisonnement, fournisseur et type de facturation restent liés dans chaque mesure.

## Affectation retenue et qualification des modèles

La configuration produit conserve le **modèle NVIDIA compatible le plus coûteux pour l’orchestration**, le même pour la maintenance, **quatre exécutants NVIDIA peu coûteux au départ**, puis revue et sécurité avec escalade. Ultra et Lightning sont des candidats à qualifier, pas des identifiants définitivement retenus. Leur coût relatif se compare sur le même profil de tokens. GPT 5.6 Sol reste la référence de cette note ; le modèle Codex du benchmark est figé séparément.

Qualifier sur 20 à 30 cas représentatifs distincts des briefs finaux : exactitude des compositions, respect du schéma, appels d’outils, refus hors permissions, détection d’une capacité absente, durée et coût avec toutes les reprises. Conserver les résultats par rôle, tâche et endpoint. Une mise à jour du modèle ou de son adaptateur déclenche une requalification ciblée ; aucun score général ne suffit à autoriser un rôle.

## Autres modèles NVIDIA

Le parcours Physical AI mentionne aussi Cosmos, GROOT et Sonic. Ils ne sont pas assimilables aux quatre LLM de ce tableau. Les déploiements sur AI Cloud ou sur endpoints dédiés demandent un chiffrage de l’infrastructure ; cette note n’établit aucun tarif au token pour eux. Nebius indique également Nano Omni comme disponible uniquement sur endpoint dédié. [Physical AI](https://nebiusglobalaihackathon.devpost.com/) · [Disponibilité Nebius](https://nebius.com/services/token-factory/models/nvidia-nemotron-models-inference)

