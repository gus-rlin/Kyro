use kyro_app::contract::Schema;
use kyro_domain::{Environment, spec::AppSpec};
use kyro_factory::{assembler::*, catalogue::*, crypto::*, digest, digest_bytes, resolver::*};
use serde_json::json;
#[cfg(all(unix, feature = "test-support"))]
use std::process::{Command, Stdio};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};
use uuid::Uuid;

mod support;
use support::{Temp, keys, qualified_fixture, signer, trust};

#[test]
fn portable_git_bundle_contains_only_verified_reference_and_clones_the_exact_sources() {
    let f = Fixture::new();
    let lock = f.lock();
    let bundle = assemble(&f.temp.0, &f.spec, &lock, &f.catalogue, &trust(), &f.permit).unwrap();
    let directory = f.temp.0.join("portable");
    let (receipt, bytes) = kyro_factory::export::portable_bundle(&bundle, &directory).unwrap();
    fs::write(
        directory.join("untracked-private.txt"),
        b"must never travel",
    )
    .unwrap();
    assert!(kyro_factory::export::portable_bundle(&bundle, &directory).is_err());
    fs::remove_file(directory.join("untracked-private.txt")).unwrap();
    let (again, replayed) = kyro_factory::export::portable_bundle(&bundle, &directory).unwrap();
    assert_eq!(receipt, again);
    assert_eq!(bytes, replayed);
    fs::write(f.temp.0.join("application.bundle"), bytes).unwrap();
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "clone",
            "--quiet",
            "--branch",
            "kyro/application",
        ])
        .arg(f.temp.0.join("application.bundle"))
        .arg(f.temp.0.join("cloned"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (path, bytes) in &bundle.files {
        assert_eq!(
            fs::read(f.temp.0.join("cloned").join(path)).unwrap(),
            *bytes
        );
    }
    assert!(!f.temp.0.join("cloned/untracked-private.txt").exists());
}
struct Fixture {
    temp: Temp,
    catalogue: SignedCatalogue,
    permit: Permit,
    spec: AppSpec,
}
impl Fixture {
    fn new() -> Self {
        let temp = Temp::new();
        fs::create_dir_all(temp.0.join("crates/kyro-app/src")).unwrap();
        let sources = BTreeMap::from([
            ("Cargo.toml", "[workspace]\n"),
            ("Cargo.lock", "version = 4\n"),
            (
                "crates/kyro-app/Cargo.toml",
                "[package]\nname = \"kyro-app\"\n",
            ),
            ("crates/kyro-app/src/main.rs", "fn main() {}\n"),
        ]);
        let mut files = BTreeMap::new();
        for (path, text) in sources {
            fs::write(temp.0.join(path), text).unwrap();
            files.insert(path.to_owned(), digest_bytes(text.as_bytes()));
        }
        let source_digest = digest(&files).unwrap();
        let sign = signer(Purpose::Catalogue);
        let mut entries = BTreeMap::new();
        for id in ["B031", "B032"] {
            let m = ComponentManifest {
                schema_version: 1,
                id: id.into(),
                version: "1.0.0".into(),
                source_digest: source_digest.clone(),
                source_files: files.clone(),
                migration_digests: BTreeMap::new(),
                configuration: Schema::Object {
                    properties: BTreeMap::from([(
                        "reference".into(),
                        Schema::String {
                            max_length: 64,
                            values: BTreeSet::new(),
                        },
                    )]),
                    required: BTreeSet::new(),
                    additional: false,
                },
                dependencies: if id == "B032" {
                    BTreeMap::from([("B031".into(), BTreeSet::from(["1.0.0".into()]))])
                } else {
                    BTreeMap::new()
                },
                capabilities: BTreeSet::from(["data.read".into()]),
                ports: if id == "B032" {
                    BTreeMap::from([(
                        "entity".into(),
                        Port {
                            component_id: "B031".into(),
                            data_type: "record".into(),
                            required: true,
                        },
                    )])
                } else {
                    BTreeMap::new()
                },
                output_type: Some("record".into()),
                effects: BTreeSet::from(["read".into()]),
                qualification_digest: digest_bytes(
                    b"public synthetic unit fixture; no application qualification",
                ),
                criteria_digest: digest_bytes(b"protected synthetic signature fixture criteria"),
            };
            entries.insert(
                id.into(),
                BTreeMap::from([("1.0.0".into(), qualified_fixture(m))]),
            );
        }
        let catalogue = Catalogue {
            schema_version: 1,
            revision: 1,
            entries,
        };
        let signature = sign.sign(&catalogue).unwrap();
        let spec=serde_json::from_value(json!({"schema_version":1,"nodes":[{"id":"data","kind":"B031","properties":{"version":"1.0.0","configuration":{"reference":"test"}}},{"id":"list","kind":"B032","properties":{"version":"1.0.0","bindings":{"entity":"data"}}}],"preferences":{}})).unwrap();
        let permit = Permit {
            project_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            source_revision: 1,
            environment: Environment::Development,
            capabilities: BTreeSet::from(["data.read".into()]),
        };
        Self {
            temp,
            catalogue: SignedCatalogue {
                catalogue,
                signature,
            },
            permit,
            spec,
        }
    }
    fn lock(&self) -> kyro_domain::factory::SignedCompositionLock {
        seal(
            resolve(&self.spec, &self.catalogue, &trust(), &self.permit).unwrap(),
            &signer(Purpose::Composition),
        )
        .unwrap()
    }
    fn bundle(&self) -> SourceBundle {
        assemble(
            &self.temp.0,
            &self.spec,
            &self.lock(),
            &self.catalogue,
            &trust(),
            &self.permit,
        )
        .unwrap()
    }
}
#[test]
fn signatures_roles_keys_revocations_and_locked_context() {
    let f = Fixture::new();
    let lock = f.lock();
    verify(&lock, &f.spec, &f.catalogue, &trust(), &f.permit).unwrap();
    let mut tampered = lock.clone();
    tampered.lock.application_id = Uuid::new_v4();
    assert!(verify(&tampered, &f.spec, &f.catalogue, &trust(), &f.permit).is_err());
    let mut forged = lock.clone();
    forged.signature = signer(Purpose::Catalogue).sign(&forged.lock).unwrap();
    assert!(verify(&forged, &f.spec, &f.catalogue, &trust(), &f.permit).is_err());
    let revoked = f
        .catalogue
        .changed(
            "B031",
            "1.0.0",
            Admission::Revoked,
            &signer(Purpose::Catalogue),
        )
        .unwrap();
    assert_eq!(
        resolve(&f.spec, &revoked, &trust(), &f.permit)
            .unwrap_err()
            .code,
        "component_revoked"
    );
    assert!(
        revoked
            .changed(
                "B031",
                "1.0.0",
                Admission::Admitted,
                &signer(Purpose::Catalogue)
            )
            .is_err()
    );
    let mut wrong = f.permit.clone();
    wrong.source_revision += 1;
    assert!(verify(&lock, &f.spec, &f.catalogue, &trust(), &wrong).is_err());
    let duplicate = Trust::new(vec![
        PublicIdentity {
            id: "catalogue".into(),
            purposes: BTreeSet::from([Purpose::Catalogue]),
            pem: keys()[0].1.clone(),
        },
        PublicIdentity {
            id: "evidence".into(),
            purposes: BTreeSet::from([Purpose::Evidence]),
            pem: keys()[0].1.clone(),
        },
    ]);
    assert!(duplicate.is_err());
}
#[test]
fn admission_requires_independent_evidence_for_the_exact_manifest_and_criteria() {
    let mut f = Fixture::new();
    f.catalogue
        .catalogue
        .entries
        .get_mut("B031")
        .unwrap()
        .get_mut("1.0.0")
        .unwrap()
        .qualification = None;
    f.catalogue.signature = signer(Purpose::Catalogue)
        .sign(&f.catalogue.catalogue)
        .unwrap();
    assert_eq!(
        f.catalogue.verify(&trust(), 1).unwrap_err().code,
        "component_qualification_missing"
    );
    let mut f = Fixture::new();
    let entry = f
        .catalogue
        .catalogue
        .entries
        .get_mut("B031")
        .unwrap()
        .get_mut("1.0.0")
        .unwrap();
    let proof = entry.qualification.as_mut().unwrap();
    proof.signature = signer(Purpose::Catalogue)
        .sign(&proof.qualification)
        .unwrap();
    entry.component.manifest.qualification_digest = digest(proof).unwrap();
    entry.component.signature = signer(Purpose::Catalogue)
        .sign(&entry.component.manifest)
        .unwrap();
    f.catalogue.signature = signer(Purpose::Catalogue)
        .sign(&f.catalogue.catalogue)
        .unwrap();
    assert!(f.catalogue.verify(&trust(), 1).is_err());
    let mut f = Fixture::new();
    let entry = f
        .catalogue
        .catalogue
        .entries
        .get_mut("B031")
        .unwrap()
        .get_mut("1.0.0")
        .unwrap();
    entry.component.manifest.criteria_digest = digest_bytes(b"changed protected criteria");
    entry.component.signature = signer(Purpose::Catalogue)
        .sign(&entry.component.manifest)
        .unwrap();
    f.catalogue.signature = signer(Purpose::Catalogue)
        .sign(&f.catalogue.catalogue)
        .unwrap();
    assert_eq!(
        f.catalogue.verify(&trust(), 1).unwrap_err().code,
        "component_qualification_invalid"
    );
    let mut f = Fixture::new();
    let entry = f
        .catalogue
        .catalogue
        .entries
        .get_mut("B031")
        .unwrap()
        .get_mut("1.0.0")
        .unwrap();
    entry.admission = Admission::Pending;
    entry.qualification = None;
    assert_eq!(
        f.catalogue
            .changed(
                "B031",
                "1.0.0",
                Admission::Admitted,
                &signer(Purpose::Catalogue)
            )
            .unwrap_err()
            .code,
        "component_qualification_missing"
    );
}
#[test]
fn unavailable_capabilities_cycles_types_and_free_code_are_refused() {
    let f = Fixture::new();
    let mut permit = f.permit.clone();
    permit.capabilities.clear();
    assert_eq!(
        resolve(&f.spec, &f.catalogue, &trust(), &permit)
            .unwrap_err()
            .code,
        "component_capability_refused"
    );
    let mut code = f.spec.clone();
    code.nodes[0]
        .properties
        .insert("code".into(), json!("fn arbitrary() {}"));
    assert_eq!(
        resolve(&code, &f.catalogue, &trust(), &f.permit)
            .unwrap_err()
            .code,
        "node_properties_invalid"
    );
    let mut unknown = f.spec.clone();
    unknown.nodes[0].kind = "B999".into();
    assert_eq!(
        resolve(&unknown, &f.catalogue, &trust(), &f.permit)
            .unwrap_err()
            .code,
        "component_absent"
    );
    let mut cycle = f.spec.clone();
    cycle.nodes[0]
        .properties
        .insert("depends_on".into(), json!(["list"]));
    assert_eq!(
        resolve(&cycle, &f.catalogue, &trust(), &f.permit)
            .unwrap_err()
            .code,
        "composition_cycle"
    );
    let mut dangling = f.spec.clone();
    dangling.nodes[1]
        .properties
        .insert("bindings".into(), json!({"entity":"absent"}));
    assert_eq!(
        resolve(&dangling, &f.catalogue, &trust(), &f.permit)
            .unwrap_err()
            .code,
        "node_reference_missing"
    );
    let mut incompatible = f.spec.clone();
    incompatible.nodes.push(kyro_domain::spec::AppNode {
        id: "other_list".into(),
        kind: "B032".into(),
        properties: serde_json::from_value(json!({"version":"1.0.0","bindings":{"entity":"data"}}))
            .unwrap(),
    });
    incompatible.nodes[1]
        .properties
        .insert("bindings".into(), json!({"entity":"other_list"}));
    assert_eq!(
        resolve(&incompatible, &f.catalogue, &trust(), &f.permit)
            .unwrap_err()
            .code,
        "port_type_incompatible"
    );
}
#[test]
fn deterministic_sources_resume_and_tamper_refusal() {
    let f = Fixture::new();
    let a = f.bundle();
    let b = f.bundle();
    assert_eq!(a.manifest, b.manifest);
    assert_eq!(a.files, b.files);
    let first = f.temp.0.join("first");
    let second = f.temp.0.join("second");
    a.write(&first).unwrap();
    b.write(&second).unwrap();
    a.write(&first).unwrap();
    fs::write(first.join("application.json"), b"{}").unwrap();
    assert_eq!(a.write(&first).unwrap_err().code, "source_resume_diverged");
    fs::write(
        f.temp.0.join("crates/kyro-app/src/main.rs"),
        b"fn main() {panic!()}\n",
    )
    .unwrap();
    assert_eq!(
        assemble(
            &f.temp.0,
            &f.spec,
            &f.lock(),
            &f.catalogue,
            &trust(),
            &f.permit
        )
        .err()
        .unwrap()
        .code,
        "source_digest_mismatch"
    );
}

#[test]
fn source_manifest_cannot_relabel_the_lock_or_omit_a_migration() {
    let f = Fixture::new();
    let mut bundle = f.bundle();
    bundle.verify().unwrap();
    bundle.manifest.lock_digest = "0".repeat(64);
    assert_eq!(bundle.verify().unwrap_err().code, "source_bundle_corrupt");
    let mut bundle = f.bundle();
    bundle.manifest.migrations.insert(
        "crates/kyro-app/migrations/not_admitted.sql".into(),
        "0".repeat(64),
    );
    assert_eq!(bundle.verify().unwrap_err().code, "source_bundle_corrupt");
    let mut bundle = f.bundle();
    bundle
        .manifest
        .files
        .values_mut()
        .next()
        .unwrap()
        .components
        .insert("B158".into());
    assert_eq!(bundle.verify().unwrap_err().code, "source_bundle_corrupt");
}
#[test]
fn source_resume_refuses_extra_cargo_configuration_before_any_write() {
    let f = Fixture::new();
    let bundle = f.bundle();
    let path = f.temp.0.join("injected");
    fs::create_dir_all(path.join(".cargo")).unwrap();
    fs::write(
        path.join(".cargo/config.toml"),
        b"[build]\nrustc = 'untrusted'\n",
    )
    .unwrap();
    assert_eq!(
        bundle.write(&path).unwrap_err().code,
        "source_output_unexpected"
    );
    assert!(!path.join("Cargo.toml").exists());
}
#[test]
fn source_resume_recovers_only_exact_partial_atomic_files() {
    let f = Fixture::new();
    let bundle = f.bundle();
    let path = f.temp.0.join("partial");
    fs::create_dir(&path).unwrap();
    let temp = path.join(format!(".Cargo.toml.{}.tmp", Uuid::new_v4()));
    fs::write(&temp, &bundle.files["Cargo.toml"][..4]).unwrap();
    bundle.write(&path).unwrap();
    assert!(!temp.exists());
    assert_eq!(
        fs::read(path.join("Cargo.toml")).unwrap(),
        bundle.files["Cargo.toml"]
    );
    let temp = path.join(format!(".Cargo.toml.{}.tmp", Uuid::new_v4()));
    fs::write(&temp, b"bad").unwrap();
    assert_eq!(
        bundle.write(&path).unwrap_err().code,
        "source_resume_diverged"
    );
    assert!(temp.exists());
}
#[test]
fn signed_lock_preserves_preferences_and_rejects_tampering() {
    let mut f = Fixture::new();
    f.spec.preferences.insert("locale".into(), json!("fr-FR"));
    f.spec
        .preferences
        .insert("time_zone".into(), json!("Europe/Paris"));
    let mut lock = f.lock();
    assert_eq!(lock.lock.preferences["locale"], "fr-FR");
    verify(&lock, &f.spec, &f.catalogue, &trust(), &f.permit).unwrap();
    lock.lock
        .preferences
        .insert("locale".into(), json!("en-US"));
    assert!(verify(&lock, &f.spec, &f.catalogue, &trust(), &f.permit).is_err());
    f.spec
        .preferences
        .insert("time_zone".into(), json!("unknown/time_zone"));
    assert!(resolve(&f.spec, &f.catalogue, &trust(), &f.permit).is_err());
}
#[cfg(all(unix, feature = "test-support"))]
#[test]
fn git_export_resumes_after_commit_and_preserves_existing_references() {
    let f = Fixture::new();
    let bundle = f.bundle();
    let path = f.temp.0.join("export");
    assert_eq!(
        kyro_factory::export::interrupt_after_commit(&bundle, &path)
            .unwrap_err()
            .code,
        "export_interrupted_after_commit"
    );
    let result = kyro_factory::export::export(&bundle, &path).unwrap();
    assert_eq!(
        kyro_factory::export::export(&bundle, &path).unwrap(),
        result
    );
    let second = f.temp.0.join("another");
    assert_eq!(
        kyro_factory::export::export(&bundle, &second)
            .unwrap()
            .commit,
        result.commit
    );
    let foreign = f.temp.0.join("foreign");
    fs::create_dir(&foreign).unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&foreign)
            .arg("init")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        kyro_factory::export::export(&bundle, &foreign)
            .unwrap_err()
            .code,
        "existing_git_repository_refused"
    );
    assert!(!foreign.join("Cargo.toml").exists());
    let bad = f.temp.0.join("symlink");
    fs::create_dir(&bad).unwrap();
    std::os::unix::fs::symlink(
        f.temp.0.join("Cargo.toml"),
        bad.join("source-manifest.json"),
    )
    .unwrap();
    assert_eq!(
        bundle.write(&bad).unwrap_err().code,
        "source_output_refused"
    );
}
