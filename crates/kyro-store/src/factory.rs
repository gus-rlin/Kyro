//! Non-sensitive factory metadata. Signature validation is supplied by the
//! factory/attestor; PostgreSQL keeps identity, epoch and lease fences atomic.
use crate::{Store, store::map_database_error};
use kyro_domain::{Environment, Error, Result, factory::valid_digest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

/// Loaded under project/job locks and fresh grants, never from a request body.
pub struct FactorySnapshot {
    pub spec: kyro_domain::spec::AppSpec,
    pub signed_catalogue: Value,
    pub catalogue_revision: u64,
    pub catalogue_digest: String,
    pub deadline: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactoryArtifactInput {
    pub id: Uuid,
    pub application_id: Uuid,
    pub image_digest: String,
    pub lock_digest: String,
    pub source_digest: String,
    pub evidence_digest: String,
    pub release_digest: String,
    pub signed_release: Value,
    pub signed_evidence: Value,
    pub source_manifest: Value,
}
impl FactoryArtifactInput {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_nil()
            || self.application_id.is_nil()
            || !self
                .image_digest
                .strip_prefix("sha256:")
                .is_some_and(valid_digest)
            || [
                &self.lock_digest,
                &self.source_digest,
                &self.evidence_digest,
                &self.release_digest,
            ]
            .iter()
            .any(|d| !valid_digest(d))
        {
            return Err(Error::Invalid("invalid factory artifact reference".into()));
        }
        for (object, max) in [
            (&self.signed_release, 16384),
            (&self.signed_evidence, 32768),
            (&self.source_manifest, 524288),
        ] {
            if !object.is_object()
                || serde_json::to_vec(object)
                    .map_err(|_| Error::Internal)?
                    .len()
                    > max
            {
                return Err(Error::ResourceLimit);
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct FactoryArtifact {
    pub project_id: Uuid,
    pub job_id: Uuid,
    pub source_revision: i64,
    pub environment: Environment,
    pub artifact: FactoryArtifactInput,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip)]
    pub job_generation: i64,
    #[serde(skip)]
    pub lease_owner: Uuid,
}
#[derive(FromRow)]
struct ArtifactRow {
    id: Uuid,
    project_id: Uuid,
    job_id: Uuid,
    job_generation: i64,
    lease_owner: Uuid,
    application_id: Uuid,
    environment: String,
    source_revision: i64,
    image_digest: String,
    lock_digest: String,
    source_digest: String,
    evidence_digest: String,
    release_digest: String,
    signed_release: Value,
    signed_evidence: Value,
    source_manifest: Value,
    created_at: chrono::DateTime<chrono::Utc>,
}
impl Store {
    /// Administrative publication only: API/worker roles have SELECT alone.
    /// The table lock serializes the initial publication as well as successors.
    /// Cryptographic/history validation stays with the operator's factory role.
    pub async fn publish_factory_catalogue<F>(
        &self,
        revision: u64,
        hash: &str,
        body: &Value,
        validate: F,
    ) -> Result<()>
    where
        F: FnOnce(Option<&Value>) -> Result<()>,
    {
        if revision == 0
            || revision > i64::MAX as u64
            || !valid_digest(hash)
            || !body.is_object()
            || serde_json::to_vec(body).map_err(|_| Error::Internal)?.len() > 8388608
        {
            return Err(Error::Invalid("invalid catalogue publication".into()));
        }
        let mut tx = self.pool.begin().await.map_err(map_database_error)?;
        sqlx::query("LOCK TABLE public.factory_catalogue_state IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(map_database_error)?;
        let previous: Option<(i64, Value)> = sqlx::query_as(
            "SELECT revision,signed_catalogue FROM public.factory_catalogue_state WHERE singleton",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?;
        if previous
            .as_ref()
            .is_some_and(|(old, _)| *old >= revision as i64)
        {
            return Err(Error::Conflict("catalogue revision must advance".into()));
        }
        validate(previous.as_ref().map(|(_, body)| body))?;
        sqlx::query("INSERT INTO public.factory_catalogue_state(singleton,revision,catalogue_digest,signed_catalogue) VALUES(TRUE,$1,$2,$3) ON CONFLICT(singleton) DO UPDATE SET revision=EXCLUDED.revision,catalogue_digest=EXCLUDED.catalogue_digest,signed_catalogue=EXCLUDED.signed_catalogue,updated_at=clock_timestamp()")
            .bind(revision as i64).bind(hash).bind(body).execute(&mut *tx).await.map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;
        Ok(())
    }

    pub async fn get_factory_artifact(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        artifact_id: Uuid,
    ) -> Result<FactoryArtifact> {
        let mut tx = self.begin_actor(actor_id).await?;
        let artifact = self
            .get_factory_artifact_in(&mut tx, actor_id, project_id, artifact_id)
            .await?;
        tx.commit().await.map_err(map_database_error)?;
        Ok(artifact)
    }
    /// Read a protected artifact without nesting a transaction inside a locked transition.
    pub async fn get_factory_artifact_in(
        &self,
        conn: &mut PgConnection,
        actor_id: Uuid,
        project_id: Uuid,
        artifact_id: Uuid,
    ) -> Result<FactoryArtifact> {
        Self::authorize_demand_in(
            &mut *conn,
            actor_id,
            project_id,
            &["read"],
            &kyro_domain::identity::GrantDemand {
                job_attempts: None,
                job_ttl_secs: None,
                model_input_bytes: None,
                model_output_tokens: None,
                changeset_operations: None,
            },
        )
        .await?;
        let row=sqlx::query_as::<_,ArtifactRow>("SELECT id,project_id,job_id,job_generation,lease_owner,application_id,environment,source_revision,image_digest,lock_digest,source_digest,evidence_digest,release_digest,signed_release,signed_evidence,source_manifest,created_at FROM factory_artifacts WHERE project_id=$1 AND id=$2 AND environment=$3")
            .bind(project_id).bind(artifact_id).bind(self.environment.as_str()).fetch_optional(&mut *conn).await.map_err(map_database_error)?.ok_or(Error::NotFound)?;
        let artifact = FactoryArtifactInput {
            id: row.id,
            application_id: row.application_id,
            image_digest: row.image_digest,
            lock_digest: row.lock_digest,
            source_digest: row.source_digest,
            evidence_digest: row.evidence_digest,
            release_digest: row.release_digest,
            signed_release: row.signed_release,
            signed_evidence: row.signed_evidence,
            source_manifest: row.source_manifest,
        };
        artifact.validate()?;
        Ok(FactoryArtifact {
            project_id: row.project_id,
            job_id: row.job_id,
            job_generation: row.job_generation,
            lease_owner: row.lease_owner,
            source_revision: row.source_revision,
            environment: match row.environment.as_str() {
                "development" => Environment::Development,
                "production" => Environment::Production,
                _ => return Err(Error::Internal),
            },
            artifact,
            created_at: row.created_at,
        })
    }
    /// The body is operator-owned. Callers must verify its signature, revision
    /// and canonical digest against their pinned trust before resolving a lock.
    pub async fn factory_catalogue(&self) -> Result<(u64, String, Value)> {
        let row:Option<(i64,String,Value)>=sqlx::query_as("SELECT revision,catalogue_digest,signed_catalogue FROM factory_catalogue_state WHERE singleton")
            .fetch_optional(&self.pool).await.map_err(map_database_error)?;
        let (revision, digest, body) = row.ok_or(Error::Unavailable)?;
        Ok((
            u64::try_from(revision).map_err(|_| Error::Internal)?,
            digest,
            body,
        ))
    }
}
