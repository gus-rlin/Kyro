# Contribuer à Kyro

`main` est la référence commune de l’IDE et du backend. Les branches de livraison servent à préparer un changement vérifiable ; elles peuvent être supprimées sur GitHub une fois leur intégration confirmée. Les worktrees locaux contenant du travail en cours doivent être préservés.

## Préparer un changement

1. Partir de `origin/main` à jour, dans une branche `gus-rlin/<chantier>` ou le préfixe utilisé par votre fork.
2. Lire le [README](README.md), l’[index documentaire](docs/README.md) et les contrats du périmètre concerné. Respecter les instructions locales lorsqu’elles existent.
3. Identifier le comportement attendu, les critères de réussite et les vérifications utiles avant de modifier les sources.
4. Garder le changement ciblé. Conserver les choix de stack : cœur, blocs et assembleur Rust ; PostgreSQL ; IDE React/TypeScript ; interfaces applicatives prévues en Leptos.

## Vérifier et livrer

Les commandes de base figurent dans le [README](README.md#vérifier-et-contribuer). Les guides P1–P3 expliquent les recettes PostgreSQL, les constructions protégées et leurs configurations isolées.

- Pour une correction de logique, conserver une reproduction qui expose le défaut, puis vérifier la correction et les régressions pertinentes.
- Indiquer les contrôles réellement exécutés, les services ou doublures utilisés, les échecs et les vérifications impossibles. Ne pas assimiler une compilation à une preuve de parcours complet.
- Actualiser les contrats et guides touchés. Une attestation historique ne qualifie pas automatiquement des sources modifiées.
- Ne versionner ni secrets, ni configuration privée, ni dépendances installées, ni binaires générés. Utiliser des fixtures synthétiques et expurger les preuves publiables.
- Les essais modèles réels nécessitent un budget explicite et l’autorisation correspondante. Les tests ordinaires utilisent des fournisseurs synthétiques.

Avant la PR, examiner `git diff --check`, le contenu exact du diff et les effets sur les accès, données, migrations, reprises et budgets concernés. Décrire le résultat pour un lecteur qui n’a pas suivi le chantier. Après les contrôles et la fusion, vérifier l’ascendance avant de retirer une branche distante ; garder une branche locale divergente tant que son travail n’a pas été examiné.
