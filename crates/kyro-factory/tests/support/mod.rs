// Shared integration fixture: each test binary consumes a different subset.
#![allow(dead_code)]
use kyro_factory::crypto::*;
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
};
use uuid::Uuid;
use zeroize::Zeroizing;
pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("kyro-factory-test-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if self.0.parent() == Some(std::env::temp_dir().as_path())
            && self
                .0
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("kyro-factory-test-"))
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
pub fn keys() -> &'static Vec<(Vec<u8>, Vec<u8>)> {
    static KEYS: OnceLock<Vec<(Vec<u8>, Vec<u8>)>> = OnceLock::new();
    KEYS.get_or_init(|| {
        (0..4)
            .map(|_| {
                let temp = Temp::new();
                let private = temp.0.join("private.pem");
                let public = temp.0.join("public.pem");
                assert!(
                    Command::new("openssl")
                        .args([
                            "genpkey",
                            "-algorithm",
                            "RSA",
                            "-pkeyopt",
                            "rsa_keygen_bits:2048",
                            "-out"
                        ])
                        .arg(&private)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .unwrap()
                        .success()
                );
                assert!(
                    Command::new("openssl")
                        .args(["pkey", "-pubout", "-in"])
                        .arg(&private)
                        .arg("-out")
                        .arg(&public)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .unwrap()
                        .success()
                );
                (fs::read(private).unwrap(), fs::read(public).unwrap())
            })
            .collect()
    })
}
pub fn signer(purpose: Purpose) -> Signer {
    let index = match purpose {
        Purpose::Catalogue => 0,
        Purpose::Composition => 1,
        Purpose::Evidence => 2,
        Purpose::Release => 3,
    };
    Signer::from_pem(
        format!("role-{index}"),
        purpose,
        Zeroizing::new(keys()[index].0.clone()),
    )
    .unwrap()
}
pub fn trust() -> Trust {
    Trust::new(
        [
            Purpose::Catalogue,
            Purpose::Composition,
            Purpose::Evidence,
            Purpose::Release,
        ]
        .into_iter()
        .enumerate()
        .map(|(i, p)| PublicIdentity {
            id: format!("role-{i}"),
            purposes: BTreeSet::from([p]),
            pem: keys()[i].1.clone(),
        })
        .collect(),
    )
    .unwrap()
}
/// Signature/serialization fixture only. No runtime qualification is inferred.
pub fn qualified_fixture(
    mut manifest: kyro_factory::catalogue::ComponentManifest,
) -> kyro_factory::catalogue::Entry {
    use kyro_factory::{catalogue::*, digest, digest_bytes};
    let receipt = CaseReceipt {
        input_digest: digest_bytes(b"public synthetic signature fixture input"),
        observed_digest: digest_bytes(b"fixture serialized, not an application scenario"),
        passed: true,
    };
    let qualification = ComponentQualification {
        kind: "component_qualification".into(),
        schema_version: 1,
        component_id: manifest.id.clone(),
        version: manifest.version.clone(),
        subject_digest: manifest.qualification_subject_digest().unwrap(),
        source_digest: manifest.source_digest.clone(),
        criteria_digest: manifest.criteria_digest.clone(),
        verifier_version: "kyro-component-verifier-1".into(),
        run_id: Uuid::new_v4(),
        cases: ["nominal", "refusal", "failure", "invariant"]
            .into_iter()
            .map(|s| (s.to_owned(), receipt.clone()))
            .collect(),
        report_digest: digest_bytes(b"synthetic signature fixture only"),
        validation_environment: "synthetic_integration".into(),
    };
    let proof = SignedComponentQualification {
        signature: signer(Purpose::Evidence).sign(&qualification).unwrap(),
        qualification,
    };
    manifest.qualification_digest = digest(&proof).unwrap();
    Entry {
        component: SignedManifest {
            signature: signer(Purpose::Catalogue).sign(&manifest).unwrap(),
            manifest,
        },
        admission: Admission::Admitted,
        qualification: Some(proof),
    }
}
