//! Reuses a previously observed OCI candidate, not a builder-provided report.
//! Disposable workload credentials and signing fixtures have no production use.
mod support;
use kyro_factory::{
    artifacts::*, crypto::Purpose, digest_bytes, sandbox::SandboxConfig, verifier::*,
};
use std::{collections::BTreeMap, path::Path};

#[tokio::test]
#[ignore = "requires the controller, prepared tools root and observed OCI candidate"]
async fn candidate_is_tested_in_a_fresh_protected_postgres_sentry() {
    let config: SandboxConfig =
        serde_json::from_str(&std::env::var("KYRO_P2_SANDBOX_CONFIG").expect("prepared profile"))
            .unwrap();
    // This fixture's expected values are pinned to build-08's protected harness
    // receipt. The unsigned file beside the OCI candidate is never consulted.
    let binding08:ArtifactBinding=serde_json::from_value(serde_json::json!({
        "schema_version":1,"project_id":"815b30e0-2426-45a1-ae95-e02088508575",
        "application_id":"e1f36e8e-2981-474d-915b-511e98507717","revision":0,"environment":"development",
        "lock_digest":"1d69af7b6282c4c6f884bffceee35b65bf9a40ff96a39a686d23c825f9f74de5",
        "source_digest":"cac7b8e022fc12c4256ffc1742ca9fc73bb2c8a10430f00e6995914b6024e693",
        "migration_digest":"2913fa1327d87975521cadb7076cd64e52d5d9a2925c4f15ba803ae6679bba01",
        "configuration_digest":"0c3c33ff7fcc59113a38683c1e8b57ddbd2a521d791d346a59b8498b0869e3d5",
        "runtime_base_digest":"28ba8b59979827812bec5b6dfa9092cb1c0ef7fb0867525afcd0be6aa0fbc6cc",
        "tools_image_digest":"1041325424523de591e16b820dc9778db7644a2dc5517341b683132fde1193bc",
        "sandbox_profile_digest":"a3c09922b4f86b2a1e67522f1b5567b02b5193aafc70771629156e5e3d5cc39e"
    })).unwrap();
    let binding09:ArtifactBinding=serde_json::from_value(serde_json::json!({
        "schema_version":1,"project_id":"ede28fbf-5711-4696-b361-c2e2825b347e","application_id":"368e7ab3-9fae-41ed-8a46-dca1813e7247","revision":0,"environment":"development",
        "lock_digest":"8d41b46905584733d426f299ec65ae58a002358848237b4a3b852482f2f5c762",
        "source_digest":"c45a2e943f4d2ca74375bf3de857cdd87333a036499b953beb2cbf1986086773",
        "migration_digest":"2913fa1327d87975521cadb7076cd64e52d5d9a2925c4f15ba803ae6679bba01",
        "configuration_digest":"0c3c33ff7fcc59113a38683c1e8b57ddbd2a521d791d346a59b8498b0869e3d5",
        "runtime_base_digest":"28ba8b59979827812bec5b6dfa9092cb1c0ef7fb0867525afcd0be6aa0fbc6cc",
        "tools_image_digest":"1041325424523de591e16b820dc9778db7644a2dc5517341b683132fde1193bc",
        "sandbox_profile_digest":"c506766b3aef520a898e7bf8bd378bba378e6deab19491ce996805eccd91fa39"
    })).unwrap();
    let (directory, image_digest, binding) =
        match std::env::var("KYRO_P2_VERIFY_OBSERVED_CANDIDATE").as_deref() {
            Ok("build09") => (
                "/tmp/kyro-p2-candidate-current-20261005",
                "sha256:c3fc2dcce34204645967e6b3153d6ae687d36e0095439d2ac8b2bc9c076509b0",
                binding09,
            ),
            Err(std::env::VarError::NotPresent) | Ok("build08") => (
                "/tmp/kyro-p2-candidate-20261005",
                "sha256:a5e9058e930a945a08c1012fd675686f5d413fed34b4ffd50483724074cff408",
                binding08,
            ),
            _ => panic!("unknown observed fixture"),
        };
    let image = OciArtifact::read(Path::new(directory), image_digest, binding).unwrap();
    let migrations: BTreeMap<_, _> = std::fs::read_dir("/workspace/crates/kyro-app/migrations")
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let path = format!(
                "crates/kyro-app/migrations/{}",
                e.file_name().to_str().unwrap()
            );
            (path, std::fs::read(e.path()).unwrap())
        })
        .collect();
    let criteria = ProtectedCriteria::records(&config).unwrap();
    let verifier = ProtectedVerifier::new(config).unwrap();
    let observed = verifier
        .records(&image, &migrations)
        .await
        .unwrap_or_else(|error| {
            if let Some(log) = &error.execution {
                eprintln!(
                    "{}\n{}",
                    String::from_utf8_lossy(&log.stdout),
                    String::from_utf8_lossy(&log.stderr)
                );
            }
            panic!("{error}");
        });
    println!("{}", serde_json::to_string(observed.report()).unwrap());
    let evidence = observed
        .attest(&support::signer(Purpose::Evidence))
        .unwrap();
    evidence
        .verify(
            &image,
            &criteria.digest().unwrap(),
            criteria.required_checks(),
            &support::trust(),
        )
        .unwrap();
    let release = SignedRelease::create(
        uuid::Uuid::new_v4(),
        &image,
        &evidence,
        &criteria.digest().unwrap(),
        criteria.required_checks(),
        &support::trust(),
        &support::signer(Purpose::Release),
    )
    .unwrap();
    release
        .verify(
            &image,
            &evidence,
            &criteria.digest().unwrap(),
            criteria.required_checks(),
            &support::trust(),
        )
        .unwrap();
    let mut wrong_migrations = migrations;
    let first = wrong_migrations.values_mut().next().unwrap();
    first.push(b' ');
    assert_eq!(
        verifier
            .records(&image, &wrong_migrations)
            .await
            .err()
            .unwrap()
            .code,
        "verifier_inputs_changed"
    );
    println!(
        "{}",
        serde_json::json!({"kind":"protected_verification_and_release","checks":criteria.required_checks().len(),
        "evidence_digest":kyro_factory::digest(&evidence).unwrap(),"release_digest":kyro_factory::digest(&release).unwrap(),
        "changed_migration_refused":true,"driver_digest":digest_bytes(include_bytes!("../../../scripts/p2/sandbox/verify-records.mjs"))})
    );
}
