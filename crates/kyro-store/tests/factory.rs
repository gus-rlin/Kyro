//! Real PostgreSQL transaction/RLS fences. Catalogue and signed JSON below are
//! deliberately unsigned data fixtures: this test makes no attestation claim.
use kyro_domain::{
    Environment, Error,
    factory::{CompositionLock, LockedComponent, LockedNode, SignedCompositionLock},
    spec::{AppNode, ChangeOperation, ChangeSet},
    task::{JobErrorCode, JobPayload, JobResult, JobStatus},
};
use kyro_store::{Store, factory::FactoryArtifactInput, projects::CreateProjectInput};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn artifact(project: Uuid) -> FactoryArtifactInput {
    let h = hash(b"unsigned transaction fixture, never a verified application");
    FactoryArtifactInput {
        id: Uuid::new_v4(),
        application_id: project,
        image_digest: format!("sha256:{h}"),
        lock_digest: h.clone(),
        source_digest: h.clone(),
        evidence_digest: h.clone(),
        release_digest: h,
        signed_release: json!({"transaction_fixture":true}),
        signed_evidence: json!({"transaction_fixture":true}),
        source_manifest: json!({"transaction_fixture":true}),
    }
}
async fn publish(admin: &Store) -> (u64, String) {
    let current: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM factory_catalogue_state WHERE singleton")
            .fetch_optional(&admin.pool)
            .await
            .unwrap();
    let revision = current.unwrap_or(0) + 1;
    let h = hash(Uuid::new_v4().as_bytes());
    sqlx::query("INSERT INTO factory_catalogue_state(singleton,revision,catalogue_digest,signed_catalogue) VALUES(TRUE,$1,$2,$3) ON CONFLICT(singleton) DO UPDATE SET revision=EXCLUDED.revision,catalogue_digest=EXCLUDED.catalogue_digest,signed_catalogue=EXCLUDED.signed_catalogue")
        .bind(revision).bind(&h).bind(json!({"transaction_fixture":true})).execute(&admin.pool).await.unwrap();
    (revision as u64, h)
}
async fn project(
    api: &Store,
    actor: Uuid,
    organization: Uuid,
    revision: u64,
    catalogue_digest: String,
) -> (Uuid, SignedCompositionLock) {
    let snapshot = api
        .create_project(
            actor,
            CreateProjectInput {
                organization_id: organization,
                name: format!("factory-{}", Uuid::new_v4()),
                data_policy: None,
                limits: None,
            },
        )
        .await
        .unwrap();
    let id = snapshot.project.id;
    let node = AppNode {
        id: "records".into(),
        kind: "B031".into(),
        properties: json!({"version":"0.1.0","configuration":{}})
            .as_object()
            .unwrap()
            .clone(),
    };
    let result = api
        .apply_changes(
            actor,
            id,
            0,
            "factory-spec",
            &ChangeSet {
                operations: vec![ChangeOperation::AddNode { node }],
            },
        )
        .await
        .unwrap();
    let h = hash(b"public unsigned transaction fixture");
    let lock = CompositionLock {
        schema_version: 1,
        project_id: id,
        application_id: id,
        source_revision: 1,
        environment: Environment::Development,
        preferences: BTreeMap::new(),
        spec_digest: hash(&serde_json::to_vec(&result.revision.spec).unwrap()),
        catalogue_revision: revision,
        catalogue_digest,
        components: BTreeMap::from([(
            "B031".into(),
            LockedComponent {
                id: "B031".into(),
                version: "0.1.0".into(),
                manifest_digest: h.clone(),
                source_digest: h,
                migration_digests: BTreeMap::new(),
            },
        )]),
        nodes: BTreeMap::from([(
            "records".into(),
            LockedNode {
                component_id: "B031".into(),
                configuration: json!({}),
                depends_on: BTreeSet::new(),
                bindings: BTreeMap::new(),
            },
        )]),
        order: vec!["records".into()],
        capabilities: BTreeSet::from(["records".into()]),
        toolchain: "rust-1.96.1-linux-x86_64".into(),
    };
    (
        id,
        SignedCompositionLock {
            lock,
            signature: "unsigned transaction fixture".into(),
        },
    )
}
#[tokio::test]
#[ignore = "requires dedicated migrated P1 PostgreSQL API, worker and admin roles"]
async fn factory_artifacts_commit_atomically_and_refuse_stale_cancelled_revoked_and_foreign_attempts()
 {
    let api = Store::connect(&std::env::var("KYRO_TEST_DATABASE_URL").unwrap(), 8)
        .await
        .unwrap();
    let worker = Store::connect(&std::env::var("KYRO_TEST_WORKER_DATABASE_URL").unwrap(), 8)
        .await
        .unwrap();
    let admin = Store::connect(&std::env::var("KYRO_TEST_ADMIN_DATABASE_URL").unwrap(), 8)
        .await
        .unwrap();
    let mut serial = admin.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(160016)")
        .execute(&mut *serial)
        .await
        .unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&admin.pool)
        .await
        .unwrap();
    assert!(
        database.starts_with("kyro_p1_test_"),
        "factory fixture cleanup requires a dedicated test database"
    );
    // Failed earlier recipes may leave eligible fixture jobs. Close only this
    // suite's synthetic identities before exercising the global queue claim.
    sqlx::query("UPDATE jobs SET status='failed',lease_until=NULL,lease_owner=NULL,error_code='execution_failed' WHERE actor_id IN(SELECT id FROM actors WHERE issuer='https://factory.test.invalid') AND payload->>'kind'='build_application' AND status IN('pending','running')")
        .execute(&admin.pool).await.unwrap();
    let owner = api
        .upsert_oidc_actor("https://factory.test.invalid", &Uuid::new_v4().to_string())
        .await
        .unwrap();
    let org = api
        .create_organization(owner, &format!("factory-{}", Uuid::new_v4()))
        .await
        .unwrap();
    let (revision, h) = publish(&admin).await;
    let mut passed = Vec::new();
    for scenario in [
        "atomic",
        "cancelled",
        "source_stale",
        "lease_expired",
        "grant_revoked",
        "catalogue_revoked",
    ] {
        let (id, lock) = project(&api, owner, org.id, revision, h.clone()).await;
        let payload = JobPayload::BuildApplication { lock };
        let job = api
            .enqueue_job(owner, id, 1, scenario, payload.clone(), Some(1), Some(60))
            .await
            .unwrap();
        assert_eq!(
            api.enqueue_job(owner, id, 1, scenario, payload, Some(1), Some(60))
                .await
                .unwrap()
                .id,
            job.id
        );
        let lease = worker
            .claim_next_job(Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.job_id, job.id);
        let data = artifact(id);
        worker.factory_snapshot(&lease).await.unwrap();
        if scenario == "atomic" {
            let mut wrong = lease.clone();
            wrong.generation += 1;
            assert!(matches!(
                worker
                    .finish_build_application(&wrong, &data, |_| Ok(()))
                    .await,
                Err(Error::Conflict(_))
            ));
            let mut wrong = lease.clone();
            wrong.lease_owner = Uuid::new_v4();
            assert!(matches!(
                worker.factory_snapshot(&wrong).await,
                Err(Error::Conflict(_))
            ));
            assert!(
                worker
                    .finish_build_application(&lease, &data, |_| Err(Error::Invalid(
                        "signature fixture refused".into()
                    )))
                    .await
                    .is_err()
            );
            let count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM factory_artifacts WHERE job_id=$1")
                    .bind(job.id)
                    .fetch_one(&admin.pool)
                    .await
                    .unwrap();
            assert_eq!(count, 0);
            assert_eq!(
                api.get_job(owner, id, job.id).await.unwrap().status,
                JobStatus::Running
            );
            let done = worker
                .finish_build_application(&lease, &data, |_| Ok(()))
                .await
                .unwrap();
            assert_eq!(done.status, JobStatus::Succeeded);
            assert_eq!(
                done.result,
                Some(JobResult::BuildApplication {
                    artifact_id: data.id,
                    image_digest: data.image_digest.clone(),
                    release_digest: data.release_digest.clone()
                })
            );
            api.get_factory_artifact(owner, id, data.id).await.unwrap();
            api.apply_changes(
                owner,
                id,
                1,
                "factory-after-release",
                &ChangeSet {
                    operations: vec![ChangeOperation::SetPreference {
                        key: "locale".into(),
                        value: json!("fr-FR"),
                    }],
                },
            )
            .await
            .unwrap();
            // An identical intent replays its signed revision-1 lock after the
            // project advances; a revision/options change is a new intent.
            let replay = api
                .replay_factory_build(owner, id, 1, scenario, Some(1), Some(60))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(replay.id, job.id);
            assert_eq!(replay.status, JobStatus::Succeeded);
            assert!(matches!(
                api.replay_factory_build(owner, id, 2, scenario, Some(1), Some(60))
                    .await,
                Err(Error::IdempotencyConflict)
            ));
            let event_count:i64=sqlx::query_scalar("SELECT count(*) FROM events WHERE project_id=$1 AND type='job.succeeded' AND payload->>'job_id'=$2")
                .bind(id).bind(job.id.to_string()).fetch_one(&admin.pool).await.unwrap();
            assert_eq!(event_count, 1);
            assert!(
                worker
                    .finish_build_application(&lease, &data, |_| Ok(()))
                    .await
                    .is_err()
            );
            let other = api
                .upsert_oidc_actor("https://factory.test.invalid", &Uuid::new_v4().to_string())
                .await
                .unwrap();
            assert!(api.get_factory_artifact(other, id, data.id).await.is_err());
            assert!(
                api.replay_factory_build(other, id, 1, scenario, Some(1), Some(60))
                    .await
                    .is_err()
            );
            assert!(
                api.get_factory_artifact(owner, Uuid::new_v4(), data.id)
                    .await
                    .is_err()
            );
            let mut tx = worker.begin_actor(owner).await.unwrap();
            assert!(
                sqlx::query("UPDATE factory_artifacts SET image_digest=image_digest WHERE id=$1")
                    .bind(data.id)
                    .execute(&mut *tx)
                    .await
                    .is_err()
            );
            tx.rollback().await.unwrap();
            let mut tx = worker.begin_actor(owner).await.unwrap();
            assert!(
                sqlx::query("DELETE FROM factory_artifacts WHERE id=$1")
                    .bind(data.id)
                    .execute(&mut *tx)
                    .await
                    .is_err()
            );
            tx.rollback().await.unwrap();
            let production = api.clone().with_environment(Environment::Production);
            assert!(
                production
                    .get_factory_artifact(owner, id, data.id)
                    .await
                    .is_err()
            );
        } else {
            match scenario {
                "cancelled" => {
                    api.cancel_job(owner, id, job.id).await.unwrap();
                }
                "source_stale" => {
                    api.apply_changes(
                        owner,
                        id,
                        1,
                        "mutate-after-claim",
                        &ChangeSet {
                            operations: vec![ChangeOperation::SetPreference {
                                key: "locale".into(),
                                value: json!("fr-FR"),
                            }],
                        },
                    )
                    .await
                    .unwrap();
                }
                "lease_expired" => {
                    sqlx::query("UPDATE jobs SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(job.id).execute(&admin.pool).await.unwrap();
                }
                "grant_revoked" => {
                    sqlx::query("UPDATE capability_grants SET revoked_at=clock_timestamp() WHERE project_id=$1 AND actor_id=$2 AND 'read'=ANY(actions) AND revoked_at IS NULL").bind(id).bind(owner).execute(&admin.pool).await.unwrap();
                }
                "catalogue_revoked" => {
                    let mut fence = api.begin_actor(owner).await.unwrap();
                    let accepted: bool =
                        sqlx::query_scalar("SELECT kyro_lock_factory_catalogue($1,$2,$3)")
                            .bind(id)
                            .bind(revision as i64)
                            .bind(&h)
                            .fetch_one(&mut *fence)
                            .await
                            .unwrap();
                    assert!(accepted);
                    let admin2 = admin.clone();
                    let mut publishing = tokio::spawn(async move { publish(&admin2).await });
                    assert!(
                        tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            &mut publishing
                        )
                        .await
                        .is_err(),
                        "operator revocation must wait for a fenced completion"
                    );
                    fence.commit().await.unwrap();
                    publishing.await.unwrap();
                }
                _ => unreachable!(),
            }
            assert!(worker.factory_snapshot(&lease).await.is_err());
            assert!(
                worker
                    .finish_build_application(&lease, &data, |_| Ok(()))
                    .await
                    .is_err()
            );
            let count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM factory_artifacts WHERE job_id=$1")
                    .bind(job.id)
                    .fetch_one(&admin.pool)
                    .await
                    .unwrap();
            assert_eq!(count, 0);
            assert_ne!(
                api.get_job(owner, id, job.id).await.ok().map(|j| j.status),
                Some(JobStatus::Succeeded)
            );
            // Only fixture cleanup; terminal cancellation is tested through the
            // actual worker path when its lease/grants still permit it.
            if scenario == "cancelled" {
                assert_eq!(
                    worker
                        .fail_job(&lease, JobErrorCode::Cancelled, false)
                        .await
                        .unwrap()
                        .status,
                    JobStatus::Cancelled
                );
            }
            sqlx::query("UPDATE jobs SET status='failed',lease_until=NULL,lease_owner=NULL,error_code='execution_failed' WHERE id=$1 AND status='running'").bind(job.id).execute(&admin.pool).await.unwrap();
        }
        passed.push(scenario);
    }
    println!(
        "{}",
        json!({"kind":"p1_factory_transaction_fences","scenarios":passed,"signed_data":"unsigned transaction fixtures, no runtime qualification","database":"real PostgreSQL"})
    );
    serial.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated migrated P1 PostgreSQL API, worker and admin roles"]
async fn administrative_publication_is_serialized_validated_and_runtime_read_only() {
    let admin = Store::connect(&std::env::var("KYRO_TEST_ADMIN_DATABASE_URL").unwrap(), 4)
        .await
        .unwrap();
    let api = Store::connect(&std::env::var("KYRO_TEST_DATABASE_URL").unwrap(), 2)
        .await
        .unwrap();
    let worker = Store::connect(&std::env::var("KYRO_TEST_WORKER_DATABASE_URL").unwrap(), 2)
        .await
        .unwrap();
    let mut serial = admin.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(160016)")
        .execute(&mut *serial)
        .await
        .unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&admin.pool)
        .await
        .unwrap();
    assert!(database.starts_with("kyro_p1_test_"));
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM factory_catalogue_state WHERE singleton)")
            .fetch_one(&admin.pool)
            .await
            .unwrap();
    if !exists {
        let initial = json!({"transaction_fixture":"initial_publication", "revision":1});
        admin
            .publish_factory_catalogue(
                1,
                &hash(&serde_json::to_vec(&initial).unwrap()),
                &initial,
                |previous| {
                    assert!(
                        previous.is_none(),
                        "first publication must see no predecessor"
                    );
                    Ok(())
                },
            )
            .await
            .unwrap();
    }
    let before = admin.factory_catalogue().await.unwrap();
    let revision = before.0 + 1;
    let body = json!({"transaction_fixture":"publication", "revision":revision});
    let digest = hash(&serde_json::to_vec(&body).unwrap());
    assert!(
        api.publish_factory_catalogue(revision, &digest, &body, |_| Ok(()))
            .await
            .is_err()
    );
    assert!(
        worker
            .publish_factory_catalogue(revision, &digest, &body, |_| Ok(()))
            .await
            .is_err()
    );
    assert!(
        admin
            .publish_factory_catalogue(revision, &digest, &body, |_| Err(Error::Invalid(
                "fixture validation rejection".into()
            )))
            .await
            .is_err()
    );
    assert_eq!(admin.factory_catalogue().await.unwrap(), before);
    let validate = |previous: Option<&serde_json::Value>| {
        assert_eq!(previous, Some(&before.2));
        Ok(())
    };
    let (one, two) = tokio::join!(
        admin.publish_factory_catalogue(revision, &digest, &body, validate),
        admin.publish_factory_catalogue(revision, &digest, &body, validate)
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    assert!(matches!(one, Err(Error::Conflict(_))) || matches!(two, Err(Error::Conflict(_))));
    assert_eq!(
        admin.factory_catalogue().await.unwrap(),
        (revision, digest, body)
    );
    serial.commit().await.unwrap();
}
