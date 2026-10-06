//! Regression for read-only planning and single-connection coordinator progress.
#[path = "support/fixture.rs"]
mod fixture;
use fixture::Fixture;
use kyro_domain::{
    Action,
    agents::{Role, RunStatus},
    identity::MembershipRole,
};

#[tokio::test]
#[ignore = "requires dedicated migrated PostgreSQL API/worker/admin roles"]
async fn model_admission_refuses_less_than_minimum_run_ttl_without_effects() {
    let f = Fixture::new().await;
    let run = f.start("deadline-plan", true).await;
    assert_eq!(f.wave().await, 1);
    let run = f
        .coordinator
        .advance(
            &f.api,
            &f.admission,
            f.owner,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Planned);
    let run = f
        .coordinator
        .execute_plan(&f.api, f.owner, run.project_id, run.id, run.version)
        .await
        .unwrap();
    let (mut tx, mut run, _, _) = f
        .api
        .begin_agent(f.owner, run.project_id, run.id, Some(run.version))
        .await
        .unwrap();
    run.deadline = chrono::Utc::now() + chrono::Duration::seconds(9);
    kyro_store::Store::save_agent_in(&mut tx, &mut run)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before_calls = run.calls.len();
    let before_tokens = run.reserved_tokens;
    let before_provider_calls = f.provider.calls.load(std::sync::atomic::Ordering::SeqCst);
    let run = f
        .coordinator
        .advance(
            &f.api,
            &f.admission,
            f.owner,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Blocked);
    assert_eq!(run.diagnostic.as_deref(), Some("plan_resource_limit"));
    assert_eq!(run.calls.len(), before_calls);
    assert_eq!(run.reserved_tokens, before_tokens);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE project_id=$1")
        .bind(run.project_id)
        .fetch_one(&f.admin.pool)
        .await
        .unwrap();
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM effects WHERE project_id=$1")
        .bind(run.project_id)
        .fetch_one(&f.admin.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    assert_eq!(effects, 1);
    assert_eq!(f.wave().await, 0);
    assert_eq!(
        f.provider.calls.load(std::sync::atomic::Ordering::SeqCst),
        before_provider_calls
    );

    // Enough time still admits all four executors, with persisted deadlines bounded by the run.
    let run = f.start("deadline-allowed", true).await;
    assert_eq!(f.wave().await, 1);
    let run = f
        .coordinator
        .advance(
            &f.api,
            &f.admission,
            f.owner,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    let run = f
        .coordinator
        .execute_plan(&f.api, f.owner, run.project_id, run.id, run.version)
        .await
        .unwrap();
    let (mut tx, mut run, _, _) = f
        .api
        .begin_agent(f.owner, run.project_id, run.id, Some(run.version))
        .await
        .unwrap();
    run.deadline = chrono::Utc::now() + chrono::Duration::seconds(30);
    kyro_store::Store::save_agent_in(&mut tx, &mut run)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let run = f
        .coordinator
        .advance(
            &f.api,
            &f.admission,
            f.owner,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Executing);
    assert_eq!(run.calls.len(), 5);
    for call in run.calls.iter().skip(1) {
        let job = f
            .api
            .get_job(f.owner, run.project_id, call.job_id)
            .await
            .unwrap();
        assert!(job.deadline <= run.deadline);
        assert!(job.deadline >= job.created_at + chrono::Duration::seconds(10));
    }
    assert_eq!(f.wave().await, 4);
    f.coordinator
        .cancel(&f.api, f.owner, run.project_id, run.id, run.version)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated migrated PostgreSQL API/worker/admin roles"]
async fn metadata_planning_needs_no_write_but_integration_does() {
    let f = Fixture::new().await;
    let owned = f.start("owner-setup", true).await;
    f.coordinator
        .cancel(&f.api, f.owner, owned.project_id, owned.id, owned.version)
        .await
        .unwrap();
    assert_eq!(f.wave().await, 0);
    let actor = f
        .api
        .upsert_oidc_actor("https://p3.test.invalid", "readonly-planner")
        .await
        .unwrap();
    f.api
        .add_organization_member(f.owner, f.org, actor, MembershipRole::Member)
        .await
        .unwrap();
    f.api
        .create_capability_grant(
            f.owner,
            actor,
            owned.project_id,
            &[Action::Read, Action::Execute, Action::Model],
            &["*".into()],
            None,
        )
        .await
        .unwrap();
    assert!(
        f.api
            .authorize(actor, owned.project_id, "write")
            .await
            .is_err()
    );
    let run = f
        .coordinator
        .create(
            &f.api,
            &f.admission,
            actor,
            owned.project_id,
            0,
            "readonly-plan",
            owned.request.clone(),
        )
        .await
        .unwrap();
    assert_eq!(f.wave().await, 1);
    let run = f
        .coordinator
        .advance(
            &f.api,
            &f.admission,
            actor,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Planned);
    for (id, version) in [(owned.id, owned.version), (run.id, run.version + 1)] {
        let mut tx = f.api.begin_actor(actor).await.unwrap();
        assert!(
            sqlx::query("SELECT public.kyro_append_agent_event($1,$2)")
                .bind(id)
                .bind(version)
                .execute(&mut *tx)
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
    }
    let worker_can_emit: bool = sqlx::query_scalar("SELECT has_function_privilege('kyro_worker','public.kyro_append_agent_event(uuid,bigint)','EXECUTE')")
        .fetch_one(&f.admin.pool).await.unwrap();
    assert!(!worker_can_emit);
    let run = f
        .coordinator
        .compact(
            &f.api,
            actor,
            run.project_id,
            run.id,
            run.version,
            Role::Orchestrator,
        )
        .await
        .unwrap();
    let run = f
        .coordinator
        .execute_plan(&f.api, actor, run.project_id, run.id, run.version)
        .await
        .unwrap();
    let run = f
        .coordinator
        .advance(
            &f.api,
            &f.admission,
            actor,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(f.wave().await, 4);
    let run = f
        .coordinator
        .advance(
            &f.api,
            &f.admission,
            actor,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(f.wave().await, 2);
    let run = f
        .coordinator
        .advance(
            &f.api,
            &f.admission,
            actor,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Integrating);
    let run = f
        .coordinator
        .advance(
            &f.api,
            &f.admission,
            actor,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Blocked);
    assert_eq!(
        f.api
            .get_project(f.owner, run.project_id)
            .await
            .unwrap()
            .revision
            .revision,
        0
    );
    let mut request = owned.request.clone();
    request.plan_only = true;
    let run = f
        .coordinator
        .create(
            &f.api,
            &f.admission,
            actor,
            owned.project_id,
            0,
            "readonly-cancel",
            request,
        )
        .await
        .unwrap();
    assert_eq!(
        f.coordinator
            .cancel(&f.api, actor, run.project_id, run.id, run.version)
            .await
            .unwrap()
            .status,
        RunStatus::Cancelled
    );
    // Settle the cancelled queued job before the next scenario shares this database.
    assert_eq!(f.wave().await, 0);
}

#[tokio::test]
#[ignore = "requires dedicated migrated PostgreSQL API/worker/admin roles"]
async fn coordinator_progresses_with_one_connection() {
    let f = Fixture::new().await;
    let run = f.start("single-connection", true).await;
    let other = f.start("single-connection-concurrent", true).await;
    assert_eq!(f.wave().await, 2);
    let single = kyro_store::Store::connect(&std::env::var("KYRO_TEST_DATABASE_URL").unwrap(), 1)
        .await
        .unwrap();
    let (run, other) = tokio::join!(
        f.coordinator.advance(
            &single,
            &f.admission,
            f.owner,
            run.project_id,
            run.id,
            Some(run.version),
        ),
        f.coordinator.advance(
            &single,
            &f.admission,
            f.owner,
            other.project_id,
            other.id,
            Some(other.version)
        )
    );
    let run = run.unwrap();
    let other = other.unwrap();
    assert_eq!(other.status, RunStatus::Planned);
    f.coordinator
        .cancel(&single, f.owner, other.project_id, other.id, other.version)
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Planned);
    let run = f
        .coordinator
        .execute_plan(&single, f.owner, run.project_id, run.id, run.version)
        .await
        .unwrap();
    let run = f
        .coordinator
        .advance(
            &single,
            &f.admission,
            f.owner,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(f.wave().await, 4);
    let run = f
        .coordinator
        .advance(
            &single,
            &f.admission,
            f.owner,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(f.wave().await, 2);
    let run = f
        .coordinator
        .advance(
            &single,
            &f.admission,
            f.owner,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Integrating);
    let run = f
        .coordinator
        .advance(
            &single,
            &f.admission,
            f.owner,
            run.project_id,
            run.id,
            Some(run.version),
        )
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Building);
    assert_eq!(
        f.coordinator
            .advance(
                &single,
                &f.admission,
                f.owner,
                run.project_id,
                run.id,
                Some(run.version)
            )
            .await
            .unwrap()
            .version,
        run.version
    );
    f.coordinator
        .cancel(&single, f.owner, run.project_id, run.id, run.version)
        .await
        .unwrap();
    assert_eq!(f.wave().await, 0);
}
