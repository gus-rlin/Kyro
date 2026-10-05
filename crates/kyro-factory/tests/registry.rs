mod support;
use kyro_domain::{
    Environment,
    spec::{AppNode, AppSpec},
};
use kyro_factory::{
    builtins,
    crypto::Purpose,
    digest,
    resolver::{Permit, resolve},
};
use serde_json::json;
use std::{collections::BTreeSet, path::Path};
use uuid::Uuid;

#[test]
fn resolver_refuses_invalid_fixed_values_and_bound_references_before_build() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let sign = support::signer(Purpose::Catalogue);
    let mut catalogue = builtins::pending(root, 1, &sign).unwrap().catalogue;
    // Signature/admission doubles only: this test checks semantic resolution,
    // never claims runtime qualification for these fabricated receipts.
    for versions in catalogue.entries.values_mut() {
        let entry = versions.get_mut(builtins::VERSION).unwrap();
        *entry = support::qualified_fixture(entry.component.manifest.clone());
    }
    let catalogue = kyro_factory::catalogue::SignedCatalogue {
        signature: sign.sign(&catalogue).unwrap(),
        catalogue,
    };
    let permit = Permit {
        project_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        source_revision: 1,
        environment: Environment::Development,
        capabilities: catalogue
            .catalogue
            .entries
            .values()
            .flat_map(|v| v[builtins::VERSION].component.manifest.capabilities.clone())
            .collect(),
    };
    let spec = |configuration| AppSpec {
        nodes: vec![AppNode {
            id: "records".into(),
            kind: "B031".into(),
            properties: json!({"version":builtins::VERSION,"configuration":configuration})
                .as_object()
                .unwrap()
                .clone(),
        }],
        ..AppSpec::default()
    };
    assert!(
        resolve(
            &spec(json!({"defaults":{"entity":"ticket"},"reference":"ticket"})),
            &catalogue,
            &support::trust(),
            &permit
        )
        .is_ok()
    );
    for name in ["bad name", "../ticket", "UPPER", "1ticket"] {
        assert!(
            resolve(
                &spec(json!({"defaults":{"entity":name}})),
                &catalogue,
                &support::trust(),
                &permit
            )
            .is_err(),
            "invalid fixed entity {name}"
        );
    }
    let booking_spec = |slot: &str, bound: bool| {
        let mut nodes = Vec::new();
        for id in ["B111", "B112", "B113", "B114"] {
            let configuration = if id == "B114" {
                if bound {
                    json!({})
                } else {
                    json!({"defaults":{"slot_id":slot},"allowed_actions":["reserve"]})
                }
            } else if id == "B113" {
                json!({"reference":slot})
            } else {
                json!({})
            };
            let bindings = if id == "B114" && bound {
                json!({"slot_id":"b113"})
            } else {
                json!({})
            };
            nodes.push(AppNode{id:id.to_ascii_lowercase(),kind:id.into(),properties:json!({"version":builtins::VERSION,"configuration":configuration,"bindings":bindings}).as_object().unwrap().clone()});
        }
        AppSpec {
            nodes,
            ..AppSpec::default()
        }
    };
    for bound in [false, true] {
        let valid = resolve(
            &booking_spec(&Uuid::new_v4().to_string(), bound),
            &catalogue,
            &support::trust(),
            &permit,
        );
        assert!(valid.is_ok(), "valid slot, bound={bound}: {valid:?}");
        for value in ["not-a-uuid", "00000000-0000-0000-0000-000000000000"] {
            assert!(
                resolve(
                    &booking_spec(value, bound),
                    &catalogue,
                    &support::trust(),
                    &permit
                )
                .is_err(),
                "invalid slot reference {value}, bound={bound}"
            );
        }
    }
}
#[test]
fn complete_registry_is_signed_but_every_unqualified_component_remains_unavailable() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let catalogue = builtins::pending(root, 1, &support::signer(Purpose::Catalogue)).unwrap();
    catalogue.verify(&support::trust(), 1).unwrap();
    assert_eq!(catalogue.catalogue.entries.len(), 147);
    let entry = |id: &str| {
        &catalogue.catalogue.entries[id][builtins::VERSION]
            .component
            .manifest
    };
    assert!(
        entry("B031")
            .configuration
            .validate(&json!({"defaults":{"entity":"ticket"}}))
            .is_ok()
    );
    assert!(
        entry("B153")
            .configuration
            .validate(&json!({"defaults":{"entity":"ticket"}}))
            .is_err()
    );
    assert_eq!(entry("B142").effects, BTreeSet::from(["read".into()]));
    assert!(entry("B153").effects.contains("network"));
    assert!(entry("B114").dependencies.contains_key("B113"));
    assert!(
        entry("B162")
            .configuration
            .validate(&json!({"allowed_actions":["admit_component"]}))
            .is_ok()
    );
    assert!(
        entry("B162")
            .configuration
            .validate(&json!({"allowed_actions":["assemble"]}))
            .is_err()
    );
    assert!(
        catalogue
            .catalogue
            .entries
            .values()
            .flat_map(|versions| versions.values())
            .all(
                |entry| entry.admission == kyro_factory::catalogue::Admission::Pending
                    && entry.qualification.is_none()
            )
    );
    let spec = AppSpec {
        nodes: vec![AppNode {
            id: "records".into(),
            kind: "B031".into(),
            properties: json!({"version":builtins::VERSION,"configuration":{}})
                .as_object()
                .unwrap()
                .clone(),
        }],
        ..AppSpec::default()
    };
    let permit = Permit {
        project_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        source_revision: 1,
        environment: Environment::Development,
        capabilities: BTreeSet::from(["component.b031".into()]),
    };
    assert!(resolve(&spec, &catalogue, &support::trust(), &permit).is_err());
    println!(
        "{}",
        json!({"kind":"complete_pending_registry","entries":147,"digest":digest(&catalogue.catalogue).unwrap(),"bytes":serde_json::to_vec(&catalogue).unwrap().len(),"admitted":0})
    );
}

#[test]
fn admission_requires_distinct_evidence_and_exact_immutable_subject() {
    use kyro_factory::{catalogue::Admission, crypto::Purpose};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let sign = support::signer(Purpose::Catalogue);
    let evidence = support::signer(Purpose::Evidence);
    let trust = support::trust();
    let pending = builtins::pending(root, 1, &sign).unwrap();
    let manifest = pending.catalogue.entries["B031"][builtins::VERSION]
        .component
        .manifest
        .clone();
    // This exercises signatures only; actual integration qualification is a
    // separate report, never inferred from this public unit fixture.
    let proof = support::qualified_fixture(manifest).qualification.unwrap();
    assert!(
        pending
            .changed("B031", builtins::VERSION, Admission::Admitted, &sign)
            .is_err()
    );
    let admitted = pending.admit(proof.clone(), &trust, &sign).unwrap();
    assert_eq!(admitted.catalogue.revision, 2);
    admitted.admitted("B031", builtins::VERSION).unwrap();
    assert_eq!(
        admitted
            .admit(proof.clone(), &trust, &sign)
            .unwrap()
            .catalogue
            .revision,
        2
    );
    for mutation in ["source", "failed_case", "missing_case", "criteria"] {
        let mut wrong = proof.clone();
        match mutation {
            "source" => wrong.qualification.source_digest = "a".repeat(64),
            "failed_case" => wrong.qualification.cases.get_mut("failure").unwrap().passed = false,
            "missing_case" => {
                wrong.qualification.cases.remove("invariant");
            }
            _ => wrong.qualification.criteria_digest = "b".repeat(64),
        }
        wrong.signature = evidence.sign(&wrong.qualification).unwrap();
        assert!(pending.admit(wrong, &trust, &sign).is_err(), "{mutation}");
    }
    assert!(pending.admit(proof.clone(), &trust, &evidence).is_err());
    let revoked = admitted
        .changed("B031", builtins::VERSION, Admission::Revoked, &sign)
        .unwrap();
    assert!(revoked.admit(proof, &trust, &sign).is_err());
}

#[test]
fn publication_cannot_erase_versions_replace_subjects_or_restore_revocation() {
    use kyro_factory::catalogue::{Admission, SignedCatalogue};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let sign = support::signer(Purpose::Catalogue);
    let trust = support::trust();
    let original = builtins::pending(root, 1, &sign).unwrap();
    let revoked = original
        .changed("B031", builtins::VERSION, Admission::Revoked, &sign)
        .unwrap();
    original.verify_successor(&revoked, &trust).unwrap();
    let fresh = builtins::pending(root, 3, &sign).unwrap();
    assert!(revoked.verify_successor(&fresh, &trust).is_err());
    for change in ["erase", "source", "proof"] {
        let mut next = revoked.catalogue.clone();
        next.revision = 3;
        match change {
            "erase" => {
                next.entries.remove("B031");
            }
            "source" => {
                let entry = next
                    .entries
                    .get_mut("B032")
                    .unwrap()
                    .get_mut(builtins::VERSION)
                    .unwrap();
                entry.component.manifest.effects.insert("network".into());
                entry.component.signature = sign.sign(&entry.component.manifest).unwrap();
            }
            _ => {
                next.entries
                    .get_mut("B031")
                    .unwrap()
                    .get_mut(builtins::VERSION)
                    .unwrap()
                    .admission = Admission::Pending;
            }
        }
        let signed = SignedCatalogue {
            signature: sign.sign(&next).unwrap(),
            catalogue: next,
        };
        assert!(
            revoked.verify_successor(&signed, &trust).is_err(),
            "{change}"
        );
    }
}

#[test]
fn qualification_batch_is_atomic_bounded_and_replayable() {
    use kyro_factory::catalogue::Admission;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let sign = support::signer(Purpose::Catalogue);
    let trust = support::trust();
    let pending = builtins::pending(root, 1, &sign).unwrap();
    let proofs: Vec<_> = ["B031", "B032"]
        .into_iter()
        .map(|id| {
            support::qualified_fixture(
                pending.catalogue.entries[id][builtins::VERSION]
                    .component
                    .manifest
                    .clone(),
            )
            .qualification
            .unwrap()
        })
        .collect();
    assert!(pending.admit_batch(vec![], &trust, &sign).is_err());
    assert!(
        pending
            .admit_batch(vec![proofs[0].clone(), proofs[0].clone()], &trust, &sign)
            .is_err()
    );
    let mut failed = proofs.clone();
    failed[1]
        .qualification
        .cases
        .get_mut("failure")
        .unwrap()
        .passed = false;
    failed[1].signature = support::signer(Purpose::Evidence)
        .sign(&failed[1].qualification)
        .unwrap();
    assert!(pending.admit_batch(failed, &trust, &sign).is_err());
    assert_eq!(
        pending.catalogue.entries["B031"][builtins::VERSION].admission,
        Admission::Pending
    );
    let admitted = pending.admit_batch(proofs.clone(), &trust, &sign).unwrap();
    assert_eq!(admitted.catalogue.revision, 2);
    pending.verify_successor(&admitted, &trust).unwrap();
    for id in ["B031", "B032"] {
        admitted.admitted(id, builtins::VERSION).unwrap();
    }
    assert_eq!(
        admitted
            .admit_batch(proofs, &trust, &sign)
            .unwrap()
            .catalogue
            .revision,
        2
    );
}
