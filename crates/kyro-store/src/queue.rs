//! PostgreSQL-backed durable jobs.
//!
//! Queue transactions are short. Model requests are dispatched by `kyro-gateway`
//! only after their effect intent is committed; project mutations use the
//! project helper and the job CAS in one transaction.

use chrono::{DateTime, Utc};
use kyro_domain::{
    Environment, Error, Result,
    identity::GrantDemand,
    model::{
        EffectIntent, EffectReconcileOutcome, EffectReconciliationContext, EffectStatus,
        ModelEffectContext, ReconcileEffectRequest,
    },
    spec::{ChangeSet, ProjectLimits},
    task::{Job, JobCursor, JobErrorCode, JobLease, JobPage, JobPayload, JobResult, JobStatus},
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection, Row};
use uuid::Uuid;

use crate::{Store, projects::apply_changes_in};

const MAX_IDEMPOTENCY_KEY_BYTES: usize = 200;
const MAX_PAYLOAD_BYTES: usize = 1_048_576;
const MAX_PAGE_SIZE: u16 = 100;
const MIN_LEASE_SECONDS: i64 = 2;
const MAX_LEASE_SECONDS: i64 = 120;

#[derive(FromRow)]
struct JobRow {
    id: Uuid,
    project_id: Uuid,
    actor_id: Uuid,
    environment: String,
    source_revision: i64,
    payload: Value,
    status: String,
    attempts: i32,
    max_attempts: i32,
    generation: i64,
    lease_owner: Option<String>,
    lease_until: Option<DateTime<Utc>>,
    deadline: DateTime<Utc>,
    cancel_requested: bool,
    result: Option<Value>,
    error_code: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl JobRow {
    fn into_job(self) -> Result<Job> {
        let status = JobStatus::parse(&self.status).ok_or(Error::Internal)?;
        let payload: JobPayload = serde_json::from_value(self.payload)
            .map_err(|_| Error::Invalid("payload de job non pris en charge".into()))?;
        let payload = payload.summary();
        let result = self
            .result
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::Internal)?;
        let error_code = match self.error_code.as_deref() {
            Some(code) => Some(JobErrorCode::parse(code).ok_or(Error::Internal)?),
            None => None,
        };

        Ok(Job {
            id: self.id,
            project_id: self.project_id,
            actor_id: self.actor_id,
            environment: parse_environment(&self.environment)?,
            source_revision: self.source_revision,
            payload,
            status,
            attempts: u8::try_from(self.attempts).map_err(|_| Error::Internal)?,
            max_attempts: u8::try_from(self.max_attempts).map_err(|_| Error::Internal)?,
            generation: self.generation,
            lease_owner: self
                .lease_owner
                .map(|owner| Uuid::parse_str(&owner).map_err(|_| Error::Internal))
                .transpose()?,
            lease_until: self.lease_until,
            deadline: self.deadline,
            cancel_requested: self.cancel_requested,
            result,
            error_code,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(FromRow)]
struct ProjectAdmissionRow {
    current_revision: i64,
    limits: Value,
}

#[derive(Serialize)]
struct AdmissionFingerprint<'a> {
    environment: &'a str,
    actor_id: Uuid,
    project_id: Uuid,
    source_revision: i64,
    payload: &'a JobPayload,
    max_attempts: Option<u8>,
    ttl_seconds: Option<u32>,
}

#[derive(Serialize)]
struct EffectReconciliationFingerprint<'a> {
    environment: &'a str,
    actor_id: Uuid,
    project_id: Uuid,
    effect_id: Uuid,
    request: &'a ReconcileEffectRequest,
}

impl Store {
    /// Admit a job under the actor's `execute` grant and project resource limits.
    /// The project lock serializes both idempotency and active/queued quotas.
    pub async fn enqueue_job(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        source_revision: i64,
        idempotency_key: &str,
        payload: JobPayload,
        max_attempts: Option<u8>,
        ttl_seconds: Option<u32>,
    ) -> Result<Job> {
        validate_idempotency_key(idempotency_key)?;
        if matches!(&payload, JobPayload::ReconcileEffect { .. }) {
            return Err(Error::Invalid(
                "les rapprochements doivent utiliser l'admission Budget/Manage".into(),
            ));
        }
        if source_revision < 0 {
            return Err(Error::Invalid("révision source invalide".into()));
        }
        let payload_json = serde_json::to_value(&payload)
            .map_err(|_| Error::Invalid("payload invalide".into()))?;
        if serde_json::to_vec(&payload_json)
            .map_err(|_| Error::Invalid("payload invalide".into()))?
            .len()
            > MAX_PAYLOAD_BYTES
        {
            return Err(Error::ResourceLimit);
        }
        payload.validate_for_queue()?;

        let mut tx = self.begin_actor(actor_id).await?;
        let project = sqlx::query_as::<_, ProjectAdmissionRow>(
            "SELECT current_revision, limits FROM public.kyro_lock_project_for_actor($1, ARRAY['execute']::TEXT[])",
        )
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        .ok_or(Error::NotFound)?;
        let no_demand = empty_grant_demand();
        authorize_job_grants(&mut *tx, actor_id, project_id, &payload, &no_demand).await?;

        let fingerprint = admission_fingerprint(
            self.environment,
            actor_id,
            project_id,
            source_revision,
            &payload,
            max_attempts,
            ttl_seconds,
        )?;
        if let Some(existing) = sqlx::query_as::<_, ExistingCommandRow>(
            "SELECT fingerprint, result FROM change_commands \
             WHERE project_id = $1 AND idempotency_key = $2",
        )
        .bind(project_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        {
            if existing.fingerprint.as_slice() != fingerprint.as_slice() {
                return Err(Error::IdempotencyConflict);
            }
            let job_id = existing
                .result
                .get("job_id")
                .and_then(Value::as_str)
                .and_then(|raw| Uuid::parse_str(raw).ok())
                .ok_or(Error::Internal)?;
            let row = fetch_job_row(&mut *tx, project_id, job_id, self.environment).await?;
            let persisted =
                decode_supported_payload(row.payload.clone()).map_err(|_| Error::Internal)?;
            let demand = grant_demand_for_row(&row, &persisted)?;
            authorize_job_grants(&mut *tx, actor_id, project_id, &persisted, &demand).await?;
            tx.commit().await.map_err(map_database_error)?;
            return row.into_job();
        }

        let limits: ProjectLimits =
            serde_json::from_value(project.limits).map_err(|_| Error::Internal)?;
        limits.validate().map_err(|_| Error::Internal)?;

        let requested_attempts = max_attempts.unwrap_or(limits.max_job_attempts);
        let requested_ttl = ttl_seconds.unwrap_or(limits.job_ttl_secs);
        if !(1..=limits.max_job_attempts.min(3)).contains(&requested_attempts) {
            return Err(Error::ResourceLimit);
        }
        if !(10..=limits.job_ttl_secs.min(1_800)).contains(&requested_ttl) {
            return Err(Error::ResourceLimit);
        }

        let demand = grant_demand_for_job(&payload, requested_attempts, requested_ttl)?;
        authorize_job_grants(&mut *tx, actor_id, project_id, &payload, &demand).await?;

        if project.current_revision != source_revision {
            return Err(Error::StaleRevision {
                expected: source_revision,
                current: project.current_revision,
            });
        }

        let counts = sqlx::query_as::<_, QueueCounts>(
            "SELECT \
                count(*) FILTER (WHERE status = 'pending')::BIGINT AS queued \
             FROM jobs WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_database_error)?;
        if counts.queued >= i64::from(limits.max_queued_jobs) {
            return Err(Error::ResourceLimit);
        }

        let job_id = Uuid::new_v4();
        let row = sqlx::query_as::<_, JobRow>(
            "WITH job_time AS (SELECT clock_timestamp() AS created_at) \
             INSERT INTO jobs (id, project_id, actor_id, environment, source_revision, payload, status, \
                 attempts, max_attempts, generation, lease_owner, lease_until, deadline, \
                 cancel_requested, result, error_code, created_at, updated_at) \
             SELECT $1, $2, $3, $4, $5, $6, 'pending', 0, $7, 0, NULL, NULL, \
                 job_time.created_at + ($8::BIGINT * interval '1 second'), false, NULL, NULL, \
                 job_time.created_at, job_time.created_at FROM job_time \
             RETURNING id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
                 max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
                 result, error_code, created_at, updated_at",
        )
        .bind(job_id)
        .bind(project_id)
        .bind(actor_id)
        .bind(self.environment.as_str())
        .bind(source_revision)
        .bind(payload_json)
        .bind(i32::from(requested_attempts))
        .bind(i64::from(requested_ttl))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_database_error)?;

        sqlx::query(
            "INSERT INTO change_commands (project_id, idempotency_key, fingerprint, result, command_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, clock_timestamp())",
        )
        .bind(project_id)
        .bind(idempotency_key)
        .bind(fingerprint.to_vec())
        .bind(json!({ "job_id": job_id }))
        .bind(Uuid::new_v4())
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?;

        Store::append_event(
            &mut *tx,
            project_id,
            "job.queued",
            json!({ "job_id": job_id, "status": "pending" }),
        )
        .await?;
        tx.commit().await.map_err(map_database_error)?;
        row.into_job()
    }

    /// Queue a financial reconciliation for a visible synthetic unknown effect.
    /// Budget or manage authority is sufficient; source revision is recorded only
    /// to satisfy the job FK and is deliberately absent from the fingerprint.
    pub async fn enqueue_effect_reconciliation(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        effect_id: Uuid,
        idempotency_key: &str,
        request: ReconcileEffectRequest,
    ) -> Result<Job> {
        validate_idempotency_key(idempotency_key)?;
        request.validate()?;
        let payload = JobPayload::ReconcileEffect {
            effect_id,
            request: request.clone(),
        };
        payload.validate_for_queue()?;
        let payload_json = serde_json::to_value(&payload)
            .map_err(|_| Error::Invalid("payload de réconciliation invalide".into()))?;
        if serde_json::to_vec(&payload_json)
            .map_err(|_| Error::Invalid("payload de réconciliation invalide".into()))?
            .len()
            > MAX_PAYLOAD_BYTES
        {
            return Err(Error::ResourceLimit);
        }

        let mut tx = self.begin_actor(actor_id).await?;
        let project = sqlx::query_as::<_, ProjectAdmissionRow>(
            "SELECT current_revision, limits FROM public.kyro_lock_project_for_actor($1, ARRAY['budget', 'manage']::TEXT[])",
        )
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        .ok_or(Error::NotFound)?;
        Store::authorize_demand_in(
            &mut *tx,
            actor_id,
            project_id,
            &["budget", "manage"],
            &empty_grant_demand(),
        )
        .await?;

        let fingerprint = effect_reconciliation_fingerprint(
            self.environment,
            actor_id,
            project_id,
            effect_id,
            &request,
        )?;
        if let Some(existing) = sqlx::query_as::<_, ExistingCommandRow>(
            "SELECT fingerprint, result FROM change_commands \
             WHERE project_id = $1 AND idempotency_key = $2",
        )
        .bind(project_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        {
            if existing.fingerprint.as_slice() != fingerprint.as_slice() {
                return Err(Error::IdempotencyConflict);
            }
            let job_id = existing
                .result
                .get("job_id")
                .and_then(Value::as_str)
                .and_then(|raw| Uuid::parse_str(raw).ok())
                .ok_or(Error::Internal)?;
            let row = fetch_job_row(&mut *tx, project_id, job_id, self.environment).await?;
            tx.commit().await.map_err(map_database_error)?;
            return row.into_job();
        }

        let limits: ProjectLimits =
            serde_json::from_value(project.limits).map_err(|_| Error::Internal)?;
        limits.validate().map_err(|_| Error::Internal)?;
        let ttl = limits.job_ttl_secs.min(1_800);
        let attempts = limits.max_job_attempts.min(3);
        if !(10..=1_800).contains(&ttl) || !(1..=3).contains(&attempts) {
            return Err(Error::Internal);
        }

        let target = sqlx::query_as::<_, EffectTargetRow>(
            "SELECT e.job_id FROM effects e \
             JOIN jobs target ON target.id = e.job_id AND target.project_id = e.project_id \
             WHERE e.id = $1 AND e.project_id = $2 \
               AND e.intent->'registration'->>'provider_kind' = 'synthetic' \
               AND e.status = 'unknown' AND target.status = 'unknown' \
               AND target.environment = $3",
        )
        .bind(effect_id)
        .bind(project_id)
        .bind(self.environment.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        .ok_or(Error::NotFound)?;
        let _target_job_id = target.job_id;

        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM jobs WHERE project_id = $1 AND status = 'pending'",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_database_error)?;
        if queued >= i64::from(limits.max_queued_jobs) {
            return Err(Error::ResourceLimit);
        }

        let job_id = Uuid::new_v4();
        let row = sqlx::query_as::<_, JobRow>(
            "WITH job_time AS (SELECT clock_timestamp() AS created_at) \
             INSERT INTO jobs (id, project_id, actor_id, environment, source_revision, payload, status, \
                 attempts, max_attempts, generation, lease_owner, lease_until, deadline, \
                 cancel_requested, result, error_code, created_at, updated_at) \
             SELECT $1, $2, $3, $4, $5, $6, 'pending', 0, $7, 0, NULL, NULL, \
                 job_time.created_at + ($8::BIGINT * interval '1 second'), false, NULL, NULL, \
                 job_time.created_at, job_time.created_at FROM job_time \
             RETURNING id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
                 max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
                 result, error_code, created_at, updated_at",
        )
        .bind(job_id)
        .bind(project_id)
        .bind(actor_id)
        .bind(self.environment.as_str())
        .bind(project.current_revision)
        .bind(payload_json)
        .bind(i32::from(attempts))
        .bind(i64::from(ttl))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_database_error)?;

        sqlx::query(
            "INSERT INTO change_commands (project_id, idempotency_key, fingerprint, result, command_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, clock_timestamp())",
        )
        .bind(project_id)
        .bind(idempotency_key)
        .bind(fingerprint.to_vec())
        .bind(json!({ "job_id": job_id }))
        .bind(Uuid::new_v4())
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?;

        Store::append_event(
            &mut *tx,
            project_id,
            "job.queued",
            json!({ "job_id": job_id, "status": "pending" }),
        )
        .await?;
        tx.commit().await.map_err(map_database_error)?;
        row.into_job()
    }

    pub async fn get_job(&self, actor_id: Uuid, project_id: Uuid, job_id: Uuid) -> Result<Job> {
        let mut tx = self.begin_actor(actor_id).await?;
        let row = fetch_job_row(&mut *tx, project_id, job_id, self.environment).await?;
        let payload = decode_supported_payload(row.payload.clone()).map_err(|_| Error::Internal)?;
        if row.actor_id == actor_id && matches!(payload, JobPayload::ReconcileEffect { .. }) {
            Store::authorize_demand_in(
                &mut *tx,
                actor_id,
                project_id,
                &["budget", "manage"],
                &empty_grant_demand(),
            )
            .await?;
        } else {
            Store::authorize_in(&mut *tx, actor_id, project_id, "read").await?;
        }
        tx.commit().await.map_err(map_database_error)?;
        row.into_job()
    }

    pub async fn list_jobs(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        limit: u16,
        before: Option<JobCursor>,
    ) -> Result<JobPage> {
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(Error::ResourceLimit);
        }
        let mut tx = self.begin_actor(actor_id).await?;
        Store::authorize_in(&mut *tx, actor_id, project_id, "read").await?;
        let rows = sqlx::query_as::<_, JobRow>(
            "SELECT id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
                 max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
                 result, error_code, created_at, updated_at \
             FROM jobs WHERE project_id = $1 AND environment = $2 \
                 AND ($3::TIMESTAMPTZ IS NULL OR (created_at, id) < ($3, $4)) \
             ORDER BY created_at DESC, id DESC LIMIT $5",
        )
        .bind(project_id)
        .bind(self.environment.as_str())
        .bind(before.as_ref().map(|cursor| cursor.created_at.clone()))
        .bind(before.map(|cursor| cursor.id))
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;

        let mut jobs = rows
            .into_iter()
            .map(JobRow::into_job)
            .collect::<Result<Vec<_>>>()?;
        let has_more = jobs.len() > usize::from(limit);
        if has_more {
            jobs.pop();
        }
        let next_cursor = if has_more {
            jobs.last().map(|job| JobCursor {
                created_at: job.created_at.clone(),
                id: job.id,
            })
        } else {
            None
        };
        Ok(JobPage {
            items: jobs,
            next_cursor,
        })
    }

    /// Persist a cancellation request. The worker terminalizes pending jobs and
    /// stops running jobs before their next side effect; sending effects remain
    /// uncertain until reconciled.
    pub async fn cancel_job(&self, actor_id: Uuid, project_id: Uuid, job_id: Uuid) -> Result<Job> {
        let mut tx = self.begin_actor(actor_id).await?;
        lock_project(&mut *tx, project_id, &["execute", "manage"]).await?;
        let row = sqlx::query_as::<_, JobRow>(
            "SELECT id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
                 max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
                 result, error_code, created_at, updated_at \
             FROM jobs WHERE project_id = $1 AND id = $2 AND environment = $3 FOR UPDATE",
        )
        .bind(project_id)
        .bind(job_id)
        .bind(self.environment.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        .ok_or(Error::NotFound)?;

        let status = JobStatus::parse(&row.status).ok_or(Error::Internal)?;
        if row.actor_id == actor_id {
            Store::authorize_in(&mut *tx, actor_id, project_id, "execute").await?;
        } else {
            Store::authorize_in(&mut *tx, actor_id, project_id, "manage").await?;
        }
        if status.is_terminal() {
            return Err(Error::Conflict("job is already terminal".into()));
        }
        if row.cancel_requested {
            tx.commit().await.map_err(map_database_error)?;
            return row.into_job();
        }

        let updated = sqlx::query_as::<_, JobRow>(
            "UPDATE jobs SET cancel_requested = true \
             WHERE project_id = $1 AND id = $2 AND environment = $3 AND status = $4 \
             RETURNING id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
                 max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
                 result, error_code, created_at, updated_at",
        )
        .bind(project_id)
        .bind(job_id)
        .bind(self.environment.as_str())
        .bind(status.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_database_error)?;
        Store::append_event(
            &mut *tx,
            project_id,
            "job.cancel_requested",
            json!({ "job_id": job_id, "generation": row.generation, "status": status.as_str() }),
        )
        .await?;
        tx.commit().await.map_err(map_database_error)?;
        updated.into_job()
    }

    /// Claim one eligible job using the dedicated cross-project worker policy.
    /// The project row serializes its active-job cap. When durable dispatch is
    /// paused, model jobs that would need an HTTP call remain pending.
    pub async fn claim_next_job(
        &self,
        lease_owner: Uuid,
        lease_seconds: u64,
    ) -> Result<Option<JobLease>> {
        if !(MIN_LEASE_SECONDS as u64..=MAX_LEASE_SECONDS as u64).contains(&lease_seconds) {
            return Err(Error::Invalid("durée de bail hors limites".into()));
        }

        let mut tx = self.pool.begin().await.map_err(map_database_error)?;
        set_worker_claim(&mut *tx, self.environment, true).await?;
        let external_sends_enabled: bool = sqlx::query_scalar(
            "SELECT external_sends_enabled FROM public.runtime_control WHERE id = 1",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        .ok_or(Error::Unavailable)?;
        let candidates = sqlx::query_as::<_, ClaimCandidate>(
            "WITH ranked AS (\
                 SELECT j.id, j.project_id, j.deadline, j.created_at, \
                        row_number() OVER (PARTITION BY j.project_id \
                            ORDER BY (NOT $2 AND COALESCE(j.payload->>'kind', '') <> 'model_call') DESC, \
                                     (j.deadline <= clock_timestamp()) DESC, j.created_at, j.id) AS project_rank \
                 FROM jobs j \
                 WHERE j.environment = $1 AND (j.status = 'pending' OR \
                       (j.status = 'running' AND j.lease_until <= clock_timestamp())) \
             ) \
             SELECT id, project_id FROM ranked \
             WHERE project_rank = 1 \
             ORDER BY (deadline <= clock_timestamp()) DESC, random() \
             LIMIT 32",
        )
        .bind(self.environment.as_str())
        .bind(external_sends_enabled)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_database_error)?;

        if candidates.is_empty() {
            set_worker_claim(&mut *tx, self.environment, false).await?;
            tx.commit().await.map_err(map_database_error)?;
            return Ok(None);
        }

        for hint in candidates {
            // Do not lock the job first: appending its event takes the project
            // row, so all worker paths must use project→job ordering.
            let project = lock_project_for_worker_job(&mut *tx, hint.id, true).await?;
            let Some(project) = project else {
                continue;
            };
            if project.project_id != hint.project_id {
                return Err(Error::Internal);
            }
            let _current_revision = project.current_revision;

            // The project lock serializes claims for this tenant. Check its
            // bounded quota before taking the job lock, then recheck eligibility
            // while acquiring that job with SKIP LOCKED.
            let active_limit: i32 =
                sqlx::query_scalar("SELECT GREATEST(public.kyro_project_active_limit($1), 1)")
                    .bind(project.project_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(map_database_error)?;
            let active_jobs: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM jobs WHERE project_id = $1 AND environment = $2 \
                 AND status = 'running' AND lease_until > clock_timestamp()",
            )
            .bind(project.project_id)
            .bind(self.environment.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_database_error)?;
            if active_jobs >= i64::from(active_limit) {
                continue;
            }

            let candidate = sqlx::query_as::<_, JobRow>(
                "SELECT id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
                        max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
                        result, error_code, created_at, updated_at \
                 FROM jobs WHERE id = $1 AND project_id = $2 AND environment = $3 \
                   AND (status = 'pending' OR (status = 'running' AND lease_until <= clock_timestamp())) \
                 FOR UPDATE SKIP LOCKED",
            )
            .bind(hint.id)
            .bind(hint.project_id)
            .bind(self.environment.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_database_error)?;
            let Some(candidate) = candidate else {
                continue;
            };

            // Once the cross-tenant job row has been selected and locked, disable
            // the traversal GUC before setting actor context or touching effects.
            set_worker_claim(&mut *tx, self.environment, false).await?;
            set_actor_context(&mut *tx, candidate.actor_id, self.environment).await?;
            set_accounting_job_context(&mut *tx, candidate.id).await?;
            let effect = find_job_effect(&mut *tx, candidate.project_id, candidate.id).await?;
            let effect_status = effect.as_ref().map(|effect| effect.status.as_str());
            if effect_status == Some("sending") {
                let effect = effect.as_ref().ok_or(Error::Internal)?;
                mark_expired_sending_unknown_tx(
                    &mut *tx,
                    effect.id,
                    candidate.id,
                    candidate.project_id,
                )
                .await?;
            }

            let status = JobStatus::parse(&candidate.status).ok_or(Error::Internal)?;
            if !matches!(status, JobStatus::Pending | JobStatus::Running) {
                return Err(Error::Internal);
            }
            let deadline_expired = candidate.deadline <= utc_now(&mut *tx).await?;
            let effect_unknown = matches!(effect_status, Some("sending" | "unknown"));

            if effect_unknown {
                let effect = effect.as_ref().ok_or(Error::Internal)?;
                set_queue_write_context(&mut *tx, self.environment).await?;
                update_claim_candidate(
                    &mut *tx,
                    &candidate,
                    JobStatus::Unknown,
                    Some(JobErrorCode::GatewayUnavailable),
                    Some(JobResult::ModelCall {
                        effect_id: effect.id,
                        status: EffectStatus::Unknown,
                    }),
                )
                .await?;
                Store::append_event(
                &mut *tx,
                candidate.project_id,
                "job.unknown",
                json!({ "job_id": candidate.id, "generation": candidate.generation, "status": "unknown" }),
            )
            .await?;
                set_worker_claim(&mut *tx, self.environment, false).await?;
                tx.commit().await.map_err(map_database_error)?;
                return Ok(None);
            }

            if candidate.cancel_requested {
                set_queue_write_context(&mut *tx, self.environment).await?;
                update_claim_candidate(
                    &mut *tx,
                    &candidate,
                    JobStatus::Cancelled,
                    Some(JobErrorCode::Cancelled),
                    None,
                )
                .await?;
                if effect
                    .as_ref()
                    .is_some_and(|effect| effect.status == "prepared")
                {
                    set_worker_claim(&mut *tx, self.environment, false).await?;
                    if !crate::budget::release_prepared_effect_accounting(
                        &mut *tx,
                        candidate.id,
                        candidate.project_id,
                    )
                    .await?
                    {
                        return Err(Error::Conflict(
                            "prepared effect changed during cancellation".into(),
                        ));
                    }
                }
                set_queue_write_context(&mut *tx, self.environment).await?;
                Store::append_event(
                &mut *tx,
                candidate.project_id,
                "job.cancelled",
                json!({ "job_id": candidate.id, "generation": candidate.generation, "status": "cancelled" }),
            )
            .await?;
                set_worker_claim(&mut *tx, self.environment, false).await?;
                tx.commit().await.map_err(map_database_error)?;
                return Ok(None);
            }

            let is_cached_success = effect_status == Some("succeeded");
            let attempts_exhausted =
                candidate.attempts >= candidate.max_attempts && !is_cached_success;
            if deadline_expired || attempts_exhausted {
                let code = if deadline_expired {
                    JobErrorCode::DeadlineExpired
                } else {
                    JobErrorCode::AttemptsExceeded
                };
                set_queue_write_context(&mut *tx, self.environment).await?;
                update_claim_candidate(&mut *tx, &candidate, JobStatus::Failed, Some(code), None)
                    .await?;
                if effect
                    .as_ref()
                    .is_some_and(|effect| effect.status == "prepared")
                {
                    set_worker_claim(&mut *tx, self.environment, false).await?;
                    if !crate::budget::release_prepared_effect_accounting(
                        &mut *tx,
                        candidate.id,
                        candidate.project_id,
                    )
                    .await?
                    {
                        return Err(Error::Conflict(
                            "prepared effect changed during job expiry".into(),
                        ));
                    }
                }
                set_queue_write_context(&mut *tx, self.environment).await?;
                Store::append_event(
                &mut *tx,
                candidate.project_id,
                "job.failed",
                json!({ "job_id": candidate.id, "generation": candidate.generation, "status": "failed" }),
            )
            .await?;
                set_worker_claim(&mut *tx, self.environment, false).await?;
                tx.commit().await.map_err(map_database_error)?;
                return Ok(None);
            }

            if effect_status == Some("failed") || effect_status == Some("cancelled") {
                let terminal = if effect_status == Some("cancelled") {
                    JobStatus::Cancelled
                } else {
                    JobStatus::Failed
                };
                let error_code =
                    (terminal == JobStatus::Failed).then_some(JobErrorCode::ExecutionFailed);
                set_queue_write_context(&mut *tx, self.environment).await?;
                update_claim_candidate(
                    &mut *tx,
                    &candidate,
                    terminal,
                    error_code,
                    effect.as_ref().map(|effect| JobResult::ModelCall {
                        effect_id: effect.id,
                        status: parse_effect_status(&effect.status).unwrap_or(EffectStatus::Failed),
                    }),
                )
                .await?;
                Store::append_event(
                &mut *tx,
                candidate.project_id,
                if terminal == JobStatus::Cancelled { "job.cancelled" } else { "job.failed" },
                json!({ "job_id": candidate.id, "generation": candidate.generation, "status": terminal.as_str() }),
            )
            .await?;
                set_worker_claim(&mut *tx, self.environment, false).await?;
                tx.commit().await.map_err(map_database_error)?;
                return Ok(None);
            }

            // The effects table is intentionally unavailable to cross-project
            // queue traversal. Inspect it only after locking this job and
            // setting its accounting context. A persistent outbound pause must
            // not consume attempts for a fresh or merely prepared model call.
            let paused_model_call = !external_sends_enabled
                && matches!(
                    decode_supported_payload(candidate.payload.clone()),
                    Ok(JobPayload::ModelCall { .. })
                )
                && matches!(effect_status, None | Some("prepared"));
            if paused_model_call {
                set_worker_claim(&mut *tx, self.environment, true).await?;
                continue;
            }

            set_queue_write_context(&mut *tx, self.environment).await?;

            let payload = match decode_supported_payload(candidate.payload.clone()) {
                Ok(payload) => payload,
                Err(()) => {
                    set_queue_write_context(&mut *tx, self.environment).await?;
                    update_claim_candidate(
                        &mut *tx,
                        &candidate,
                        JobStatus::Failed,
                        Some(JobErrorCode::UnsupportedPayload),
                        None,
                    )
                    .await?;
                    if effect
                        .as_ref()
                        .is_some_and(|effect| effect.status == "prepared")
                    {
                        set_worker_claim(&mut *tx, self.environment, false).await?;
                        if !crate::budget::release_prepared_effect_accounting(
                            &mut *tx,
                            candidate.id,
                            candidate.project_id,
                        )
                        .await?
                        {
                            return Err(Error::Conflict(
                                "prepared effect changed during payload rejection".into(),
                            ));
                        }
                    }
                    set_queue_write_context(&mut *tx, self.environment).await?;
                    Store::append_event(
                    &mut *tx,
                    candidate.project_id,
                    "job.failed",
                    json!({ "job_id": candidate.id, "generation": candidate.generation, "status": "failed" }),
                )
                .await?;
                    set_worker_claim(&mut *tx, self.environment, false).await?;
                    tx.commit().await.map_err(map_database_error)?;
                    return Ok(None);
                }
            };

            // Reusing a durable successful effect is a close-only generation, not
            // another execution attempt. All other claims consume one bounded try.
            let increments_attempt = !is_cached_success;
            let lease_until: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE jobs SET status = 'running', attempts = attempts + $6::INTEGER, generation = generation + 1, \
                 lease_owner = $2::TEXT, \
                 lease_until = LEAST(clock_timestamp() + ($3::BIGINT * interval '1 second'), deadline), \
                 updated_at = clock_timestamp() \
             WHERE id = $1 AND environment = $4 AND generation = $5 \
               AND ((status = 'pending') OR (status = 'running' AND lease_until <= clock_timestamp())) \
             RETURNING lease_until",
        )
        .bind(candidate.id)
        .bind(lease_owner.to_string())
        .bind(i64::try_from(lease_seconds).map_err(|_| Error::Invalid("durée de bail hors limites".into()))?)
        .bind(self.environment.as_str())
        .bind(candidate.generation)
        .bind(if increments_attempt { 1_i32 } else { 0_i32 })
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        .ok_or_else(|| Error::Conflict("lease lost".into()))?;
            let new_generation = candidate
                .generation
                .checked_add(1)
                .ok_or(Error::ResourceLimit)?;
            let attempts =
                u8::try_from(candidate.attempts + if increments_attempt { 1_i32 } else { 0_i32 })
                    .map_err(|_| Error::ResourceLimit)?;
            Store::append_event(
            &mut *tx,
            candidate.project_id,
            "job.claimed",
            json!({ "job_id": candidate.id, "generation": new_generation, "status": "running", "attempts": attempts }),
        )
        .await?;
            set_worker_claim(&mut *tx, self.environment, false).await?;
            tx.commit().await.map_err(map_database_error)?;

            return Ok(Some(JobLease {
                job_id: candidate.id,
                project_id: candidate.project_id,
                actor_id: candidate.actor_id,
                environment: self.environment,
                source_revision: candidate.source_revision,
                payload,
                attempts,
                max_attempts: u8::try_from(candidate.max_attempts).map_err(|_| Error::Internal)?,
                generation: new_generation,
                lease_owner,
                lease_until,
                deadline: candidate.deadline,
                cancel_requested: false,
            }));
        }

        set_worker_claim(&mut *tx, self.environment, false).await?;
        tx.commit().await.map_err(map_database_error)?;
        Ok(None)
    }

    /// Renew a lease while preserving its generation and owner fence.
    pub async fn heartbeat_job(
        &self,
        lease: &JobLease,
        lease_seconds: u64,
    ) -> Result<kyro_domain::task::JobHeartbeat> {
        if lease.environment != self.environment
            || !(MIN_LEASE_SECONDS as u64..=MAX_LEASE_SECONDS as u64).contains(&lease_seconds)
        {
            return Err(Error::Invalid("heartbeat de job invalide".into()));
        }
        let mut tx = self.pool.begin().await.map_err(map_database_error)?;
        set_worker_claim(&mut *tx, self.environment, true).await?;
        let row = sqlx::query(
            "UPDATE jobs SET lease_until = LEAST(clock_timestamp() + ($5::BIGINT * interval '1 second'), deadline), \
                 updated_at = clock_timestamp() \
             WHERE id = $1 AND project_id = $2 AND environment = $3 AND status = 'running' \
               AND generation = $4 AND lease_owner = $6::TEXT \
               AND lease_until > clock_timestamp() AND deadline > clock_timestamp() \
             RETURNING lease_until, cancel_requested",
        )
        .bind(lease.job_id)
        .bind(lease.project_id)
        .bind(self.environment.as_str())
        .bind(lease.generation)
        .bind(i64::try_from(lease_seconds).map_err(|_| Error::ResourceLimit)?)
        .bind(lease.lease_owner.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        .ok_or_else(|| Error::Conflict("lease lost".into()))?;
        let heartbeat = kyro_domain::task::JobHeartbeat {
            lease_until: row.try_get("lease_until").map_err(map_database_error)?,
            cancel_requested: row
                .try_get("cancel_requested")
                .map_err(map_database_error)?,
        };
        set_worker_claim(&mut *tx, self.environment, false).await?;
        tx.commit().await.map_err(map_database_error)?;
        Ok(heartbeat)
    }

    /// Apply project changes and complete the job under one project→job lock
    /// order and one transaction. A crash cannot commit only half of this pair.
    pub async fn finish_apply_changes(&self, lease: &JobLease, changes: &ChangeSet) -> Result<Job> {
        let mut tx = self.begin_actor(lease.actor_id).await?;
        let current_revision = lock_project(&mut *tx, lease.project_id, &["execute"]).await?;
        let row = lock_job(&mut *tx, lease.project_id, lease.job_id).await?;
        ensure_current_lease(&row, lease)?;
        let persisted =
            decode_supported_payload(row.payload.clone()).map_err(|_| Error::Internal)?;
        let JobPayload::ApplyChanges {
            changes: persisted_changes,
        } = &persisted
        else {
            return Err(Error::Conflict("job payload changed before apply".into()));
        };
        if persisted != lease.payload || persisted_changes != changes {
            return Err(Error::Conflict("job payload changed before apply".into()));
        }
        let demand = grant_demand_for_row(&row, &persisted)?;
        let authorization = authorize_job_grants(
            &mut *tx,
            lease.actor_id,
            lease.project_id,
            &persisted,
            &demand,
        )
        .await;
        if matches!(&authorization, Err(Error::ResourceLimit)) {
            let updated = set_actor_job_status(
                &mut *tx,
                &row,
                JobStatus::Stale,
                Some(JobErrorCode::PermissionRevoked),
                None,
            )
            .await?;
            append_job_status_event(
                &mut *tx,
                lease.project_id,
                lease.job_id,
                lease.generation,
                JobStatus::Stale,
            )
            .await?;
            tx.commit().await.map_err(map_database_error)?;
            return updated.into_job();
        }
        authorization?;

        if row.cancel_requested {
            let updated = set_actor_job_status(
                &mut *tx,
                &row,
                JobStatus::Cancelled,
                Some(JobErrorCode::Cancelled),
                None,
            )
            .await?;
            append_job_status_event(
                &mut *tx,
                lease.project_id,
                lease.job_id,
                lease.generation,
                JobStatus::Cancelled,
            )
            .await?;
            tx.commit().await.map_err(map_database_error)?;
            return updated.into_job();
        }
        if current_revision != lease.source_revision {
            let updated = set_actor_job_status(
                &mut *tx,
                &row,
                JobStatus::Stale,
                Some(JobErrorCode::SourceStale),
                None,
            )
            .await?;
            append_job_status_event(
                &mut *tx,
                lease.project_id,
                lease.job_id,
                lease.generation,
                JobStatus::Stale,
            )
            .await?;
            tx.commit().await.map_err(map_database_error)?;
            return updated.into_job();
        }

        let applied = apply_changes_in(
            &mut *tx,
            lease.actor_id,
            lease.project_id,
            lease.source_revision,
            &format!("job:{}", lease.job_id),
            persisted_changes,
        )
        .await?;
        let result = JobResult::ApplyChanges {
            revision: applied.revision.revision,
        };
        let updated =
            set_actor_job_status(&mut *tx, &row, JobStatus::Succeeded, None, Some(result)).await?;
        append_job_status_event(
            &mut *tx,
            lease.project_id,
            lease.job_id,
            lease.generation,
            JobStatus::Succeeded,
        )
        .await?;
        tx.commit().await.map_err(map_database_error)?;
        updated.into_job()
    }

    /// Persist a model effect reference only after grants and the source revision
    /// still match. The model response itself is stored on the separately gated effect.
    pub async fn finish_model_job(
        &self,
        lease: &JobLease,
        effect_id: Uuid,
        effect_status: EffectStatus,
    ) -> Result<Job> {
        let mut tx = self.begin_actor(lease.actor_id).await?;
        let current_revision = lock_project(&mut *tx, lease.project_id, &["execute"]).await?;
        let row = lock_job(&mut *tx, lease.project_id, lease.job_id).await?;
        ensure_current_lease(&row, lease)?;
        let persisted =
            decode_supported_payload(row.payload.clone()).map_err(|_| Error::Internal)?;
        let JobPayload::ModelCall { .. } = &persisted else {
            return Err(Error::Conflict(
                "job payload changed before model finish".into(),
            ));
        };
        if persisted != lease.payload {
            return Err(Error::Conflict(
                "job payload changed before model finish".into(),
            ));
        }
        let demand = grant_demand_for_row(&row, &persisted)?;
        let authorization = authorize_job_grants(
            &mut *tx,
            lease.actor_id,
            lease.project_id,
            &persisted,
            &demand,
        )
        .await;
        if matches!(&authorization, Err(Error::ResourceLimit)) {
            let reference = Some(JobResult::ModelCall {
                effect_id,
                status: effect_status,
            });
            let updated = set_actor_job_status(
                &mut *tx,
                &row,
                JobStatus::Stale,
                Some(JobErrorCode::PermissionRevoked),
                reference,
            )
            .await?;
            append_job_status_event(
                &mut *tx,
                lease.project_id,
                lease.job_id,
                lease.generation,
                JobStatus::Stale,
            )
            .await?;
            tx.commit().await.map_err(map_database_error)?;
            return updated.into_job();
        }
        authorization?;
        if current_revision != lease.source_revision {
            let updated = set_actor_job_status(
                &mut *tx,
                &row,
                JobStatus::Stale,
                Some(JobErrorCode::SourceStale),
                None,
            )
            .await?;
            append_job_status_event(
                &mut *tx,
                lease.project_id,
                lease.job_id,
                lease.generation,
                JobStatus::Stale,
            )
            .await?;
            tx.commit().await.map_err(map_database_error)?;
            return updated.into_job();
        }

        let result = Some(JobResult::ModelCall {
            effect_id,
            status: effect_status,
        });
        let (job_status, error_code, visible_result) = if row.cancel_requested {
            (JobStatus::Cancelled, Some(JobErrorCode::Cancelled), None)
        } else {
            match effect_status {
                EffectStatus::Succeeded => (JobStatus::Succeeded, None, result),
                EffectStatus::Unknown | EffectStatus::Sending => (
                    JobStatus::Unknown,
                    Some(JobErrorCode::GatewayUnavailable),
                    result,
                ),
                EffectStatus::Failed => (
                    JobStatus::Failed,
                    Some(JobErrorCode::ExecutionFailed),
                    result,
                ),
                EffectStatus::Cancelled => {
                    (JobStatus::Cancelled, Some(JobErrorCode::Cancelled), result)
                }
                EffectStatus::Prepared => {
                    return Err(Error::Conflict(
                        "prepared effect cannot finish a job".into(),
                    ));
                }
            }
        };
        let updated =
            set_actor_job_status(&mut *tx, &row, job_status, error_code, visible_result).await?;
        append_job_status_event(
            &mut *tx,
            lease.project_id,
            lease.job_id,
            lease.generation,
            job_status,
        )
        .await?;
        tx.commit().await.map_err(map_database_error)?;
        updated.into_job()
    }

    /// Reconcile a synthetic unknown effect and close the reconciliation command
    /// atomically. The command source revision is not a financial fencing token;
    /// the target job's source and grants are checked by the gateway finalizer.
    pub async fn finish_effect_reconciliation_job(
        &self,
        lease: &JobLease,
        effect_id: Uuid,
        request: &ReconcileEffectRequest,
        validate_registry: impl FnOnce(&EffectIntent) -> Result<()> + Send,
    ) -> Result<Job> {
        if lease.environment != self.environment {
            return Err(Error::Invalid("environnement de job invalide".into()));
        }
        request.validate()?;
        let JobPayload::ReconcileEffect {
            effect_id: leased_effect_id,
            request: leased_request,
        } = &lease.payload
        else {
            return Err(Error::Invalid(
                "type de job de réconciliation invalide".into(),
            ));
        };
        if *leased_effect_id != effect_id || leased_request != request {
            return Err(Error::Conflict("reconciliation request changed".into()));
        }

        let mut tx = self.begin_actor(lease.actor_id).await?;
        // This is an unlocked hint only. Its policy requires the operator to
        // currently see the project through Budget or Manage authority.
        let target_job_id: Uuid =
            sqlx::query_scalar("SELECT job_id FROM effects WHERE id = $1 AND project_id = $2")
                .bind(effect_id)
                .bind(lease.project_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_database_error)?
                .ok_or(Error::NotFound)?;

        // The security-definer helper locks the project first and verifies that
        // the worker's accounting context is scoped to the command job.
        set_accounting_job_context(&mut *tx, lease.job_id).await?;
        let project = lock_project_for_worker_job(&mut *tx, lease.job_id, false)
            .await?
            .ok_or(Error::NotFound)?;
        if project.project_id != lease.project_id {
            return Err(Error::NotFound);
        }

        // The target ID came from a non-locking effect hint. Lock both jobs in a
        // stable order before any effect, reservation, or budget row is touched.
        let mut job_ids = [lease.job_id, target_job_id];
        job_ids.sort_unstable();
        let mut command_row = None;
        let mut target_row = None;
        for job_id in job_ids {
            set_accounting_job_context(&mut *tx, job_id).await?;
            let row = lock_accounting_job(&mut *tx, lease.project_id, job_id).await?;
            if job_id == lease.job_id {
                command_row = Some(row);
            } else {
                target_row = Some(row);
            }
        }
        let command_row = command_row.ok_or(Error::Internal)?;
        let target_row = target_row.ok_or(Error::Internal)?;
        ensure_current_lease(&command_row, lease)?;
        if target_row.id == command_row.id || target_row.project_id != lease.project_id {
            return Err(Error::Conflict("reconciliation target changed".into()));
        }
        let persisted =
            decode_supported_payload(command_row.payload.clone()).map_err(|_| Error::Internal)?;
        if !matches!(persisted, JobPayload::ReconcileEffect { effect_id: stored_effect, request: ref stored_request } if stored_effect == effect_id && stored_request == request)
        {
            return Err(Error::Conflict("reconciliation request changed".into()));
        }

        let now = utc_now(&mut *tx).await?;
        if command_row
            .lease_until
            .is_none_or(|lease_until| lease_until <= now)
        {
            return Err(Error::Conflict("lease lost".into()));
        }
        if command_row.cancel_requested {
            set_accounting_job_context(&mut *tx, lease.job_id).await?;
            let updated = set_worker_job_status(
                &mut *tx,
                &command_row,
                JobStatus::Cancelled,
                Some(JobErrorCode::Cancelled),
                None,
            )
            .await?;
            append_job_status_event(
                &mut *tx,
                lease.project_id,
                lease.job_id,
                lease.generation,
                JobStatus::Cancelled,
            )
            .await?;
            tx.commit().await.map_err(map_database_error)?;
            return updated.into_job();
        }
        if command_row.deadline <= now {
            set_accounting_job_context(&mut *tx, lease.job_id).await?;
            let updated = set_worker_job_status(
                &mut *tx,
                &command_row,
                JobStatus::Failed,
                Some(JobErrorCode::DeadlineExpired),
                None,
            )
            .await?;
            append_job_status_event(
                &mut *tx,
                lease.project_id,
                lease.job_id,
                lease.generation,
                JobStatus::Failed,
            )
            .await?;
            tx.commit().await.map_err(map_database_error)?;
            return updated.into_job();
        }

        set_accounting_job_context(&mut *tx, target_job_id).await?;
        let context = EffectReconciliationContext {
            project_id: lease.project_id,
            actor_id: lease.actor_id,
            effect_id,
            target_job_id,
            current_revision: project.current_revision,
        };
        let outcome = match crate::budget::reconcile_model_effect_in(
            &mut *tx,
            &context,
            request.clone(),
            validate_registry,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(Error::Forbidden | Error::NotFound | Error::Unauthorized) => {
                // The gateway performs the Budget|Manage check only after it
                // has acquired effect/reservation/budget locks. It guarantees
                // these denials happen before any accounting mutation.
                set_accounting_job_context(&mut *tx, lease.job_id).await?;
                let updated = set_worker_job_status(
                    &mut *tx,
                    &command_row,
                    JobStatus::Stale,
                    Some(JobErrorCode::PermissionRevoked),
                    None,
                )
                .await?;
                append_job_status_event(
                    &mut *tx,
                    lease.project_id,
                    lease.job_id,
                    lease.generation,
                    JobStatus::Stale,
                )
                .await?;
                tx.commit().await.map_err(map_database_error)?;
                return updated.into_job();
            }
            Err(error) => return Err(error),
        };
        if outcome.effect_id != effect_id
            || outcome.job_id != target_job_id
            || !matches!(
                outcome.status,
                EffectStatus::Succeeded | EffectStatus::Failed | EffectStatus::Cancelled
            )
        {
            return Err(Error::Internal);
        }

        set_accounting_job_context(&mut *tx, lease.job_id).await?;
        let updated = set_worker_job_status_before_deadline(
            &mut *tx,
            &command_row,
            JobStatus::Succeeded,
            None,
            Some(JobResult::ModelCall {
                effect_id,
                status: outcome.status,
            }),
        )
        .await?;
        append_job_status_event(
            &mut *tx,
            lease.project_id,
            lease.job_id,
            lease.generation,
            JobStatus::Succeeded,
        )
        .await?;
        tx.commit().await.map_err(map_database_error)?;
        updated.into_job()
    }

    /// Record a bounded worker failure. Internal retries use the stable job ID;
    /// a model effect is retried only when it was never sent or already succeeded.
    pub async fn fail_job(
        &self,
        lease: &JobLease,
        code: JobErrorCode,
        retryable_internal: bool,
    ) -> Result<Job> {
        if lease.environment != self.environment {
            return Err(Error::Invalid("environnement de job invalide".into()));
        }
        let mut tx = self.pool.begin().await.map_err(map_database_error)?;
        set_worker_claim(&mut *tx, self.environment, true).await?;
        let project = lock_project_for_worker_job(&mut *tx, lease.job_id, false)
            .await?
            .ok_or(Error::NotFound)?;
        if project.project_id != lease.project_id {
            return Err(Error::NotFound);
        }
        let row = sqlx::query_as::<_, JobRow>(
            "SELECT id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
                    max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
                    result, error_code, created_at, updated_at \
             FROM jobs WHERE id = $1 AND project_id = $2 AND environment = $3 FOR UPDATE",
        )
        .bind(lease.job_id)
        .bind(lease.project_id)
        .bind(self.environment.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        .ok_or(Error::NotFound)?;
        ensure_current_lease(&row, lease)?;

        let is_model = matches!(lease.payload, JobPayload::ModelCall { .. });
        let now = utc_now(&mut *tx).await?;
        let model_effect = if is_model {
            set_worker_claim(&mut *tx, self.environment, false).await?;
            set_actor_context(&mut *tx, row.actor_id, self.environment).await?;
            set_accounting_job_context(&mut *tx, row.id).await?;
            find_job_effect(&mut *tx, row.project_id, row.id).await?
        } else {
            None
        };

        if let Some(effect) = model_effect.as_ref() {
            match effect.status.as_str() {
                "sending" => {
                    mark_expired_sending_unknown_tx(&mut *tx, effect.id, row.id, row.project_id)
                        .await?;
                    set_queue_write_context(&mut *tx, self.environment).await?;
                    let updated = set_worker_job_status(
                        &mut *tx,
                        &row,
                        JobStatus::Unknown,
                        Some(JobErrorCode::GatewayUnavailable),
                        Some(JobResult::ModelCall {
                            effect_id: effect.id,
                            status: EffectStatus::Unknown,
                        }),
                    )
                    .await?;
                    append_job_status_event(
                        &mut *tx,
                        row.project_id,
                        row.id,
                        row.generation,
                        JobStatus::Unknown,
                    )
                    .await?;
                    set_worker_claim(&mut *tx, self.environment, false).await?;
                    tx.commit().await.map_err(map_database_error)?;
                    return updated.into_job();
                }
                "unknown" => {
                    set_queue_write_context(&mut *tx, self.environment).await?;
                    let updated = set_worker_job_status(
                        &mut *tx,
                        &row,
                        JobStatus::Unknown,
                        Some(JobErrorCode::GatewayUnavailable),
                        Some(JobResult::ModelCall {
                            effect_id: effect.id,
                            status: EffectStatus::Unknown,
                        }),
                    )
                    .await?;
                    append_job_status_event(
                        &mut *tx,
                        row.project_id,
                        row.id,
                        row.generation,
                        JobStatus::Unknown,
                    )
                    .await?;
                    set_worker_claim(&mut *tx, self.environment, false).await?;
                    tx.commit().await.map_err(map_database_error)?;
                    return updated.into_job();
                }
                "failed" | "cancelled" => {
                    let terminal = if effect.status == "cancelled" {
                        JobStatus::Cancelled
                    } else {
                        JobStatus::Failed
                    };
                    let terminal_code = if terminal == JobStatus::Cancelled {
                        JobErrorCode::Cancelled
                    } else {
                        JobErrorCode::ExecutionFailed
                    };
                    set_queue_write_context(&mut *tx, self.environment).await?;
                    let updated = set_worker_job_status(
                        &mut *tx,
                        &row,
                        terminal,
                        Some(terminal_code),
                        Some(JobResult::ModelCall {
                            effect_id: effect.id,
                            status: parse_effect_status(&effect.status).ok_or(Error::Internal)?,
                        }),
                    )
                    .await?;
                    append_job_status_event(
                        &mut *tx,
                        row.project_id,
                        row.id,
                        row.generation,
                        terminal,
                    )
                    .await?;
                    set_worker_claim(&mut *tx, self.environment, false).await?;
                    tx.commit().await.map_err(map_database_error)?;
                    return updated.into_job();
                }
                "prepared" | "succeeded" => {}
                _ => return Err(Error::Internal),
            }
        }

        let retryable_effect = model_effect
            .as_ref()
            .is_some_and(|effect| effect.status == "succeeded")
            && matches!(
                code,
                JobErrorCode::RetryableInternal | JobErrorCode::GatewayUnavailable
            );
        let retry = (retryable_internal || retryable_effect)
            && (row.attempts < row.max_attempts || retryable_effect)
            && row.deadline > now
            && !row.cancel_requested;
        set_queue_write_context(&mut *tx, self.environment).await?;

        if retry {
            let updated =
                set_worker_job_status(&mut *tx, &row, JobStatus::Pending, Some(code), None).await?;
            Store::append_event(
                &mut *tx,
                row.project_id,
                "job.retry_scheduled",
                json!({ "job_id": row.id, "generation": row.generation, "status": "pending", "attempts": row.attempts }),
            )
            .await?;
            set_worker_claim(&mut *tx, self.environment, false).await?;
            tx.commit().await.map_err(map_database_error)?;
            return updated.into_job();
        }

        let (terminal, terminal_code) = if row.cancel_requested || code == JobErrorCode::Cancelled {
            (JobStatus::Cancelled, JobErrorCode::Cancelled)
        } else if code == JobErrorCode::PermissionRevoked || code == JobErrorCode::SourceStale {
            (JobStatus::Stale, code)
        } else if row.deadline <= now {
            (JobStatus::Failed, JobErrorCode::DeadlineExpired)
        } else if row.attempts >= row.max_attempts {
            (JobStatus::Failed, JobErrorCode::AttemptsExceeded)
        } else {
            (JobStatus::Failed, code)
        };
        let result = model_effect
            .as_ref()
            .filter(|effect| effect.status == "succeeded")
            .map(|effect| JobResult::ModelCall {
                effect_id: effect.id,
                status: EffectStatus::Succeeded,
            });
        let updated =
            set_worker_job_status(&mut *tx, &row, terminal, Some(terminal_code), result).await?;
        if model_effect
            .as_ref()
            .is_some_and(|effect| effect.status == "prepared")
        {
            // The terminal job CAS is already locked; only this never-sent
            // effect can release its held reservation. Sending/unknown never do.
            set_worker_claim(&mut *tx, self.environment, false).await?;
            if !crate::budget::release_prepared_effect_accounting(&mut *tx, row.id, row.project_id)
                .await?
            {
                return Err(Error::Conflict(
                    "prepared effect changed during terminal failure".into(),
                ));
            }
            set_queue_write_context(&mut *tx, self.environment).await?;
        }
        append_job_status_event(&mut *tx, row.project_id, row.id, row.generation, terminal).await?;
        set_worker_claim(&mut *tx, self.environment, false).await?;
        tx.commit().await.map_err(map_database_error)?;
        updated.into_job()
    }
}

#[derive(FromRow)]
struct ExistingCommandRow {
    fingerprint: Vec<u8>,
    result: Value,
}

#[derive(FromRow)]
struct QueueCounts {
    queued: i64,
}

#[derive(FromRow)]
struct ClaimCandidate {
    id: Uuid,
    project_id: Uuid,
}

#[derive(FromRow)]
struct LockedProject {
    project_id: Uuid,
    current_revision: i64,
}

#[derive(FromRow)]
struct EffectTargetRow {
    job_id: Uuid,
}

#[derive(FromRow)]
struct EffectLookupRow {
    id: Uuid,
    status: String,
}

async fn fetch_job_row(
    conn: &mut PgConnection,
    project_id: Uuid,
    job_id: Uuid,
    environment: Environment,
) -> Result<JobRow> {
    sqlx::query_as::<_, JobRow>(
        "SELECT id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
             max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
             result, error_code, created_at, updated_at \
         FROM jobs WHERE project_id = $1 AND id = $2 AND environment = $3",
    )
    .bind(project_id)
    .bind(job_id)
    .bind(environment.as_str())
    .fetch_optional(conn)
    .await
    .map_err(map_database_error)?
    .ok_or(Error::NotFound)
}

async fn lock_project(conn: &mut PgConnection, project_id: Uuid, actions: &[&str]) -> Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "SELECT current_revision FROM public.kyro_lock_project_for_actor($1, $2)",
    )
    .bind(project_id)
    .bind(actions)
    .fetch_optional(conn)
    .await
    .map_err(map_database_error)?
    .ok_or(Error::NotFound)
}

async fn lock_project_for_worker_job(
    conn: &mut PgConnection,
    job_id: Uuid,
    skip_locked: bool,
) -> Result<Option<LockedProject>> {
    sqlx::query_as::<_, LockedProject>(
        "SELECT project_id, current_revision \
         FROM public.kyro_lock_project_for_job($1, $2)",
    )
    .bind(job_id)
    .bind(skip_locked)
    .fetch_optional(conn)
    .await
    .map_err(map_database_error)
}

async fn lock_accounting_job(
    conn: &mut PgConnection,
    project_id: Uuid,
    job_id: Uuid,
) -> Result<JobRow> {
    sqlx::query_as::<_, JobRow>(
        "SELECT id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
                max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
                result, error_code, created_at, updated_at \
         FROM jobs WHERE project_id = $1 AND id = $2 \
           AND environment = current_setting('kyro.environment', true) FOR UPDATE",
    )
    .bind(project_id)
    .bind(job_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_database_error)?
    .ok_or(Error::NotFound)
}

async fn lock_job(conn: &mut PgConnection, project_id: Uuid, job_id: Uuid) -> Result<JobRow> {
    sqlx::query_as::<_, JobRow>(
        "SELECT id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
                max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, \
                result, error_code, created_at, updated_at \
         FROM jobs WHERE project_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(project_id)
    .bind(job_id)
    .fetch_optional(conn)
    .await
    .map_err(map_database_error)?
    .ok_or(Error::NotFound)
}

async fn find_job_effect(
    conn: &mut PgConnection,
    project_id: Uuid,
    job_id: Uuid,
) -> Result<Option<EffectLookupRow>> {
    sqlx::query_as::<_, EffectLookupRow>(
        "SELECT id, status FROM effects WHERE project_id = $1 AND job_id = $2",
    )
    .bind(project_id)
    .bind(job_id)
    .fetch_optional(conn)
    .await
    .map_err(map_database_error)
}

async fn set_worker_claim(
    conn: &mut PgConnection,
    environment: Environment,
    enabled: bool,
) -> Result<()> {
    sqlx::query(
        "SELECT set_config('kyro.environment', $1, true), set_config('kyro.queue_claim', $2, true)",
    )
    .bind(environment.as_str())
    .bind(if enabled { "on" } else { "off" })
    .execute(&mut *conn)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn set_actor_context(
    conn: &mut PgConnection,
    actor_id: Uuid,
    environment: Environment,
) -> Result<()> {
    sqlx::query("SELECT set_config('kyro.queue_claim', 'off', true), set_config('kyro.actor_id', $1, true), set_config('kyro.environment', $2, true), set_config('kyro.accounting_job_id', '', true)")
        .bind(actor_id.to_string())
        .bind(environment.as_str())
        .execute(&mut *conn)
        .await
        .map_err(map_database_error)?;
    Ok(())
}

/// Enable access only to accounting rows linked to the already-locked job.
/// This is distinct from `kyro.queue_claim`, which grants cross-project job
/// traversal and is always turned off before entering an actor context.
pub(crate) async fn set_accounting_job_context(
    conn: &mut PgConnection,
    job_id: Uuid,
) -> Result<()> {
    sqlx::query("SELECT set_config('kyro.accounting_job_id', $1, true)")
        .bind(job_id.to_string())
        .execute(&mut *conn)
        .await
        .map_err(map_database_error)?;
    Ok(())
}

async fn set_queue_write_context(conn: &mut PgConnection, environment: Environment) -> Result<()> {
    sqlx::query("SELECT set_config('kyro.actor_id', '', true), set_config('kyro.environment', $1, true), set_config('kyro.queue_claim', 'on', true), set_config('kyro.accounting_job_id', '', true)")
        .bind(environment.as_str())
        .execute(&mut *conn)
        .await
        .map_err(map_database_error)?;
    Ok(())
}

async fn utc_now(conn: &mut PgConnection) -> Result<DateTime<Utc>> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(conn)
        .await
        .map_err(map_database_error)
}

async fn update_claim_candidate(
    conn: &mut PgConnection,
    row: &JobRow,
    status: JobStatus,
    error_code: Option<JobErrorCode>,
    result: Option<JobResult>,
) -> Result<()> {
    let result = result
        .map(|value| serde_json::to_value(value).map_err(|_| Error::Internal))
        .transpose()?;
    let changed = sqlx::query(
        "UPDATE jobs SET status = $4, error_code = $5, result = $6, lease_owner = NULL, \
             lease_until = NULL, updated_at = clock_timestamp() \
         WHERE id = $1 AND project_id = $2 AND environment = $3 AND generation = $7 AND status = $8",
    )
    .bind(row.id)
    .bind(row.project_id)
    .bind(&row.environment)
    .bind(status.as_str())
    .bind(error_code.map(JobErrorCode::as_str))
    .bind(result)
    .bind(row.generation)
    .bind(&row.status)
    .execute(&mut *conn)
    .await
    .map_err(map_database_error)?;
    if changed.rows_affected() != 1 {
        return Err(Error::Conflict("job changed during queue recovery".into()));
    }
    Ok(())
}

async fn set_actor_job_status(
    conn: &mut PgConnection,
    row: &JobRow,
    status: JobStatus,
    error_code: Option<JobErrorCode>,
    result: Option<JobResult>,
) -> Result<JobRow> {
    set_job_status(conn, row, status, error_code, result, true, true).await
}

async fn set_worker_job_status(
    conn: &mut PgConnection,
    row: &JobRow,
    status: JobStatus,
    error_code: Option<JobErrorCode>,
    result: Option<JobResult>,
) -> Result<JobRow> {
    set_job_status(conn, row, status, error_code, result, true, false).await
}

async fn set_worker_job_status_before_deadline(
    conn: &mut PgConnection,
    row: &JobRow,
    status: JobStatus,
    error_code: Option<JobErrorCode>,
    result: Option<JobResult>,
) -> Result<JobRow> {
    set_job_status(conn, row, status, error_code, result, true, true).await
}

async fn set_job_status(
    conn: &mut PgConnection,
    row: &JobRow,
    status: JobStatus,
    error_code: Option<JobErrorCode>,
    result: Option<JobResult>,
    require_live_lease: bool,
    require_unexpired_deadline: bool,
) -> Result<JobRow> {
    let result = result
        .map(|value| serde_json::to_value(value).map_err(|_| Error::Internal))
        .transpose()?;
    let lease_clause = match (require_live_lease, require_unexpired_deadline) {
        (true, true) => {
            "AND status = 'running' AND generation = $7 AND lease_owner = $8 \
             AND lease_until > clock_timestamp() AND deadline > clock_timestamp()"
        }
        (true, false) => {
            "AND status = 'running' AND generation = $7 AND lease_owner = $8 \
             AND lease_until > clock_timestamp()"
        }
        (false, _) => "AND generation = $7",
    };
    let query = format!(
        "UPDATE jobs SET status = $3, error_code = $4, result = $5, lease_owner = NULL, \
             lease_until = NULL, updated_at = clock_timestamp() \
         WHERE id = $1 AND project_id = $2 AND environment = $6 {lease_clause} \
         RETURNING id, project_id, actor_id, environment, source_revision, payload, status, attempts, \
             max_attempts, generation, lease_owner, lease_until, deadline, cancel_requested, result, \
             error_code, created_at, updated_at"
    );
    let mut query = sqlx::query_as::<_, JobRow>(&query)
        .bind(row.id)
        .bind(row.project_id)
        .bind(status.as_str())
        .bind(error_code.map(JobErrorCode::as_str))
        .bind(result)
        .bind(row.environment.as_str());
    // Keep stable bind indexes for both actor and worker terminal transitions.
    // The environment predicate is useful only for the service query, while the
    // actor-scoped query already has an RLS environment context.
    query = query.bind(row.generation);
    if require_live_lease {
        query = query.bind(row.lease_owner.as_deref().ok_or(Error::Internal)?);
    }
    let result = query
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_database_error)?
        .ok_or_else(|| Error::Conflict("lease lost".into()))?;
    Ok(result)
}

async fn append_job_status_event(
    conn: &mut PgConnection,
    project_id: Uuid,
    job_id: Uuid,
    generation: i64,
    status: JobStatus,
) -> Result<()> {
    let kind = match status {
        JobStatus::Succeeded => "job.succeeded",
        JobStatus::Failed => "job.failed",
        JobStatus::Cancelled => "job.cancelled",
        JobStatus::Unknown => "job.unknown",
        JobStatus::Stale => "job.stale",
        JobStatus::Pending => "job.retry_scheduled",
        JobStatus::Running => "job.claimed",
    };
    set_accounting_job_context(conn, job_id).await?;
    Store::append_event(
        conn,
        project_id,
        kind,
        json!({ "job_id": job_id, "generation": generation, "status": status.as_str() }),
    )
    .await?;
    Ok(())
}

fn ensure_current_lease(row: &JobRow, lease: &JobLease) -> Result<()> {
    let expected_owner = lease.lease_owner.to_string();
    if row.id != lease.job_id
        || row.project_id != lease.project_id
        || row.actor_id != lease.actor_id
        || row.environment != lease.environment.as_str()
        || row.status != JobStatus::Running.as_str()
        || row.generation != lease.generation
        || row.lease_owner.as_deref() != Some(expected_owner.as_str())
        || row.lease_until.is_none()
    {
        return Err(Error::Conflict("lease lost".into()));
    }
    Ok(())
}

fn decode_supported_payload(value: Value) -> std::result::Result<JobPayload, ()> {
    let payload: JobPayload = serde_json::from_value(value).map_err(|_| ())?;
    payload.validate_for_queue().map_err(|_| ())?;
    Ok(payload)
}

fn parse_effect_status(value: &str) -> Option<EffectStatus> {
    match value {
        "prepared" => Some(EffectStatus::Prepared),
        "sending" => Some(EffectStatus::Sending),
        "succeeded" => Some(EffectStatus::Succeeded),
        "failed" => Some(EffectStatus::Failed),
        "unknown" => Some(EffectStatus::Unknown),
        "cancelled" => Some(EffectStatus::Cancelled),
        _ => None,
    }
}

/// Shared project→job guard used by the gateway before preparing or sending an
/// effect. Settlement after a request intentionally uses its own accounting
/// service path and does not call this guard again.
pub(crate) async fn lock_validate_model_job_tx(
    store: &Store,
    conn: &mut PgConnection,
    context: &ModelEffectContext,
    request: &kyro_domain::model::ModelRequest,
) -> Result<()> {
    let current_revision = lock_project(conn, context.project_id, &["execute"]).await?;
    let row = lock_job(conn, context.project_id, context.job_id).await?;
    let lease_owner = context.lease_owner.to_string();
    let requested_job =
        decode_supported_payload(row.payload.clone()).map_err(|_| Error::Internal)?;
    let matches_request = matches!(
        &requested_job,
        JobPayload::ModelCall { request: stored } if stored == request
    );
    if matches_request {
        let demand = grant_demand_for_row(&row, &requested_job)?;
        match authorize_job_grants(
            conn,
            context.actor_id,
            context.project_id,
            &requested_job,
            &demand,
        )
        .await
        {
            Ok(()) => {}
            Err(Error::ResourceLimit) => return Err(Error::Forbidden),
            Err(error) => return Err(error),
        }
    } else {
        return Err(Error::Conflict("persisted model request changed".into()));
    }
    if current_revision != context.source_revision {
        return Err(Error::StaleRevision {
            expected: context.source_revision,
            current: current_revision,
        });
    }
    if row.actor_id != context.actor_id
        || row.environment != store.environment.as_str()
        || row.source_revision != context.source_revision
        || row.status != JobStatus::Running.as_str()
        || row.generation != context.generation
        || row.lease_owner.as_deref() != Some(lease_owner.as_str())
        || row
            .lease_until
            .as_ref()
            .is_none_or(|lease_until| lease_until < &context.lease_until)
        || row.cancel_requested
        || !matches_request
    {
        return Err(Error::Conflict(
            "model job lease is no longer current".into(),
        ));
    }
    let active: bool = sqlx::query_scalar(
        "SELECT lease_until > clock_timestamp() AND deadline > clock_timestamp() \
         FROM jobs WHERE id = $1 AND generation = $2",
    )
    .bind(context.job_id)
    .bind(context.generation)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_database_error)?
    .ok_or(Error::NotFound)?;
    if !active {
        return Err(Error::Conflict("model job lease expired".into()));
    }
    Ok(())
}

/// Mark an expired outbound intent uncertain without releasing its reservation.
/// The operation is idempotent and never transitions a prepared effect to sent.
pub(crate) async fn mark_expired_sending_unknown_tx(
    conn: &mut PgConnection,
    effect_id: Uuid,
    job_id: Uuid,
    project_id: Uuid,
) -> Result<()> {
    if let Some(generation) = sqlx::query_scalar::<_, i64>(
        "UPDATE effects SET status = 'unknown', updated_at = clock_timestamp() \
         WHERE id = $1 AND job_id = $2 AND project_id = $3 AND status = 'sending' \
         RETURNING generation",
    )
    .bind(effect_id)
    .bind(job_id)
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_database_error)?
    {
        Store::append_event(
            conn,
            project_id,
            "effect.unknown",
            json!({ "effect_id": effect_id, "job_id": job_id, "generation": generation, "status": "unknown" }),
        )
        .await?;
    }
    Ok(())
}

/// Finalize the already-locked target job after the model store reconciles its
/// accounting rows. The caller holds project, command, and target locks before
/// effect/reservation/budget locks; this helper does not acquire new locks.
pub(crate) async fn reconcile_model_effect_tx(
    conn: &mut PgConnection,
    context: &EffectReconciliationContext,
    outcome: &EffectReconcileOutcome,
) -> Result<()> {
    if !matches!(
        outcome.status,
        EffectStatus::Succeeded | EffectStatus::Failed | EffectStatus::Cancelled
    ) || outcome.effect_id != context.effect_id
        || outcome.job_id != context.target_job_id
    {
        return Err(Error::Conflict(
            "effect reconciliation is not terminal or does not match the target".into(),
        ));
    }

    let job = sqlx::query(
        "SELECT project_id, actor_id, source_revision, generation, deadline, cancel_requested, status, result, \
                payload, max_attempts, created_at \
         FROM jobs WHERE id = $1 AND project_id = $2 \
           AND environment = current_setting('kyro.environment', true) FOR UPDATE",
    )
    .bind(context.target_job_id)
    .bind(context.project_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_database_error)?
    .ok_or_else(|| Error::Conflict("reconciliation target changed".into()))?;
    let project_id: Uuid = job.try_get("project_id").map_err(map_database_error)?;
    let actor_id: Uuid = job.try_get("actor_id").map_err(map_database_error)?;
    let source_revision: i64 = job.try_get("source_revision").map_err(map_database_error)?;
    let generation: i64 = job.try_get("generation").map_err(map_database_error)?;
    let deadline: DateTime<Utc> = job.try_get("deadline").map_err(map_database_error)?;
    let cancel_requested: bool = job
        .try_get("cancel_requested")
        .map_err(map_database_error)?;
    let existing_status: String = job.try_get("status").map_err(map_database_error)?;
    let existing_result: Option<Value> = job.try_get("result").map_err(map_database_error)?;
    let payload: Value = job.try_get("payload").map_err(map_database_error)?;
    let max_attempts: i32 = job.try_get("max_attempts").map_err(map_database_error)?;
    let created_at: DateTime<Utc> = job.try_get("created_at").map_err(map_database_error)?;
    if project_id != context.project_id || generation != outcome.generation {
        return Err(Error::Conflict("target job generation changed".into()));
    }
    let expected_result = JobResult::ModelCall {
        effect_id: outcome.effect_id,
        status: outcome.status,
    };
    if existing_status != JobStatus::Unknown.as_str() {
        let prior_result = existing_result
            .map(serde_json::from_value::<JobResult>)
            .transpose()
            .map_err(|_| Error::Internal)?;
        if JobStatus::parse(&existing_status).is_some_and(JobStatus::is_terminal)
            && prior_result.as_ref() == Some(&expected_result)
        {
            // A second idempotent reconciliation command can observe the
            // already-integrated target without duplicating its event or write.
            return Ok(());
        }
        return Err(Error::Conflict("target job is already terminal".into()));
    }
    if existing_result
        .as_ref()
        .and_then(|value| value.get("effect_id"))
        .and_then(Value::as_str)
        != Some(outcome.effect_id.to_string().as_str())
    {
        return Err(Error::Conflict(
            "unknown target effect reference changed".into(),
        ));
    }

    // Financial reconciliation is retained even when the target may no longer
    // be integrated. Only the original business grants, source, TTL, and cancel
    // state decide the target job's terminal status.
    let (status, code) = if cancel_requested || outcome.status == EffectStatus::Cancelled {
        (JobStatus::Cancelled, Some(JobErrorCode::Cancelled))
    } else if deadline <= utc_now(conn).await? {
        (JobStatus::Failed, Some(JobErrorCode::DeadlineExpired))
    } else {
        let payload = decode_supported_payload(payload).map_err(|_| Error::Internal)?;
        if !matches!(payload, JobPayload::ModelCall { .. }) {
            return Err(Error::Conflict(
                "reconciliation target is no longer a model job".into(),
            ));
        }
        let demand = grant_demand_for_schedule(&payload, max_attempts, deadline, created_at)?;
        // Check the locked target author's current grants, then restore the
        // Budget operator before any target/event/command accounting writes.
        sqlx::query("SELECT set_config('kyro.actor_id', $1, true)")
            .bind(actor_id.to_string())
            .execute(&mut *conn)
            .await
            .map_err(map_database_error)?;
        let authorization = async {
            Store::authorize_demand_in(conn, actor_id, project_id, &["execute"], &demand).await?;
            Store::authorize_demand_in(conn, actor_id, project_id, &["model"], &demand).await
        }
        .await;
        sqlx::query("SELECT set_config('kyro.actor_id', $1, true), set_config('kyro.accounting_job_id', $2, true)")
            .bind(context.actor_id.to_string()).bind(context.target_job_id.to_string())
            .execute(&mut *conn).await.map_err(map_database_error)?;
        match authorization {
            Err(
                Error::Forbidden | Error::NotFound | Error::Unauthorized | Error::ResourceLimit,
            ) => (JobStatus::Stale, Some(JobErrorCode::PermissionRevoked)),
            Err(error) => return Err(error),
            Ok(()) if context.current_revision != source_revision => {
                (JobStatus::Stale, Some(JobErrorCode::SourceStale))
            }
            Ok(()) => match outcome.status {
                EffectStatus::Succeeded => (JobStatus::Succeeded, None),
                EffectStatus::Failed => (JobStatus::Failed, Some(JobErrorCode::ExecutionFailed)),
                EffectStatus::Cancelled => unreachable!("handled above"),
                EffectStatus::Prepared | EffectStatus::Sending | EffectStatus::Unknown => {
                    unreachable!("terminal statuses checked above")
                }
            },
        }
    };
    let result = serde_json::to_value(expected_result).map_err(|_| Error::Internal)?;
    let changed = sqlx::query(
        "UPDATE jobs SET status = $4, error_code = $5, result = $6, updated_at = clock_timestamp() \
         WHERE id = $1 AND project_id = $2 AND environment = current_setting('kyro.environment', true) \
           AND status = 'unknown' AND generation = $3 AND result->>'effect_id' = $7",
    )
    .bind(context.target_job_id)
    .bind(project_id)
    .bind(outcome.generation)
    .bind(status.as_str())
    .bind(code.map(JobErrorCode::as_str))
    .bind(result)
    .bind(outcome.effect_id.to_string())
    .execute(&mut *conn)
    .await
    .map_err(map_database_error)?;
    if changed.rows_affected() != 1 {
        return Err(Error::Conflict(
            "unknown target job changed before reconciliation".into(),
        ));
    }
    Store::append_event(
        conn,
        project_id,
        "job.reconciled",
        json!({ "job_id": context.target_job_id, "effect_id": outcome.effect_id, "generation": outcome.generation, "status": status.as_str() }),
    )
    .await?;
    Ok(())
}

fn validate_idempotency_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || !key.is_ascii()
        || key.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(Error::Invalid("clé d’idempotence invalide".into()));
    }
    Ok(())
}

fn empty_grant_demand() -> GrantDemand {
    GrantDemand {
        job_attempts: None,
        job_ttl_secs: None,
        model_input_bytes: None,
        model_output_tokens: None,
        changeset_operations: None,
    }
}

async fn authorize_job_grants(
    conn: &mut PgConnection,
    actor_id: Uuid,
    project_id: Uuid,
    payload: &JobPayload,
    demand: &GrantDemand,
) -> Result<()> {
    let actions: &[&str] = match payload {
        JobPayload::ApplyChanges { .. } => &["execute", "write"],
        JobPayload::ModelCall { .. } => &["execute", "model"],
        JobPayload::ReconcileEffect { .. } => {
            return Err(Error::Invalid(
                "les rapprochements ont une autorisation séparée".into(),
            ));
        }
    };
    for action in actions {
        Store::authorize_demand_in(conn, actor_id, project_id, &[*action], demand).await?;
    }
    Ok(())
}

fn grant_demand_for_job(payload: &JobPayload, attempts: u8, ttl_secs: u32) -> Result<GrantDemand> {
    let (model_input_bytes, model_output_tokens, changeset_operations) = match payload {
        JobPayload::ApplyChanges { changes } => (
            None,
            None,
            Some(u32::try_from(changes.operations.len()).map_err(|_| Error::ResourceLimit)?),
        ),
        JobPayload::ModelCall { request } => (
            Some(
                u32::try_from(
                    serde_json::to_vec(&request.input)
                        .map_err(|_| Error::Invalid("entrée modèle invalide".into()))?
                        .len(),
                )
                .map_err(|_| Error::ResourceLimit)?,
            ),
            Some(request.max_output_tokens),
            None,
        ),
        JobPayload::ReconcileEffect { .. } => {
            return Err(Error::Invalid(
                "un rapprochement ne porte pas de demande de ressources".into(),
            ));
        }
    };
    Ok(GrantDemand {
        job_attempts: Some(u32::from(attempts)),
        job_ttl_secs: Some(ttl_secs),
        model_input_bytes,
        model_output_tokens,
        changeset_operations,
    })
}

fn grant_demand_for_row(row: &JobRow, payload: &JobPayload) -> Result<GrantDemand> {
    grant_demand_for_schedule(payload, row.max_attempts, row.deadline, row.created_at)
}

fn grant_demand_for_schedule(
    payload: &JobPayload,
    max_attempts: i32,
    deadline: DateTime<Utc>,
    created_at: DateTime<Utc>,
) -> Result<GrantDemand> {
    let attempts = u8::try_from(max_attempts).map_err(|_| Error::Internal)?;
    let ttl_nanos = deadline
        .signed_duration_since(created_at)
        .num_nanoseconds()
        .ok_or(Error::ResourceLimit)?;
    if ttl_nanos <= 0 {
        return Err(Error::ResourceLimit);
    }
    let ttl_secs = u32::try_from((i128::from(ttl_nanos) + 999_999_999) / 1_000_000_000)
        .map_err(|_| Error::ResourceLimit)?;
    grant_demand_for_job(payload, attempts, ttl_secs)
}

fn effect_reconciliation_fingerprint(
    environment: Environment,
    actor_id: Uuid,
    project_id: Uuid,
    effect_id: Uuid,
    request: &ReconcileEffectRequest,
) -> Result<[u8; 32]> {
    let canonical = serde_json::to_vec(&EffectReconciliationFingerprint {
        environment: environment.as_str(),
        actor_id,
        project_id,
        effect_id,
        request,
    })
    .map_err(|_| Error::Invalid("commande de réconciliation invalide".into()))?;
    Ok(Sha256::digest(canonical).into())
}

fn parse_environment(raw: &str) -> Result<Environment> {
    match raw {
        "development" => Ok(Environment::Development),
        "production" => Ok(Environment::Production),
        _ => Err(Error::Internal),
    }
}

fn admission_fingerprint(
    environment: Environment,
    actor_id: Uuid,
    project_id: Uuid,
    source_revision: i64,
    payload: &JobPayload,
    max_attempts: Option<u8>,
    ttl_seconds: Option<u32>,
) -> Result<[u8; 32]> {
    let canonical = serde_json::to_vec(&AdmissionFingerprint {
        environment: environment.as_str(),
        actor_id,
        project_id,
        source_revision,
        payload,
        max_attempts,
        ttl_seconds,
    })
    .map_err(|_| Error::Invalid("commande de job invalide".into()))?;
    Ok(Sha256::digest(canonical).into())
}

pub(crate) fn map_database_error(error: sqlx::Error) -> Error {
    match error {
        sqlx::Error::Database(error)
            if matches!(error.code().as_deref(), Some("57014" | "55P03")) =>
        {
            Error::Unavailable
        }
        sqlx::Error::PoolClosed | sqlx::Error::PoolTimedOut => Error::Unavailable,
        _ => Error::Internal,
    }
}
