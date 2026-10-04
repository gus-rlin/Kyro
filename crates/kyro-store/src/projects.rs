//! Project, immutable AppSpec revision, decision, and event persistence.

use crate::Store;
use crate::store::map_database_error as db_error;
use chrono::{DateTime, Utc};
use kyro_domain::identity::GrantDemand;
use kyro_domain::model::DataPolicy;
use kyro_domain::spec::{AppSpec, ChangeSet, ProjectLimits, SpecError, apply_changes};
use kyro_domain::{Error, Event, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Decode, PgConnection, Postgres, Row, Type, postgres::PgRow};
use uuid::Uuid;

const MAX_PROJECT_NAME_BYTES: usize = 128;
const MAX_DECISION_KIND_BYTES: usize = 64;
const MAX_DECISION_PAYLOAD_BYTES: usize = 24 * 1024;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 200;
const MAX_PAGE_SIZE: i64 = 100;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectInput {
    pub organization_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub data_policy: Option<DataPolicy>,
    #[serde(default)]
    pub limits: Option<ProjectLimits>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub name: String,
    pub current_revision: i64,
    pub event_sequence: i64,
    pub data_policy: DataPolicy,
    pub limits: ProjectLimits,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub project: Project,
    pub revision: AppRevision,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCursor {
    pub updated_at: DateTime<Utc>,
    pub id: Uuid,
}

pub struct ProjectPage {
    pub items: Vec<Project>,
    pub next_cursor: Option<ProjectCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventFeedPage {
    pub earliest_retained: Option<i64>,
    pub latest_sequence: i64,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppRevision {
    pub project_id: Uuid,
    pub revision: i64,
    pub spec: AppSpec,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplyChangesResult {
    pub command_id: Uuid,
    pub revision: AppRevision,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectDecision {
    pub id: Uuid,
    pub project_id: Uuid,
    pub revision: i64,
    pub actor_id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddDecisionInput {
    pub revision: i64,
    pub kind: String,
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredCommandResult {
    command_id: Uuid,
    revision: i64,
}

impl Project {
    fn from_row(row: &PgRow) -> Result<Self> {
        Ok(Self {
            id: column(row, "id")?,
            organization_id: column(row, "organization_id")?,
            name: column(row, "name")?,
            current_revision: column(row, "current_revision")?,
            event_sequence: column(row, "event_sequence")?,
            data_policy: decode_json(column(row, "data_policy")?)?,
            limits: decode_json(column(row, "limits")?)?,
            created_by: column(row, "created_by")?,
            created_at: column(row, "created_at")?,
            updated_at: column(row, "updated_at")?,
        })
    }
}

impl AppRevision {
    fn from_row(row: &PgRow) -> Result<Self> {
        Ok(Self {
            project_id: column(row, "project_id")?,
            revision: column(row, "revision")?,
            spec: decode_json(column(row, "spec")?)?,
            created_by: column(row, "created_by")?,
            created_at: column(row, "created_at")?,
        })
    }
}

impl ProjectDecision {
    fn from_row(row: &PgRow) -> Result<Self> {
        Ok(Self {
            id: column(row, "id")?,
            project_id: column(row, "project_id")?,
            revision: column(row, "revision")?,
            actor_id: column(row, "actor_id")?,
            kind: column(row, "kind")?,
            payload: column(row, "payload")?,
            created_at: column(row, "created_at")?,
        })
    }
}

impl Store {
    pub async fn list_projects(
        &self,
        actor_id: Uuid,
        limit: u16,
        before: Option<ProjectCursor>,
    ) -> Result<ProjectPage> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid(
                "project page size is outside the allowed range".into(),
            ));
        }
        let mut tx = self.begin_actor(actor_id).await?;
        let rows = sqlx::query(
            "SELECT p.id, p.organization_id, p.name, p.current_revision, p.event_sequence, \
                    p.data_policy, p.limits, p.created_by, p.created_at, p.updated_at \
             FROM projects p \
             WHERE ($2::TIMESTAMPTZ IS NULL OR p.updated_at < $2 \
                    OR (p.updated_at = $2 AND p.id > $3)) AND EXISTS ( \
                 SELECT 1 FROM capability_grants g \
                 WHERE g.project_id = p.id AND g.actor_id = $1 \
                   AND g.environment = current_setting('kyro.environment', true) \
                   AND g.revoked_at IS NULL \
                   AND (g.expires_at IS NULL OR g.expires_at > clock_timestamp()) \
                   AND g.actions @> ARRAY['read']::TEXT[] \
                   AND (g.resources @> ARRAY['*']::TEXT[] OR p.id::TEXT = ANY(g.resources)) \
             ) \
             ORDER BY p.updated_at DESC, p.id ASC LIMIT $4",
        )
        .bind(actor_id)
        .bind(before.as_ref().map(|cursor| cursor.updated_at))
        .bind(before.as_ref().map(|cursor| cursor.id))
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?;
        let mut projects = rows
            .iter()
            .map(Project::from_row)
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await.map_err(db_error)?;
        let next_cursor = if projects.len() > usize::from(limit) {
            projects.pop();
            projects.last().map(|project| ProjectCursor {
                updated_at: project.updated_at,
                id: project.id,
            })
        } else {
            None
        };
        Ok(ProjectPage {
            items: projects,
            next_cursor,
        })
    }

    /// Create a project only for an organization owner. The owner grant, empty
    /// revision and zero-unit budget are committed with the project itself.
    pub async fn create_project(
        &self,
        actor_id: Uuid,
        input: CreateProjectInput,
    ) -> Result<ProjectSnapshot> {
        validate_project_name(&input.name)?;
        let data_policy = input.data_policy.unwrap_or_default();
        data_policy.validate()?;
        let limits = input.limits.unwrap_or_default();
        validate_limits(&limits)?;

        let project_id = Uuid::new_v4();
        let spec = AppSpec::default();
        let spec_json = serde_json::to_value(&spec).map_err(|_| Error::Internal)?;
        let policy_json = serde_json::to_value(&data_policy).map_err(|_| Error::Internal)?;
        let limits_json = serde_json::to_value(&limits).map_err(|_| Error::Internal)?;

        let mut tx = self.begin_actor(actor_id).await?;
        let is_owner: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM memberships \
             WHERE organization_id = $1 AND actor_id = $2 AND role = 'owner')",
        )
        .bind(input.organization_id)
        .bind(actor_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        if !is_owner {
            return Err(Error::Forbidden);
        }

        sqlx::query(
            "INSERT INTO projects \
             (id, organization_id, name, current_revision, event_sequence, data_policy, limits, created_by) \
             VALUES ($1, $2, $3, 0, 0, $4, $5, $6)",
        )
        .bind(project_id)
        .bind(input.organization_id)
        .bind(&input.name)
        .bind(policy_json)
        .bind(limits_json)
        .bind(actor_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;

        // RLS hides a project until it has a grant. This narrowly scoped
        // SECURITY DEFINER helper verifies the stored org/creator relationship.
        Store::grant_project_owner_in(&mut *tx, actor_id, project_id).await?;

        sqlx::query(
            "INSERT INTO app_revisions (project_id, revision, spec, created_by) \
             VALUES ($1, 0, $2, $3)",
        )
        .bind(project_id)
        .bind(spec_json)
        .bind(actor_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        sqlx::query(
            "INSERT INTO project_budgets \
             (project_id, limit_units, reserved_units, spent_units) VALUES ($1, 0, 0, 0)",
        )
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let initialization_decision = AddDecisionInput {
            revision: 0,
            kind: "project.initialized".into(),
            payload: json!({"data_policy": data_policy, "limits": limits}),
        };
        validate_decision(&initialization_decision)?;
        let initialization_decision_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO decisions (id, project_id, revision, actor_id, kind, payload) \
             VALUES ($1, $2, 0, $3, $4, $5)",
        )
        .bind(initialization_decision_id)
        .bind(project_id)
        .bind(actor_id)
        .bind(&initialization_decision.kind)
        .bind(initialization_decision.payload)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        Store::append_event(
            &mut *tx,
            project_id,
            "project.created",
            json!({
                "project_id": project_id,
                "revision": 0,
                "decision_id": initialization_decision_id,
                "status": "created"
            }),
        )
        .await?;

        let snapshot = load_snapshot(&mut *tx, project_id).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(snapshot)
    }

    pub async fn get_project(&self, actor_id: Uuid, project_id: Uuid) -> Result<ProjectSnapshot> {
        let mut tx = self.begin_actor(actor_id).await?;
        Store::authorize_in(&mut *tx, actor_id, project_id, "read").await?;
        let snapshot = load_snapshot(&mut *tx, project_id).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(snapshot)
    }

    /// Read only the policy required for a model dispatch. This uses `model`
    /// authority rather than requiring project-wide `read` visibility.
    pub async fn get_model_policy(&self, actor_id: Uuid, project_id: Uuid) -> Result<DataPolicy> {
        let mut tx = self.begin_actor(actor_id).await?;
        Store::authorize_in(&mut *tx, actor_id, project_id, "model").await?;
        let policy =
            sqlx::query_scalar::<_, Value>("SELECT data_policy FROM projects WHERE id = $1")
                .bind(project_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?
                .ok_or(Error::NotFound)?;
        let policy: DataPolicy = decode_json(policy)?;
        policy.validate()?;
        tx.commit().await.map_err(db_error)?;
        Ok(policy)
    }

    pub async fn get_revision(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        revision: i64,
    ) -> Result<AppRevision> {
        if revision < 0 {
            return Err(Error::Invalid("revision must be non-negative".into()));
        }
        let mut tx = self.begin_actor(actor_id).await?;
        Store::authorize_in(&mut *tx, actor_id, project_id, "read").await?;
        let revision = load_revision(&mut *tx, project_id, revision).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(revision)
    }

    pub async fn apply_changes(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
        changes: &ChangeSet,
    ) -> Result<ApplyChangesResult> {
        let mut tx = self.begin_actor(actor_id).await?;
        let result = apply_changes_in(
            &mut *tx,
            actor_id,
            project_id,
            expected_revision,
            idempotency_key,
            changes,
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }

    pub async fn list_decisions(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        after_id: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<ProjectDecision>> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let mut tx = self.begin_actor(actor_id).await?;
        Store::authorize_in(&mut *tx, actor_id, project_id, "read").await?;
        let rows = if let Some(after_id) = after_id {
            sqlx::query(
                "SELECT id, project_id, revision, actor_id, kind, payload, created_at \
                 FROM decisions WHERE project_id = $1 AND id < $2 \
                 ORDER BY id DESC LIMIT $3",
            )
            .bind(project_id)
            .bind(after_id)
            .bind(limit)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?
        } else {
            sqlx::query(
                "SELECT id, project_id, revision, actor_id, kind, payload, created_at \
                 FROM decisions WHERE project_id = $1 ORDER BY id DESC LIMIT $2",
            )
            .bind(project_id)
            .bind(limit)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?
        };
        let decisions = rows
            .iter()
            .map(ProjectDecision::from_row)
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await.map_err(db_error)?;
        Ok(decisions)
    }

    pub async fn add_decision(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        input: AddDecisionInput,
    ) -> Result<ProjectDecision> {
        validate_decision(&input)?;
        let mut tx = self.begin_actor(actor_id).await?;
        Store::authorize_in(&mut *tx, actor_id, project_id, "write").await?;
        let revision_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM app_revisions \
             WHERE project_id = $1 AND revision = $2)",
        )
        .bind(project_id)
        .bind(input.revision)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        if !revision_exists {
            return Err(Error::NotFound);
        }

        let decision_id = Uuid::new_v4();
        let row = sqlx::query(
            "INSERT INTO decisions (id, project_id, revision, actor_id, kind, payload) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id, project_id, revision, actor_id, kind, payload, created_at",
        )
        .bind(decision_id)
        .bind(project_id)
        .bind(input.revision)
        .bind(actor_id)
        .bind(&input.kind)
        .bind(input.payload)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        Store::append_event(
            &mut *tx,
            project_id,
            "project.decision.added",
            json!({
                "project_id": project_id,
                "decision_id": decision_id,
                "revision": input.revision,
                "status": "recorded"
            }),
        )
        .await?;
        let decision = ProjectDecision::from_row(&row)?;
        tx.commit().await.map_err(db_error)?;
        Ok(decision)
    }

    pub async fn update_data_policy(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        policy: DataPolicy,
    ) -> Result<ProjectSnapshot> {
        let mut tx = self.begin_actor(actor_id).await?;
        let project = self
            .update_data_policy_in(&mut *tx, actor_id, project_id, expected_revision, &policy)
            .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(project)
    }

    pub async fn update_limits(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        limits: ProjectLimits,
    ) -> Result<ProjectSnapshot> {
        let mut tx = self.begin_actor(actor_id).await?;
        let project = self
            .update_limits_in(&mut *tx, actor_id, project_id, expected_revision, &limits)
            .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(project)
    }

    pub async fn list_events(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        after_sequence: i64,
        limit: i64,
    ) -> Result<Vec<Event>> {
        Ok(self
            .read_event_page(actor_id, project_id, after_sequence, limit)
            .await?
            .events)
    }

    pub async fn event_bounds(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
    ) -> Result<(Option<i64>, Option<i64>)> {
        let page = self.read_event_page(actor_id, project_id, 0, 1).await?;
        Ok((page.earliest_retained, Some(page.latest_sequence)))
    }

    pub async fn read_event_page(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        after_sequence: i64,
        limit: i64,
    ) -> Result<EventFeedPage> {
        if after_sequence < 0 {
            return Err(Error::Invalid("event cursor must be non-negative".into()));
        }
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let mut tx = self.begin_actor(actor_id).await?;
        Store::authorize_in(&mut *tx, actor_id, project_id, "read").await?;
        let rows = sqlx::query(
            "SELECT bounds.earliest_sequence AS earliest_retained, \
                    bounds.latest_sequence, \
                    visible.project_id AS event_project_id, \
                    visible.sequence AS event_sequence, \
                    visible.type AS event_type, \
                    visible.payload AS event_payload, \
                    visible.actor_id AS event_actor_id, \
                    visible.created_at AS event_created_at \
             FROM public.kyro_event_history_bounds($1) AS bounds \
             LEFT JOIN LATERAL ( \
                 SELECT project_id, sequence, type, payload, actor_id, created_at \
                 FROM public.events \
                 WHERE project_id = $1 AND sequence > $2 \
                 ORDER BY sequence ASC LIMIT $3 \
             ) AS visible ON TRUE \
             ORDER BY visible.sequence ASC NULLS LAST",
        )
        .bind(project_id)
        .bind(after_sequence)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?;
        let metadata = rows.first().ok_or(Error::NotFound)?;
        let earliest_retained = column(metadata, "earliest_retained")?;
        let latest_sequence = column(metadata, "latest_sequence")?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(sequence) = column::<Option<i64>>(&row, "event_sequence")? else {
                continue;
            };
            events.push(Event {
                project_id: column(&row, "event_project_id")?,
                sequence,
                kind: column(&row, "event_type")?,
                payload: column(&row, "event_payload")?,
                actor_id: column(&row, "event_actor_id")?,
                created_at: column(&row, "event_created_at")?,
            });
        }
        tx.commit().await.map_err(db_error)?;
        Ok(EventFeedPage {
            earliest_retained,
            latest_sequence,
            events,
        })
    }
}

/// Transactional helper shared with QueueStore. The caller owns `conn`'s
/// transaction, so its revision, event, command result, and job completion can
/// be committed atomically.
pub async fn apply_changes_in(
    conn: &mut PgConnection,
    actor_id: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    idempotency_key: &str,
    changes: &ChangeSet,
) -> Result<ApplyChangesResult> {
    validate_idempotency_key(idempotency_key)?;
    if expected_revision < 0 {
        return Err(Error::Invalid("revision must be non-negative".into()));
    }
    let project =
        sqlx::query("SELECT current_revision, limits FROM projects WHERE id = $1 FOR UPDATE")
            .bind(project_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_error)?
            .ok_or(Error::NotFound)?;
    let current_revision: i64 = column(&project, "current_revision")?;
    let limits: ProjectLimits = decode_json(column(&project, "limits")?)?;
    validate_limits(&limits)?;

    // A successful replay wins over a stale If-Match and returns the original
    // immutable revision, while a key reused for a different command is a 409.
    changes.validate().map_err(invalid_spec)?;
    let changeset_operations =
        u32::try_from(changes.operations.len()).map_err(|_| Error::ResourceLimit)?;
    let demand = GrantDemand {
        changeset_operations: Some(changeset_operations),
        ..GrantDemand::default()
    };
    Store::authorize_demand_in(conn, actor_id, project_id, &["write"], &demand).await?;
    let fingerprint = command_fingerprint(project_id, expected_revision, changes)?;
    if let Some(row) = sqlx::query(
        "SELECT fingerprint, result FROM change_commands \
         WHERE project_id = $1 AND idempotency_key = $2",
    )
    .bind(project_id)
    .bind(idempotency_key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(db_error)?
    {
        let stored_fingerprint: Vec<u8> = column(&row, "fingerprint")?;
        if stored_fingerprint != fingerprint {
            return Err(Error::IdempotencyConflict);
        }
        let stored: StoredCommandResult = decode_json(column(&row, "result")?)?;
        let revision = load_revision(conn, project_id, stored.revision).await?;
        return Ok(ApplyChangesResult {
            command_id: stored.command_id,
            revision,
        });
    }

    if expected_revision != current_revision {
        return Err(Error::StaleRevision {
            expected: expected_revision,
            current: current_revision,
        });
    }
    let new_revision = current_revision
        .checked_add(1)
        .ok_or(Error::ResourceLimit)?;
    // max_revisions counts the initial revision 0, so this ID must remain
    // strictly below the configured count.
    if u64::try_from(new_revision).unwrap_or(u64::MAX) >= u64::from(limits.max_revisions) {
        return Err(Error::ResourceLimit);
    }

    let current_spec: AppSpec = decode_json(
        sqlx::query_scalar::<_, Value>(
            "SELECT spec FROM app_revisions WHERE project_id = $1 AND revision = $2",
        )
        .bind(project_id)
        .bind(current_revision)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_error)?
        .ok_or(Error::NotFound)?,
    )?;
    let next_spec = apply_changes(&current_spec, changes).map_err(invalid_spec)?;
    let spec_json = serde_json::to_value(&next_spec).map_err(|_| Error::Internal)?;
    let command_id = Uuid::new_v4();
    let row = sqlx::query(
        "INSERT INTO app_revisions (project_id, revision, spec, created_by) \
         VALUES ($1, $2, $3, $4) \
         RETURNING project_id, revision, spec, created_by, created_at",
    )
    .bind(project_id)
    .bind(new_revision)
    .bind(spec_json)
    .bind(actor_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(db_error)?;
    sqlx::query(
        "UPDATE projects SET current_revision = $2, updated_at = clock_timestamp() WHERE id = $1",
    )
    .bind(project_id)
    .bind(new_revision)
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    Store::append_event(
        conn,
        project_id,
        "project.revision.created",
        json!({
            "project_id": project_id,
            "revision": new_revision,
            "command_id": command_id,
            "status": "created"
        }),
    )
    .await?;

    let stored_result = StoredCommandResult {
        command_id,
        revision: new_revision,
    };
    let result_json = serde_json::to_value(stored_result).map_err(|_| Error::Internal)?;
    sqlx::query(
        "INSERT INTO change_commands \
         (project_id, idempotency_key, fingerprint, result, command_id) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(project_id)
    .bind(idempotency_key)
    .bind(fingerprint)
    .bind(result_json)
    .bind(command_id)
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;

    Ok(ApplyChangesResult {
        command_id,
        revision: AppRevision::from_row(&row)?,
    })
}

/// Locks the project row in the caller's transaction and verifies a job's
/// source revision before the worker locks its job row.
pub async fn validate_source_revision_in(
    conn: &mut PgConnection,
    project_id: Uuid,
    expected_revision: i64,
) -> Result<i64> {
    if expected_revision < 0 {
        return Err(Error::Invalid("revision must be non-negative".into()));
    }
    let current_revision: i64 =
        sqlx::query_scalar("SELECT current_revision FROM projects WHERE id = $1 FOR UPDATE")
            .bind(project_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_error)?
            .ok_or(Error::NotFound)?;
    if expected_revision != current_revision {
        return Err(Error::StaleRevision {
            expected: expected_revision,
            current: current_revision,
        });
    }
    Ok(current_revision)
}

impl Store {
    pub async fn update_data_policy_in(
        &self,
        conn: &mut PgConnection,
        actor_id: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        policy: &DataPolicy,
    ) -> Result<ProjectSnapshot> {
        policy.validate()?;
        update_project_metadata_revision_in(
            conn,
            actor_id,
            project_id,
            expected_revision,
            Some(policy),
            None,
            "project.data_policy.updated",
        )
        .await
    }

    pub async fn update_limits_in(
        &self,
        conn: &mut PgConnection,
        actor_id: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        limits: &ProjectLimits,
    ) -> Result<ProjectSnapshot> {
        validate_limits(limits)?;
        update_project_metadata_revision_in(
            conn,
            actor_id,
            project_id,
            expected_revision,
            None,
            Some(limits),
            "project.limits.updated",
        )
        .await
    }
}

async fn update_project_metadata_revision_in(
    conn: &mut PgConnection,
    actor_id: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    data_policy: Option<&DataPolicy>,
    limits: Option<&ProjectLimits>,
    decision_kind: &str,
) -> Result<ProjectSnapshot> {
    if expected_revision < 0 || data_policy.is_some() == limits.is_some() {
        return Err(Error::Invalid("invalid project metadata update".into()));
    }

    let project = sqlx::query(
        "SELECT current_revision, data_policy, limits FROM projects WHERE id = $1 FOR UPDATE",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(db_error)?
    .ok_or(Error::NotFound)?;
    Store::authorize_in(conn, actor_id, project_id, "manage").await?;
    let current_revision: i64 = column(&project, "current_revision")?;
    if current_revision != expected_revision {
        return Err(Error::StaleRevision {
            expected: expected_revision,
            current: current_revision,
        });
    }
    let current_limits: ProjectLimits = decode_json(column(&project, "limits")?)?;
    validate_limits(&current_limits)?;
    let next_limits = limits.unwrap_or(&current_limits);
    validate_limits(next_limits)?;

    let new_revision = current_revision
        .checked_add(1)
        .ok_or(Error::ResourceLimit)?;
    if u64::try_from(new_revision).unwrap_or(u64::MAX) >= u64::from(next_limits.max_revisions) {
        return Err(Error::ResourceLimit);
    }

    let current_spec: AppSpec = decode_json(
        sqlx::query_scalar::<_, Value>(
            "SELECT spec FROM app_revisions WHERE project_id = $1 AND revision = $2",
        )
        .bind(project_id)
        .bind(current_revision)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_error)?
        .ok_or(Error::NotFound)?,
    )?;
    current_spec.validate().map_err(invalid_spec)?;
    let spec_json = serde_json::to_value(&current_spec).map_err(|_| Error::Internal)?;
    let policy_json = data_policy
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| Error::Internal)?;
    let limits_json = limits
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| Error::Internal)?;

    let decision_payload = if let Some(policy) = data_policy {
        json!({"data_policy": policy})
    } else {
        json!({"limits": limits.expect("one project metadata update is present")})
    };
    let decision_input = AddDecisionInput {
        revision: new_revision,
        kind: decision_kind.to_owned(),
        payload: decision_payload,
    };
    validate_decision(&decision_input)?;

    sqlx::query(
        "INSERT INTO app_revisions (project_id, revision, spec, created_by) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(project_id)
    .bind(new_revision)
    .bind(spec_json)
    .bind(actor_id)
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    let updated = sqlx::query(
        "UPDATE projects SET current_revision = $3, \
                data_policy = COALESCE($4, data_policy), \
                limits = COALESCE($5, limits), updated_at = clock_timestamp() \
         WHERE id = $1 AND current_revision = $2",
    )
    .bind(project_id)
    .bind(expected_revision)
    .bind(new_revision)
    .bind(policy_json)
    .bind(limits_json)
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    if updated.rows_affected() != 1 {
        return Err(Error::StaleRevision {
            expected: expected_revision,
            current: current_revision,
        });
    }

    let decision_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO decisions (id, project_id, revision, actor_id, kind, payload) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(decision_id)
    .bind(project_id)
    .bind(new_revision)
    .bind(actor_id)
    .bind(decision_kind)
    .bind(decision_input.payload)
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    Store::append_event(
        conn,
        project_id,
        decision_kind,
        json!({
            "project_id": project_id,
            "revision": new_revision,
            "decision_id": decision_id,
            "status": "updated"
        }),
    )
    .await?;

    load_snapshot(conn, project_id).await
}

async fn load_snapshot(conn: &mut PgConnection, project_id: Uuid) -> Result<ProjectSnapshot> {
    let project = load_project(conn, project_id).await?;
    let revision = load_revision(conn, project_id, project.current_revision).await?;
    Ok(ProjectSnapshot { project, revision })
}

async fn load_project(conn: &mut PgConnection, project_id: Uuid) -> Result<Project> {
    let row = sqlx::query(
        "SELECT id, organization_id, name, current_revision, event_sequence, \
                data_policy, limits, created_by, created_at, updated_at \
         FROM projects WHERE id = $1 FOR SHARE",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(db_error)?
    .ok_or(Error::NotFound)?;
    Project::from_row(&row)
}

async fn load_revision(
    conn: &mut PgConnection,
    project_id: Uuid,
    revision: i64,
) -> Result<AppRevision> {
    let row = sqlx::query(
        "SELECT project_id, revision, spec, created_by, created_at \
         FROM app_revisions WHERE project_id = $1 AND revision = $2",
    )
    .bind(project_id)
    .bind(revision)
    .fetch_optional(&mut *conn)
    .await
    .map_err(db_error)?
    .ok_or(Error::NotFound)?;
    AppRevision::from_row(&row)
}

fn decode_json<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| Error::Internal)
}

fn column<T>(row: &PgRow, name: &str) -> Result<T>
where
    T: for<'row> Decode<'row, Postgres> + Type<Postgres>,
{
    row.try_get(name).map_err(db_error)
}

fn validate_project_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name.len() > MAX_PROJECT_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        return Err(Error::Invalid("invalid project name".into()));
    }
    Ok(())
}

fn validate_limits(limits: &ProjectLimits) -> Result<()> {
    limits
        .validate()
        .map_err(|_| Error::Invalid("invalid project limits".into()))
}

fn validate_idempotency_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_IDEMPOTENCY_KEY_BYTES || key.chars().any(char::is_control)
    {
        return Err(Error::Invalid("invalid idempotency key".into()));
    }
    Ok(())
}

fn validate_decision(input: &AddDecisionInput) -> Result<()> {
    if input.revision < 0
        || input.kind.is_empty()
        || input.kind.len() > MAX_DECISION_KIND_BYTES
        || !input
            .kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
    {
        return Err(Error::Invalid("invalid decision".into()));
    }
    let payload = serde_json::to_vec(&input.payload)
        .map_err(|_| Error::Invalid("invalid decision".into()))?;
    if payload.len() > MAX_DECISION_PAYLOAD_BYTES || !bounded_decision_json(&input.payload, 1) {
        return Err(Error::Invalid("decision payload exceeds bounds".into()));
    }
    Ok(())
}

fn bounded_decision_json(value: &Value, depth: usize) -> bool {
    if depth > 24 {
        return false;
    }
    match value {
        Value::String(text) => text.len() <= 8 * 1024,
        Value::Array(values) => {
            values.len() <= 128
                && values
                    .iter()
                    .all(|value| bounded_decision_json(value, depth + 1))
        }
        Value::Object(values) => {
            values.len() <= 128
                && values
                    .iter()
                    .all(|(key, value)| key.len() <= 128 && bounded_decision_json(value, depth + 1))
        }
        _ => true,
    }
}

fn invalid_spec(error: SpecError) -> Error {
    let _ = error;
    Error::Invalid("invalid AppSpec changes".into())
}

fn command_fingerprint(
    project_id: Uuid,
    expected_revision: i64,
    changes: &ChangeSet,
) -> Result<Vec<u8>> {
    let value = json!({
        "action": "apply_changes",
        "project_id": project_id,
        "expected_revision": expected_revision,
        "body": changes,
    });
    let mut canonical = Vec::new();
    write_canonical_json(&value, &mut canonical)?;
    Ok(Sha256::digest(canonical).to_vec())
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        Value::String(value) => {
            let encoded = serde_json::to_vec(value).map_err(|_| Error::Internal)?;
            output.extend_from_slice(&encoded);
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                let encoded_key = serde_json::to_vec(key).map_err(|_| Error::Internal)?;
                output.extend_from_slice(&encoded_key);
                output.push(b':');
                write_canonical_json(value, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}
