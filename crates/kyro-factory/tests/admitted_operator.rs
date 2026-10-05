//! Uses the actual retained qualification campaign, never signature fixtures.
use kyro_domain::{
    Environment,
    spec::{AppNode, AppSpec},
};
use kyro_factory::{
    assembler::{assemble, collect_runtime_sources},
    builtins,
    catalogue::SignedCatalogue,
    crypto::{PublicIdentity, Purpose, Trust},
    digest,
    resolver::{Permit, resolve, seal},
    service::{TrustedKey, load_signer},
};
use serde_json::json;
use std::{collections::BTreeSet, fs, path::Path};
use uuid::Uuid;

#[test]
#[ignore = "requires actual admitted catalogue, public trust and operator composition key outside the repository"]
fn actual_admitted_registry_resolves_and_assembles_three_reusing_compositions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let catalogue: SignedCatalogue = serde_json::from_slice(
        &fs::read(std::env::var_os("KYRO_P2_ADMITTED_CATALOGUE").unwrap()).unwrap(),
    )
    .unwrap();
    let keys: Vec<TrustedKey> = serde_json::from_slice(
        &fs::read(std::env::var_os("KYRO_FACTORY_TRUST_FILE").unwrap()).unwrap(),
    )
    .unwrap();
    let trust = Trust::new(
        keys.into_iter()
            .map(|key| PublicIdentity {
                id: key.id,
                purposes: BTreeSet::from([key.purpose]),
                pem: key.public_pem.into_bytes(),
            })
            .collect(),
    )
    .unwrap();
    catalogue
        .verify(&trust, catalogue.catalogue.revision)
        .unwrap();
    assert_eq!(catalogue.catalogue.entries.len(), 147);
    let source_digest = digest(&collect_runtime_sources(root).unwrap()).unwrap();
    for (id, versions) in &catalogue.catalogue.entries {
        assert_eq!(versions.len(), 1);
        let manifest = catalogue.admitted(id, builtins::VERSION).unwrap();
        assert_eq!(manifest.source_digest, source_digest);
        assert_eq!(
            versions[builtins::VERSION]
                .qualification
                .as_ref()
                .unwrap()
                .qualification
                .validation_environment,
            "synthetic_integration"
        );
    }
    let key = std::env::var_os("KYRO_FACTORY_COMPOSITION_KEY_FILE").unwrap();
    let signer = load_signer(
        Path::new(&key),
        std::env::var("KYRO_FACTORY_COMPOSITION_KEY_ID").unwrap(),
        Purpose::Composition,
    )
    .unwrap();
    let shared = [
        "B031", "B032", "B033", "B034", "B035", "B050", "B051", "B054", "B055", "B056",
    ];
    let mut common = None;
    let mut observations = Vec::new();
    for (family, additional) in [
        (
            "booking",
            &["B111", "B112", "B113", "B114", "B115", "B119"][..],
        ),
        ("support", &["B013", "B014", "B133"][..]),
        ("stock", &["B121", "B122", "B123", "B124", "B130"][..]),
    ] {
        let ids: BTreeSet<_> = shared
            .iter()
            .copied()
            .chain(additional.iter().copied())
            .collect();
        let spec = AppSpec {
            nodes: ids
                .iter()
                .map(|id| AppNode {
                    id: format!("block_{}", id.to_ascii_lowercase()),
                    kind: id.to_string(),
                    properties: json!({"version":builtins::VERSION,"configuration":{}})
                        .as_object()
                        .unwrap()
                        .clone(),
                })
                .collect(),
            ..AppSpec::default()
        };
        let permit = Permit {
            project_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            source_revision: 1,
            environment: Environment::Development,
            capabilities: builtins::capabilities(),
        };
        let lock = seal(
            resolve(&spec, &catalogue, &trust, &permit).unwrap(),
            &signer,
        )
        .unwrap();
        let reused: Vec<_> = shared
            .iter()
            .map(|id| lock.lock.components[*id].clone())
            .collect();
        if let Some(previous) = &common {
            assert_eq!(previous, &reused);
        } else {
            common = Some(reused);
        }
        let first = assemble(root, &spec, &lock, &catalogue, &trust, &permit).unwrap();
        let again = assemble(root, &spec, &lock, &catalogue, &trust, &permit).unwrap();
        assert_eq!(first.manifest, again.manifest);
        assert_eq!(first.files, again.files);
        observations.push(json!({"family":family,"components":lock.lock.components.len(),"source_manifest_digest":digest(&first.manifest).unwrap(),"deterministic_sources":true}));
    }
    println!(
        "{}",
        json!({"kind":"actual_admitted_operator_compositions","catalogue_revision":catalogue.catalogue.revision,
        "admitted":147,"source_digest":source_digest,"shared_components":shared,"compositions":observations,
        "signature_fixtures":false,"scope":"local synthetic qualification; no new build or provider calls"})
    );
}
