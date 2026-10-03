# Contrat de recette de la partie 1 — version 1

Date : 2026-10-03. Périmètre : socle de pilotage, sans assembleur, interface ou équipe de modèles. Référence : [plan initial](../../nvidia-hackathon/Construire%20le%20backend%20de%20Kyro%20en%20cinq%20parties.md).

Le superviseur fixe ces scénarios avant l'implémentation. Les fournisseurs d'identité et d'inférence de la recette utilisent uniquement des identités, données et réponses synthétiques ; PostgreSQL, l'API HTTP et le worker sont réels. L'appel NVIDIA réel demandé par le plan reste à qualifier avec une clé : la substitution en recette suit la demande explicite de l'utilisateur et ne vaut pas qualification du fournisseur.

| ID | Comportement attendu | Preuve à produire |
| --- | --- | --- |
| P1-01 | Une installation verrouillée démarre API et worker ; les migrations sont rejouables et leurs verrous empêchent une double application. | Build avec Cargo.lock, démarrage, santé et migration répétée/concurrente. |
| P1-02 | Les identités, sessions et organisations sont persistées ; expiration et révocation s'appliquent côté serveur. | Recette OIDC synthétique avec PKCE/state/nonce/issuer/audience, cookie opaque, CSRF et refus de session révoquée. |
| P1-03 | Deux identités ne lisent ni ne modifient les projets, révisions, travaux, budgets ou événements de l'autre. | Requêtes HTTP croisées et contrôle RLS avec rôle non propriétaire ; réutilisation de connexion sans contexte résiduel. |
| P1-04 | Une révision immuable et un ChangeSet typé préservent identifiants et propriétés non ciblées. | Lecture ancienne/nouvelle révision, conflit sur révision attendue obsolète, refus d'une opération inconnue ou surdimensionnée. |
| P1-05 | Une commande répétée avec la même clé et les mêmes paramètres ne provoque qu'une mutation ; avec d'autres paramètres elle est refusée. | Requêtes séquentielles/concurrentes et comptage de révisions/événements. |
| P1-06 | Un travail admis survit à l'arrêt du worker ; un bail expiré est repris avec une génération supérieure. | Arrêt/redémarrage d'un processus worker et achèvement depuis PostgreSQL. |
| P1-07 | Un ancien worker, une permission révoquée ou une révision modifiée ne permettent plus d'intégrer un résultat. | Résultats tardifs refusés par génération, bail, état, autorité et révision. |
| P1-08 | Les tentatives, échéances, volumes et admissions sont bornés ; annulation et erreur ne provoquent pas une boucle. | Saturation et transitions terminales ; aucune nouvelle action après annulation. |
| P1-09 | Des réservations concurrentes ne dépassent pas le plafond ; les inconnus restent provisionnés et le rapprochement est idempotent. | Concurrence PostgreSQL près du plafond, comptabilité et refus d'usage impossible. |
| P1-10 | Une intention précède chaque appel externe ; réponse perdue ou interruption après envoi produit un inconnu sans nouvel appel automatique. | Simulateur HTTP avec compteur, timeout/interruption, redémarrage et rapprochement autorisé. |
| P1-11 | Passerelle : destinataire, catégorie, format, délai, taille et référence de secret sont contrôlés. | Appel structuré synthétique, destinations/secrets factices interdits, réponse malformée et erreur sans contenu sensible. |
| P1-12 | Les événements sont persistés atomiquement, ordonnés par projet, rejouables via Last-Event-ID et privés. | Reconnexion SSE, contrôle des curseurs, interruption du flux après révocation. |
| P1-13 | La restauration isolée conserve projets, anciennes révisions, droits, travaux, outbox, intentions et réserves. | Sauvegarde PostgreSQL réelle, restauration dans une base distincte, worker arrêté et émissions externes suspendues. |
| P1-14 | Les contrats, limites, configuration, procédures et preuves sont documentés ; la revue indépendante accepte l'état final. | Rapport de recette reproductible, contrôle des dépendances et revue Sol PASS ≥ 8/10 sans constat critique/élevé non résolu. |

Les échecs et contrôles impossibles sont conservés dans le suivi. Aucun déploiement, SLA, gain mesuré ou appel fournisseur réel n'est revendiqué sur la seule base de cette recette locale.
