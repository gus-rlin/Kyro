//! Explicit paid acceptance only. The campaign launcher claims each phase once
//! and holds its entire P1 project ceiling against the cumulative EUR budget.
#[path = "support/candidate.rs"]
mod candidate;
#[path = "support/fixture.rs"]
mod fixture;
use fixture::Fixture;
use kyro_domain::agents::{Role, RunStatus};
use serde_json::json;
use std::collections::BTreeSet;

#[tokio::test]
#[ignore = "requires explicitly claimed <=1 EUR NVIDIA phase and isolated PostgreSQL"]
async fn nvidia_qualifies_seven_roles_on_native_contracts() {
    let f = Fixture::nvidia(false).await;
    let run = f.start("live-seven-roles", false).await;
    assert_eq!(f.wave().await, 1);
    let planned = f.advance(&run).await;
    assert_eq!(
        planned.status,
        RunStatus::Executing,
        "{:?}",
        planned.diagnostic
    );
    assert_eq!(planned.plan.as_ref().unwrap().tasks.len(), 4);
    let executing = f.advance(&planned).await;
    assert_eq!(f.wave().await, 4);
    let reviewing = f.advance(&executing).await;
    assert_eq!(
        reviewing.status,
        RunStatus::Reviewing,
        "{:?}",
        reviewing.diagnostic
    );
    assert_eq!(f.wave().await, 2);
    let integrating = f.advance(&reviewing).await;
    assert_eq!(
        integrating.status,
        RunStatus::Integrating,
        "{:?}",
        integrating.diagnostic
    );
    let measurements = kyro_agents::measurements::collect(&f.api, f.owner, &integrating)
        .await
        .unwrap();
    assert_eq!(measurements.len(), 7);
    assert_eq!(
        measurements.iter().map(|m| m.role).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            Role::Orchestrator,
            Role::Pixel,
            Role::Moka,
            Role::Kiwi,
            Role::Biscotte,
            Role::Review,
            Role::Security
        ])
    );
    assert!(measurements.iter().all(|m| m.status == "succeeded"
        && m.provider_request_id.is_some()
        && m.usage.is_some()
        && m.estimated_units.is_some()
        && m.contract_failure.is_none()));
    assert!(
        measurements
            .iter()
            .all(|m| m.registration.provider == "nebius"
                && m.registration.retention_seconds.is_none())
    );
    let budget = f
        .api
        .get_budget(f.owner, integrating.project_id)
        .await
        .unwrap();
    assert_eq!(budget.reserved_units, 0);
    assert!((1..=100_000_000).contains(&budget.limit_units));
    let mut executor_intervals = Vec::new();
    for call in integrating.calls.iter().filter(|c| c.role.executor()) {
        let effect = f
            .api
            .get_effect_for_job(f.owner, integrating.project_id, call.job_id)
            .await
            .unwrap()
            .unwrap();
        executor_intervals.push((effect.created_at, effect.updated_at));
    }
    // Stored effect intervals establish overlapping execution, not a timing delay or invented provider duration.
    assert!(
        executor_intervals
            .iter()
            .map(|(start, _)| start)
            .max()
            .unwrap()
            < executor_intervals.iter().map(|(_, end)| end).min().unwrap()
    );
    let report = json!({"kind":"p3_nvidia_seven_roles","status":"integrating","phase":std::env::var("KYRO_P3_LIVE_PHASE").unwrap(),"run":integrating,
        "model_calls":measurements,"budget":budget,"executor_effect_intervals":executor_intervals,"model_weights_versions":null,"invoiced_cost":null,"cache":"unknown unless provided in usage",
        "catalogue_qualification":"signature fixtures; not renewal of all admissions","environment":"real PostgreSQL and NVIDIA/Nebius; no build in this phase"});
    println!("{report}");
    if let Ok(path) = std::env::var("KYRO_P3_LIVE_REPORT") {
        let path = std::path::Path::new(&path);
        assert_eq!(path.parent(), Some(std::path::Path::new("/tmp")));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("kyro-p3-nvidia-")
        );
        std::fs::write(path, serde_json::to_vec(&report).unwrap()).unwrap();
    }
    let cancelled = f
        .coordinator
        .cancel(
            &f.api,
            f.owner,
            integrating.project_id,
            integrating.id,
            integrating.version,
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE status='pending'")
        .fetch_one(&f.admin.pool)
        .await
        .unwrap();
    assert_eq!(pending, 0);
}

#[tokio::test]
#[ignore = "requires explicitly claimed <=1 EUR NVIDIA phase plus real gVisor/attestor"]
async fn nvidia_builds_a_protected_verified_candidate() {
    candidate::verify(Fixture::nvidia(true).await, true).await;
}
