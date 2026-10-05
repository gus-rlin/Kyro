//! A real offline build recipe. Requires the operator's prepared gVisor tools.
//! Its signed test catalogue is not an admission of the entire application set.
mod support;
use kyro_app::contract::Schema;
use kyro_domain::{
    Environment,
    spec::{AppNode, AppSpec},
};
use kyro_factory::{
    artifacts::*, assembler::*, catalogue::*, crypto::Purpose, digest, digest_bytes, resolver::*,
    sandbox::*,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use support::{qualified_fixture, signer, trust};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires prepared offline gVisor tools and Docker controller"]
async fn real_offline_sources_compile_and_package_exact_oci() {
    let config: SandboxConfig = serde_json::from_str(
        &std::env::var("KYRO_P2_SANDBOX_CONFIG").expect("prepared profile required"),
    )
    .unwrap();
    let root = Path::new("/workspace");
    let sources = collect_runtime_sources(root).unwrap();
    let migration_digests = sources
        .iter()
        .filter(|(p, _)| p.starts_with("crates/kyro-app/migrations/"))
        .map(|(p, h)| (p.clone(), h.clone()))
        .collect();
    let m = ComponentManifest {
        schema_version: 1,
        id: "B031".into(),
        version: "0.1.0".into(),
        source_digest: digest(&sources).unwrap(),
        source_files: sources,
        migration_digests,
        configuration: serde_json::from_value::<Schema>(
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        )
        .unwrap(),
        dependencies: BTreeMap::new(),
        capabilities: BTreeSet::from(["records".into()]),
        ports: BTreeMap::new(),
        output_type: Some("record".into()),
        effects: BTreeSet::from(["read".into(), "write".into()]),
        qualification_digest: digest_bytes(
            b"synthetic build harness, not full component qualification",
        ),
        criteria_digest: digest_bytes(b"synthetic fixture catalogue used only to build sources"),
    };
    let c = Catalogue {
        schema_version: 1,
        revision: 1,
        entries: BTreeMap::from([(
            "B031".into(),
            BTreeMap::from([("0.1.0".into(), qualified_fixture(m))]),
        )]),
    };
    let catalogue = SignedCatalogue {
        signature: signer(Purpose::Catalogue).sign(&c).unwrap(),
        catalogue: c,
    };
    let mut spec = AppSpec::default();
    spec.nodes.push(AppNode {
        id: "records".into(),
        kind: "B031".into(),
        properties: json!({"version":"0.1.0","configuration":{}})
            .as_object()
            .unwrap()
            .clone(),
    });
    let permit = Permit {
        project_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        source_revision: 0,
        environment: Environment::Development,
        capabilities: BTreeSet::from(["records".into()]),
    };
    let lock = seal(
        resolve(&spec, &catalogue, &trust(), &permit).unwrap(),
        &signer(Purpose::Composition),
    )
    .unwrap();
    let bundle = assemble(root, &spec, &lock, &catalogue, &trust(), &permit).unwrap();
    let output = DockerSandbox::new(config.clone())
        .unwrap()
        .build(&bundle)
        .await
        .unwrap_or_else(|error| {
            if let Some(log) = &error.execution {
                // This harness has no credentials or private user inputs. Production
                // API diagnostics do not copy these raw, bounded operator logs.
                eprintln!(
                    "{}\n{}",
                    String::from_utf8_lossy(&log.stdout),
                    String::from_utf8_lossy(&log.stderr)
                );
            }
            panic!("{error}")
        });
    assert_eq!(output.binaries.len(), 3);
    assert!(!output.run_id.is_nil());
    let base_root = Path::new("/tools-root");
    let base: BTreeMap<_, _> = BASE_FILES
        .into_iter()
        .map(|p| (p.to_owned(), std::fs::read(base_root.join(p)).unwrap()))
        .collect();
    let base_hashes: BTreeMap<_, _> = base
        .iter()
        .map(|(p, b)| (p.clone(), digest_bytes(b)))
        .collect();
    let configuration = &bundle.files["application.json"];
    let binding = ArtifactBinding {
        schema_version: 1,
        project_id: permit.project_id,
        application_id: permit.application_id,
        revision: 0,
        environment: permit.environment,
        lock_digest: digest(&lock).unwrap(),
        source_digest: digest(&bundle.manifest).unwrap(),
        migration_digest: digest(&bundle.manifest.migrations).unwrap(),
        configuration_digest: digest_bytes(configuration),
        runtime_base_digest: digest(&base_hashes).unwrap(),
        tools_image_digest: config.tools_image.strip_prefix("sha256:").unwrap().into(),
        sandbox_profile_digest: output.profile_digest.clone(),
    };
    let image = package(binding, &base, &output.binaries, configuration).unwrap();
    image.verify().unwrap();
    assert_eq!(image.runtime_files().unwrap().len(), 9);
    let report = json!({"kind":"real_offline_build","run_id":output.run_id,"source_digest":digest(&bundle.manifest).unwrap(),"oci_digest":image.image_digest,"profile_digest":output.profile_digest,"binary_sizes":output.binaries.iter().map(|(p,b)|(p,b.len())).collect::<BTreeMap<_,_>>(),"logs_truncated":output.logs_truncated});
    println!("{}", serde_json::to_string(&report).unwrap());
    if let Ok(directory) = std::env::var("KYRO_P2_CANDIDATE_OUTPUT") {
        let directory = Path::new(&directory);
        assert!(
            directory.parent() == Some(Path::new("/tmp"))
                && directory
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("kyro-p2-candidate-")
        );
        image.write(directory).unwrap();
        // Exact public inputs are retained for reproduction. No signing private
        // key or disposable workload credential is part of this snapshot.
        bundle.write(&directory.join("source-snapshot")).unwrap();
        std::fs::write(
            directory.join("catalogue-fixture.json"),
            serde_json::to_vec(&catalogue).unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.join("artifact-binding.json"),
            serde_json::to_vec(&image.binding).unwrap(),
        )
        .unwrap();
    }
}
