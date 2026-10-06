//! Shared protected chain for synthetic and real-provider acceptance.
use super::fixture::Fixture;
use kyro_domain::{agents::RunStatus, task::JobStatus};
use serde_json::json;
use std::{sync::Arc, time::Duration};

pub(super) async fn verify(f: Fixture, live: bool) {
    let initial = f.start("protected-factory", false).await;
    assert_eq!(f.wave().await, 1);
    let planned = f.advance(&initial).await;
    assert_eq!(planned.status, RunStatus::Executing);
    let executing = f.advance(&planned).await;
    assert_eq!(f.wave().await, 1);
    let reviewing = f.advance(&executing).await;
    assert_eq!(reviewing.status, RunStatus::Reviewing);
    assert_eq!(f.wave().await, 2);
    let integrating = f.advance(&reviewing).await;
    assert_eq!(integrating.status, RunStatus::Integrating);
    let building = f.advance(&integrating).await;
    assert_eq!(
        building.status,
        RunStatus::Building,
        "{:?}",
        building.diagnostic
    );
    let mut attestor = f.attestor().await;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let store = f.worker.clone();
    let gateway = f.gateway.clone();
    let control = f.coordinator.factory.clone();
    let worker = tokio::spawn(async move {
        kyro_worker::run_with_factory(&store, &gateway, 25, 30, shutdown, Some(&control)).await
    });
    let job_id = building.build_job_id.unwrap();
    let job = tokio::time::timeout(Duration::from_secs(1000), async {
        loop {
            let job = f
                .api
                .get_job(f.owner, building.project_id, job_id)
                .await
                .unwrap();
            if job.status.is_terminal() {
                return job;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap().unwrap();
    let _ = attestor.kill().await;
    let _ = attestor.wait().await;
    if job.status != JobStatus::Succeeded {
        for line in std::fs::read_to_string(f.root().join("attestor-private.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("protected verification failed: "))
        {
            eprintln!("{line}");
        }
    }
    assert_eq!(job.status, JobStatus::Succeeded, "{:?}", job.error_code);
    let single = kyro_store::Store::connect(&std::env::var("KYRO_TEST_DATABASE_URL").unwrap(), 1)
        .await
        .unwrap();
    let verified = f
        .coordinator
        .advance(
            &single,
            &f.admission,
            f.owner,
            building.project_id,
            building.id,
            Some(building.version),
        )
        .await
        .unwrap();
    assert_eq!(
        verified.status,
        RunStatus::Verified,
        "{:?}",
        verified.diagnostic
    );
    let artifact = f
        .api
        .get_factory_artifact(f.owner, verified.project_id, verified.artifact_id.unwrap())
        .await
        .unwrap();
    f.coordinator
        .factory
        .verify_stored_artifact(&artifact)
        .unwrap();
    assert_eq!(artifact.job_id, job_id);
    assert_eq!(artifact.source_revision, 1);
    assert_eq!(verified.calls.len(), 4);
    let usage = kyro_agents::measurements::collect(&f.api, f.owner, &verified)
        .await
        .unwrap();
    assert_eq!(usage.len(), 4);
    assert!(usage.iter().all(|m| m.usage.is_some()));
    let report = json!({"kind":"p3_agents_protected_factory","status":"verified","project_id":verified.project_id,"run_id":verified.id,
        "artifact_id":artifact.artifact.id,"image_digest":artifact.artifact.image_digest,"release_digest":artifact.artifact.release_digest,
        "source_digest":artifact.artifact.source_digest,"checks":artifact.artifact.signed_evidence["evidence"]["required_checks"],
        "catalogue_version":kyro_factory::builtins::VERSION,"catalogue_qualification":"signature fixtures, not renewal of 147 admissions",
        "models":if live {"real NVIDIA/Nebius, native JSON v2; standard retention unknown"} else {"synthetic HTTP, no NVIDIA inference"},"model_calls":usage,"environment":"local gVisor/real PostgreSQL, no deployment",
        "budget":f.api.get_budget(f.owner,verified.project_id).await.unwrap()});
    println!("{report}");
    if let Ok(path) = std::env::var("KYRO_P3_FACTORY_PROOF_DIR") {
        let output = std::path::Path::new(&path);
        assert!(
            output.is_absolute()
                && output.parent() == Some(std::path::Path::new("/tmp"))
                && output
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("kyro-p3-factory-proof-")
        );
        std::fs::create_dir(output).unwrap();
        std::fs::write(
            output.join("report.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        std::fs::write(
            output.join("artifact.json"),
            serde_json::to_vec(&artifact).unwrap(),
        )
        .unwrap();
        std::fs::write(
            output.join("run.json"),
            serde_json::to_vec(&verified).unwrap(),
        )
        .unwrap();
        std::fs::write(
            output.join("public-trust.json"),
            serde_json::to_vec(&f.public_trust()).unwrap(),
        )
        .unwrap();
        // Only the public sources and OCI candidate survive disposable signing-key cleanup.
        let attempt = f
            .coordinator
            .factory
            .config()
            .archive_root
            .join(job_id.to_string())
            .join(format!("attempt-{}", job.generation));
        assert!(
            std::process::Command::new("tar")
                .arg("-cf")
                .arg(output.join("sources-and-candidate.tar"))
                .arg("-C")
                .arg(attempt)
                .args(["sources", "candidate"])
                .status()
                .unwrap()
                .success()
        );
    }
    // A second coordinator read after reconnect preserves the verified reference without another build or call.
    let restarted =
        kyro_store::Store::connect(&std::env::var("KYRO_TEST_DATABASE_URL").unwrap(), 4)
            .await
            .unwrap();
    let _ = Arc::new(
        restarted
            .get_agent_run(f.owner, verified.project_id, verified.id)
            .await
            .unwrap(),
    );
    assert_eq!(f.advance(&verified).await.version, verified.version);
}
