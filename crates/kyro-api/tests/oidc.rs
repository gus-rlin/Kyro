use kyro_api::identity::{AuthConfig, AuthError, OidcProviderConfig};
use kyro_domain::Environment;
use url::Url;

fn provider(
    issuer: &str,
    endpoint_origin: &str,
    client_secret: Option<&str>,
) -> OidcProviderConfig {
    let redirect_uri = if endpoint_origin.starts_with("https://") {
        "https://api.example.test/v1/auth/callback"
    } else {
        "http://127.0.0.1:8080/v1/auth/callback"
    };
    OidcProviderConfig {
        issuer: issuer.to_owned(),
        authorization_endpoint: Url::parse(&format!("{endpoint_origin}/authorize")).unwrap(),
        token_endpoint: Url::parse(&format!("{endpoint_origin}/token")).unwrap(),
        jwks_uri: Url::parse(&format!("{endpoint_origin}/jwks")).unwrap(),
        redirect_uri: Url::parse(redirect_uri).unwrap(),
        client_id: "test-client".to_owned(),
        client_secret: client_secret.map(str::to_owned),
    }
}

#[test]
fn synthetic_provider_requires_explicit_development_and_loopback_configuration() {
    let local = provider(
        "http://localhost:9000/issuer",
        "http://localhost:9000",
        None,
    );
    let config = AuthConfig::new(
        Environment::Development,
        local.clone(),
        "http://localhost:3000",
        true,
    )
    .unwrap();
    assert!(config.synthetic_provider);

    assert_eq!(
        AuthConfig::new(
            Environment::Production,
            local,
            "https://ui.example.test",
            true
        )
        .err(),
        Some(AuthError::InvalidConfiguration)
    );

    let remote = provider("https://id.example.test", "https://id.example.test", None);
    assert_eq!(
        AuthConfig::new(
            Environment::Development,
            remote,
            "http://localhost:3000",
            true
        )
        .err(),
        Some(AuthError::InvalidConfiguration)
    );
}

#[test]
fn global_session_admission_limit_is_bounded_and_configurable() {
    let local = provider(
        "http://localhost:9000/issuer",
        "http://localhost:9000",
        None,
    );
    let configured = AuthConfig::with_limits(
        Environment::Development,
        local.clone(),
        "http://localhost:3000",
        true,
        chrono::Duration::hours(8),
        chrono::Duration::minutes(10),
        3,
    )
    .unwrap();
    assert_eq!(configured.max_active_sessions_global, 3);

    for invalid_limit in [0, 100_001] {
        assert_eq!(
            AuthConfig::with_limits(
                Environment::Development,
                local.clone(),
                "http://localhost:3000",
                true,
                chrono::Duration::hours(8),
                chrono::Duration::minutes(10),
                invalid_limit,
            )
            .err(),
            Some(AuthError::InvalidConfiguration)
        );
    }
}

#[test]
fn provider_debug_output_redacts_the_client_secret() {
    let config = AuthConfig::new(
        Environment::Production,
        provider(
            "https://id.example.test/issuer",
            "https://id.example.test",
            Some("synthetic-test-secret-do-not-use"),
        ),
        "https://ui.example.test",
        false,
    )
    .unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("synthetic-test-secret-do-not-use"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn auth_error_codes_and_statuses_are_stable() {
    assert_eq!(AuthError::InvalidFlow.code(), "invalid_auth_flow");
    assert_eq!(AuthError::InvalidFlow.status().as_u16(), 400);
    assert_eq!(AuthError::InvalidRequest.code(), "invalid_request");
    assert_eq!(AuthError::InvalidRequest.status().as_u16(), 400);
    assert_eq!(AuthError::NotFound.status().as_u16(), 404);
    assert_eq!(AuthError::Unauthenticated.status().as_u16(), 401);
    assert_eq!(AuthError::Conflict.status().as_u16(), 409);
    assert_eq!(AuthError::Unavailable.status().as_u16(), 503);
}

/// Run with `--ignored` against a freshly migrated PostgreSQL database whose
/// application URL connects as the non-owner `kyro_api` role.
#[tokio::test]
#[ignore = "requires a migrated PostgreSQL database and KYRO_TEST_DATABASE_URL"]
async fn postgres_persists_flows_sessions_and_organization_membership() {
    use chrono::{Duration, Utc};
    use kyro_domain::{Action, Error, GrantDemand, GrantLimits, MembershipRole, NewLoginFlow};
    use kyro_store::Store;
    use uuid::Uuid;

    fn synthetic_hash(value: &str) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        Sha256::digest(value.as_bytes()).into()
    }

    let database_url = std::env::var("KYRO_TEST_DATABASE_URL")
        .expect("set KYRO_TEST_DATABASE_URL to a migrated test database as kyro_api");
    let store = Store::connect(&database_url, 4)
        .await
        .expect("connect as kyro_api")
        .with_environment(Environment::Development);

    let issuer = format!("https://identity.example.test/{}", Uuid::new_v4());
    let owner = store
        .upsert_oidc_actor(&issuer, "owner-subject")
        .await
        .expect("persist OIDC actor");
    let member_issuer = format!("https://identity.example.test/{}", Uuid::new_v4());
    let member = store
        .upsert_oidc_actor(&member_issuer, "member-subject")
        .await
        .expect("persist another OIDC actor");
    let outsider_issuer = format!("https://identity.example.test/{}", Uuid::new_v4());
    let outsider = store
        .upsert_oidc_actor(&outsider_issuer, "outsider-subject")
        .await
        .expect("persist outsider actor");

    let fixture_id = Uuid::new_v4();
    let state_hash = synthetic_hash(&format!("{fixture_id}:state"));
    let nonce_hash = synthetic_hash(&format!("{fixture_id}:nonce"));
    let browser_binding_hash = synthetic_hash(&format!("{fixture_id}:browser-binding"));
    let verifier = "A".repeat(43);
    store
        .create_login_flow(NewLoginFlow {
            issuer: issuer.clone(),
            state_hash,
            nonce_hash,
            browser_binding_hash,
            pkce_verifier: verifier.clone(),
            expires_at: Utc::now() + Duration::minutes(5),
        })
        .await
        .expect("persist one-use OIDC flow");
    assert!(matches!(
        store
            .consume_login_flow(
                &issuer,
                &state_hash,
                &synthetic_hash(&format!("{fixture_id}:wrong-browser-binding")),
            )
            .await,
        Err(Error::Unauthorized)
    ));
    let consumed = store
        .consume_login_flow(&issuer, &state_hash, &browser_binding_hash)
        .await
        .expect("consume matching flow once");
    assert_eq!(consumed.pkce_verifier, verifier);
    assert_eq!(consumed.nonce_hash, nonce_hash);
    assert!(matches!(
        store
            .consume_login_flow(&issuer, &state_hash, &browser_binding_hash)
            .await,
        Err(Error::Unauthorized)
    ));

    let token_hash = synthetic_hash(&format!("{fixture_id}:session-token"));
    let csrf_hash = synthetic_hash(&format!("{fixture_id}:session-csrf"));
    let first_session = store
        .create_session_with_limit(
            owner,
            &token_hash,
            &csrf_hash,
            Utc::now() + Duration::minutes(5),
            1,
        )
        .await
        .expect("persist opaque session hash");
    assert_eq!(
        store
            .lookup_active_session(&token_hash)
            .await
            .expect("look up active session")
            .id,
        first_session.id
    );
    assert!(matches!(
        store
            .create_session_with_limit(
                owner,
                &synthetic_hash(&format!("{fixture_id}:rejected-session-token")),
                &synthetic_hash(&format!("{fixture_id}:rejected-session-csrf")),
                Utc::now() + Duration::minutes(5),
                1,
            )
            .await,
        Err(Error::ResourceLimit)
    ));

    let rotated_token_hash = synthetic_hash(&format!("{fixture_id}:rotated-session-token"));
    let rotated_csrf_hash = synthetic_hash(&format!("{fixture_id}:rotated-session-csrf"));
    let rotated = store
        .rotate_session_with_limit(
            first_session.id,
            owner,
            &rotated_token_hash,
            &rotated_csrf_hash,
            Utc::now() + Duration::minutes(5),
            1,
        )
        .await
        .expect("rotate without exceeding the global session cap");
    assert!(matches!(
        store.lookup_active_session(&token_hash).await,
        Err(Error::Unauthorized)
    ));
    store
        .revoke_session(rotated.id, owner)
        .await
        .expect("revoke current session");
    assert!(matches!(
        store.lookup_active_session(&rotated_token_hash).await,
        Err(Error::Unauthorized)
    ));

    let organization = store
        .create_organization(owner, "Synthetic identity test")
        .await
        .expect("create organization and owner membership atomically");
    store
        .add_organization_member(owner, organization.id, member, MembershipRole::Member)
        .await
        .expect("owner may add a member");
    assert_eq!(
        store
            .list_organization_members(owner, organization.id)
            .await
            .expect("owner can list organization members")
            .len(),
        2
    );

    let project = store
        .create_project(
            owner,
            kyro_store::projects::CreateProjectInput {
                organization_id: organization.id,
                name: "Synthetic grant limit test".into(),
                data_policy: None,
                limits: None,
            },
        )
        .await
        .expect("create a project with its atomic owner grant");
    let project_id = project.project.id;
    let resources = ["*".to_owned()];
    let split_fact_grants = [
        GrantLimits {
            max_job_attempts: Some(1),
            max_model_input_bytes: Some(4096),
            ..Default::default()
        },
        GrantLimits {
            max_job_attempts: Some(3),
            max_model_input_bytes: Some(1024),
            ..Default::default()
        },
    ];
    for limits in &split_fact_grants {
        store
            .create_capability_grant_with_limits(
                owner,
                member,
                project_id,
                &[Action::Execute],
                &resources,
                limits,
                None,
            )
            .await
            .expect("owner can create a bounded member grant");
    }
    let demand = GrantDemand {
        job_attempts: Some(2),
        job_ttl_secs: Some(30),
        model_input_bytes: Some(2048),
        model_output_tokens: Some(512),
        changeset_operations: Some(4),
    };
    let mut split_tx = store
        .begin_actor(member)
        .await
        .expect("set the member actor and environment");
    assert!(matches!(
        Store::authorize_demand_in(&mut *split_tx, member, project_id, &["execute"], &demand,)
            .await,
        Err(Error::ResourceLimit)
    ));
    split_tx
        .rollback()
        .await
        .expect("release candidate grant locks");

    let complete_limits = GrantLimits {
        max_job_attempts: Some(3),
        max_job_ttl_secs: Some(1800),
        max_model_input_bytes: Some(4096),
        max_model_output_tokens: Some(1024),
        max_changeset_operations: Some(8),
    };
    for action in [Action::Execute, Action::Model] {
        store
            .create_capability_grant_with_limits(
                owner,
                member,
                project_id,
                &[action],
                &resources,
                &complete_limits,
                None,
            )
            .await
            .expect("persist a grant that covers all requested facts");
    }
    for action in ["execute", "model"] {
        let mut authorization_tx = store
            .begin_actor(member)
            .await
            .expect("set the member actor and environment");
        Store::authorize_demand_in(
            &mut *authorization_tx,
            member,
            project_id,
            &[action],
            &demand,
        )
        .await
        .expect("one matching grant covers every requested fact");
        authorization_tx
            .commit()
            .await
            .expect("commit successful grant admission");
    }
    let mut missing_action_tx = store
        .begin_actor(member)
        .await
        .expect("set the member actor and environment");
    assert!(matches!(
        Store::authorize_demand_in(
            &mut *missing_action_tx,
            member,
            project_id,
            &["write"],
            &demand,
        )
        .await,
        Err(Error::Forbidden)
    ));
    missing_action_tx
        .rollback()
        .await
        .expect("release project grant locks");

    assert!(matches!(
        store
            .add_organization_member(member, organization.id, outsider, MembershipRole::Member)
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        store
            .list_organization_members(outsider, organization.id)
            .await,
        Err(Error::NotFound)
    ));
    store
        .remove_organization_member(owner, organization.id, member)
        .await
        .expect("owner can remove a member");
    assert!(matches!(
        store
            .list_organization_members(member, organization.id)
            .await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        store.authorize(owner, Uuid::new_v4(), "read").await,
        Err(Error::NotFound)
    ));
}
