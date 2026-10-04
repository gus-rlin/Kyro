//! PostgreSQL integration coverage for durable queue admission demands.
//!
//! Run against a disposable migrated database with `KYRO_TEST_DATABASE_URL`
//! set to the `kyro_api` role. Ignored by default because the test creates
//! synthetic actors, grants, and projects.

use kyro_domain::{Action, Environment, Error};
use kyro_domain::{
    identity::{GrantLimits, MembershipRole},
    model::{
        CONSERVATIVE_TOKEN_OVERHEAD, DataCategory, ModelEffectContext, ModelEffectPreparation,
        ModelEffectStore, ModelFailureCode, ModelInput, ModelPurpose, ModelRegistrationSnapshot,
        ModelRequest, ModelResponse, ModelUsage, StructuredModelOutput,
    },
    spec::{ChangeOperation, ChangeSet},
    task::{JobPayload, JobResult, JobStatus},
};
use kyro_store::{Store, projects::CreateProjectInput};
use serde_json::{json, to_vec};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a dedicated migrated PostgreSQL database in KYRO_TEST_DATABASE_URL"]
async fn postgres_queue_admission_requires_each_grant_to_cover_the_full_demand() {
    let database_url = std::env::var("KYRO_TEST_DATABASE_URL")
        .expect("set KYRO_TEST_DATABASE_URL to a disposable kyro_api database");
    let store = Store::connect(&database_url, 8)
        .await
        .expect("connect to test PostgreSQL")
        .with_environment(Environment::Development);
    store
        .check_ready()
        .await
        .expect("database schema and role are ready");
    // Both queue regressions claim globally eligible jobs in the same test DB.
    // Serialize their fixtures so another test's pending job cannot be claimed.
    let mut serial = store
        .pool
        .begin()
        .await
        .expect("queue test lock transaction");
    sqlx::query("SELECT pg_advisory_xact_lock(160016)")
        .execute(&mut *serial)
        .await
        .expect("serialize queue fixtures");

    let owner = store
        .upsert_oidc_actor(
            "https://queue.test.invalid",
            &format!("owner-{}", Uuid::new_v4()),
        )
        .await
        .expect("create synthetic owner");
    let organization = store
        .create_organization(owner, &format!("queue-{}", Uuid::new_v4()))
        .await
        .expect("create synthetic organization");
    let project = store
        .create_project(
            owner,
            CreateProjectInput {
                organization_id: organization.id,
                name: format!("queue-{}", Uuid::new_v4()),
                data_policy: None,
                limits: None,
            },
        )
        .await
        .expect("create synthetic project");

    let split_limits_actor = add_member(&store, owner, organization.id, "split-limits").await;
    let changes_actor = add_member(&store, owner, organization.id, "changes-grants").await;
    let project_id = project.project.id;

    grant(
        &store,
        owner,
        split_limits_actor,
        project_id,
        Action::Execute,
        GrantLimits {
            max_job_attempts: Some(1),
            max_job_ttl_secs: Some(20),
            max_changeset_operations: Some(1),
            ..GrantLimits::default()
        },
    )
    .await;
    grant(
        &store,
        owner,
        split_limits_actor,
        project_id,
        Action::Write,
        GrantLimits {
            max_job_attempts: Some(2),
            max_job_ttl_secs: Some(20),
            max_changeset_operations: Some(1),
            ..GrantLimits::default()
        },
    )
    .await;
    grant(
        &store,
        owner,
        split_limits_actor,
        project_id,
        Action::Execute,
        GrantLimits {
            max_job_attempts: Some(2),
            max_job_ttl_secs: Some(10),
            max_changeset_operations: Some(1),
            ..GrantLimits::default()
        },
    )
    .await;
    let split_limits_result = store
        .enqueue_job(
            split_limits_actor,
            project_id,
            0,
            "split-limits",
            change_payload(),
            Some(2),
            Some(20),
        )
        .await;
    assert!(
        matches!(&split_limits_result, Err(Error::ResourceLimit)),
        "split per-dimension grants must be rejected with ResourceLimit: {split_limits_result:?}"
    );

    let complete_changes_limits = GrantLimits {
        max_job_attempts: Some(2),
        max_job_ttl_secs: Some(20),
        max_changeset_operations: Some(1),
        ..GrantLimits::default()
    };
    grant(
        &store,
        owner,
        changes_actor,
        project_id,
        Action::Execute,
        complete_changes_limits.clone(),
    )
    .await;
    grant(
        &store,
        owner,
        changes_actor,
        project_id,
        Action::Write,
        complete_changes_limits,
    )
    .await;
    grant(
        &store,
        owner,
        changes_actor,
        project_id,
        Action::Read,
        GrantLimits::default(),
    )
    .await;
    let changes = change_set();
    let changes_job = store
        .enqueue_job(
            changes_actor,
            project_id,
            0,
            "separate-write-execute-grants",
            JobPayload::ApplyChanges {
                changes: changes.clone(),
            },
            Some(2),
            Some(20),
        )
        .await
        .expect("Execute and Write grants may be separate when each covers all facts");
    assert_eq!(changes_job.status, JobStatus::Pending);
    assert_eq!(changes_job.attempts, 0);
    assert_eq!(changes_job.max_attempts, 2);
    assert_eq!(
        changes_job
            .deadline
            .signed_duration_since(changes_job.created_at)
            .num_seconds(),
        20,
        "the admitted TTL demand must equal the stored deadline interval"
    );
    let read_job = store
        .get_job(changes_actor, project_id, changes_job.id)
        .await
        .expect("the admitted job must decode on an actor-authorized read");
    assert_eq!(read_job.id, changes_job.id);
    assert_eq!(read_job.attempts, 0);

    let worker_database_url = std::env::var("KYRO_TEST_WORKER_DATABASE_URL")
        .expect("set KYRO_TEST_WORKER_DATABASE_URL to a disposable kyro_worker database");
    let worker = Store::connect(&worker_database_url, 4)
        .await
        .expect("connect to test PostgreSQL as kyro_worker")
        .with_environment(Environment::Development);
    let lease = worker
        .claim_next_job(Uuid::new_v4(), 20)
        .await
        .expect("the worker must claim the admitted job")
        .expect("the admitted job must be claimable");
    assert_eq!(lease.job_id, changes_job.id);
    assert_eq!(lease.attempts, 1);
    let finished = worker
        .finish_apply_changes(&lease, &changes)
        .await
        .expect("the claimed job must apply atomically and finish");
    assert_eq!(finished.status, JobStatus::Succeeded);
    assert_eq!(
        finished.result,
        Some(JobResult::ApplyChanges { revision: 1 })
    );
    let persisted = store
        .get_job(changes_actor, project_id, changes_job.id)
        .await
        .expect("the completed result must remain readable");
    assert_eq!(persisted.status, JobStatus::Succeeded);
    assert_eq!(
        persisted.result,
        Some(JobResult::ApplyChanges { revision: 1 })
    );
    let events = store
        .list_events(changes_actor, project_id, 0, 100)
        .await
        .expect("the job and revision events must be readable");
    let changes_job_id = changes_job.id.to_string();
    assert!(events.iter().any(|event| {
        event.kind == "job.succeeded"
            && event
                .payload
                .get("job_id")
                .and_then(serde_json::Value::as_str)
                == Some(changes_job_id.as_str())
    }));
    let mut read_tx = store
        .begin_actor(changes_actor)
        .await
        .expect("begin actor-scoped outbox verification");
    let succeeded_outbox_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM public.outbox_events \
         WHERE project_id = $1 AND topic = 'project.event' AND payload->>'type' = 'job.succeeded'",
    )
    .bind(project_id)
    .fetch_one(&mut *read_tx)
    .await
    .expect("read the job success outbox row");
    read_tx
        .commit()
        .await
        .expect("finish actor-scoped outbox verification");
    assert_eq!(succeeded_outbox_events, 1);
}

#[tokio::test]
#[ignore = "requires a dedicated migrated PostgreSQL database in KYRO_TEST_DATABASE_URL"]
async fn postgres_model_queue_admission_does_not_require_write_and_enforces_input_limit() {
    exercise_model_queue("synthetic-local", false).await;
}

#[tokio::test]
#[ignore = "requires a dedicated migrated PostgreSQL database and admin fixture URL"]
async fn postgres_named_synthetic_reconciliation_accepts_budget_only_and_rejects_cloud() {
    exercise_model_queue("synthetic-alternate", true).await;
}

async fn exercise_model_queue(destination: &str, reconcile: bool) {
    let database_url = std::env::var("KYRO_TEST_DATABASE_URL")
        .expect("set KYRO_TEST_DATABASE_URL to a disposable kyro_api database");
    let store = Store::connect(&database_url, 8)
        .await
        .expect("connect to test PostgreSQL")
        .with_environment(Environment::Development);
    store
        .check_ready()
        .await
        .expect("database schema and role are ready");
    // Both queue regressions claim globally eligible jobs in the same test DB.
    // Serialize their fixtures so another test's pending job cannot be claimed.
    let mut serial = store
        .pool
        .begin()
        .await
        .expect("queue test lock transaction");
    sqlx::query("SELECT pg_advisory_xact_lock(160016)")
        .execute(&mut *serial)
        .await
        .expect("serialize queue fixtures");

    let owner = store
        .upsert_oidc_actor(
            "https://queue.test.invalid",
            &format!("owner-{}", Uuid::new_v4()),
        )
        .await
        .expect("create synthetic owner");
    let organization = store
        .create_organization(owner, &format!("queue-{}", Uuid::new_v4()))
        .await
        .expect("create synthetic organization");
    let project = store
        .create_project(
            owner,
            CreateProjectInput {
                organization_id: organization.id,
                name: format!("queue-{}", Uuid::new_v4()),
                data_policy: Some(kyro_domain::model::DataPolicy {
                    allowed_destinations: [destination.to_owned()].into_iter().collect(),
                    allowed_categories: [DataCategory::UserRequest].into_iter().collect(),
                    allowed_purposes: [ModelPurpose::Generation].into_iter().collect(),
                    limits: Default::default(),
                }),
                limits: None,
            },
        )
        .await
        .expect("create synthetic project");
    let model_actor = add_member(&store, owner, organization.id, "model-grants").await;
    let oversized_model_actor = add_member(&store, owner, organization.id, "model-input-cap").await;
    let project_id = project.project.id;

    let mut model_request = model_request(json!({ "prompt": "synthetic model input" }));
    model_request.destination_id = destination.into();
    let input_bytes = u32::try_from(to_vec(&model_request.input).unwrap().len()).unwrap();
    let complete_model_limits = GrantLimits {
        max_job_attempts: Some(2),
        max_job_ttl_secs: Some(20),
        max_model_input_bytes: Some(input_bytes),
        max_model_output_tokens: Some(32),
        ..GrantLimits::default()
    };
    grant(
        &store,
        owner,
        model_actor,
        project_id,
        Action::Execute,
        complete_model_limits.clone(),
    )
    .await;
    grant(
        &store,
        owner,
        model_actor,
        project_id,
        Action::Model,
        complete_model_limits,
    )
    .await;
    let admitted = store
        .enqueue_job(
            model_actor,
            project_id,
            0,
            "separate-model-execute-grants",
            JobPayload::ModelCall {
                request: model_request.clone(),
            },
            Some(2),
            Some(20),
        )
        .await
        .expect("Execute and Model grants may be separate without Write");
    let replay = store
        .enqueue_job(
            model_actor,
            project_id,
            0,
            "separate-model-execute-grants",
            JobPayload::ModelCall {
                request: model_request.clone(),
            },
            Some(2),
            Some(20),
        )
        .await
        .expect("model admission replay without Write");
    assert_eq!(replay.id, admitted.id);

    let other = store
        .create_project(
            owner,
            CreateProjectInput {
                organization_id: organization.id,
                name: format!("other-{}", Uuid::new_v4()),
                data_policy: None,
                limits: None,
            },
        )
        .await
        .expect("second project for correlation regression");
    grant(
        &store,
        owner,
        model_actor,
        other.project.id,
        Action::Read,
        GrantLimits::default(),
    )
    .await;
    // Own job from project A must not authorize a command in readable project B.
    for (scope, result) in [
        (other.project.id, json!({ "job_id": admitted.id })),
        (project_id, json!({ "job_id": "not-a-uuid" })),
        (project_id, json!({ "job_id": Uuid::new_v4() })),
        (project_id, json!({ "job_id": admitted.id, "extra": true })),
    ] {
        let mut tx = store.begin_actor(model_actor).await.unwrap();
        let denied = sqlx::query("INSERT INTO change_commands (project_id, idempotency_key, fingerprint, result, command_id, created_at) VALUES ($1, $2, $3, $4, $5, clock_timestamp())")
            .bind(scope).bind(format!("denied-{}", Uuid::new_v4())).bind(vec![0_u8;32])
            .bind(result).bind(Uuid::new_v4()).execute(&mut *tx).await;
        assert!(denied.is_err(), "foreign/malformed job reference admitted");
        tx.rollback().await.unwrap();
    }
    let mut tx = store.begin_actor(model_actor).await.unwrap();
    let changed = sqlx::query("UPDATE projects SET name = 'forbidden' WHERE id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        changed.rows_affected(),
        0,
        "model permission must not confer Write"
    );
    tx.rollback().await.unwrap();
    let wrong_env = store.clone().with_environment(Environment::Production);
    assert!(
        wrong_env
            .enqueue_job(
                model_actor,
                project_id,
                0,
                "wrong-environment",
                JobPayload::ModelCall {
                    request: model_request.clone()
                },
                Some(2),
                Some(20)
            )
            .await
            .is_err()
    );
    let no_grant_actor = add_member(&store, owner, organization.id, "no-grants").await;
    assert!(
        store
            .enqueue_job(
                no_grant_actor,
                project_id,
                0,
                "no-authority",
                JobPayload::ModelCall {
                    request: model_request.clone()
                },
                Some(2),
                Some(20)
            )
            .await
            .is_err()
    );

    let worker_url = database_url.replace("kyro_api@", "kyro_worker@");
    assert_ne!(
        worker_url, database_url,
        "test URL must name the runtime role"
    );
    let worker = Store::connect(&worker_url, 8).await.expect("worker role");
    let lease = worker
        .claim_next_job(Uuid::new_v4(), 30)
        .await
        .unwrap()
        .expect("model lease");
    assert_eq!(lease.job_id, admitted.id);
    let context = ModelEffectContext {
        project_id,
        actor_id: model_actor,
        job_id: lease.job_id,
        source_revision: lease.source_revision,
        generation: lease.generation,
        lease_owner: lease.lease_owner,
        lease_until: lease.lease_until,
        deadline: lease.deadline,
    };
    let pricing = serde_json::from_value(json!({
        "version":"synthetic-regression-v1", "effective_date":"2026-10-03", "currency":"SYN",
        "unit":"synthetic_budget_unit", "unit_scale":1,
        "input_units_per_million_tokens":1000000, "output_units_per_million_tokens":1000000,
    }))
    .unwrap();
    let registration = ModelRegistrationSnapshot {
        destination_id: model_request.destination_id.clone(),
        provider: "synthetic".into(),
        provider_kind: kyro_domain::model::ModelProviderKind::Synthetic,
        model: model_request.model.clone(),
        model_version: None,
        output_schema_id: "synthetic-output".into(),
        output_schema_version: "1".into(),
        output_schema_hash: [0; 32],
        pricing,
        retention_seconds: Some(0),
    };
    #[derive(serde::Serialize)]
    struct FingerprintMaterial<'a> {
        request: &'a ModelRequest,
        registration: &'a ModelRegistrationSnapshot,
    }
    let fingerprint = Sha256::digest(
        to_vec(&FingerprintMaterial {
            request: &model_request,
            registration: &registration,
        })
        .unwrap(),
    );
    let conservative_input_tokens = input_bytes + CONSERVATIVE_TOKEN_OVERHEAD;
    let reservation_units = registration
        .pricing
        .reservation_units(conservative_input_tokens, model_request.max_output_tokens)
        .unwrap();
    let preparation = ModelEffectPreparation {
        context: context.clone(),
        request: model_request.clone(),
        allow_new_effect: true,
        fingerprint: fingerprint.into(),
        input_bytes,
        conservative_input_tokens,
        reservation_units,
        registration,
    };
    let initial_prepare = worker.prepare_model_effect(preparation.clone()).await;
    assert!(
        matches!(initial_prepare, Err(Error::BudgetExceeded)),
        "first effect must enforce the empty budget instead of losing the budget row under RLS: {initial_prepare:?}"
    );
    let mut verify = store.begin_actor(owner).await.unwrap();
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM effects WHERE job_id = $1")
        .bind(admitted.id)
        .fetch_one(&mut *verify)
        .await
        .unwrap();
    let reserves: i64 =
        sqlx::query_scalar("SELECT count(*) FROM budget_reservations WHERE job_id = $1")
            .bind(admitted.id)
            .fetch_one(&mut *verify)
            .await
            .unwrap();
    assert_eq!(
        (effects, reserves),
        (0, 0),
        "over-budget prepare must roll back its intent and reservation"
    );
    verify.commit().await.unwrap();
    store
        .update_budget(owner, project_id, 0, reservation_units, "SYN".into(), 1)
        .await
        .unwrap();
    let prepared = worker
        .prepare_model_effect(preparation)
        .await
        .expect("first effect prepares with Execute+Model and no Write/Budget");
    worker
        .mark_sending(&context, prepared.intent.id, &model_request)
        .await
        .unwrap();
    worker
        .mark_unknown(
            &context,
            prepared.intent.id,
            ModelFailureCode::InvalidResponse,
        )
        .await
        .expect("model failure event must not roll back sending-to-unknown");
    let observed = store
        .get_effect(owner, project_id, prepared.intent.id)
        .await
        .unwrap();
    let public = serde_json::to_value(&observed).unwrap();
    assert!(public.get("fingerprint").is_none());
    assert!(public["intent"].get("fingerprint").is_none());
    let roundtrip: kyro_domain::model::EffectRecordView = serde_json::from_value(public).unwrap();
    assert_eq!(roundtrip, observed);
    let page = store
        .list_effects(owner, project_id, 100, None)
        .await
        .unwrap();
    let public_page = serde_json::to_value(page).unwrap();
    assert!(
        public_page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|effect| effect.get("fingerprint").is_none()
                && effect["intent"].get("fingerprint").is_none())
    );
    // The durable intent still carries the original hash used by preparation/replay.
    let durable = serde_json::to_value(&prepared.intent).unwrap();
    assert_eq!(
        durable["fingerprint"],
        serde_json::json!(prepared.intent.fingerprint)
    );
    assert_eq!(observed.status, kyro_domain::model::EffectStatus::Unknown);
    assert_eq!(
        observed.failure_code,
        Some(ModelFailureCode::InvalidResponse)
    );
    assert_eq!(
        observed.reservation_status,
        kyro_domain::model::ReservationStatus::Held
    );
    // Accounting context must expose only its exact job/environment, even for locking.
    for (pool, accounting_job, environment, queried_project, expected) in [
        (&worker.pool, admitted.id, "development", project_id, true),
        (
            &worker.pool,
            Uuid::new_v4(),
            "development",
            project_id,
            false,
        ),
        (&worker.pool, admitted.id, "production", project_id, false),
        (
            &worker.pool,
            admitted.id,
            "development",
            other.project.id,
            false,
        ),
        (&store.pool, admitted.id, "development", project_id, false),
    ] {
        let mut boundary = pool.begin().await.unwrap();
        sqlx::query("SELECT set_config('kyro.actor_id', '', true), set_config('kyro.queue_claim', 'off', true), set_config('kyro.environment', $1, true), set_config('kyro.accounting_job_id', $2, true)")
            .bind(environment).bind(accounting_job.to_string()).execute(&mut *boundary).await.unwrap();
        let visible: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM jobs WHERE id = $1 AND project_id = $2 FOR UPDATE")
                .bind(admitted.id)
                .bind(queried_project)
                .fetch_optional(&mut *boundary)
                .await
                .unwrap();
        assert_eq!(
            visible.is_some(),
            expected,
            "accounting lock crossed its boundary"
        );
        boundary.rollback().await.unwrap();
    }
    let response = ModelResponse {
        destination_id: model_request.destination_id.clone(),
        provider: "synthetic".into(),
        model: model_request.model.clone(),
        model_version: None,
        output: StructuredModelOutput {
            schema_id: "synthetic-output".into(),
            schema_version: "1".into(),
            data: json!({"ok":true}),
        },
        usage: Some(ModelUsage {
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_input_tokens: Some(0),
        }),
        pricing: prepared.intent.registration.pricing.clone(),
    };
    if reconcile {
        use kyro_domain::model::{ReconcileEffectRequest, ReconciledEffectDecision};
        worker
            .fail_job(
                &lease,
                kyro_domain::task::JobErrorCode::GatewayUnavailable,
                false,
            )
            .await
            .unwrap();
        let accountant = add_member(&store, owner, organization.id, "budget-only").await;
        grant(
            &store,
            owner,
            accountant,
            project_id,
            Action::Budget,
            GrantLimits::default(),
        )
        .await;
        assert!(store.get_project(accountant, project_id).await.is_err());
        let proof = ReconcileEffectRequest {
            evidence_id: "synthetic-alternate-proof".into(),
            decision: ReconciledEffectDecision::Processed {
                response: response.clone(),
            },
        };
        for (actor, project, effect) in [
            (no_grant_actor, project_id, prepared.intent.id),
            (accountant, other.project.id, prepared.intent.id),
            (accountant, project_id, Uuid::new_v4()),
        ] {
            assert!(
                store
                    .enqueue_effect_reconciliation(
                        actor,
                        project,
                        effect,
                        "reconciliation-refused",
                        proof.clone()
                    )
                    .await
                    .is_err()
            );
        }
        assert!(
            store
                .clone()
                .with_environment(Environment::Production)
                .enqueue_effect_reconciliation(
                    accountant,
                    project_id,
                    prepared.intent.id,
                    "wrong-environment",
                    proof.clone()
                )
                .await
                .is_err()
        );
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&std::env::var("KYRO_TEST_DATABASE_ADMIN_URL").unwrap())
            .await
            .unwrap();
        sqlx::query("UPDATE effects SET intent = jsonb_set(intent, '{registration,provider_kind}', '\"cloud\"') WHERE id = $1").bind(prepared.intent.id).execute(&admin).await.unwrap();
        assert!(
            store
                .enqueue_effect_reconciliation(
                    accountant,
                    project_id,
                    prepared.intent.id,
                    "cloud-refused",
                    proof.clone()
                )
                .await
                .is_err()
        );
        sqlx::query("UPDATE effects SET intent = jsonb_set(intent, '{registration,provider_kind}', '\"synthetic\"') WHERE id = $1").bind(prepared.intent.id).execute(&admin).await.unwrap();
        let command = store
            .enqueue_effect_reconciliation(
                accountant,
                project_id,
                prepared.intent.id,
                "named-synthetic",
                proof.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .enqueue_effect_reconciliation(
                    accountant,
                    project_id,
                    prepared.intent.id,
                    "named-synthetic",
                    proof.clone()
                )
                .await
                .unwrap()
                .id,
            command.id
        );
        let reconciliation_lease = worker
            .claim_next_job(Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reconciliation_lease.job_id, command.id);
        let completed = worker
            .finish_effect_reconciliation_job(
                &reconciliation_lease,
                prepared.intent.id,
                &proof,
                |intent| {
                    assert_eq!(intent.registration.destination_id, destination);
                    assert_eq!(
                        intent.registration.provider_kind,
                        kyro_domain::model::ModelProviderKind::Synthetic
                    );
                    Ok(())
                },
            )
            .await
            .unwrap();
        assert_eq!(completed.status, JobStatus::Succeeded);
        let effect = store
            .get_effect(owner, project_id, prepared.intent.id)
            .await
            .unwrap();
        assert_eq!(effect.status, kyro_domain::model::EffectStatus::Succeeded);
        assert_eq!(
            effect.reservation_status,
            kyro_domain::model::ReservationStatus::Settled
        );
        let mut tx = store.begin_actor(owner).await.unwrap();
        let counters: (i64, i64) = sqlx::query_as(
            "SELECT reserved_units, spent_units FROM project_budgets WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(counters, (0, 2));
        tx.commit().await.unwrap();
        return;
    }
    for _ in 0..2 {
        assert_eq!(
            worker
                .settle_effect(&context, prepared.intent.id, response.clone())
                .await
                .expect("known usage settles atomically and replay is idempotent"),
            kyro_domain::model::EffectStatus::Succeeded
        );
    }
    let mut accounting = store.begin_actor(owner).await.unwrap();
    let counters: (i64, i64) = sqlx::query_as(
        "SELECT reserved_units, spent_units FROM project_budgets WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_one(&mut *accounting)
    .await
    .unwrap();
    assert_eq!(counters, (0, 2));
    let ledger: (i64, i64) = sqlx::query_as("SELECT count(*), sum(units)::bigint FROM usage_ledger WHERE job_id = $1 AND kind = 'settlement'")
        .bind(admitted.id).fetch_one(&mut *accounting).await.unwrap();
    assert_eq!(ledger, (1, 2));
    accounting.commit().await.unwrap();
    let cancel = store
        .cancel_job(model_actor, project_id, admitted.id)
        .await
        .expect("cancel running model job with Execute and no Write");
    assert!(cancel.cancel_requested);
    assert_eq!(cancel.status, JobStatus::Running);
    let mut tx = store.begin_actor(model_actor).await.unwrap();
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE project_id = $1 AND type = 'job.cancel_requested' AND payload->>'job_id' = $2 AND payload->>'status' = 'running'")
        .bind(project_id).bind(admitted.id.to_string()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(events, 1);
    tx.commit().await.unwrap();
    worker
        .fail_job(&lease, kyro_domain::task::JobErrorCode::Cancelled, false)
        .await
        .unwrap();

    let too_small_input_grant = GrantLimits {
        max_job_attempts: Some(2),
        max_job_ttl_secs: Some(20),
        max_model_input_bytes: Some(1),
        max_model_output_tokens: Some(32),
        ..GrantLimits::default()
    };
    grant(
        &store,
        owner,
        oversized_model_actor,
        project_id,
        Action::Execute,
        GrantLimits {
            max_job_attempts: Some(2),
            max_job_ttl_secs: Some(20),
            max_model_output_tokens: Some(32),
            ..GrantLimits::default()
        },
    )
    .await;
    grant(
        &store,
        owner,
        oversized_model_actor,
        project_id,
        Action::Model,
        too_small_input_grant,
    )
    .await;
    assert!(input_bytes > 1);
    assert!(matches!(
        store
            .enqueue_job(
                oversized_model_actor,
                project_id,
                0,
                "model-input-too-large",
                JobPayload::ModelCall {
                    request: model_request,
                },
                Some(2),
                Some(20),
            )
            .await,
        Err(Error::ResourceLimit)
    ));
}

async fn add_member(store: &Store, owner: Uuid, organization_id: Uuid, label: &str) -> Uuid {
    let actor = store
        .upsert_oidc_actor(
            "https://queue.test.invalid",
            &format!("{label}-{}", Uuid::new_v4()),
        )
        .await
        .expect("create synthetic member");
    store
        .add_organization_member(owner, organization_id, actor, MembershipRole::Member)
        .await
        .expect("add member to synthetic organization");
    actor
}

async fn grant(
    store: &Store,
    owner: Uuid,
    actor: Uuid,
    project_id: Uuid,
    action: Action,
    limits: GrantLimits,
) {
    store
        .create_capability_grant_with_limits(
            owner,
            actor,
            project_id,
            &[action],
            &[project_id.to_string()],
            &limits,
            None,
        )
        .await
        .expect("create scoped synthetic grant");
}

fn change_payload() -> JobPayload {
    JobPayload::ApplyChanges {
        changes: change_set(),
    }
}

fn change_set() -> ChangeSet {
    ChangeSet {
        operations: vec![ChangeOperation::SetPreference {
            key: "theme".into(),
            value: json!("dark"),
        }],
    }
}

fn model_request(content: serde_json::Value) -> ModelRequest {
    ModelRequest {
        destination_id: "synthetic-local".into(),
        model: "model-v1".into(),
        input: ModelInput {
            purpose: ModelPurpose::Generation,
            categories: [DataCategory::UserRequest].into_iter().collect(),
            content,
        },
        max_output_tokens: 32,
        deadline_ms: 1_000,
    }
}
