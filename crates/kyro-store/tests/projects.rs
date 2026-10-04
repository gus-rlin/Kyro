//! PostgreSQL integration coverage for revision CAS, idempotency, and tenant isolation.
//!
//! Run against an isolated, migrated PostgreSQL database with
//! `KYRO_TEST_DATABASE_URL` set to the `kyro_api` runtime role. These tests are
//! ignored by default because they create persistent synthetic actors/projects.

use kyro_domain::identity::{GrantLimits, MembershipRole};
use kyro_domain::model::DataPolicy;
use kyro_domain::spec::{ChangeOperation, ChangeSet, ProjectLimits};
use kyro_domain::{Action, Environment, Error};
use kyro_store::{Store, projects::CreateProjectInput};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

fn preference_change(key: &str, value: &str) -> ChangeSet {
    ChangeSet {
        operations: vec![ChangeOperation::SetPreference {
            key: key.to_owned(),
            value: json!(value),
        }],
    }
}

fn two_preference_changes() -> ChangeSet {
    ChangeSet {
        operations: vec![
            ChangeOperation::SetPreference {
                key: "first".into(),
                value: json!("one"),
            },
            ChangeOperation::SetPreference {
                key: "second".into(),
                value: json!("two"),
            },
        ],
    }
}

#[tokio::test]
#[ignore = "requires a dedicated migrated PostgreSQL database and admin fixture URL"]
async fn postgres_project_pages_retrieve_more_than_one_thousand_without_crossing_actor_or_environment()
 {
    let url = std::env::var("KYRO_TEST_DATABASE_URL").unwrap();
    let admin_url = std::env::var("KYRO_TEST_DATABASE_ADMIN_URL").unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await
        .unwrap();
    let store = Store::connect(&url, 2).await.unwrap();
    let owner = store
        .upsert_oidc_actor("https://pages.test.invalid", &Uuid::new_v4().to_string())
        .await
        .unwrap();
    let other = store
        .upsert_oidc_actor("https://pages.test.invalid", &Uuid::new_v4().to_string())
        .await
        .unwrap();
    let org = store
        .create_organization(owner, "page fixture")
        .await
        .unwrap();
    let first = store
        .create_project(
            owner,
            CreateProjectInput {
                organization_id: org.id,
                name: "page fixture".into(),
                data_policy: None,
                limits: None,
            },
        )
        .await
        .unwrap();
    // Clone validated project configuration; seed cardinality with the fixture admin.
    let ids: Vec<Uuid> = sqlx::query_scalar("INSERT INTO projects (organization_id, name, data_policy, limits, created_by, updated_at) SELECT organization_id, 'page fixture', data_policy, limits, created_by, updated_at FROM projects, generate_series(1, 1005) WHERE id = $1 RETURNING id")
        .bind(first.project.id).fetch_all(&admin).await.unwrap();
    sqlx::query("INSERT INTO capability_grants (actor_id, project_id, actions, resources, environment, created_by) SELECT $1, id, ARRAY['read'], ARRAY[id::text], 'development', $1 FROM projects WHERE id = ANY($2)")
        .bind(owner).bind(&ids).execute(&admin).await.unwrap();
    let page = store.list_projects(owner, 1000, None).await.unwrap();
    assert_eq!(page.items.len(), 1000);
    let cursor = page
        .next_cursor
        .clone()
        .expect("remaining six projects advertised");
    let tail = store
        .list_projects(owner, 1000, Some(cursor.clone()))
        .await
        .unwrap();
    assert_eq!(tail.items.len(), 6);
    assert!(tail.next_cursor.is_none());
    let mut observed: Vec<_> = page
        .items
        .iter()
        .chain(tail.items.iter())
        .map(|project| project.id)
        .collect();
    observed.sort();
    observed.dedup();
    assert_eq!(observed.len(), 1006);
    assert!(
        store
            .list_projects(other, 1000, None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .list_projects(other, 1000, Some(cursor.clone()))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .clone()
            .with_environment(Environment::Production)
            .list_projects(owner, 1000, None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // A cursor does not freeze permissions: revoke one second-page grant and re-read.
    sqlx::query("UPDATE capability_grants SET revoked_at = clock_timestamp() WHERE actor_id = $1 AND project_id = $2")
        .bind(owner).bind(tail.items[0].id).execute(&admin).await.unwrap();
    assert_eq!(
        store
            .list_projects(owner, 1000, Some(cursor))
            .await
            .unwrap()
            .items
            .len(),
        5
    );
    for limit in [0, 1001] {
        assert!(matches!(
            store.list_projects(owner, limit, None).await,
            Err(Error::Invalid(_))
        ));
    }
}

#[tokio::test]
#[ignore = "requires a dedicated migrated PostgreSQL database in KYRO_TEST_DATABASE_URL"]
async fn postgres_projects_enforce_isolation_cas_and_idempotency() {
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

    let owner_a = store
        .upsert_oidc_actor(
            "https://projects.test.invalid",
            &format!("owner-a-{}", Uuid::new_v4()),
        )
        .await
        .expect("create synthetic actor A");
    let owner_b = store
        .upsert_oidc_actor(
            "https://projects.test.invalid",
            &format!("owner-b-{}", Uuid::new_v4()),
        )
        .await
        .expect("create synthetic actor B");
    let organization_a = store
        .create_organization(owner_a, &format!("project-test-a-{}", Uuid::new_v4()))
        .await
        .expect("create organization A");
    let organization_b = store
        .create_organization(owner_b, &format!("project-test-b-{}", Uuid::new_v4()))
        .await
        .expect("create organization B");
    let project_a = store
        .create_project(
            owner_a,
            CreateProjectInput {
                organization_id: organization_a.id,
                name: format!("project-a-{}", Uuid::new_v4()),
                data_policy: None,
                limits: None,
            },
        )
        .await
        .expect("create project A");
    let project_b = store
        .create_project(
            owner_b,
            CreateProjectInput {
                organization_id: organization_b.id,
                name: format!("project-b-{}", Uuid::new_v4()),
                data_policy: None,
                limits: None,
            },
        )
        .await
        .expect("create project B");

    assert_eq!(project_a.project.current_revision, 0);
    assert_eq!(project_a.revision.revision, 0);
    assert!(matches!(
        store.get_project(owner_a, project_b.project.id).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        store.get_project(owner_b, project_a.project.id).await,
        Err(Error::NotFound)
    ));

    let replayable_change = preference_change("theme", "dark");
    let (first, replay) = tokio::join!(
        store.apply_changes(
            owner_a,
            project_a.project.id,
            0,
            "concurrent-replay",
            &replayable_change,
        ),
        store.apply_changes(
            owner_a,
            project_a.project.id,
            0,
            "concurrent-replay",
            &replayable_change,
        )
    );
    let first = first.expect("first identical command succeeds");
    let replay = replay.expect("concurrent identical command replays");
    assert_eq!(first.command_id, replay.command_id);
    assert_eq!(first.revision.revision, 1);
    assert_eq!(replay.revision.revision, 1);

    let late_replay = store
        .apply_changes(
            owner_a,
            project_a.project.id,
            0,
            "concurrent-replay",
            &replayable_change,
        )
        .await
        .expect("an identical retry replays even after the source revision is stale");
    assert_eq!(first.command_id, late_replay.command_id);
    assert_eq!(first.revision.revision, late_replay.revision.revision);

    assert!(matches!(
        store
            .apply_changes(
                owner_a,
                project_a.project.id,
                0,
                "concurrent-replay",
                &preference_change("theme", "light"),
            )
            .await,
        Err(Error::IdempotencyConflict)
    ));
    let replay_events = store
        .list_events(
            owner_a,
            project_a.project.id,
            project_a.project.event_sequence,
            100,
        )
        .await
        .expect("read events after the creation snapshot");
    assert_eq!(
        replay_events.len(),
        1,
        "one mutation emits one revision event"
    );
    assert!(
        replay_events[0]
            .payload
            .as_object()
            .expect("event is a JSON object")
            .keys()
            .all(|key| matches!(
                key.as_str(),
                "project_id" | "revision" | "command_id" | "status"
            ))
    );

    let change_left = preference_change("winner", "left");
    let change_right = preference_change("winner", "right");
    let (left, right) = tokio::join!(
        store.apply_changes(owner_b, project_b.project.id, 0, "cas-left", &change_left),
        store.apply_changes(owner_b, project_b.project.id, 0, "cas-right", &change_right)
    );
    let (winner, stale) = match (left, right) {
        (Ok(_), Err(error)) => ("left", error),
        (Err(error), Ok(_)) => ("right", error),
        _ => panic!("exactly one concurrent writer must advance revision 0"),
    };
    assert!(matches!(
        stale,
        Error::StaleRevision {
            expected: 0,
            current: 1
        }
    ));

    let current_b = store
        .get_project(owner_b, project_b.project.id)
        .await
        .expect("read project B after CAS");
    assert_eq!(current_b.project.current_revision, 1);
    assert_eq!(
        current_b.revision.spec.preferences["winner"].as_str(),
        Some(winner)
    );

    let stable_spec = current_b.revision.spec.clone();
    let mut next_limits = ProjectLimits::default();
    next_limits.max_active_jobs = 5;
    next_limits.max_revisions = 4;
    let after_limits = store
        .update_limits(owner_b, project_b.project.id, 1, next_limits)
        .await
        .expect("limits update creates a versioned revision");
    assert_eq!(after_limits.project.current_revision, 2);
    assert_eq!(after_limits.revision.spec, stable_spec);

    let after_policy = store
        .update_data_policy(owner_b, project_b.project.id, 2, DataPolicy::default())
        .await
        .expect("policy update creates a versioned revision");
    assert_eq!(after_policy.project.current_revision, 3);
    assert_eq!(after_policy.revision.spec, stable_spec);
    assert!(matches!(
        store
            .update_data_policy(owner_b, project_b.project.id, 2, DataPolicy::default(),)
            .await,
        Err(Error::StaleRevision {
            expected: 2,
            current: 3
        })
    ));

    let mut too_small = ProjectLimits::default();
    too_small.max_revisions = 3;
    assert!(matches!(
        store
            .update_limits(owner_b, project_b.project.id, 3, too_small)
            .await,
        Err(Error::ResourceLimit)
    ));

    let decisions = store
        .list_decisions(owner_a, project_a.project.id, None, 100)
        .await
        .expect("read project A decisions");
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].revision, 0);
    assert_eq!(decisions[0].kind, "project.initialized");
    assert_eq!(
        store
            .list_decisions(owner_b, project_b.project.id, None, 100)
            .await
            .expect("read project B configuration decisions")
            .len(),
        3
    );
    assert_eq!(
        store
            .list_projects(owner_a, 1000, None)
            .await
            .expect("list actor A projects")
            .items
            .iter()
            .map(|project| project.id)
            .collect::<Vec<_>>(),
        vec![project_a.project.id]
    );

    let limited_writer = store
        .upsert_oidc_actor(
            "https://projects.test.invalid",
            &format!("limited-writer-{}", Uuid::new_v4()),
        )
        .await
        .expect("create synthetic limited writer");
    store
        .add_organization_member(
            owner_a,
            organization_a.id,
            limited_writer,
            MembershipRole::Member,
        )
        .await
        .expect("add writer to owner organization");
    let limited_project = store
        .create_project(
            owner_a,
            CreateProjectInput {
                organization_id: organization_a.id,
                name: format!("limited-project-{}", Uuid::new_v4()),
                data_policy: None,
                limits: None,
            },
        )
        .await
        .expect("create project for grant-limit coverage");
    let project_resource = limited_project.project.id.to_string();
    let initial_limit = GrantLimits {
        max_changeset_operations: Some(2),
        ..GrantLimits::default()
    };
    let initial_grant = store
        .create_capability_grant_with_limits(
            owner_a,
            limited_writer,
            limited_project.project.id,
            &[Action::Write],
            std::slice::from_ref(&project_resource),
            &initial_limit,
            None,
        )
        .await
        .expect("grant writer permission for up to two operations");

    let two_operations = two_preference_changes();
    let initial_result = store
        .apply_changes(
            limited_writer,
            limited_project.project.id,
            0,
            "limited-replay",
            &two_operations,
        )
        .await
        .expect("a two-operation change fits the initial grant");
    assert_eq!(initial_result.revision.revision, 1);

    store
        .revoke_capability_grant(owner_a, limited_project.project.id, initial_grant.id)
        .await
        .expect("revoke the wider grant");
    let stricter_limit = GrantLimits {
        max_changeset_operations: Some(1),
        ..GrantLimits::default()
    };
    store
        .create_capability_grant_with_limits(
            owner_a,
            limited_writer,
            limited_project.project.id,
            &[Action::Write],
            &[project_resource],
            &stricter_limit,
            None,
        )
        .await
        .expect("grant writer permission for one operation");

    assert!(matches!(
        store
            .apply_changes(
                limited_writer,
                limited_project.project.id,
                0,
                "limited-replay",
                &two_operations,
            )
            .await,
        Err(Error::ResourceLimit)
    ));
    assert!(matches!(
        store
            .apply_changes(
                limited_writer,
                limited_project.project.id,
                1,
                "limited-new-two-op",
                &two_operations,
            )
            .await,
        Err(Error::ResourceLimit)
    ));
    let one_operation = store
        .apply_changes(
            limited_writer,
            limited_project.project.id,
            1,
            "limited-one-op",
            &preference_change("third", "three"),
        )
        .await
        .expect("one operation fits the stricter grant");
    assert_eq!(one_operation.revision.revision, 2);
    assert_eq!(
        one_operation.revision.spec.preferences["first"],
        json!("one")
    );
    assert_eq!(
        one_operation.revision.spec.preferences["second"],
        json!("two")
    );
    assert_eq!(
        one_operation.revision.spec.preferences["third"],
        json!("three")
    );
}

#[tokio::test]
#[ignore = "requires isolated migrated PostgreSQL API and admin URLs in KYRO_TEST_DATABASE_URL and KYRO_TEST_DATABASE_ADMIN_URL"]
async fn postgres_event_feed_tracks_global_bounds_with_hidden_events_and_purge() {
    let database_url = std::env::var("KYRO_TEST_DATABASE_URL")
        .expect("set KYRO_TEST_DATABASE_URL to an isolated kyro_api database");
    let admin_url = std::env::var("KYRO_TEST_DATABASE_ADMIN_URL")
        .expect("set KYRO_TEST_DATABASE_ADMIN_URL to the same isolated database");

    let store = Store::connect(&database_url, 4)
        .await
        .unwrap_or_else(|_| panic!("connect to test PostgreSQL API role"))
        .with_environment(Environment::Development);
    store
        .check_ready()
        .await
        .expect("database schema and API role are ready");
    let api_database_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&store.pool)
        .await
        .expect("read test database name through API role");
    assert!(
        api_database_name.starts_with("kyro_p1_test_"),
        "purge coverage only runs against a kyro_p1_test_* database"
    );

    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await
        .unwrap_or_else(|_| panic!("connect to isolated PostgreSQL admin role"));
    let admin_database_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&admin_pool)
        .await
        .expect("read test database name through admin role");
    assert_eq!(
        api_database_name, admin_database_name,
        "API and admin URLs must point to the same isolated database"
    );

    let owner = store
        .upsert_oidc_actor(
            "https://events.test.invalid",
            &format!("event-owner-{}", Uuid::new_v4()),
        )
        .await
        .expect("create synthetic event owner");
    let organization = store
        .create_organization(owner, &format!("event-org-{}", Uuid::new_v4()))
        .await
        .expect("create synthetic organization");
    let snapshot = store
        .create_project(
            owner,
            CreateProjectInput {
                organization_id: organization.id,
                name: format!("event-project-{}", Uuid::new_v4()),
                data_policy: None,
                limits: None,
            },
        )
        .await
        .expect("create synthetic event project");
    assert_eq!(snapshot.project.event_sequence, 1);

    let mut append_tx = store.begin_actor(owner).await.expect("begin event append");
    let hidden_job_id = Uuid::new_v4();
    Store::append_event(
        &mut *append_tx,
        snapshot.project.id,
        "job.hidden.fixture",
        json!({ "job_id": hidden_job_id }),
    )
    .await
    .expect("append event with a non-existent job reference");
    append_tx.commit().await.expect("commit hidden event");

    let first_page = store
        .read_event_page(owner, snapshot.project.id, 0, 100)
        .await
        .expect("read visible events and global bounds atomically");
    assert_eq!(first_page.earliest_retained, Some(1));
    assert_eq!(first_page.latest_sequence, 2);
    assert_eq!(
        first_page
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1],
        "the hidden sequence 2 does not appear in the visible feed"
    );

    let hidden_only_page = store
        .read_event_page(owner, snapshot.project.id, 1, 100)
        .await
        .expect("read after the last visible sequence");
    assert_eq!(hidden_only_page.earliest_retained, Some(1));
    assert_eq!(hidden_only_page.latest_sequence, 2);
    assert!(hidden_only_page.events.is_empty());

    let mut admin_tx = admin_pool
        .begin()
        .await
        .expect("begin scoped purge transaction");
    let created_by: Uuid =
        sqlx::query_scalar("SELECT created_by FROM public.projects WHERE id = $1")
            .bind(snapshot.project.id)
            .fetch_one(&mut *admin_tx)
            .await
            .expect("verify the project belongs to this test actor before purge");
    assert_eq!(
        created_by, owner,
        "only purge the test actor's fixture project"
    );
    sqlx::query("DELETE FROM public.outbox_events WHERE project_id = $1")
        .bind(snapshot.project.id)
        .execute(&mut *admin_tx)
        .await
        .expect("purge only this fixture project's outbox rows");
    sqlx::query("DELETE FROM public.events WHERE project_id = $1")
        .bind(snapshot.project.id)
        .execute(&mut *admin_tx)
        .await
        .expect("purge only this fixture project's event rows");
    admin_tx.commit().await.expect("commit fixture-only purge");

    let purged_page = store
        .read_event_page(owner, snapshot.project.id, 0, 100)
        .await
        .expect("read global sequence after complete retention purge");
    assert_eq!(purged_page.earliest_retained, None);
    assert_eq!(purged_page.latest_sequence, 2);
    assert!(purged_page.events.is_empty());

    admin_pool.close().await;
}
