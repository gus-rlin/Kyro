//! Operator-only signed registry management. Generation never implies admission.
use kyro_factory::{
    builtins,
    catalogue::{Admission, ComponentQualification, SignedCatalogue, SignedComponentQualification},
    crypto::{PublicIdentity, Purpose, Trust},
    service::{TrustedKey, load_signer, read_regular},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("catalogue operation refused; private details withheld");
        std::process::exit(1);
    }
}
fn trust() -> Result<Trust, ()> {
    let path = std::env::var_os("KYRO_FACTORY_TRUST_FILE").ok_or(())?;
    let keys: Vec<TrustedKey> =
        serde_json::from_slice(&read_regular(Path::new(&path), 262144).map_err(|_| ())?)
            .map_err(|_| ())?;
    Trust::new(
        keys.into_iter()
            .map(|key| PublicIdentity {
                id: key.id,
                purposes: BTreeSet::from([key.purpose]),
                pem: key.public_pem.into_bytes(),
            })
            .collect(),
    )
    .map_err(|_| ())
}
async fn run() -> Result<(), ()> {
    let mut args = std::env::args().skip(1);
    let first = args.next().ok_or(())?;
    if first == "seal" {
        // The trusted operator evaluates the test reports. Sealing authenticates
        // that decision; it never treats an unsigned PASS from a builder as proof.
        let catalogue_path = PathBuf::from(args.next().ok_or(())?);
        let observations_path = PathBuf::from(args.next().ok_or(())?);
        let output = PathBuf::from(args.next().ok_or(())?);
        if args.next().is_some() {
            return Err(());
        }
        let trust = trust()?;
        let original: SignedCatalogue =
            serde_json::from_slice(&read_regular(&catalogue_path, 8388608).map_err(|_| ())?)
                .map_err(|_| ())?;
        original
            .verify(&trust, original.catalogue.revision)
            .map_err(|_| ())?;
        let observations: Vec<ComponentQualification> =
            serde_json::from_slice(&read_regular(&observations_path, 1048576).map_err(|_| ())?)
                .map_err(|_| ())?;
        if observations.is_empty() || observations.len() > 147 {
            return Err(());
        }
        let key = std::env::var_os("KYRO_FACTORY_EVIDENCE_KEY_FILE").ok_or(())?;
        let id = std::env::var("KYRO_FACTORY_EVIDENCE_KEY_ID").map_err(|_| ())?;
        let signer = load_signer(Path::new(&key), id, Purpose::Evidence).map_err(|_| ())?;
        let mut subjects = BTreeSet::new();
        let mut proofs = Vec::new();
        for qualification in observations {
            if !subjects.insert((
                qualification.component_id.clone(),
                qualification.version.clone(),
            )) {
                return Err(());
            }
            let mut manifest = original
                .catalogue
                .entries
                .get(&qualification.component_id)
                .and_then(|versions| versions.get(&qualification.version))
                .ok_or(())?
                .component
                .manifest
                .clone();
            let proof = SignedComponentQualification {
                signature: signer.sign(&qualification).map_err(|_| ())?,
                qualification,
            };
            manifest.qualification_digest = kyro_factory::digest(&proof).map_err(|_| ())?;
            proof.verify(&manifest, &trust).map_err(|_| ())?;
            proofs.push(proof);
        }
        save(&output, &proofs)?;
        println!("qualification decisions sealed: {}", proofs.len());
        return Ok(());
    }
    if first == "publish" {
        let path = PathBuf::from(args.next().ok_or(())?);
        if args.next().is_some() {
            return Err(());
        }
        let trust = trust()?;
        let catalogue: SignedCatalogue =
            serde_json::from_slice(&read_regular(&path, 8388608).map_err(|_| ())?)
                .map_err(|_| ())?;
        catalogue
            .verify(&trust, catalogue.catalogue.revision)
            .map_err(|_| ())?;
        let body = serde_json::to_value(&catalogue).map_err(|_| ())?;
        let hash = kyro_factory::digest(&catalogue.catalogue).map_err(|_| ())?;
        let url = std::env::var("KYRO_DATABASE_ADMIN_URL").map_err(|_| ())?;
        let store = kyro_store::Store::connect(&url, 1).await.map_err(|_| ())?;
        store
            .publish_factory_catalogue(catalogue.catalogue.revision, &hash, &body, |previous| {
                if let Some(previous) = previous {
                    let previous: SignedCatalogue = serde_json::from_value(previous.clone())
                        .map_err(|_| {
                            kyro_domain::Error::Invalid("invalid previous catalogue".into())
                        })?;
                    previous
                        .verify_successor(&catalogue, &trust)
                        .map_err(kyro_factory::service::domain_error)?;
                }
                Ok(())
            })
            .await
            .map_err(|_| ())?;
        println!(
            "catalogue published: revision {}, digest {hash}",
            catalogue.catalogue.revision
        );
        return Ok(());
    }
    let key = std::env::var_os("KYRO_FACTORY_CATALOGUE_KEY_FILE").ok_or(())?;
    let id = std::env::var("KYRO_FACTORY_CATALOGUE_KEY_ID").map_err(|_| ())?;
    let signer = load_signer(Path::new(&key), id, Purpose::Catalogue).map_err(|_| ())?;
    let (catalogue, output) = if matches!(first.as_str(), "admit" | "admit-all" | "change") {
        let path = PathBuf::from(args.next().ok_or(())?);
        let trust = trust()?;
        let original: SignedCatalogue =
            serde_json::from_slice(&read_regular(&path, 8388608).map_err(|_| ())?)
                .map_err(|_| ())?;
        original
            .verify(&trust, original.catalogue.revision)
            .map_err(|_| ())?;
        let result = if first == "admit-all" {
            let proof_path = PathBuf::from(args.next().ok_or(())?);
            let proofs: Vec<SignedComponentQualification> =
                serde_json::from_slice(&read_regular(&proof_path, 1048576).map_err(|_| ())?)
                    .map_err(|_| ())?;
            original
                .admit_batch(proofs, &trust, &signer)
                .map_err(|_| ())?
        } else if first == "admit" {
            let proof_path = PathBuf::from(args.next().ok_or(())?);
            let proof: SignedComponentQualification =
                serde_json::from_slice(&read_regular(&proof_path, 65536).map_err(|_| ())?)
                    .map_err(|_| ())?;
            original.admit(proof, &trust, &signer).map_err(|_| ())?
        } else {
            let component = args.next().ok_or(())?;
            let version = args.next().ok_or(())?;
            let admission = match args.next().ok_or(())?.as_str() {
                "pending" => Admission::Pending,
                "deprecated" => Admission::Deprecated,
                "revoked" => Admission::Revoked,
                _ => return Err(()),
            };
            original
                .changed(&component, &version, admission, &signer)
                .map_err(|_| ())?
        };
        result
            .verify(&trust, original.catalogue.revision)
            .map_err(|_| ())?;
        (result, PathBuf::from(args.next().ok_or(())?))
    } else {
        // Original three-argument generation command remains compatible.
        let root = PathBuf::from(first);
        let revision = args.next().ok_or(())?.parse().map_err(|_| ())?;
        if !root.is_absolute() {
            return Err(());
        }
        (
            builtins::pending(&root, revision, &signer).map_err(|_| ())?,
            PathBuf::from(args.next().ok_or(())?),
        )
    };
    if args.next().is_some() {
        return Err(());
    }
    let admitted = catalogue
        .catalogue
        .entries
        .values()
        .flat_map(|versions| versions.values())
        .filter(|entry| entry.admission == Admission::Admitted)
        .count();
    save(&output, &catalogue)?;
    println!(
        "registry saved: {} components, {admitted} admitted, revision {}",
        catalogue.catalogue.entries.len(),
        catalogue.catalogue.revision
    );
    Ok(())
}

fn save(path: &Path, value: &impl serde::Serialize) -> Result<(), ()> {
    use std::io::Write;
    if !path.is_absolute() {
        return Err(());
    }
    let bytes = serde_json::to_vec(value).map_err(|_| ())?;
    if bytes.len() > 8388608 {
        return Err(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| ())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| ())
}
