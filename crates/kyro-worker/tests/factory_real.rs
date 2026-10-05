//! Real P1 worker -> offline builder -> separate attestor process -> fresh
//! protected PostgreSQL verifier -> atomic release. Catalogue admissions
//! here is a signature fixture; it does not qualify the complete P2 catalogue.
#[path = "../../kyro-factory/tests/support/mod.rs"]
mod support;
use kyro_domain::{
    Environment,
    spec::{AppNode, ChangeOperation, ChangeSet, ProjectLimits},
    task::{JobPayload, JobResult, JobStatus},
};
use kyro_factory::{
    artifacts::*,
    assembler::*,
    catalogue::*,
    crypto::Purpose,
    digest, digest_bytes,
    sandbox::SandboxConfig,
    service::{FactoryControl, OperatorConfig},
};
use kyro_gateway::{Gateway, GatewayConfig};
use kyro_store::{Store, projects::CreateProjectInput};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::process::Command;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires prepared Docker controller, gVisor tools and dedicated P1 PostgreSQL roles"]
async fn durable_worker_builds_and_separate_attestor_commits_only_observed_release() {
    let family = std::env::var("KYRO_P2_FACTORY_COMPOSITION").unwrap_or_else(|_| "records".into());
    let family_ids: &[&str] = match family.as_str() {
        "records" => &[],
        "booking" => &["B111", "B112", "B113", "B114", "B115", "B119"],
        "support" => &["B013", "B014", "B133"],
        "stock" => &["B121", "B122", "B123", "B124", "B130"],
        "all" => &[
            "B013", "B014", "B111", "B112", "B113", "B114", "B115", "B119", "B121", "B122", "B123",
            "B124", "B130", "B133",
        ],
        _ => panic!("unknown protected composition fixture"),
    };
    let ids: BTreeSet<&str> = [
        "B031", "B032", "B033", "B034", "B035", "B050", "B051", "B054", "B055", "B056",
    ]
    .into_iter()
    .chain(family_ids.iter().copied())
    .collect();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .try_init();
    let sandbox: SandboxConfig =
        serde_json::from_str(&std::env::var("KYRO_P2_SANDBOX_CONFIG").unwrap()).unwrap();
    let api = Store::connect(&std::env::var("KYRO_TEST_DATABASE_URL").unwrap(), 8)
        .await
        .unwrap();
    let worker = Store::connect(&std::env::var("KYRO_TEST_WORKER_DATABASE_URL").unwrap(), 8)
        .await
        .unwrap();
    let admin = Store::connect(&std::env::var("KYRO_TEST_ADMIN_DATABASE_URL").unwrap(), 8)
        .await
        .unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&admin.pool)
        .await
        .unwrap();
    assert!(database.starts_with("kyro_p1_test_"));
    let mut serial = admin.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(160016)")
        .execute(&mut *serial)
        .await
        .unwrap();
    let directory = support::Temp::new();
    let root = &directory.0;
    for name in ["archive", "base", "socket"] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    let mut base = BTreeMap::new();
    for file in BASE_FILES {
        let target = root.join("base").join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let bytes = std::fs::read(Path::new("/tools-root").join(file)).unwrap();
        std::fs::write(target, &bytes).unwrap();
        base.insert(file.to_owned(), bytes);
    }
    let base_hashes: BTreeMap<_, _> = base
        .iter()
        .map(|(p, b)| (p.clone(), digest_bytes(b)))
        .collect();
    let trust:Vec<_>=[Purpose::Catalogue,Purpose::Composition,Purpose::Evidence,Purpose::Release].into_iter().enumerate().map(|(i,p)|json!({"id":format!("role-{i}"),"purpose":p,"public_pem":String::from_utf8(support::keys()[i].1.clone()).unwrap()})).collect();
    let config = json!({"source_root":"/workspace","archive_root":root.join("archive"),"tools_root":"/tools-root","runtime_base_root":root.join("base"),"runtime_base_digest":digest(&base_hashes).unwrap(),
        "attestor_socket":root.join("socket/attestor.sock"),"sandbox":sandbox,"capabilities":kyro_factory::builtins::capabilities(),"trust":trust});
    let config_path = root.join("operator.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let control = Arc::new(
        FactoryControl::new(serde_json::from_value::<OperatorConfig>(config).unwrap()).unwrap(),
    );
    // Resolve the real contracts, dependency versions, ports and action schemas.
    // Only qualification signatures are fixtures; the campaign separately binds
    // these actual manifests to the retained application recipes.
    let pending = kyro_factory::builtins::pending(
        Path::new("/workspace"),
        1,
        &support::signer(Purpose::Catalogue),
    )
    .unwrap();
    let current: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM factory_catalogue_state WHERE singleton")
            .fetch_optional(&admin.pool)
            .await
            .unwrap();
    let entries = ids
        .iter()
        .map(|id| {
            let component = pending.catalogue.entries[*id][kyro_factory::builtins::VERSION]
                .component
                .manifest
                .clone();
            (
                id.to_string(),
                BTreeMap::from([(
                    kyro_factory::builtins::VERSION.into(),
                    support::qualified_fixture(component),
                )]),
            )
        })
        .collect();
    let catalogue = Catalogue {
        schema_version: 1,
        revision: (current.unwrap_or(0) + 1) as u64,
        entries,
    };
    let catalogue = SignedCatalogue {
        signature: support::signer(Purpose::Catalogue)
            .sign(&catalogue)
            .unwrap(),
        catalogue,
    };
    sqlx::query("INSERT INTO factory_catalogue_state(singleton,revision,catalogue_digest,signed_catalogue) VALUES(TRUE,$1,$2,$3) ON CONFLICT(singleton) DO UPDATE SET revision=EXCLUDED.revision,catalogue_digest=EXCLUDED.catalogue_digest,signed_catalogue=EXCLUDED.signed_catalogue")
        .bind(catalogue.catalogue.revision as i64).bind(digest(&catalogue.catalogue).unwrap()).bind(serde_json::to_value(&catalogue).unwrap()).execute(&admin.pool).await.unwrap();
    let owner = api
        .upsert_oidc_actor(
            "https://factory.real.test.invalid",
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
    let org = api
        .create_organization(owner, &format!("real-factory-{}", Uuid::new_v4()))
        .await
        .unwrap();
    let project = api
        .create_project(
            owner,
            CreateProjectInput {
                organization_id: org.id,
                name: "real factory synthetic".into(),
                data_policy: None,
                limits: Some(ProjectLimits {
                    job_ttl_secs: 900,
                    ..ProjectLimits::default()
                }),
            },
        )
        .await
        .unwrap();
    let project_id = project.project.id;
    api.apply_changes(
        owner,
        project_id,
        0,
        "real-factory-spec",
        &ChangeSet {
            operations: ids
                .iter()
                .map(|id| ChangeOperation::AddNode {
                    node: AppNode {
                        id: format!("block_{}", id.to_ascii_lowercase()),
                        kind: id.to_string(),
                        properties:
                            json!({"version":kyro_factory::builtins::VERSION,"configuration":{}})
                                .as_object()
                                .unwrap()
                                .clone(),
                    },
                })
                .collect(),
        },
    )
    .await
    .unwrap();
    let lock = control
        .composition(
            &api,
            owner,
            project_id,
            1,
            &support::signer(Purpose::Composition),
        )
        .await
        .unwrap();
    control
        .admit(&api, owner, project_id, 1, &lock)
        .await
        .unwrap();
    let mut changed = lock.clone();
    changed.lock.application_id = Uuid::new_v4();
    assert!(
        control
            .admit(&api, owner, project_id, 1, &changed)
            .await
            .is_err()
    );
    let mut command = Command::new("/workspace/target/debug/kyro-attestor");
    command
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/tmp")
        .env("KYRO_ENV", "development")
        .env(
            "KYRO_WORKER_DATABASE_URL",
            std::env::var("KYRO_TEST_WORKER_DATABASE_URL").unwrap(),
        )
        .env("KYRO_FACTORY_CONFIG_FILE", &config_path);
    for (prefix, index) in [("KYRO_FACTORY_EVIDENCE", 2), ("KYRO_FACTORY_RELEASE", 3)] {
        let key = root.join(format!("key-{index}.pem"));
        std::fs::write(&key, &support::keys()[index].0).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        command
            .env(format!("{prefix}_KEY_FILE"), key)
            .env(format!("{prefix}_KEY_ID"), format!("role-{index}"));
    }
    let stderr_path = root.join("attestor-private.log");
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .kill_on_drop(true);
    let mut attestor = command.spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !control.config().attestor_socket.exists() {
            assert!(
                attestor.try_wait().unwrap().is_none(),
                "separate attestor must start"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let job = api
        .enqueue_job(
            owner,
            project_id,
            1,
            "real-factory-build",
            JobPayload::BuildApplication { lock: lock.clone() },
            Some(2),
            Some(900),
        )
        .await
        .unwrap();
    // Prove the server-side context independently before dispatch, so a failure
    // is localized before running a several-minute offline compilation.
    let lease = worker
        .claim_next_job(Uuid::new_v4(), 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.job_id, job.id);
    let snapshot = worker.factory_snapshot(&lease).await;
    assert!(
        snapshot.is_ok(),
        "factory protected snapshot must load: {:?}",
        snapshot.err()
    );
    let prepared = control.prepare(&worker, &lease).await;
    assert!(
        prepared.is_ok(),
        "factory source preparation must succeed: {:?}",
        prepared.err()
    );
    // Requeue only this synthetic preflight lease through the regular retry
    // path, retaining an attempt for the real worker below.
    worker
        .fail_job(
            &lease,
            kyro_domain::task::JobErrorCode::RetryableInternal,
            true,
        )
        .await
        .unwrap();
    let gateway = Gateway::new(
        GatewayConfig::for_admission_from_registry_json(
            include_bytes!("../../../tests/fixtures/models.synthetic.e2e.json"),
            Environment::Development,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker_clone = worker.clone();
    let control_clone = control.clone();
    let working = tokio::spawn(async move {
        kyro_worker::run_with_factory(
            &worker_clone,
            &gateway,
            25,
            30,
            shutdown_rx,
            Some(&control_clone),
        )
        .await
    });
    let done = tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let current = api.get_job(owner, project_id, job.id).await.unwrap();
            if current.status.is_terminal() {
                break current;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .unwrap();
    shutdown_tx.send(true).unwrap();
    working.await.unwrap().unwrap();
    let _ = attestor.kill().await;
    let _ = attestor.wait().await;
    if done.status != JobStatus::Succeeded {
        let log = std::fs::read_to_string(&stderr_path).unwrap_or_default();
        for line in log
            .lines()
            .filter(|s| s.starts_with("protected verification failed: "))
        {
            eprintln!("{line}");
        }
    }
    assert_eq!(
        done.status,
        JobStatus::Succeeded,
        "private attestor log withheld; job error: {:?}",
        done.error_code
    );
    let Some(JobResult::BuildApplication {
        artifact_id,
        image_digest,
        release_digest,
    }) = done.result
    else {
        panic!("artifact reference required")
    };
    let stored = api
        .get_factory_artifact(owner, project_id, artifact_id)
        .await
        .unwrap();
    control.verify_stored_artifact(&stored).unwrap();
    let release: SignedRelease =
        serde_json::from_value(stored.artifact.signed_release.clone()).unwrap();
    let candidate = control
        .config()
        .archive_root
        .join(job.id.to_string())
        .join(format!("attempt-{}", stored.job_generation))
        .join("candidate");
    let image =
        OciArtifact::read(&candidate, &image_digest, release.release.binding.clone()).unwrap();
    image.verify().unwrap();
    assert_eq!(digest(&release).unwrap(), release_digest);
    verify_http_delivery(&api, &admin, &control, owner, project_id, &stored).await;
    let proof = json!({"kind":"p1_worker_separate_attestor_real_build","status":"succeeded","image_digest":image_digest,"release_digest":release_digest,"source_digest":stored.artifact.source_digest,
        "checks":stored.artifact.signed_evidence["evidence"]["required_checks"],"generation":stored.job_generation,"catalogue":"signature fixtures; not qualification of 147 components","composition":family,"shared_components":["B031","B032","B033","B034","B035","B050","B051","B054","B055","B056"],"providers":"none"});
    println!("{proof}");
    // Only public inputs go to the optional durable evidence directory. Private
    // disposable signing keys and database/session credentials are excluded.
    if let Ok(output) = std::env::var("KYRO_P2_FACTORY_EVIDENCE_OUTPUT") {
        let output = Path::new(&output);
        assert!(
            output.is_absolute()
                && output.parent() == Some(Path::new("/tmp"))
                && output
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("kyro-p2-factory-proof-")
        );
        std::fs::create_dir(output).unwrap();
        std::fs::write(
            output.join("report.json"),
            serde_json::to_vec(&proof).unwrap(),
        )
        .unwrap();
        std::fs::write(
            output.join("catalogue.json"),
            serde_json::to_vec(&catalogue).unwrap(),
        )
        .unwrap();
        std::fs::write(
            output.join("artifact.json"),
            serde_json::to_vec(&stored).unwrap(),
        )
        .unwrap();
        std::fs::write(
            output.join("trust.json"),
            serde_json::to_vec(&trust).unwrap(),
        )
        .unwrap();
        image.write(&output.join("oci")).unwrap();
        let mut bundle_files = BTreeMap::new();
        let source: SourceManifest =
            serde_json::from_value(stored.artifact.source_manifest).unwrap();
        let source_root = candidate.parent().unwrap().join("sources");
        for path in source.files.keys() {
            let mut bytes = Vec::new();
            std::fs::File::open(source_root.join(path))
                .unwrap()
                .take(2097153)
                .read_to_end(&mut bytes)
                .unwrap();
            bundle_files.insert(path.clone(), bytes);
        }
        SourceBundle {
            manifest: source,
            files: bundle_files,
        }
        .write(&output.join("source-snapshot"))
        .unwrap();
    }
    serial.commit().await.unwrap();
}

async fn verify_http_delivery(
    api: &Store,
    admin: &Store,
    control: &Arc<FactoryControl>,
    owner: Uuid,
    project_id: Uuid,
    stored: &kyro_store::factory::FactoryArtifact,
) {
    use base64::{
        Engine,
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    };
    use kyro_api::{
        AppState,
        factory::FactoryApi,
        identity::{AuthConfig, OidcProviderConfig},
    };
    use sha2::{Digest, Sha256};
    let token = [78u8; 32];
    let csrf = [79u8; 32];
    let token_hash: [u8; 32] = Sha256::digest(token).into();
    let csrf_hash: [u8; 32] = Sha256::digest(csrf).into();
    let session = api
        .create_session(
            owner,
            &token_hash,
            &csrf_hash,
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .unwrap();
    let cookie = format!("kyro_session={}", URL_SAFE_NO_PAD.encode(token));
    let auth = AuthConfig::new(
        Environment::Development,
        OidcProviderConfig {
            issuer: "http://127.0.0.1:9000/issuer".into(),
            authorization_endpoint: "http://127.0.0.1:9000/authorize".parse().unwrap(),
            token_endpoint: "http://127.0.0.1:9000/token".parse().unwrap(),
            jwks_uri: "http://127.0.0.1:9000/jwks".parse().unwrap(),
            redirect_uri: "http://127.0.0.1:8080/v1/auth/callback".parse().unwrap(),
            client_id: "synthetic-delivery".into(),
            client_secret: None,
        },
        "http://127.0.0.1:3000",
        true,
    )
    .unwrap();
    let config = kyro_domain::Config {
        environment: Environment::Development,
        database_url: std::env::var("KYRO_TEST_DATABASE_URL").unwrap(),
        worker_database_url: String::new(),
        bind: "127.0.0.1:0".parse().unwrap(),
        max_connections: 8,
        worker_poll_ms: 100,
        lease_seconds: 120,
        max_body_bytes: 262144,
        synthetic_providers: true,
    };
    let gateway = Gateway::new(
        GatewayConfig::for_admission_from_registry_json(
            include_bytes!("../../../tests/fixtures/models.synthetic.e2e.json"),
            Environment::Development,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    let state = AppState::new(
        api.clone(),
        Arc::new(config),
        Arc::new(auth),
        Arc::new(gateway),
    )
    .with_factory(Some(Arc::new(
        FactoryApi::new(control.clone(), support::signer(Purpose::Composition)).unwrap(),
    )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, kyro_api::router(state))
            .await
            .unwrap()
    });
    let client = reqwest::Client::new();
    let artifact_url = format!(
        "{base}/v1/projects/{project_id}/factory/artifacts/{}",
        stored.artifact.id
    );
    assert_eq!(
        client.get(&artifact_url).send().await.unwrap().status(),
        401
    );
    assert_eq!(
        client
            .get(format!(
                "{base}/v1/projects/{}/factory/artifacts/{}",
                Uuid::new_v4(),
                stored.artifact.id
            ))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let metadata = client
        .get(&artifact_url)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(metadata.status(), 200);
    assert_eq!(metadata.headers()["cache-control"], "no-store");
    let metadata: serde_json::Value = metadata.json().await.unwrap();
    assert_eq!(
        metadata["artifact"]["image_digest"],
        stored.artifact.image_digest
    );
    assert!(metadata.get("job_generation").is_none());
    let replay = || {
        client
            .post(format!("{base}/v1/projects/{project_id}/factory/builds"))
            .header("cookie", &cookie)
            .header("origin", "http://127.0.0.1:3000")
            .header("x-csrf-token", URL_SAFE_NO_PAD.encode(csrf))
            .header("if-match", "\"rev-1\"")
            .header("idempotency-key", "real-factory-build")
            .json(&json!({"max_attempts":2,"ttl_seconds":900}))
            .send()
    };
    let (a, b) = tokio::join!(replay(), replay());
    for response in [a.unwrap(), b.unwrap()] {
        assert_eq!(
            response.status(),
            202,
            "{}",
            response.text().await.unwrap_or_default()
        );
    }
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE project_id=$1 AND payload->>'kind'='build_application'",
    )
    .bind(project_id)
    .fetch_one(&admin.pool)
    .await
    .unwrap();
    assert_eq!(jobs, 1);
    let mut counts = BTreeMap::new();
    for format in ["sources", "oci", "git"] {
        let url = format!("{artifact_url}/exports/{format}");
        let response = client
            .get(&url)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            200,
            "{}",
            response.text().await.unwrap_or_default()
        );
        let index: serde_json::Value = response.json().await.unwrap();
        assert_eq!(index["source_digest"], stored.artifact.source_digest);
        assert_eq!(index["chunk_bytes"], 65536);
        let files = index["files"].as_object().unwrap();
        // All OCI and Git bytes are transported. For source delivery, include
        // Cargo.lock (multiple chunks), configuration, lock and provenance.
        let paths: Vec<_> = files
            .keys()
            .filter(|path| {
                format != "sources"
                    || [
                        "Cargo.lock",
                        "application.json",
                        "composition-lock.json",
                        "source-manifest.json",
                    ]
                    .contains(&path.as_str())
            })
            .collect();
        let mut downloaded = 0;
        for path in paths {
            let mut bytes = Vec::new();
            loop {
                let response = client
                    .get(format!("{url}/chunk"))
                    .header("cookie", &cookie)
                    .query(&[
                        ("path", path.as_str()),
                        ("offset", &bytes.len().to_string()),
                    ])
                    .send()
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    200,
                    "{}",
                    response.text().await.unwrap_or_default()
                );
                let chunk: serde_json::Value = response.json().await.unwrap();
                let data = STANDARD
                    .decode(chunk["content_base64"].as_str().unwrap())
                    .unwrap();
                assert!(!data.is_empty() && data.len() <= 65536);
                assert_eq!(chunk["chunk_sha256"], digest_bytes(&data));
                assert_eq!(chunk["file_sha256"], files[path]["sha256"]);
                bytes.extend(data);
                downloaded += 1;
                assert_eq!(chunk["next_offset"], bytes.len());
                if chunk["complete"] == true {
                    break;
                }
            }
            assert_eq!(
                bytes.len() as u64,
                files[path]["size_bytes"].as_u64().unwrap()
            );
            assert_eq!(digest_bytes(&bytes), files[path]["sha256"]);
        }
        assert_eq!(
            client
                .get(format!("{url}/chunk"))
                .header("cookie", &cookie)
                .query(&[("path", "../operator.json"), ("offset", "0")])
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        counts.insert(format, downloaded);
    }
    api.apply_changes(
        owner,
        project_id,
        1,
        "factory-http-advance",
        &ChangeSet {
            operations: vec![ChangeOperation::SetPreference {
                key: "locale".into(),
                value: json!("fr-FR"),
            }],
        },
    )
    .await
    .unwrap();
    // Reproduction of the former route: resolving before the idempotency read
    // refuses this exact original revision after it advances.
    assert!(
        control
            .composition(
                api,
                owner,
                project_id,
                1,
                &support::signer(Purpose::Composition)
            )
            .await
            .is_err()
    );
    let build_url = format!("{base}/v1/projects/{project_id}/factory/builds");
    let replay = client
        .post(&build_url)
        .header("cookie", &cookie)
        .header("origin", "http://127.0.0.1:3000")
        .header("x-csrf-token", URL_SAFE_NO_PAD.encode(csrf))
        .header("if-match", "\"rev-1\"")
        .header("idempotency-key", "real-factory-build")
        .json(&json!({"max_attempts":2,"ttl_seconds":900}))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 202);
    assert_eq!(
        replay.json::<serde_json::Value>().await.unwrap()["source_revision"],
        1
    );
    assert_eq!(
        client
            .post(&build_url)
            .header("cookie", &cookie)
            .header("origin", "http://127.0.0.1:3000")
            .header("x-csrf-token", URL_SAFE_NO_PAD.encode(csrf))
            .header("if-match", "\"rev-2\"")
            .header("idempotency-key", "real-factory-build")
            .json(&json!({"max_attempts":2,"ttl_seconds":900}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    api.revoke_session(session.id, owner).await.unwrap();
    assert_eq!(
        client
            .get(format!(
                "{artifact_url}/exports/sources/chunk?path=Cargo.lock&offset=0"
            ))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    server.abort();
    println!(
        "{}",
        json!({"kind":"factory_http_delivery","formats":counts,"session_revocation_refused":true,"provider":"none"})
    );
}
