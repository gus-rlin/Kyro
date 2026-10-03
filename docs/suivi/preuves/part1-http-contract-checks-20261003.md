# Contrôles finaux des contrats P1 HTTP

- Date : 2026-10-03 (Europe/Paris).
- Révision source intégrée : `gus-rlin/backend-p1@de3529024fda85f9449631b1cb6eb3bb85dcc1bd`.
- Révision vérifiée : `97736a4e9116cb5328c1a3d3c27812c143eb3b3a` (merge conservant l’historique dans `gus-rlin/part1-http`).
- Outils : Node.js v24.18.0; Python 3.10.6; `openapi-spec-validator` 0.8.4; cargo 1.96.1 (356927216 2026-06-26); rustc 1.96.1 (31fca3adb 2026-06-26).
- Portée : vérification de contrat et de frontière de dépendances uniquement; ne vaut pas validation d’exécution runtime.

## Node OpenAPI route/schema checker

- Commande : `node scripts/check-openapi.mjs`
- Code de sortie : `0`
```text
{"status":"ok","version":"1.0.0","pathCount":27,"operationCount":34}
```

## Normative OpenAPI validator

- Commande : `python -m openapi_spec_validator docs/backend/partie-1/openapi.v1.json`
- Code de sortie : `0`
```text
docs/backend/partie-1/openapi.v1.json: OK
```

## Domain dependency guard graph

- Commande : `cargo tree --locked -p kyro-domain --edges normal --prefix none`
- Code de sortie : `0`
```text
kyro-domain v0.1.0 (<worktree>\crates\kyro-domain)
chrono v0.4.44
num-traits v0.2.19
serde v1.0.228
serde_core v1.0.228
serde_derive v1.0.228 (proc-macro)
proc-macro2 v1.0.107
unicode-ident v1.0.26
quote v1.0.47
proc-macro2 v1.0.107 (*)
syn v2.0.119
proc-macro2 v1.0.107 (*)
quote v1.0.47 (*)
unicode-ident v1.0.26
windows-link v0.2.1
serde v1.0.228 (*)
serde_json v1.0.149
itoa v1.0.18
memchr v2.8.3
serde_core v1.0.228
zmij v1.0.23
thiserror v2.0.18
thiserror-impl v2.0.18 (proc-macro)
proc-macro2 v1.0.107 (*)
quote v1.0.47 (*)
syn v2.0.119 (*)
url v2.5.8
form_urlencoded v1.2.2
percent-encoding v2.3.2
idna v1.1.0
idna_adapter v1.2.2
icu_normalizer v2.3.0
icu_collections v2.3.0
displaydoc v0.2.7 (proc-macro)
proc-macro2 v1.0.107 (*)
quote v1.0.47 (*)
syn v3.0.6
proc-macro2 v1.0.107 (*)
quote v1.0.47 (*)
unicode-ident v1.0.26
potential_utf v0.1.6
zerovec v0.11.8
yoke v0.8.3
stable_deref_trait v1.2.1
yoke-derive v0.8.4 (proc-macro)
proc-macro2 v1.0.107 (*)
quote v1.0.47 (*)
syn v3.0.6 (*)
synstructure v0.14.0
proc-macro2 v1.0.107 (*)
quote v1.0.47 (*)
syn v3.0.6 (*)
zerofrom v0.1.8
zerofrom-derive v0.1.8 (proc-macro)
proc-macro2 v1.0.107 (*)
quote v1.0.47 (*)
syn v3.0.6 (*)
synstructure v0.14.0 (*)
zerofrom v0.1.8 (*)
zerovec-derive v0.11.6 (proc-macro)
proc-macro2 v1.0.107 (*)
quote v1.0.47 (*)
syn v3.0.6 (*)
utf8_iter v1.0.4
yoke v0.8.3 (*)
zerofrom v0.1.8 (*)
zerovec v0.11.8 (*)
icu_normalizer_data v2.3.0
icu_provider v2.3.1
displaydoc v0.2.7 (proc-macro) (*)
icu_locale_core v2.3.0
displaydoc v0.2.7 (proc-macro) (*)
litemap v0.8.3
tinystr v0.8.4
displaydoc v0.2.7 (proc-macro) (*)
zerovec v0.11.8 (*)
writeable v0.6.4
zerovec v0.11.8 (*)
writeable v0.6.4
yoke v0.8.3 (*)
zerofrom v0.1.8 (*)
zerotrie v0.2.5
displaydoc v0.2.7 (proc-macro) (*)
yoke v0.8.3 (*)
zerofrom v0.1.8 (*)
zerovec v0.11.8 (*)
smallvec v1.16.2
zerovec v0.11.8 (*)
icu_properties v2.3.0
displaydoc v0.2.7 (proc-macro) (*)
icu_collections v2.3.0 (*)
icu_locale_core v2.3.0 (*)
icu_properties_data v2.3.0
icu_provider v2.3.1 (*)
zerotrie v0.2.5 (*)
zerovec v0.11.8 (*)
smallvec v1.16.2
utf8_iter v1.0.4
percent-encoding v2.3.2
uuid v1.20.0
getrandom v0.3.4
cfg-if v1.0.5
serde_core v1.0.228
```

Le graphe `kyro-domain` ne contient aucune ligne correspondant à `^(axum|sqlx) v`; le garde-fou passe.
Le checker Node trouve 27 chemins et 34 opérations dans OpenAPI 1.0.0. Le validateur normatif accepte le document.
Limite : ces contrôles statiques ne couvrent ni les réponses runtime, ni l’authentification/autorisation effective, ni l’E2E en cours dans un autre worktree.
