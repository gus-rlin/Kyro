Cette page rassemble les règles essentielles du **Nebius x NVIDIA Global AI Hackathon**, les quatre catégories et le dossier que nous voulons préparer pour viser l’excellence. Sources vérifiées le **2 octobre 2026**. Les recommandations ci-dessous sont nos objectifs de travail, sans garantie de classement.

## Dates et échéance

**Remise au plus tard le 30 octobre 2026 à 18 h à Paris** — 10 h PDT, soit 17 h UTC. La période de création commence le 26 août 2026. Résultats prévus autour du 11 janvier 2027.

**Calendrier recoupé le 2 octobre 2026 :** le [calendrier](https://nebiusglobalaihackathon.devpost.com/details/dates) et le [règlement](https://nebiusglobalaihackathon.devpost.com/rules) indiquent désormais tous deux un jugement **du 1er au 15 décembre 2026**. L’ancienne divergence avec le 2 novembre n’est plus présente. Prévoir l’accès gratuit du jury jusqu’à la fin du jugement.

## Règles essentielles

Résumé du [règlement officiel](https://nebiusglobalaihackathon.devpost.com/rules), qui reste la référence complète.

- Application fonctionnelle utilisant au moins un modèle NVIDIA open source sur **Nebius Token Factory ou Nebius AI Cloud**. Un appel d’inférence Token Factory pendant l’exécution suffit ; Cloud permet notamment Jobs, Endpoints ou DevPods.

- Participants majeurs selon leur pays ; exclusions territoriales et conflits d’intérêts détaillés au règlement. Une équipe désigne un représentant autorisé.

- Projet nouveau ou significativement amélioré pendant la période ; expliquer les améliorations d’un projet préexistant.

- Fonctionnement conforme à la description et à la vidéo. Respecter les droits et licences du code, des données, des API et des médias.

- Dossier en **anglais**, ou accompagné de traductions anglaises.

- Accès gratuit pour le jury jusqu’à la fin du jugement, avec identifiants de test si nécessaire. Du matériel peu accessible peut devoir être présenté sur demande.

- Après l’échéance, la soumission ne peut plus être modifiée, sauf exceptions autorisées.

Le jury filtre d’abord l’adéquation réelle à la catégorie et aux technologies imposées. Ensuite, **quatre critères de même poids** : réalisation technique, design, impact potentiel et qualité de l’idée.

## Les quatre catégories

Synthèse des catégories de la [présentation officielle](https://nebiusglobalaihackathon.devpost.com/).

| Catégorie officielle | Périmètre |
| - | - |
| Coding and Agentic Engineering | Agents de programmation et outils de développement écrivant, exécutant et testant du code. |
| Best Apps and Agents | Applications utiles, copilotes et workflows automatisés avec Nemotron sur Token Factory. |
| Personal AI | Assistant privé persistant, mémoire, compétences et outils choisis par l’utilisateur. |
| Physical AI | Robotique, IoT et intelligence embarquée ; perception et action, ou modules applicatifs sans matériel. |

## Livrables officiels

D’après les [exigences de remise](https://nebiusglobalaihackathon.devpost.com/).

- Projet fonctionnel ; catégorie choisie ; description.

- URL de démo, application ou build testable, sauf Physical AI.

- Vidéo YouTube publique de **moins de trois minutes**. Physical AI : au moins une minute de fonctionnement du matériel ou, sans matériel, des modules clés.

- Dépôt public GitHub, GitLab ou Bitbucket avec licence open source visible, README d’installation et d’exécution, et explication de l’utilisation NVIDIA et Nebius.

- Retour d’expérience sur les outils utilisés ; améliorations documentées si projet préexistant.

**Marge pratique :** viser une vidéo de 2 min 50 s ; le règlement formule la limite plus strictement que la présentation.

## Notre niveau cible pour viser l’excellence

**Recommandations de travail.** Nous voulons qu’un évaluateur comprenne rapidement la valeur du produit, puisse l’essayer et retrouve les preuves de nos affirmations.

| Élément à préparer | Notre niveau cible |
| - | - |
| Produit | Le parcours de la plateforme, depuis la demande jusqu’à une application exploitable, avec des exemples montrant la réutilisation entre familles. Interface claire, attente visible, erreurs compréhensibles et reprise possible. |
| Démo | Des exemples préparés et des données d’essai, accessibles rapidement. Un résultat réel et une explication simple de son utilité. |
| Dossier | Des utilisateurs et problèmes concrets, notre solution et son bénéfice. Chaque affirmation importante renvoie à une mesure ou à un exemple vérifiable. |
| Vidéo | Montrer le parcours en action, expliquer la contribution de notre architecture et afficher une preuve du gain. Sous-titres anglais, son lisible et montage sobre. |
| Dépôt | Installation vérifiée depuis un environnement propre ; dépendances fixées, exemple de configuration sans secrets, données d’exemple et schéma d’architecture. |
| Mesures | Taux de résultats corrects, latence et coût par résultat validé sur un jeu d’essai décrit. Comparaison avec une méthode simple, mêmes entrées et mêmes conditions. |
| Retour technique | Observations concrètes : contexte, difficulté rencontrée, comportement obtenu et amélioration souhaitée. |

### Preuves à montrer selon notre catégorie

Ces propositions servent à choisir notre démonstration ; elles ne sont pas des obligations supplémentaires.

| Catégorie | Démonstration que nous proposerions |
| - | - |
| Coding and Agentic Engineering | Montrer la création puis l’évolution d’applications par composition, les sources produites et les tests ; démontrer une correction via le catalogue. |
| Best Apps and Agents | Accomplir un workflow utile de bout en bout et mesurer le temps ou le travail économisé. |
| Personal AI | Montrer une préférence mémorisée entre deux sessions, une action avec permissions explicites et la possibilité d’effacer la mémoire. |
| Physical AI | Montrer une boucle perception, décision et action, avec mesure du succès et comportement en cas d’échec. |

### Application à notre intuition de nuée d’agents

Notre [vision produit](https://chatgpt.com/space/page_c41a658d84b08191b0295f1f1cb36e2d) vise une fabrique généraliste d’applications. La démonstration doit montrer la réutilisation et la qualité sur les scénarios retenus. Le benchmark obligatoire compare Codex de zéro et notre processus sur les mêmes briefs ; les essais avec un agent seul servent ensuite à isoler les effets de coordination.

**Catégorie à confirmer lors du dossier :** Coding and Agentic Engineering paraît cohérent avec une plateforme de construction et maintenance logicielle ; expliquer que les modèles composent le catalogue et que l’assembleur produit le code. Best Apps and Agents reste à examiner selon la démonstration effectivement présentée. Ce choix de catégorie ne réduit pas le périmètre généraliste.

## Contrôle final avant la remise

Notre procédure proposée reprend les livrables ci-dessus et ajoute une vérification de qualité.

- [ ] Vérifier chaque exigence officielle contre le dossier final.

- [ ] Faire essayer le parcours central par une personne qui découvre le projet.

- [ ] Reproduire les mesures et expliquer leurs limites.

- [ ] Tester les liens et l’installation depuis un environnement propre.

- [ ] Vérifier que la vidéo montre uniquement des fonctions disponibles.

- [ ] Conserver une version identifiée du code présenté, ses preuves d’exécution NVIDIA sur Nebius et le budget d’accès du jury jusqu’à la fin du jugement.

- [ ] Soumettre avant l’échéance avec une marge pour les derniers problèmes techniques.

