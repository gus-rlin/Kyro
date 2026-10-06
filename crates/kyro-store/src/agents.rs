//! Actor-scoped P3 persistence. All coordinator transitions lock project before run.
use crate::{Store, store::map_database_error};
use kyro_domain::{
    Error, Result,
    agents::{Run, RunStatus},
    spec::AppSpec,
};
use serde_json::Value;
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

impl Store {
    pub async fn close_revoked_agent(&self, id: Uuid) -> Result<bool> {
        let mut tx = self.begin_actor(Uuid::nil()).await?;
        let closed = sqlx::query_scalar("SELECT public.kyro_block_revoked_agent_run($1)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;
        Ok(closed)
    }
    pub async fn get_agent_run(&self, actor: Uuid, project: Uuid, id: Uuid) -> Result<Run> {
        let mut tx = self.begin_actor(actor).await?;
        Self::authorize_in(&mut tx, actor, project, "read").await?;
        let body: Value = sqlx::query_scalar(
            "SELECT state FROM agent_runs WHERE id=$1 AND project_id=$2 AND environment=$3",
        )
        .bind(id)
        .bind(project)
        .bind(self.environment.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?
        .ok_or(Error::NotFound)?;
        serde_json::from_value(body).map_err(|_| Error::Internal)
    }
    pub async fn list_agent_runs(&self, actor: Uuid, project: Uuid) -> Result<Vec<Run>> {
        let mut tx = self.begin_actor(actor).await?;
        Self::authorize_in(&mut tx, actor, project, "read").await?;
        let bodies:Vec<Value>=sqlx::query_scalar("SELECT state FROM agent_runs WHERE project_id=$1 AND environment=$2 ORDER BY updated_at DESC LIMIT 32")
            .bind(project).bind(self.environment.as_str()).fetch_all(&mut *tx).await.map_err(map_database_error)?;
        bodies
            .into_iter()
            .map(|b| serde_json::from_value(b).map_err(|_| Error::Internal))
            .collect()
    }
    pub async fn agent_history(
        &self,
        actor: Uuid,
        project: Uuid,
        id: Uuid,
        before: Option<i64>,
    ) -> Result<Vec<Run>> {
        let mut tx = self.begin_actor(actor).await?;
        Self::authorize_in(&mut tx, actor, project, "read").await?;
        let bodies:Vec<Value>=sqlx::query_scalar("SELECT state FROM agent_history WHERE run_id=$1 AND project_id=$2 AND version<$3 ORDER BY version DESC LIMIT 16")
            .bind(id).bind(project).bind(before.unwrap_or(i64::MAX)).fetch_all(&mut *tx).await.map_err(map_database_error)?;
        bodies
            .into_iter()
            .map(|b| serde_json::from_value(b).map_err(|_| Error::Internal))
            .collect()
    }
    pub async fn due_agent_runs(&self) -> Result<Vec<(Uuid, Uuid, Uuid)>> {
        let mut tx = self.begin_actor(Uuid::nil()).await?;
        let runs =
            sqlx::query_as("SELECT id,project_id,actor_id FROM public.kyro_due_agent_runs()")
                .fetch_all(&mut *tx)
                .await
                .map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;
        Ok(runs)
    }
    pub async fn begin_agent(
        &self,
        actor: Uuid,
        project: Uuid,
        id: Uuid,
        version: Option<i64>,
    ) -> Result<(Transaction<'static, Postgres>, Run, i64, AppSpec)> {
        let mut tx = self.begin_actor(actor).await?;
        let current:i64=sqlx::query_scalar("SELECT current_revision FROM public.kyro_lock_project_for_actor($1,ARRAY['execute']::TEXT[])")
            .bind(project).fetch_optional(&mut *tx).await.map_err(map_database_error)?.ok_or(Error::Forbidden)?;
        Self::authorize_in(&mut tx, actor, project, "read").await?;
        let body:Value=sqlx::query_scalar("SELECT state FROM agent_runs WHERE id=$1 AND project_id=$2 AND environment=$3 FOR UPDATE")
            .bind(id).bind(project).bind(self.environment.as_str()).fetch_optional(&mut *tx).await.map_err(map_database_error)?.ok_or(Error::NotFound)?;
        let run: Run = serde_json::from_value(body).map_err(|_| Error::Internal)?;
        if run.actor_id != actor {
            return Err(Error::Forbidden);
        }
        if version.is_some_and(|v| v != run.version) {
            return Err(Error::Conflict("plan_version_changed".into()));
        }
        let body: Value = sqlx::query_scalar(
            "SELECT spec FROM app_revisions WHERE project_id=$1 AND revision=$2",
        )
        .bind(project)
        .bind(current)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_database_error)?;
        let spec = serde_json::from_value(body).map_err(|_| Error::Internal)?;
        Ok((tx, run, current, spec))
    }
    pub async fn create_agent_in(
        &self,
        conn: &mut PgConnection,
        run: &Run,
        key: &str,
        fingerprint: &str,
    ) -> Result<Run> {
        if key.is_empty()
            || key.len() > 200
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(Error::Invalid("invalid_idempotency_key".into()));
        }
        let existing:Option<(String,Value)>=sqlx::query_as("SELECT fingerprint,state FROM agent_runs WHERE project_id=$1 AND environment=$2 AND idempotency_key=$3")
            .bind(run.project_id).bind(self.environment.as_str()).bind(key).fetch_optional(&mut *conn).await.map_err(map_database_error)?;
        if let Some((old, body)) = existing {
            if old != fingerprint {
                return Err(Error::IdempotencyConflict);
            }
            let run: Run = serde_json::from_value(body).map_err(|_| Error::Internal)?;
            if run.actor_id != crate::agents::actor_in(conn).await? {
                return Err(Error::Forbidden);
            }
            return Ok(run);
        }
        sqlx::query("INSERT INTO agent_runs(id,project_id,actor_id,environment,idempotency_key,fingerprint,version,state,active) VALUES($1,$2,$3,$4,$5,$6,$7,$8,TRUE)")
            .bind(run.id).bind(run.project_id).bind(run.actor_id).bind(self.environment.as_str()).bind(key).bind(fingerprint).bind(run.version)
            .bind(serde_json::to_value(run).map_err(|_|Error::Internal)?).execute(conn).await.map_err(map_database_error)?;
        Ok(run.clone())
    }
    pub async fn save_agent_in(conn: &mut PgConnection, run: &mut Run) -> Result<()> {
        let previous = run.version;
        run.version = previous.checked_add(1).ok_or(Error::ResourceLimit)?;
        let body = serde_json::to_value(&run).map_err(|_| Error::Internal)?;
        if serde_json::to_vec(&body)
            .map_err(|_| Error::Internal)?
            .len()
            > 1_900_000
        {
            return Err(Error::ResourceLimit);
        }
        let result=sqlx::query("UPDATE agent_runs SET state=$3,version=$4,active=$5,updated_at=clock_timestamp() WHERE id=$1 AND version=$2")
            .bind(run.id).bind(previous).bind(body).bind(run.version)
            .bind(!run.status.terminal() && run.status!=RunStatus::Planned).execute(&mut *conn).await.map_err(map_database_error)?;
        if result.rows_affected() != 1 {
            return Err(Error::Conflict("plan_version_changed".into()));
        }
        sqlx::query("SELECT public.kyro_append_agent_event($1,$2)")
            .bind(run.id)
            .bind(run.version)
            .execute(conn)
            .await
            .map_err(map_database_error)?;
        Ok(())
    }
    pub async fn cancel_agent_jobs_in(conn: &mut PgConnection, run: &Run) -> Result<()> {
        let mut ids: Vec<Uuid> = run
            .calls
            .iter()
            .filter(|c| c.epoch == run.epoch)
            .map(|c| c.job_id)
            .collect();
        if let Some(id) = run.build_job_id {
            ids.push(id);
        }
        sqlx::query("UPDATE jobs SET cancel_requested=TRUE WHERE project_id=$1 AND id=ANY($2) AND status IN ('pending','running')")
            .bind(run.project_id).bind(ids).execute(conn).await.map_err(map_database_error)?;
        Ok(())
    }
}
async fn actor_in(conn: &mut PgConnection) -> Result<Uuid> {
    sqlx::query_scalar("SELECT public.kyro_actor_id()")
        .fetch_one(conn)
        .await
        .map_err(map_database_error)
}
