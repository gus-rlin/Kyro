mod support;
use kyro_factory::{artifacts::*, crypto::Purpose, digest, digest_bytes};
use std::collections::{BTreeMap, BTreeSet};
use support::{signer, trust};
use uuid::Uuid;
fn image() -> OciArtifact {
    // Synthetic serialization fixture. These bytes are not executed.
    let base: BTreeMap<_, _> = BASE_FILES
        .into_iter()
        .map(|p| (p.to_owned(), format!("public base: {p}").into_bytes()))
        .collect();
    let hashes: BTreeMap<_, _> = base
        .iter()
        .map(|(p, b)| (p.clone(), digest_bytes(b)))
        .collect();
    let config = b"{\"synthetic_serialization_fixture\":true}";
    let hash = digest_bytes(b"synthetic serialization binding");
    let binding = ArtifactBinding {
        schema_version: 1,
        project_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        revision: 5,
        environment: kyro_domain::Environment::Development,
        lock_digest: hash.clone(),
        source_digest: hash.clone(),
        migration_digest: hash.clone(),
        configuration_digest: digest_bytes(config),
        runtime_base_digest: digest(&hashes).unwrap(),
        tools_image_digest: hash.clone(),
        sandbox_profile_digest: hash,
    };
    let binaries = BINARIES
        .into_iter()
        .map(|p| (p.to_owned(), format!("\x7fELFsynthetic-{p}").into_bytes()))
        .collect();
    package(binding, &base, &binaries, config).unwrap()
}
#[test]
fn image_bytes_bind_every_descriptor_and_runtime_file() {
    let mut image = image();
    image.verify().unwrap();
    let runtime = image.runtime_files().unwrap();
    assert_eq!(runtime.len(), 9);
    let layer = image
        .files
        .iter()
        .max_by_key(|(_, v)| v.len())
        .unwrap()
        .0
        .clone();
    image.files.get_mut(&layer).unwrap()[513] ^= 1;
    assert!(image.verify().is_err());
    let mut image = self::image();
    image.files.insert("extra.txt".into(), b"extra".to_vec());
    assert!(image.verify().is_err());
    let mut image = self::image();
    image.binding.revision += 1;
    assert!(image.verify().is_err());
    let mut image = self::image();
    image.image_digest = format!("sha256:{}", digest_bytes(b"different candidate"));
    assert!(image.verify().is_err());
}
#[test]
fn oci_loading_binds_the_protected_reference_and_rejects_symlinks() {
    let image = image();
    let root = support::Temp::new();
    let dir = root.0.join("oci");
    image.write(&dir).unwrap();
    OciArtifact::read(&dir, &image.image_digest, image.binding.clone()).unwrap();
    let mut wrong = image.binding.clone();
    wrong.application_id = Uuid::new_v4();
    assert!(OciArtifact::read(&dir, &image.image_digest, wrong).is_err());
    assert!(
        OciArtifact::read(
            &dir,
            &format!("sha256:{}", digest_bytes(b"other image")),
            image.binding.clone()
        )
        .is_err()
    );
    #[cfg(unix)]
    {
        let index = std::fs::read(dir.join("index.json")).unwrap();
        std::fs::write(root.0.join("index"), index).unwrap();
        std::fs::remove_file(dir.join("index.json")).unwrap();
        std::os::unix::fs::symlink(root.0.join("index"), dir.join("index.json")).unwrap();
        assert!(OciArtifact::read(&dir, &image.image_digest, image.binding).is_err());
    }
}
#[test]
fn evidence_and_release_refuse_builder_keys_edited_reports_and_changed_images() {
    let image = image();
    let criteria = digest_bytes(b"protected independent criteria");
    let required = BTreeSet::from([
        "runtime_http".into(),
        "private_rls".into(),
        "composition_behavior".into(),
    ]);
    let e = EvidenceBundle {
        schema_version: 1,
        image_digest: image.image_digest.clone(),
        binding: image.binding.clone(),
        criteria_digest: criteria.clone(),
        verifier_version: "kyro-verifier-1".into(),
        sandbox_run_id: Uuid::new_v4(),
        required_checks: required.clone(),
        passed_checks: required.clone(),
        observed_report_digest: digest_bytes(
            b"synthetic signature fixture; no runtime qualification",
        ),
        started_at: chrono::Utc::now(),
        finished_at: chrono::Utc::now(),
    };
    let wrong = SignedEvidence {
        signature: signer(Purpose::Composition).sign(&e).unwrap(),
        evidence: e.clone(),
    };
    assert!(
        wrong
            .verify(&image, &criteria, &required, &trust())
            .is_err()
    );
    assert!(
        SignedRelease::create(
            Uuid::new_v4(),
            &image,
            &wrong,
            &criteria,
            &required,
            &trust(),
            &signer(Purpose::Release)
        )
        .is_err()
    );
    let mut evidence = SignedEvidence {
        signature: signer(Purpose::Evidence).sign(&e).unwrap(),
        evidence: e,
    };
    evidence
        .verify(&image, &criteria, &required, &trust())
        .unwrap();
    let release = SignedRelease::create(
        Uuid::new_v4(),
        &image,
        &evidence,
        &criteria,
        &required,
        &trust(),
        &signer(Purpose::Release),
    )
    .unwrap();
    release
        .verify(&image, &evidence, &criteria, &required, &trust())
        .unwrap();
    assert!(
        release
            .verify(
                &image,
                &evidence,
                &digest_bytes(b"changed criteria"),
                &required,
                &trust()
            )
            .is_err()
    );
    evidence.evidence.passed_checks.clear();
    assert!(
        evidence
            .verify(&image, &criteria, &required, &trust())
            .is_err()
    );
    assert!(
        release
            .verify(&image, &evidence, &criteria, &required, &trust())
            .is_err()
    );
}
