use crate::{Store, store::map_database_error};
use kyro_domain::{Error, Result, model::ModelEffectContext};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

#[derive(Serialize)]
pub struct ChatChunk {
    pub index: i64,
    pub text: String,
}

impl Store {
    pub async fn chat_effect_id(
        &self,
        actor: Uuid,
        project: Uuid,
        job: Uuid,
    ) -> Result<Option<Uuid>> {
        let mut tx = self.begin_actor(actor).await?;
        Store::authorize_in(&mut tx, actor, project, "read").await?;
        let id=sqlx::query_scalar("SELECT e.id FROM public.effects e JOIN public.jobs j ON j.id=e.job_id AND j.project_id=e.project_id WHERE e.project_id=$1 AND e.job_id=$2 AND j.actor_id=$3 AND j.environment=public.kyro_environment()")
            .bind(project).bind(job).bind(actor).fetch_optional(&mut *tx).await.map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;
        Ok(id)
    }
    pub async fn read_chat_chunks(
        &self,
        actor: Uuid,
        project: Uuid,
        job: Uuid,
        generation: i64,
        after: i64,
    ) -> Result<Vec<ChatChunk>> {
        let mut tx = self.begin_actor(actor).await?;
        Store::authorize_in(&mut tx, actor, project, "read").await?;
        let rows = sqlx::query("SELECT chunk_index, text FROM public.chat_stream_chunks WHERE project_id=$1 AND job_id=$2 AND generation=$3 AND chunk_index>$4 ORDER BY chunk_index LIMIT 64")
            .bind(project).bind(job).bind(generation).bind(after).fetch_all(&mut *tx).await.map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;
        rows.iter()
            .map(|row| {
                Ok(ChatChunk {
                    index: row.try_get("chunk_index").map_err(map_database_error)?,
                    text: row.try_get("text").map_err(map_database_error)?,
                })
            })
            .collect()
    }

    pub(crate) async fn chat_active(
        &self,
        context: &ModelEffectContext,
        effect: Uuid,
    ) -> Result<()> {
        let mut tx = self.begin_actor(context.actor_id).await?;
        let active: bool =
            sqlx::query_scalar("SELECT public.kyro_chat_lease_active($1,$2,$3,$4,$5)")
                .bind(context.project_id)
                .bind(context.job_id)
                .bind(effect)
                .bind(context.generation)
                .bind(context.lease_owner)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;
        if active {
            Ok(())
        } else {
            Err(Error::Conflict("chat lease inactive or cancelled".into()))
        }
    }

    pub(crate) async fn persist_chat_delta(
        &self,
        context: &ModelEffectContext,
        effect: Uuid,
        text: &str,
    ) -> Result<()> {
        if text.is_empty() || text.len() > 4096 {
            return Err(Error::ResourceLimit);
        }
        let mut tx = self.begin_actor(context.actor_id).await?;
        // Trigger takes locks before validating lease, bounds and monotonically increasing index.
        // A job has one publisher; racing/stale publishers are rejected by the trigger.
        sqlx::query("INSERT INTO public.chat_stream_chunks (project_id,job_id,effect_id,generation,lease_owner,chunk_index,text) VALUES ($1,$2,$3,$4,$5,(SELECT COALESCE(MAX(chunk_index),0)+1 FROM public.chat_stream_chunks WHERE job_id=$2 AND generation=$4),$6)")
            .bind(context.project_id).bind(context.job_id).bind(effect).bind(context.generation).bind(context.lease_owner).bind(text)
            .execute(&mut *tx).await.map_err(map_database_error)?;
        tx.commit().await.map_err(map_database_error)?;
        Ok(())
    }
}
