# Documentation de Kyro

La [présentation du produit](../README.md) décrit la vision, la stack, l’avancement et la feuille de route. Les guides ci-dessous documentent les capacités implémentées et les limites de leurs preuves.

| Besoin | Documents |
| --- | --- |
| Installer et utiliser l’IDE | [Guide desktop](../apps/desktop/README.md), [validation frontend](../apps/desktop/VALIDATION.md) |
| Brancher le chat Nano | [Configuration et activation](backend/chat-nano.md), [intégration Nebius P1](backend/partie-1/NEBIUS.md) |
| Comprendre la consolidation | [Guide d’intégration historique](backend/INTEGRATION.md), [rapport de consolidation GitHub](backend/CONSOLIDATION.md) |
| Comprendre le socle P1 | [Architecture](backend/partie-1/ARCHITECTURE.md), [périmètre](backend/partie-1/PERIMETRE.md), [manifeste](backend/partie-1/MANIFESTE.md) |
| Utiliser l’API P1 | [Contrat HTTP](backend/partie-1/HTTP.md), [OpenAPI v1](backend/partie-1/openapi.v1.json) |
| Exploiter et vérifier P1 | [Opérations](backend/partie-1/OPERATIONS.md), [acceptation](backend/partie-1/ACCEPTATION.md), [conformité](backend/partie-1/CONFORMITE.md) |
| Comprendre la fabrique P2 | [Architecture](backend/partie-2/ARCHITECTURE.md), [contrats](backend/partie-2/CONTRATS.md), [manifeste](backend/partie-2/MANIFESTE.md) |
| Exploiter et vérifier P2 | [Opérations](backend/partie-2/OPERATIONS.md), [acceptation](backend/partie-2/ACCEPTATION.md), [validation](backend/partie-2/VALIDATION.md) |
| Comprendre et configurer P3 | [Index P3](backend/partie-3/README.md), [opérations](backend/partie-3/OPERATIONS.md), [parcours IDE](backend/partie-3/IDE.md) |
| Examiner les preuves P3 | [Validation](backend/partie-3/VALIDATION.md), [essais NVIDIA](backend/partie-3/NVIDIA.md), [revue](backend/partie-3/REVIEW.md), [corrections de PR](backend/partie-3/PR-REVIEW.md) |

Les recettes distinguent services réels, fournisseurs synthétiques et doublures. Les attestations restent liées à leurs versions et empreintes de sources. P4 et P5 sont des étapes prévues ; les documents P1–P3 ne démontrent pas une publication ou une maintenance en production.

Les documents internes de conception et de suivi restent locaux selon la politique du dépôt. Les liens de cet index visent uniquement des fichiers versionnés.
