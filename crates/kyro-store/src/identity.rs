use chrono::{DateTime, Utc};
use kyro_domain::{
    Action, CapabilityGrant, ConsumedLoginFlow, Error, GrantDemand, GrantLimits, MembershipRole,
    NewLoginFlow, Organization, OrganizationMembership, Result, StoredSession,
};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::Store;

const MAX_ACTIVE_FLOWS_GLOBAL: i64 = 10_000;
const MAX_ACTIVE_FLOWS_PER_BROWSER: i64 = 5;
const MAX_ACTIVE_SESSIONS_PER_ACTOR: i64 = 8;
const MAX_ACTIVE_SESSIONS_GLOBAL_DEFAULT: i64 = 100_000;
const SESSION_CLEANUP_BATCH: i64 = 500;
const FLOW_ADMISSION_LOCK: i64 = 5_435_351_286_379_274_241;
const SESSION_GLOBAL_ADMISSION_LOCK: i64 = 5_435_351_286_379_274_242;
const SESSION_ADMISSION_LOCK_NAMESPACE: i32 = 1_263_836_495;

fn internal<T>(result: std::result::Result<T, sqlx::Error>) -> Result<T> {
    result.map_err(map_identity_database_error)
}

fn map_identity_database_error(error: sqlx::Error) -> Error {
    match error {
        sqlx::Error::Database(database_error) => match database_error.code().as_deref() {
            Some("P0002") | Some("23503") => Error::NotFound,
            Some("42501") => Error::Forbidden,
            Some("40001") | Some("23505") => {
                Error::Conflict("identity record already exists or changed concurrently".into())
            }
            Some("23514") => Error::Invalid("identity data violates a constraint".into()),
            Some("55P03") | Some("57014") | Some("53300") => Error::Unavailable,
            _ => Error::Internal,
        },
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => {
            Error::Unavailable
        }
        _ => Error::Internal,
    }
}

fn fixed_hash(bytes: Vec<u8>) -> Result<[u8; 32]> {
    bytes.try_into().map_err(|_| Error::Internal)
}

fn validate_actions(actions: &[Action]) -> Result<Vec<&'static str>> {
    let values: Vec<_> = actions.iter().map(|action| action.as_str()).collect();
    if values.is_empty() {
        return Err(Error::Invalid(
            "at least one capability action is required".into(),
        ));
    }
    let mut unique = values.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != values.len() {
        return Err(Error::Invalid("capability actions must be unique".into()));
    }
    Ok(values)
}

impl Store {
    /// Admit and persist a one-use OIDC flow. A PostgreSQL advisory lock makes
    /// both global and per-browser limits durable across API processes.
    pub async fn create_login_flow(&self, flow: NewLoginFlow) -> Result<()> {
        if flow.issuer.trim().is_empty()
            || flow.pkce_verifier.is_empty()
            || flow.expires_at <= Utc::now()
        {
            return Err(Error::Invalid("invalid OIDC login flow".into()));
        }

        let mut tx = internal(self.pool.begin().await)?;
        internal(
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(FLOW_ADMISSION_LOCK)
                .execute(&mut *tx)
                .await,
        )?;

        // Consumed and expired rows are not audit events and are removed before
        // admission so successful logins cannot grow this table indefinitely.
        internal(
            sqlx::query(
                "DELETE FROM login_flows WHERE consumed_at IS NOT NULL OR expires_at <= clock_timestamp()",
            )
            .execute(&mut *tx)
            .await,
        )?;

        let active_global: i64 = internal(
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM login_flows WHERE consumed_at IS NULL AND expires_at > clock_timestamp()",
            )
            .fetch_one(&mut *tx)
            .await,
        )?;
        if active_global >= MAX_ACTIVE_FLOWS_GLOBAL {
            internal(tx.commit().await)?;
            return Err(Error::ResourceLimit);
        }

        let active_browser: i64 = internal(
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM login_flows WHERE browser_binding_hash = $1 AND consumed_at IS NULL AND expires_at > clock_timestamp()",
            )
            .bind(flow.browser_binding_hash.as_slice())
            .fetch_one(&mut *tx)
            .await,
        )?;
        if active_browser >= MAX_ACTIVE_FLOWS_PER_BROWSER {
            internal(tx.commit().await)?;
            return Err(Error::ResourceLimit);
        }

        internal(
            sqlx::query(
                "INSERT INTO login_flows (issuer, state_hash, nonce_hash, browser_binding_hash, pkce_verifier, expires_at) VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(flow.issuer)
            .bind(flow.state_hash.as_slice())
            .bind(flow.nonce_hash.as_slice())
            .bind(flow.browser_binding_hash.as_slice())
            .bind(flow.pkce_verifier)
            .bind(flow.expires_at)
            .execute(&mut *tx)
            .await,
        )?;
        internal(tx.commit().await)
    }

    /// Atomically consumes the flow before the caller makes a token request.
    pub async fn consume_login_flow(
        &self,
        issuer: &str,
        state_hash: &[u8; 32],
        browser_binding_hash: &[u8; 32],
    ) -> Result<ConsumedLoginFlow> {
        let mut tx = internal(self.pool.begin().await)?;
        let row = internal(
            sqlx::query(
                "SELECT id, nonce_hash, pkce_verifier FROM login_flows WHERE issuer = $1 AND state_hash = $2 AND browser_binding_hash = $3 AND consumed_at IS NULL AND expires_at > clock_timestamp() FOR UPDATE",
            )
            .bind(issuer)
            .bind(state_hash.as_slice())
            .bind(browser_binding_hash.as_slice())
            .fetch_optional(&mut *tx)
            .await,
        )?
        .ok_or(Error::Unauthorized)?;
        let flow_id: Uuid = row.try_get("id").map_err(|_| Error::Internal)?;
        let nonce_hash = fixed_hash(row.try_get("nonce_hash").map_err(|_| Error::Internal)?)?;
        let pkce_verifier: String = row.try_get("pkce_verifier").map_err(|_| Error::Internal)?;
        let consumed = internal(
            sqlx::query(
                "UPDATE login_flows SET consumed_at = clock_timestamp(), pkce_verifier = NULL WHERE id = $1 AND consumed_at IS NULL AND expires_at > clock_timestamp()",
            )
            .bind(flow_id)
            .execute(&mut *tx)
            .await,
        )?;
        if consumed.rows_affected() != 1 {
            return Err(Error::Unauthorized);
        }
        internal(tx.commit().await)?;
        Ok(ConsumedLoginFlow {
            issuer: issuer.to_owned(),
            nonce_hash,
            pkce_verifier,
        })
    }

    /// Create or retrieve the stable actor for the exact `(issuer, subject)` pair.
    pub async fn upsert_oidc_actor(&self, issuer: &str, subject: &str) -> Result<Uuid> {
        if issuer.trim().is_empty() || subject.trim().is_empty() {
            return Err(Error::Invalid("issuer and subject are required".into()));
        }
        let mut tx = internal(self.pool.begin().await)?;
        internal(
            sqlx::query(
                "INSERT INTO actors (issuer, subject) VALUES ($1, $2) ON CONFLICT (issuer, subject) DO NOTHING",
            )
            .bind(issuer)
            .bind(subject)
            .execute(&mut *tx)
            .await,
        )?;
        let actor_id = internal(
            sqlx::query_scalar("SELECT id FROM actors WHERE issuer = $1 AND subject = $2")
                .bind(issuer)
                .bind(subject)
                .fetch_optional(&mut *tx)
                .await,
        )?
        .ok_or(Error::Internal)?;
        internal(tx.commit().await)?;
        Ok(actor_id)
    }

    pub async fn create_session(
        &self,
        actor_id: Uuid,
        token_hash: &[u8; 32],
        csrf_hash: &[u8; 32],
        expires_at: DateTime<Utc>,
    ) -> Result<StoredSession> {
        self.create_session_with_limit(
            actor_id,
            token_hash,
            csrf_hash,
            expires_at,
            MAX_ACTIVE_SESSIONS_GLOBAL_DEFAULT,
        )
        .await
    }

    pub async fn create_session_with_limit(
        &self,
        actor_id: Uuid,
        token_hash: &[u8; 32],
        csrf_hash: &[u8; 32],
        expires_at: DateTime<Utc>,
        max_active_global: i64,
    ) -> Result<StoredSession> {
        Self::validate_global_session_limit(max_active_global)?;
        if expires_at <= Utc::now() {
            return Err(Error::Invalid(
                "session expiry must be in the future".into(),
            ));
        }
        let mut tx = internal(self.pool.begin().await)?;
        Self::lock_global_session_admission(&mut tx).await?;
        Self::purge_old_sessions_global(&mut tx).await?;
        Self::lock_actor_admission(&mut tx, actor_id).await?;
        Self::purge_old_sessions(&mut tx, actor_id).await?;
        if Self::active_session_count_global(&mut tx).await? >= max_active_global {
            internal(tx.commit().await)?;
            return Err(Error::ResourceLimit);
        }
        if Self::active_session_count_actor(&mut tx, actor_id).await?
            >= MAX_ACTIVE_SESSIONS_PER_ACTOR
        {
            internal(tx.commit().await)?;
            return Err(Error::ResourceLimit);
        }
        let session =
            Self::insert_session(&mut tx, actor_id, token_hash, csrf_hash, expires_at).await?;
        internal(tx.commit().await)?;
        Ok(session)
    }

    pub async fn lookup_active_session(&self, token_hash: &[u8; 32]) -> Result<StoredSession> {
        let row = internal(
            sqlx::query(
                "SELECT id, actor_id, csrf_hash, expires_at FROM sessions WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > clock_timestamp()",
            )
            .bind(token_hash.as_slice())
            .fetch_optional(&self.pool)
            .await,
        )?
        .ok_or(Error::Unauthorized)?;
        Self::session_from_row(&row)
    }

    pub async fn revalidate_session(
        &self,
        session_id: Uuid,
        actor_id: Uuid,
    ) -> Result<StoredSession> {
        let row = internal(
            sqlx::query(
                "SELECT id, actor_id, csrf_hash, expires_at FROM sessions WHERE id = $1 AND actor_id = $2 AND revoked_at IS NULL AND expires_at > clock_timestamp()",
            )
            .bind(session_id)
            .bind(actor_id)
            .fetch_optional(&self.pool)
            .await,
        )?
        .ok_or(Error::Unauthorized)?;
        Self::session_from_row(&row)
    }

    pub async fn rotate_session(
        &self,
        session_id: Uuid,
        actor_id: Uuid,
        token_hash: &[u8; 32],
        csrf_hash: &[u8; 32],
        expires_at: DateTime<Utc>,
    ) -> Result<StoredSession> {
        self.rotate_session_with_limit(
            session_id,
            actor_id,
            token_hash,
            csrf_hash,
            expires_at,
            MAX_ACTIVE_SESSIONS_GLOBAL_DEFAULT,
        )
        .await
    }

    pub async fn rotate_session_with_limit(
        &self,
        session_id: Uuid,
        actor_id: Uuid,
        token_hash: &[u8; 32],
        csrf_hash: &[u8; 32],
        expires_at: DateTime<Utc>,
        max_active_global: i64,
    ) -> Result<StoredSession> {
        Self::validate_global_session_limit(max_active_global)?;
        if expires_at <= Utc::now() {
            return Err(Error::Invalid(
                "session expiry must be in the future".into(),
            ));
        }
        let mut tx = internal(self.pool.begin().await)?;
        Self::lock_global_session_admission(&mut tx).await?;
        Self::purge_old_sessions_global(&mut tx).await?;
        Self::lock_actor_admission(&mut tx, actor_id).await?;
        let revoked = internal(
            sqlx::query(
                "UPDATE sessions SET revoked_at = clock_timestamp() WHERE id = $1 AND actor_id = $2 AND revoked_at IS NULL AND expires_at > clock_timestamp()",
            )
            .bind(session_id)
            .bind(actor_id)
            .execute(&mut *tx)
            .await,
        )?;
        if revoked.rows_affected() != 1 {
            return Err(Error::Unauthorized);
        }
        Self::purge_old_sessions(&mut tx, actor_id).await?;
        if Self::active_session_count_global(&mut tx).await? >= max_active_global
            || Self::active_session_count_actor(&mut tx, actor_id).await?
                >= MAX_ACTIVE_SESSIONS_PER_ACTOR
        {
            internal(tx.commit().await)?;
            return Err(Error::ResourceLimit);
        }
        let session =
            Self::insert_session(&mut tx, actor_id, token_hash, csrf_hash, expires_at).await?;
        internal(tx.commit().await)?;
        Ok(session)
    }

    pub async fn revoke_session(&self, session_id: Uuid, actor_id: Uuid) -> Result<()> {
        let result = internal(
            sqlx::query(
                "UPDATE sessions SET revoked_at = clock_timestamp() WHERE id = $1 AND actor_id = $2 AND revoked_at IS NULL",
            )
            .bind(session_id)
            .bind(actor_id)
            .execute(&self.pool)
            .await,
        )?;
        if result.rows_affected() != 1 {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    pub async fn create_organization(&self, actor_id: Uuid, name: &str) -> Result<Organization> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
            return Err(Error::Invalid("organization name is invalid".into()));
        }
        let mut tx = self.begin_actor(actor_id).await?;
        let organization_id = Uuid::new_v4();
        internal(
            sqlx::query("INSERT INTO organizations (id, name, created_by) VALUES ($1, $2, $3)")
                .bind(organization_id)
                .bind(name)
                .bind(actor_id)
                .execute(&mut *tx)
                .await,
        )?;
        internal(
            sqlx::query(
                "INSERT INTO memberships (organization_id, actor_id, role, created_by) VALUES ($1, $2, 'owner', $2)",
            )
            .bind(organization_id)
            .bind(actor_id)
            .execute(&mut *tx)
            .await,
        )?;
        let row = internal(
            sqlx::query("SELECT id, name, created_at FROM organizations WHERE id = $1")
                .bind(organization_id)
                .fetch_one(&mut *tx)
                .await,
        )?;
        let organization = Organization {
            id: row.try_get("id").map_err(|_| Error::Internal)?,
            name: row.try_get("name").map_err(|_| Error::Internal)?,
            created_at: row.try_get("created_at").map_err(|_| Error::Internal)?,
        };
        internal(tx.commit().await)?;
        Ok(organization)
    }

    pub async fn list_organizations(&self, actor_id: Uuid) -> Result<Vec<OrganizationMembership>> {
        let mut tx = self.begin_actor(actor_id).await?;
        let rows = internal(
            sqlx::query(
                "SELECT m.organization_id, m.actor_id, m.role, o.name AS organization_name FROM memberships m JOIN organizations o ON o.id = m.organization_id WHERE m.actor_id = $1 ORDER BY o.created_at, o.id",
            )
            .bind(actor_id)
            .fetch_all(&mut *tx)
            .await,
        )?;
        let mut memberships = Vec::with_capacity(rows.len());
        for row in rows {
            let role: String = row.try_get("role").map_err(|_| Error::Internal)?;
            let role = match role.as_str() {
                "owner" => MembershipRole::Owner,
                "admin" => MembershipRole::Admin,
                "member" => MembershipRole::Member,
                _ => return Err(Error::Internal),
            };
            memberships.push(OrganizationMembership {
                organization_id: row
                    .try_get("organization_id")
                    .map_err(|_| Error::Internal)?,
                actor_id: row.try_get("actor_id").map_err(|_| Error::Internal)?,
                role,
                organization_name: row
                    .try_get("organization_name")
                    .map_err(|_| Error::Internal)?,
            });
        }
        internal(tx.commit().await)?;
        Ok(memberships)
    }

    pub async fn list_organization_members(
        &self,
        actor_id: Uuid,
        organization_id: Uuid,
    ) -> Result<Vec<OrganizationMembership>> {
        let mut tx = self.begin_actor(actor_id).await?;
        if !Self::organization_visible(&mut *tx, organization_id).await? {
            return Err(Error::NotFound);
        }
        let rows = internal(
            sqlx::query(
                "SELECT m.organization_id, m.actor_id, m.role, o.name AS organization_name FROM memberships m JOIN organizations o ON o.id = m.organization_id WHERE m.organization_id = $1 ORDER BY m.created_at, m.actor_id",
            )
            .bind(organization_id)
            .fetch_all(&mut *tx)
            .await,
        )?;
        let mut memberships = Vec::with_capacity(rows.len());
        for row in rows {
            let role: String = row.try_get("role").map_err(|_| Error::Internal)?;
            let role = match role.as_str() {
                "owner" => MembershipRole::Owner,
                "admin" => MembershipRole::Admin,
                "member" => MembershipRole::Member,
                _ => return Err(Error::Internal),
            };
            memberships.push(OrganizationMembership {
                organization_id: row
                    .try_get("organization_id")
                    .map_err(|_| Error::Internal)?,
                actor_id: row.try_get("actor_id").map_err(|_| Error::Internal)?,
                role,
                organization_name: row
                    .try_get("organization_name")
                    .map_err(|_| Error::Internal)?,
            });
        }
        internal(tx.commit().await)?;
        Ok(memberships)
    }

    pub async fn add_organization_member(
        &self,
        actor_id: Uuid,
        organization_id: Uuid,
        target_actor_id: Uuid,
        role: MembershipRole,
    ) -> Result<()> {
        if role == MembershipRole::Owner {
            return Err(Error::Invalid(
                "organization ownership cannot be transferred through membership changes".into(),
            ));
        }
        let mut tx = self.begin_actor(actor_id).await?;
        if !Self::organization_visible(&mut *tx, organization_id).await? {
            return Err(Error::NotFound);
        }
        let owns_organization: bool = internal(
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM memberships WHERE organization_id = $1 AND actor_id = $2 AND role = 'owner')",
            )
            .bind(organization_id)
            .bind(actor_id)
            .fetch_one(&mut *tx)
            .await,
        )?;
        if !owns_organization {
            return Err(Error::Forbidden);
        }
        internal(
            sqlx::query(
                "INSERT INTO memberships (organization_id, actor_id, role, created_by) VALUES ($1, $2, $3, $4)",
            )
            .bind(organization_id)
            .bind(target_actor_id)
            .bind(role.as_str())
            .bind(actor_id)
            .execute(&mut *tx)
            .await,
        )?;
        internal(tx.commit().await)
    }

    pub async fn remove_organization_member(
        &self,
        actor_id: Uuid,
        organization_id: Uuid,
        target_actor_id: Uuid,
    ) -> Result<()> {
        if actor_id == target_actor_id {
            return Err(Error::Invalid(
                "the organization owner cannot remove their own membership".into(),
            ));
        }
        let mut tx = self.begin_actor(actor_id).await?;
        if !Self::organization_visible(&mut *tx, organization_id).await? {
            return Err(Error::NotFound);
        }
        let owns_organization: bool = internal(
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM memberships WHERE organization_id = $1 AND actor_id = $2 AND role = 'owner')",
            )
            .bind(organization_id)
            .bind(actor_id)
            .fetch_one(&mut *tx)
            .await,
        )?;
        if !owns_organization {
            return Err(Error::Forbidden);
        }
        let deleted = internal(
            sqlx::query(
                "DELETE FROM memberships WHERE organization_id = $1 AND actor_id = $2 AND role <> 'owner'",
            )
            .bind(organization_id)
            .bind(target_actor_id)
            .execute(&mut *tx)
            .await,
        )?;
        if deleted.rows_affected() != 1 {
            return Err(Error::NotFound);
        }
        internal(tx.commit().await)
    }

    async fn organization_visible(conn: &mut PgConnection, organization_id: Uuid) -> Result<bool> {
        internal(
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM organizations WHERE id = $1)")
                .bind(organization_id)
                .fetch_one(&mut *conn)
                .await,
        )
    }

    /// Add the only implicit project grant: the already-established owner of
    /// the project's real organization, in the project-creation transaction.
    pub async fn grant_project_owner_in(
        conn: &mut PgConnection,
        actor_id: Uuid,
        project_id: Uuid,
    ) -> Result<Uuid> {
        let actor_matches: bool = internal(
            sqlx::query_scalar(
                "SELECT COALESCE(current_setting('kyro.actor_id', true) = $1::text, FALSE)",
            )
            .bind(actor_id)
            .fetch_one(&mut *conn)
            .await,
        )?;
        if !actor_matches {
            return Err(Error::Forbidden);
        }
        internal(
            sqlx::query_scalar("SELECT public.grant_initial_project_owner($1)")
                .bind(project_id)
                .fetch_one(&mut *conn)
                .await,
        )
    }

    pub async fn authorize(&self, actor_id: Uuid, project_id: Uuid, action: &str) -> Result<()> {
        let mut tx = self.begin_actor(actor_id).await?;
        Self::authorize_in(&mut *tx, actor_id, project_id, action).await?;
        internal(tx.commit().await)
    }

    pub async fn create_capability_grant(
        &self,
        creator_id: Uuid,
        target_actor_id: Uuid,
        project_id: Uuid,
        actions: &[Action],
        resources: &[String],
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<CapabilityGrant> {
        self.create_capability_grant_with_limits(
            creator_id,
            target_actor_id,
            project_id,
            actions,
            resources,
            &GrantLimits::default(),
            expires_at,
        )
        .await
    }

    pub async fn create_capability_grant_with_limits(
        &self,
        creator_id: Uuid,
        target_actor_id: Uuid,
        project_id: Uuid,
        actions: &[Action],
        resources: &[String],
        limits: &GrantLimits,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<CapabilityGrant> {
        let action_names = validate_actions(actions)?;
        Self::validate_project_resources(project_id, resources)?;
        limits.validate()?;
        if expires_at.is_some_and(|expiry| expiry <= Utc::now()) {
            return Err(Error::Invalid("grant expiry must be in the future".into()));
        }
        let limits_json = serde_json::to_value(limits).map_err(|_| Error::Internal)?;

        let mut tx = self.begin_actor(creator_id).await?;
        Self::require_project_owner(&mut *tx, creator_id, project_id).await?;
        let target_is_member: bool = internal(
            sqlx::query_scalar("SELECT public.has_project_org_member($1, $2)")
                .bind(project_id)
                .bind(target_actor_id)
                .fetch_one(&mut *tx)
                .await,
        )?;
        if !target_is_member {
            return Err(Error::Forbidden);
        }
        // Hold the target's membership row through commit. Removing the member
        // takes a conflicting row lock, so a concurrent grant cannot survive a
        // completed membership removal without the revocation path seeing it.
        let target_role: Option<String> = internal(
            sqlx::query_scalar(
                "SELECT m.role FROM memberships m JOIN projects p ON p.organization_id = m.organization_id WHERE p.id = $1 AND m.actor_id = $2 FOR SHARE OF m",
            )
            .bind(project_id)
            .bind(target_actor_id)
            .fetch_optional(&mut *tx)
            .await,
        )?;
        let target_role = target_role.ok_or(Error::Forbidden)?;
        if action_names.contains(&"manage") && target_role != "owner" {
            return Err(Error::Forbidden);
        }

        let grant_id = Uuid::new_v4();
        let created_at = internal(
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *tx)
                .await,
        )?;
        internal(
            sqlx::query(
                "INSERT INTO capability_grants (id, actor_id, project_id, actions, resources, limits, environment, expires_at, created_by, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(grant_id)
            .bind(target_actor_id)
            .bind(project_id)
            .bind(&action_names)
            .bind(resources)
            .bind(limits_json)
            .bind(self.environment.as_str())
            .bind(expires_at)
            .bind(creator_id)
            .bind(created_at)
            .execute(&mut *tx)
            .await,
        )?;
        internal(tx.commit().await)?;
        Ok(CapabilityGrant {
            id: grant_id,
            actor_id: target_actor_id,
            project_id,
            actions: actions.to_vec(),
            resources: resources.to_vec(),
            limits: limits.clone(),
            environment: self.environment,
            expires_at,
            revoked_at: None,
            created_by: creator_id,
            created_at,
        })
    }

    pub async fn revoke_capability_grant(
        &self,
        creator_id: Uuid,
        project_id: Uuid,
        grant_id: Uuid,
    ) -> Result<()> {
        let mut tx = self.begin_actor(creator_id).await?;
        Self::require_project_owner(&mut *tx, creator_id, project_id).await?;
        let revoked = internal(
            sqlx::query(
                "UPDATE capability_grants SET revoked_at = clock_timestamp() WHERE id = $1 AND project_id = $2 AND environment = $3 AND revoked_at IS NULL",
            )
            .bind(grant_id)
            .bind(project_id)
            .bind(self.environment.as_str())
            .execute(&mut *tx)
            .await,
        )?;
        if revoked.rows_affected() != 1 {
            return Err(Error::NotFound);
        }
        internal(tx.commit().await)
    }

    async fn require_project_owner(
        conn: &mut PgConnection,
        actor_id: Uuid,
        project_id: Uuid,
    ) -> Result<()> {
        let is_owner: bool = internal(
            sqlx::query_scalar("SELECT public.has_project_org_owner($1, $2)")
                .bind(project_id)
                .bind(actor_id)
                .fetch_one(&mut *conn)
                .await,
        )?;
        if is_owner {
            return Ok(());
        }
        let visible: bool = internal(
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
                .bind(project_id)
                .fetch_one(&mut *conn)
                .await,
        )?;
        Err(if visible {
            Error::Forbidden
        } else {
            Error::NotFound
        })
    }

    fn validate_project_resources(project_id: Uuid, resources: &[String]) -> Result<()> {
        if resources.len() != 1 {
            return Err(Error::Invalid(
                "exactly one grant resource ('*' or project UUID) is required".into(),
            ));
        }
        let project_resource = project_id.to_string();
        if resources
            .iter()
            .any(|resource| resource != "*" && resource != &project_resource)
        {
            return Err(Error::Invalid(
                "grant resources must be '*' or the project UUID".into(),
            ));
        }
        let mut unique = resources.to_vec();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() != resources.len() {
            return Err(Error::Invalid("grant resources must be unique".into()));
        }
        Ok(())
    }

    pub async fn authorize_in(
        conn: &mut PgConnection,
        actor_id: Uuid,
        project_id: Uuid,
        action: &str,
    ) -> Result<()> {
        Self::authorize_demand_in(
            conn,
            actor_id,
            project_id,
            &[action],
            &GrantDemand::default(),
        )
        .await
    }

    pub async fn authorize_any_in(
        conn: &mut PgConnection,
        actor_id: Uuid,
        project_id: Uuid,
        actions: &[&str],
    ) -> Result<()> {
        Self::authorize_demand_in(conn, actor_id, project_id, actions, &GrantDemand::default())
            .await
    }

    /// Authorize at least one compatible action using one active grant that
    /// covers every requested fact. Limits from separate grants are never
    /// combined for the same action check.
    pub async fn authorize_demand_in(
        conn: &mut PgConnection,
        actor_id: Uuid,
        project_id: Uuid,
        actions: &[&str],
        demand: &GrantDemand,
    ) -> Result<()> {
        if actions.is_empty()
            || actions.iter().any(|action| {
                !matches!(
                    *action,
                    "read" | "write" | "execute" | "model" | "manage" | "budget"
                )
            })
        {
            return Err(Error::Invalid("unsupported capability action".into()));
        }

        let project_visible: bool = internal(
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
                .bind(project_id)
                .fetch_one(&mut *conn)
                .await,
        )?;
        if !project_visible {
            return Err(Error::NotFound);
        }

        let rows = internal(
            sqlx::query(
                "SELECT id, actions, resources, environment, expires_at, revoked_at, limits FROM public.kyro_lock_actor_grants($1, $2, $3)",
            )
            .bind(actor_id)
            .bind(project_id)
            .bind(actions)
            .fetch_all(&mut *conn)
            .await,
        )?;

        let environment: Option<String> = internal(
            sqlx::query_scalar("SELECT current_setting('kyro.environment', true)")
                .fetch_one(&mut *conn)
                .await,
        )?;
        let checked_at: DateTime<Utc> = internal(
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *conn)
                .await,
        )?;

        let project_resource = project_id.to_string();
        let mut has_current_action_grant = false;
        for row in rows {
            let granted_actions: Vec<String> =
                row.try_get("actions").map_err(|_| Error::Internal)?;
            if !granted_actions
                .iter()
                .any(|granted| actions.iter().any(|requested| granted == requested))
            {
                continue;
            }
            let resources: Vec<String> = row.try_get("resources").map_err(|_| Error::Internal)?;
            if !resources
                .iter()
                .any(|resource| resource == "*" || resource == &project_resource)
            {
                continue;
            }
            let row_environment: String =
                row.try_get("environment").map_err(|_| Error::Internal)?;
            if environment.as_deref() != Some(row_environment.as_str()) {
                continue;
            }
            let revoked_at: Option<DateTime<Utc>> =
                row.try_get("revoked_at").map_err(|_| Error::Internal)?;
            let expires_at: Option<DateTime<Utc>> =
                row.try_get("expires_at").map_err(|_| Error::Internal)?;
            if revoked_at.is_some() || expires_at.is_some_and(|expiry| expiry <= checked_at) {
                continue;
            }

            has_current_action_grant = true;
            let limits_json: serde_json::Value =
                row.try_get("limits").map_err(|_| Error::Internal)?;
            let limits: GrantLimits =
                serde_json::from_value(limits_json).map_err(|_| Error::Internal)?;
            limits.validate().map_err(|_| Error::Internal)?;
            if limits.allows(demand) {
                return Ok(());
            }
        }

        Err(if has_current_action_grant {
            Error::ResourceLimit
        } else {
            Error::Forbidden
        })
    }
}

impl Store {
    fn validate_global_session_limit(limit: i64) -> Result<()> {
        if !(1..=MAX_ACTIVE_SESSIONS_GLOBAL_DEFAULT).contains(&limit) {
            return Err(Error::Invalid(
                "global active session limit is outside the allowed range".into(),
            ));
        }
        Ok(())
    }

    async fn lock_global_session_admission(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        internal(
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(SESSION_GLOBAL_ADMISSION_LOCK)
                .execute(&mut **tx)
                .await,
        )?;
        Ok(())
    }

    async fn lock_actor_admission(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor_id: Uuid,
    ) -> Result<()> {
        internal(
            sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
                .bind(SESSION_ADMISSION_LOCK_NAMESPACE)
                .bind(actor_id.to_string())
                .execute(&mut **tx)
                .await,
        )?;
        Ok(())
    }

    async fn purge_old_sessions(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor_id: Uuid,
    ) -> Result<()> {
        internal(
            sqlx::query(
                "DELETE FROM sessions WHERE id IN (SELECT id FROM sessions WHERE actor_id = $1 AND revoked_at IS NOT NULL ORDER BY created_at, id LIMIT $2)",
            )
            .bind(actor_id)
            .bind(SESSION_CLEANUP_BATCH)
            .execute(&mut **tx)
            .await,
        )?;
        internal(
            sqlx::query(
                "DELETE FROM sessions WHERE id IN (SELECT id FROM sessions WHERE actor_id = $1 AND revoked_at IS NULL AND expires_at <= clock_timestamp() ORDER BY expires_at, id LIMIT $2)",
            )
            .bind(actor_id)
            .bind(SESSION_CLEANUP_BATCH)
            .execute(&mut **tx)
            .await,
        )?;
        Ok(())
    }

    async fn purge_old_sessions_global(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        internal(
            sqlx::query(
                "DELETE FROM sessions WHERE id IN (SELECT id FROM sessions WHERE revoked_at IS NOT NULL ORDER BY created_at, id LIMIT $1)",
            )
            .bind(SESSION_CLEANUP_BATCH)
            .execute(&mut **tx)
            .await,
        )?;
        internal(
            sqlx::query(
                "DELETE FROM sessions WHERE id IN (SELECT id FROM sessions WHERE revoked_at IS NULL AND expires_at <= clock_timestamp() ORDER BY expires_at, id LIMIT $1)",
            )
            .bind(SESSION_CLEANUP_BATCH)
            .execute(&mut **tx)
            .await,
        )?;
        Ok(())
    }

    async fn active_session_count_global(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<i64> {
        internal(
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM sessions WHERE revoked_at IS NULL AND expires_at > clock_timestamp()",
            )
            .fetch_one(&mut **tx)
            .await,
        )
    }

    async fn active_session_count_actor(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor_id: Uuid,
    ) -> Result<i64> {
        internal(
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM sessions WHERE actor_id = $1 AND revoked_at IS NULL AND expires_at > clock_timestamp()",
            )
            .bind(actor_id)
            .fetch_one(&mut **tx)
            .await,
        )
    }

    async fn insert_session(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor_id: Uuid,
        token_hash: &[u8; 32],
        csrf_hash: &[u8; 32],
        expires_at: DateTime<Utc>,
    ) -> Result<StoredSession> {
        let row = internal(
            sqlx::query(
                "INSERT INTO sessions (token_hash, actor_id, csrf_hash, expires_at) VALUES ($1, $2, $3, $4) RETURNING id, actor_id, csrf_hash, expires_at",
            )
            .bind(token_hash.as_slice())
            .bind(actor_id)
            .bind(csrf_hash.as_slice())
            .bind(expires_at)
            .fetch_one(&mut **tx)
            .await,
        )?;
        Self::session_from_row(&row)
    }

    fn session_from_row(row: &sqlx::postgres::PgRow) -> Result<StoredSession> {
        Ok(StoredSession {
            id: row.try_get("id").map_err(|_| Error::Internal)?,
            actor_id: row.try_get("actor_id").map_err(|_| Error::Internal)?,
            csrf_hash: fixed_hash(row.try_get("csrf_hash").map_err(|_| Error::Internal)?)?,
            expires_at: row.try_get("expires_at").map_err(|_| Error::Internal)?,
        })
    }
}
