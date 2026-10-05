//! Revalidate public source/OCI/signature bytes after durable archive restore.
use kyro_factory::{
    artifacts::{OciArtifact, SignedEvidence, SignedRelease},
    assembler::{SourceBundle, SourceManifest},
    crypto::{PublicIdentity, Trust},
    digest,
    service::TrustedKey,
};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
};

#[test]
#[ignore = "requires four restored public release archives from the real factory recipes"]
fn restored_public_releases_preserve_sources_oci_and_independent_signatures() {
    let directory = std::env::var_os("KYRO_P2_PUBLIC_RELEASE_ROOT").unwrap();
    let mut observations = Vec::new();
    for family in ["booking", "support", "stock", "all"] {
        let root = Path::new(&directory).join(family);
        let keys: Vec<TrustedKey> =
            serde_json::from_slice(&fs::read(root.join("trust.json")).unwrap()).unwrap();
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
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("artifact.json")).unwrap()).unwrap();
        // The live Store envelope deliberately has no Deserialize implementation;
        // only its portable public input is part of this archive contract.
        let artifact: kyro_store::factory::FactoryArtifactInput =
            serde_json::from_value(stored["artifact"].clone()).unwrap();
        let evidence: SignedEvidence =
            serde_json::from_value(artifact.signed_evidence.clone()).unwrap();
        let release: SignedRelease =
            serde_json::from_value(artifact.signed_release.clone()).unwrap();
        let manifest: SourceManifest =
            serde_json::from_value(artifact.source_manifest.clone()).unwrap();
        let image = OciArtifact::read(
            &root.join("oci"),
            &artifact.image_digest,
            release.release.binding.clone(),
        )
        .unwrap();
        image.verify().unwrap();
        release
            .verify(
                &image,
                &evidence,
                &evidence.evidence.criteria_digest,
                &evidence.evidence.required_checks,
                &trust,
            )
            .unwrap();
        assert_eq!(release.release.artifact_id, artifact.id);
        assert_eq!(digest(&release).unwrap(), artifact.release_digest);
        assert_eq!(digest(&evidence).unwrap(), artifact.evidence_digest);
        assert_eq!(
            release.release.binding.source_digest,
            artifact.source_digest
        );
        assert_eq!(digest(&manifest).unwrap(), artifact.source_digest);
        let files = manifest
            .files
            .keys()
            .map(|relative| {
                assert!(
                    Path::new(relative)
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)))
                );
                let mut target = root.join("source-snapshot");
                for part in Path::new(relative).components() {
                    target.push(part);
                    assert!(
                        !fs::symlink_metadata(&target)
                            .unwrap()
                            .file_type()
                            .is_symlink()
                    );
                }
                let metadata = fs::metadata(&target).unwrap();
                assert!(metadata.is_file() && metadata.len() <= 2097152);
                (relative.clone(), fs::read(target).unwrap())
            })
            .collect();
        let sources = SourceBundle { manifest, files };
        sources.verify().unwrap();
        observations.push(json!({"family":family,"image_digest":image.image_digest,"release_digest":artifact.release_digest,"source_digest":artifact.source_digest,"protected_checks":evidence.evidence.required_checks.len()}));
    }
    println!(
        "{}",
        json!({"kind":"restored_public_release_archives","restored_releases":4,"source_oci_signature_bindings_verified":true,"new_runtime_execution":false,"observations":observations})
    );
}
