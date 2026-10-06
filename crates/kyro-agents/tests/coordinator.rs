//! Real PostgreSQL and synthetic HTTP coordinator acceptance; factory builds use a separate recipe.
#[path = "support/fixture.rs"]
mod fixture;
use fixture::*;
use kyro_agents::memory;
use kyro_domain::{
    Error,
    agents::*,
    spec::{ChangeOperation, ChangeSet},
    task::{JobPayload, JobStatus},
};
use kyro_store::Store;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::atomic::Ordering, time::Duration};
use uuid::Uuid;
#[tokio::test]
#[ignore = "requires isolated migrated PostgreSQL API/worker/admin roles"]
async fn durable_four_role_planning_refusals_compaction_and_recovery() {
    let f = Fixture::new().await;
    f.http_controls().await;
    // Plan mode remains read-only even with all four executor scopes available.
    let initial = f.start("plan-only", true).await;
    assert_eq!(f.wave().await, 1);
    let planned = f.advance(&initial).await;
    assert_eq!(planned.status, RunStatus::Planned);
    assert_eq!(
        f.api
            .get_project(f.owner, planned.project_id)
            .await
            .unwrap()
            .revision
            .revision,
        0
    );
    assert_eq!(f.wave().await, 0);
    assert!(
        f.api
            .agent_history(f.owner, planned.project_id, planned.id, None)
            .await
            .unwrap()
            .len()
            >= 2
    );
    // Restart/compaction loads the authoritative row; it does not resend the successful effect.
    let compact = f
        .coordinator
        .compact(
            &f.api,
            f.owner,
            planned.project_id,
            planned.id,
            planned.version,
            Role::Orchestrator,
        )
        .await
        .unwrap();
    let reconnected = Store::connect(&std::env::var("KYRO_TEST_DATABASE_URL").unwrap(), 8)
        .await
        .unwrap();
    let restored = reconnected
        .get_agent_run(f.owner, planned.project_id, planned.id)
        .await
        .unwrap();
    assert_eq!(
        restored.checkpoints[&Role::Orchestrator].objective,
        restored.request.request
    );
    assert_eq!(restored.reserved_tokens, compact.reserved_tokens);
    let replay = f
        .coordinator
        .create(
            &reconnected,
            &f.admission,
            f.owner,
            initial.project_id,
            0,
            "plan-only",
            initial.request.clone(),
        )
        .await
        .unwrap();
    assert_eq!(replay.id, initial.id);
    assert_eq!(f.wave().await, 0);
    let executing = f
        .coordinator
        .execute_plan(
            &f.api,
            f.owner,
            compact.project_id,
            compact.id,
            compact.version,
        )
        .await
        .unwrap();
    let dispatched = f.advance(&executing).await;
    assert_eq!(
        dispatched
            .calls
            .iter()
            .filter(|c| c.role.executor())
            .count(),
        4
    );
    *f.provider.executor_gate.lock().await =
        Some(std::sync::Arc::new(tokio::sync::Barrier::new(4)));
    assert_eq!(f.wave().await, 4);
    *f.provider.executor_gate.lock().await = None;
    assert!(f.provider.maximum.load(Ordering::SeqCst) >= 4);
    let reviewed = f.advance(&dispatched).await;
    assert_eq!(reviewed.status, RunStatus::Reviewing);
    assert_eq!(f.wave().await, 2);
    let integrating = f.advance(&reviewed).await;
    assert_eq!(integrating.status, RunStatus::Integrating);
    assert_eq!(
        f.api
            .get_project(f.owner, integrating.project_id)
            .await
            .unwrap()
            .revision
            .revision,
        0
    );
    // A retouch to a declared node cannot be overwritten by the old answer.
    f.api
        .apply_changes(
            f.owner,
            integrating.project_id,
            0,
            "client-retouch",
            &ChangeSet {
                operations: vec![ChangeOperation::AddNode {
                    node: serde_json::from_value(node("records", "B031")).unwrap(),
                }],
            },
        )
        .await
        .unwrap();
    let blocked = f.advance(&integrating).await;
    assert_eq!(blocked.status, RunStatus::Blocked);
    assert_eq!(blocked.diagnostic.as_deref(), Some("client_edit_conflict"));
    assert_eq!(
        f.api
            .get_project(f.owner, blocked.project_id)
            .await
            .unwrap()
            .revision
            .revision,
        1
    );
    // Catalogue gap and invalid executor scopes never integrate a generated application.
    f.provider.mode.store(1, Ordering::SeqCst);
    let gap = f.start("gap", false).await;
    f.wave().await;
    assert_eq!(f.advance(&gap).await.status, RunStatus::Blocked);
    f.provider.mode.store(2, Ordering::SeqCst);
    let bad = f.start("bad-scope", false).await;
    f.wave().await;
    let bad = f.advance(&bad).await;
    let bad = f.advance(&bad).await;
    f.wave().await;
    let bad = f.advance(&bad).await;
    f.wave().await;
    let bad = f.advance(&bad).await;
    assert_eq!(bad.status, RunStatus::Blocked);
    assert!(bad.calls.len() <= 9);
    assert_eq!(
        f.api
            .get_project(f.owner, bad.project_id)
            .await
            .unwrap()
            .revision
            .revision,
        0
    );
    // An in-flight old directive is cancelled atomically and is never imported into the new epoch.
    f.provider.mode.store(0, Ordering::SeqCst);
    let old = f.start("directive", true).await;
    let new = f
        .coordinator
        .revise(
            &f.api,
            &f.admission,
            f.owner,
            old.project_id,
            old.id,
            old.version,
            "New synthetic objective, retain only compatible work".into(),
        )
        .await
        .unwrap();
    let old_job = f
        .api
        .get_job(f.owner, old.project_id, old.calls[0].job_id)
        .await
        .unwrap();
    assert!(old_job.cancel_requested);
    assert_eq!(new.epoch, 2);
    // P1 visits one candidate per project per poll: first consume the cancelled predecessor.
    assert_eq!(f.wave().await, 0);
    assert_eq!(f.wave().await, 1);
    let current = f.advance(&new).await;
    assert_eq!(current.status, RunStatus::Planned);
    assert_eq!(current.calls[0].result_digest, None);
    // Foreign organization cannot read state/history/memory; CAS prevents simultaneous control edits.
    let stranger = f
        .api
        .upsert_oidc_actor("https://p3.test.invalid", &Uuid::new_v4().to_string())
        .await
        .unwrap();
    assert!(
        f.api
            .get_agent_run(stranger, current.project_id, current.id)
            .await
            .is_err()
    );
    let a = f.coordinator.compact(
        &f.api,
        f.owner,
        current.project_id,
        current.id,
        current.version,
        Role::Review,
    );
    let b = f.coordinator.compact(
        &f.api,
        f.owner,
        current.project_id,
        current.id,
        current.version,
        Role::Security,
    );
    let (a, b) = tokio::join!(a, b);
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let persisted = f
        .api
        .get_agent_run(f.owner, current.project_id, current.id)
        .await
        .unwrap();
    let mut tx = f.api.begin_actor(f.owner).await.unwrap();
    assert!(
        sqlx::query("DELETE FROM agent_history WHERE run_id=$1")
            .bind(current.id)
            .execute(&mut *tx)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let mut contextual = f
        .api
        .begin_agent(
            f.owner,
            current.project_id,
            current.id,
            Some(persisted.version),
        )
        .await
        .unwrap();
    for i in 0..24 {
        memory::remember(
            &mut contextual.1,
            Role::Orchestrator,
            MemoryKind::Hypothesis,
            format!("synthetic-observation:{i}"),
            "Synthetic uncertain observation. ".repeat(30),
        )
        .unwrap();
    }
    memory::context(&mut contextual.1, Role::Orchestrator).unwrap();
    assert!(
        contextual.1.checkpoints[&Role::Orchestrator]
            .memory_ids
            .len()
            >= 24
    );
    Store::save_agent_in(&mut contextual.0, &mut contextual.1)
        .await
        .unwrap();
    contextual.0.commit().await.unwrap();
    assert!(
        !memory::search(&contextual.1, "uncertain")
            .unwrap()
            .is_empty()
    );
    // Waiting polls do not create synthetic progress, versions or an unbounded history.
    let waiting = f.start("waiting", true).await;
    assert_eq!(f.advance(&waiting).await.version, waiting.version);
    assert_eq!(
        f.api
            .agent_history(f.owner, waiting.project_id, waiting.id, None)
            .await
            .unwrap()
            .len(),
        1
    );
    f.wave().await;
    let waiting = f.advance(&waiting).await;
    assert_eq!(waiting.status, RunStatus::Planned);

    // Both independent reviews can stop the candidate, before any application mutation.
    f.provider.mode.store(3, Ordering::SeqCst);
    let rejected = f.start("review-reject", false).await;
    f.wave().await;
    let rejected = f.advance(&rejected).await;
    let rejected = f.advance(&rejected).await;
    f.wave().await;
    let rejected = f.advance(&rejected).await;
    f.wave().await;
    assert_eq!(
        f.advance(&rejected).await.diagnostic.as_deref(),
        Some("review_or_security_rejected")
    );
    f.provider.mode.store(0, Ordering::SeqCst);

    // Unrelated edits survive; reviews must approve the new exact candidate before integration.
    let rebase = f.integrating("harmless-rebase").await;
    let edit = ChangeSet {
        operations: vec![ChangeOperation::SetPreference {
            key: "locale".into(),
            value: json!("fr-FR"),
        }],
    };
    f.api
        .apply_changes(f.owner, rebase.project_id, 0, "client-harmless", &edit)
        .await
        .unwrap();
    let rebase = f.advance(&rebase).await;
    assert_eq!(rebase.status, RunStatus::Reviewing);
    assert_eq!(rebase.source_revision, 1);
    assert_eq!(f.wave().await, 2);
    let rebase = f.advance(&rebase).await;
    let built = f.advance(&rebase).await;
    assert_eq!(built.status, RunStatus::Building);
    let project = f.api.get_project(f.owner, built.project_id).await.unwrap();
    assert_eq!(project.revision.revision, 2);
    assert_eq!(project.revision.spec.preferences["locale"], "fr-FR");
    assert_eq!(project.revision.spec.nodes.len(), 4);
    assert_eq!(f.advance(&built).await.build_job_id, built.build_job_id);
    assert_eq!(
        f.api
            .get_project(f.owner, built.project_id)
            .await
            .unwrap()
            .revision
            .revision,
        2
    );
    let lease = f
        .worker
        .claim_next_job(Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(Some(lease.job_id), built.build_job_id);
    assert!(matches!(lease.payload, JobPayload::BuildApplication { .. }));
    // A worker failure cannot be interpreted as verified, even after integration succeeded.
    f.worker
        .fail_job(
            &lease,
            kyro_domain::task::JobErrorCode::ExecutionFailed,
            false,
        )
        .await
        .unwrap();
    assert_eq!(f.advance(&built).await.status, RunStatus::Blocked);

    // Revised instructions reuse only identical completed contracts, never changed microtasks.
    let retained = f.integrating("retention").await;
    let retained = f
        .coordinator
        .revise(
            &f.api,
            &f.admission,
            f.owner,
            retained.project_id,
            retained.id,
            retained.version,
            "Keep the same four synthetic contracts".into(),
        )
        .await
        .unwrap();
    assert_eq!(retained.retained_results.len(), 4);
    assert_eq!(f.wave().await, 1);
    let retained = f.advance(&retained).await;
    assert_eq!(retained.results.len(), 4);
    assert!(
        !retained
            .calls
            .iter()
            .any(|c| c.epoch == retained.epoch && c.role.executor())
    );
    f.provider.mode.store(6, Ordering::SeqCst);
    let retained = f
        .coordinator
        .revise(
            &f.api,
            &f.admission,
            f.owner,
            retained.project_id,
            retained.id,
            retained.version,
            "Change the identity contract, retain the other three".into(),
        )
        .await
        .unwrap();
    assert_eq!(f.wave().await, 1);
    let retained = f.advance(&retained).await;
    assert_eq!(retained.results.len(), 3);
    assert!(!retained.results.contains_key("identity"));
    let retained = f.advance(&retained).await;
    assert_eq!(
        retained
            .calls
            .iter()
            .filter(|c| c.epoch == retained.epoch && c.role.executor())
            .count(),
        1
    );
    f.coordinator
        .cancel(
            &f.api,
            f.owner,
            retained.project_id,
            retained.id,
            retained.version,
        )
        .await
        .unwrap();
    assert_eq!(f.wave().await, 0);
    f.provider.mode.store(0, Ordering::SeqCst);

    // Force build admission to refuse AFTER apply_changes: the savepoint keeps both operations atomic.
    let atomic = f.integrating("atomic-integration").await;
    sqlx::query("UPDATE projects SET limits=jsonb_set(limits,'{max_queued_jobs}','1') WHERE id=$1")
        .bind(atomic.project_id)
        .execute(&f.admin.pool)
        .await
        .unwrap();
    f.api
        .enqueue_job(
            f.owner,
            atomic.project_id,
            0,
            "fill-queue",
            JobPayload::ApplyChanges {
                changes: ChangeSet {
                    operations: vec![ChangeOperation::SetPreference {
                        key: "queued".into(),
                        value: json!(true),
                    }],
                },
            },
            Some(1),
            None,
        )
        .await
        .unwrap();
    let atomic = f.advance(&atomic).await;
    assert_eq!(atomic.status, RunStatus::Blocked);
    assert_eq!(
        f.api
            .get_project(f.owner, atomic.project_id)
            .await
            .unwrap()
            .revision
            .revision,
        0
    );
    assert!(atomic.integrated_revision.is_none() && atomic.build_job_id.is_none());
    let filler = f
        .worker
        .claim_next_job(Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    f.worker
        .fail_job(
            &filler,
            kyro_domain::task::JobErrorCode::ExecutionFailed,
            false,
        )
        .await
        .unwrap();

    // Backend-owned invariants prevent omitted semantic conflicts between different node IDs.
    let semantic = f.start("semantic", true).await;
    let mut plan: Plan = serde_json::from_value(json!({"objective":"Two record nodes", "tasks":[task("a","B031",1),task("b","B031",1)],"missing_capabilities":[]})).unwrap();
    assert!(matches!(
        f.coordinator
            .replace_plan(
                &f.api,
                f.owner,
                semantic.project_id,
                semantic.id,
                semantic.version,
                plan.clone()
            )
            .await,
        Err(Error::Conflict(_))
    ));
    plan.tasks[1].dependencies.insert("a".into());
    for t in &mut plan.tasks {
        t.deterministic = Some(
            serde_json::from_value(
                json!({"operations":[{"op":"add_node","node":node(&t.id,"B031") }]}),
            )
            .unwrap(),
        );
    }
    let semantic = f
        .coordinator
        .replace_plan(
            &f.api,
            f.owner,
            semantic.project_id,
            semantic.id,
            semantic.version,
            plan,
        )
        .await
        .unwrap();
    assert!(
        !semantic.plan.as_ref().unwrap().tasks[0]
            .capabilities
            .is_empty()
    );
    assert!(
        !semantic.plan.as_ref().unwrap().tasks[0]
            .protected_criteria
            .is_empty()
    );
    let semantic = f
        .coordinator
        .execute_plan(
            &f.api,
            f.owner,
            semantic.project_id,
            semantic.id,
            semantic.version,
        )
        .await
        .unwrap();
    assert_eq!(f.wave().await, 0); // canceled planning job drained, deterministic work needs no provider
    let semantic = f.advance(&semantic).await;
    let semantic = f.advance(&semantic).await;
    assert_eq!(semantic.results.len(), 2);
    assert!(!semantic.calls.iter().any(|c| c.role.executor()));
    let semantic = f
        .coordinator
        .cancel(
            &f.api,
            f.owner,
            semantic.project_id,
            semantic.id,
            semantic.version,
        )
        .await
        .unwrap();
    assert_eq!(semantic.status, RunStatus::Cancelled);
    f.wave().await;
    f.wave().await;

    // New instructions during an emitted HTTP effect fence its settlement and preserve cumulative budgets.
    f.provider.mode.store(4, Ordering::SeqCst);
    let sending = f.start("during-call", true).await;
    let lease = f
        .worker
        .claim_next_job(Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let effect = f.effect(lease);
    let directive = async {
        tokio::time::timeout(Duration::from_secs(5), f.provider.entered.notified())
            .await
            .unwrap();
        let new = f
            .coordinator
            .revise(
                &f.api,
                &f.admission,
                f.owner,
                sending.project_id,
                sending.id,
                sending.version,
                "New safe synthetic directive".into(),
            )
            .await
            .unwrap();
        assert!(new.reserved_tokens > sending.reserved_tokens);
        f.provider.release.notify_one();
        new
    };
    let (settled, new) = tokio::join!(effect, directive);
    let settled = settled.unwrap();
    assert_eq!(settled.status, JobStatus::Cancelled);
    let old_effect: String = sqlx::query_scalar("SELECT status FROM effects WHERE job_id=$1")
        .bind(settled.id)
        .fetch_one(&f.admin.pool)
        .await
        .unwrap();
    assert_eq!(old_effect, "succeeded"); // Known usage settles once; the cancelled job cannot import its result.
    let measurements = kyro_agents::measurements::collect(&f.api, f.owner, &new)
        .await
        .unwrap();
    assert_eq!(measurements[0].job_status, "cancelled");
    assert_eq!(measurements[0].status, "succeeded");
    assert!(measurements[0].usage.is_some() && measurements[0].estimated_units.is_some());
    f.provider.mode.store(0, Ordering::SeqCst);
    f.wave().await;
    assert_eq!(f.advance(&new).await.status, RunStatus::Planned);
    let count = f.provider.calls.load(Ordering::SeqCst);
    assert_eq!(f.wave().await, 0);
    assert_eq!(f.provider.calls.load(Ordering::SeqCst), count);

    // Revocation during an actual emitted synthetic HTTP request closes metadata via a narrow helper.
    f.provider.mode.store(4, Ordering::SeqCst);
    let revoked = f.start("revoke-during-call", true).await;
    let lease = f
        .worker
        .claim_next_job(Uuid::new_v4(), 2)
        .await
        .unwrap()
        .unwrap();
    let effect = f.effect(lease);
    let revoke = async {
        tokio::time::timeout(Duration::from_secs(5), f.provider.entered.notified())
            .await
            .unwrap();
        sqlx::query("UPDATE capability_grants SET revoked_at=clock_timestamp() WHERE project_id=$1 AND actor_id=$2 AND 'model'=ANY(actions) AND revoked_at IS NULL")
            .bind(revoked.project_id).bind(f.owner).execute(&f.admin.pool).await.unwrap();
        assert!(f.api.close_revoked_agent(revoked.id).await.unwrap());
        assert!(!f.api.close_revoked_agent(revoked.id).await.unwrap());
        f.provider.release.notify_one();
    };
    let (settled, ()) = tokio::join!(effect, revoke);
    assert!(matches!(settled, Err(Error::NotFound | Error::Forbidden)));
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(
        f.worker
            .claim_next_job(Uuid::new_v4(), 2)
            .await
            .unwrap()
            .is_none()
    );
    let job_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
        .bind(revoked.calls[0].job_id)
        .fetch_one(&f.admin.pool)
        .await
        .unwrap();
    assert_eq!(job_status, "cancelled");
    let state: Value = sqlx::query_scalar("SELECT state FROM agent_runs WHERE id=$1")
        .bind(revoked.id)
        .fetch_one(&f.admin.pool)
        .await
        .unwrap();
    assert_eq!(state["status"], "blocked");
    assert_eq!(state["diagnostic"], "permission_revoked");
    assert_eq!(
        f.api.get_project(f.owner, revoked.project_id).await.err(),
        Some(Error::NotFound)
    );
    f.provider.mode.store(0, Ordering::SeqCst);
    // Kill a real worker process while the provider has observed its request.
    let interrupted = f.start("worker-kill", true).await;
    let worker_path = std::env::var("KYRO_P3_WORKER_BIN")
        .unwrap_or_else(|_| "/workspace/target/debug/kyro-worker".into());
    let mut worker = tokio::process::Command::new(worker_path);
    f.provider.mode.store(4, Ordering::SeqCst);
    worker
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("KYRO_ENV", "development")
        .env("KYRO_SYNTHETIC_PROVIDERS", "true")
        .env(
            "KYRO_WORKER_DATABASE_URL",
            std::env::var("KYRO_TEST_WORKER_DATABASE_URL").unwrap(),
        )
        .env("KYRO_WORKER_POLL_MS", "25")
        .env("KYRO_LEASE_SECONDS", "2")
        .env("KYRO_MODEL_REGISTRY_PATH", &f.registry_path)
        .env("KYRO_MODEL_ALLOW_SYNTHETIC_LOOPBACK", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut worker = worker.spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(10), f.provider.entered.notified())
        .await
        .unwrap();
    worker.start_kill().unwrap();
    assert!(!worker.wait().await.unwrap().success());
    f.provider.release.notify_one();
    tokio::time::sleep(Duration::from_millis(2300)).await;
    let before = f.provider.calls.load(Ordering::SeqCst);
    assert!(
        f.worker
            .claim_next_job(Uuid::new_v4(), 2)
            .await
            .unwrap()
            .is_none()
    );
    let recovery = f.advance(&interrupted).await;
    assert_eq!(recovery.status, RunStatus::Blocked);
    let effect_status: String = sqlx::query_scalar("SELECT status FROM effects WHERE job_id=$1")
        .bind(interrupted.calls[0].job_id)
        .fetch_one(&f.admin.pool)
        .await
        .unwrap();
    assert_eq!(effect_status, "unknown");
    assert_eq!(f.provider.calls.load(Ordering::SeqCst), before);
    assert!(recovery.checkpoints.contains_key(&Role::Orchestrator));
    assert_eq!(recovery.reserved_tokens, interrupted.reserved_tokens);
    f.provider.mode.store(0, Ordering::SeqCst);

    // Poll >32 active projects fairly, without rewriting logical versions or history.
    let mut runs = vec![];
    for i in 0..33 {
        runs.push(f.start(&format!("fairness-{i}"), true).await);
    }
    let first = f.api.due_agent_runs().await.unwrap();
    let second = f.api.due_agent_runs().await.unwrap();
    let seen: BTreeSet<_> = first.iter().chain(&second).map(|(id, _, _)| *id).collect();
    assert!(runs.iter().all(|r| seen.contains(&r.id)));
    for r in runs {
        assert_eq!(
            f.api
                .agent_history(f.owner, r.project_id, r.id, None)
                .await
                .unwrap()
                .len(),
            1
        );
        f.coordinator
            .cancel(&f.api, f.owner, r.project_id, r.id, r.version)
            .await
            .unwrap();
    }
    f.wave().await;
    println!(
        "P3 verified: read-only plan, four independent HTTP effects, protected scope, retry stop, conflict, directive cancellation, restart, compaction, RLS, CAS and immutable history; no real model or factory build claimed"
    );
}
