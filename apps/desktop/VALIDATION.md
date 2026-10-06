# Validation de la livraison frontend — 2026-10-04

## Mise à jour — 2026-10-06

La branche `gus-rlin/kyro-integration` conserve les résultats historiques
ci-dessous et résout les six échecs desktop. Build TypeScript/Vite et huit
tests du service de chat passent. Suite frontend complète avec PostgreSQL,
API/worker récents et fournisseur synthétique : **22/22 réussis**, zéro échec
ou ignoré, 89,37 s. Le détail du périmètre, du lancement et des limites est
dans [le guide d'intégration](../../docs/backend/INTEGRATION.md).
Après stabilisation du chargement des fixtures avec le runtime utilisateur
actif, les six scénarios desktop passent à nouveau en 17,9 s.

## Résultats historiques

Branche issue de main cfbe218 ; copie des sources actuelles de notre interface, sans modification du backend. Environnement Windows x64, Node.js 24.18.0.

- npm ci : réussi. Le téléchargement différé du binaire Electron échoue avec « fetch failed ». Pour la suite, réutilisation du binaire local Electron 44.5.1 déjà installé ; aucune dépendance binaire versionnée. L’installation entièrement neuve reste à vérifier sur un réseau permettant ce téléchargement.
- npm run prepare:git : réussi, MinGit 2.56.0.windows.1 téléchargé et SHA-256 vérifié.
- npm run build : TypeScript et Vite réussis avec ce runtime local.
- Suite complète initiale : 4 réussites, 7 échecs. Six échecs dans desktop.spec.mjs (anciens textes et contrôles d’accueil, dimensions, rechargement CSS) ; ils restent à réconcilier avec le frontend actuel. Ne pas considérer cette suite comme verte.
- Septième échec : le test projet ciblait les deux dialogues partageant maintenant le style project-dialog ; sélecteur corrigé pour cibler le dialogue ouvert.
- Contrôle de la frontière native : premier essai sans serveur, ECONNREFUSED ; après lancement de Vite, les 8 contrôles passent (origine, host, corps, opération et chemin invalides).

Les tests utilisent des profils temporaires et des doublures de sélection de dossiers, ainsi que Git embarqué. Ils ne prouvent pas une connexion aux modèles, un déploiement ou un paquet signé. Les détails des limites fonctionnelles sont dans README.md.

Après correction, recette ciblée `npx --no-install playwright test tests/project.spec.mjs tests/model-picker.spec.mjs tests/team-cost.spec.ts` : **5/5 réussis en 19,1 s**. Les six échecs historiques desktop ne sont pas couverts par ce résultat.
