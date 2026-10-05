//! Durable gateway port for application jobs. No socket opens while a database
//! transaction is held. Uncertain sends retain both their intent and reservation.
use super::*;
use kyro_domain::{Error as DomainError, Result as DomainResult, model::*};

pub(super) struct AppModelStore<'a> {
    pub core: &'a AppCore,
    pub original: Actor,
    pub worker: Actor,
    pub claim: JobClaim,
    pub context: ModelEffectContext,
    pub request_id: Uuid,
    pub call_key: String,
    pub bindings: Vec<SourceBinding>,
    pub policy: DataPolicy,
    pub component_id: String,
    pub roles: BTreeSet<String>,
}
impl AppModelStore<'_> {
    async fn checked(&self, context: &ModelEffectContext) -> DomainResult<AppTx> {
        if context != &self.context
            || context.lease_until <= Utc::now()
            || context.deadline <= Utc::now()
        {
            return Err(DomainError::Forbidden);
        }
        let mut tx = self
            .core
            .begin(self.original.clone())
            .await
            .map_err(domain_error)?;
        tx.require_operation(&self.component_id, "ai.request")
            .map_err(domain_error)?;
        if self.roles.is_disjoint(tx.actor().roles()) {
            return Err(DomainError::Forbidden);
        }
        let request_live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_ai_requests WHERE id=$1 AND job_id=$2 AND expires_at>clock_timestamp() AND state IN ('queued','unknown'))").bind(self.request_id).bind(self.claim.id).fetch_one(tx.conn()).await.map_err(|_|DomainError::Internal)?;
        if !request_live {
            return Err(DomainError::Forbidden);
        }
        tx.revalidate_worker(self.worker.clone())
            .await
            .map_err(domain_error)?;
        let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_jobs WHERE id=$1 AND state='leased' AND lease_id=$2 AND generation=$3 AND lease_owner=$4 AND lease_until>clock_timestamp() AND deadline>clock_timestamp())").bind(self.claim.id).bind(self.claim.lease_id).bind(self.claim.generation).bind(self.worker.principal_id()).fetch_one(tx.conn()).await.map_err(|_|DomainError::Internal)?;
        if !live {
            return Err(DomainError::Conflict("stale_application_job".into()));
        }
        validate_bindings(&mut tx, &self.bindings)
            .await
            .map_err(domain_error)?;
        Ok(tx)
    }
    async fn transition(
        &self,
        context: &ModelEffectContext,
        id: Uuid,
        status: &str,
        failure: Option<ModelFailureCode>,
        release: bool,
    ) -> DomainResult<()> {
        let mut tx = self.checked(context).await?;
        tx.lock_record_key("ai.effect", id)
            .await
            .map_err(domain_error)?;
        let r=sqlx::query("SELECT status,reserved_units,reserved_tokens,reservation_status FROM app_ai_effects WHERE id=$1 AND request_id=$2 AND call_key=$3 FOR UPDATE").bind(id).bind(self.request_id).bind(&self.call_key).fetch_optional(tx.conn()).await.map_err(|_|DomainError::Internal)?.ok_or(DomainError::NotFound)?;
        let prior: String = r.try_get("status").map_err(|_| DomainError::Internal)?;
        if prior == status {
            return tx.commit().await.map_err(domain_error);
        }
        if (release && prior != "prepared" && !(status == "failed" && prior == "sending"))
            || (!release && prior != "sending")
        {
            return Err(DomainError::Conflict(
                "invalid_application_effect_transition".into(),
            ));
        }
        if release
            && r.try_get::<String, _>("reservation_status")
                .map_err(|_| DomainError::Internal)?
                == "held"
        {
            tx.release_quota(
                "ai_budget_units",
                r.try_get("reserved_units")
                    .map_err(|_| DomainError::Internal)?,
            )
            .await
            .map_err(domain_error)?;
            tx.release_quota(
                "ai_tokens",
                r.try_get("reserved_tokens")
                    .map_err(|_| DomainError::Internal)?,
            )
            .await
            .map_err(domain_error)?;
        }
        sqlx::query("UPDATE app_ai_effects SET status=$2,failure_code=$3,reservation_status=CASE WHEN $4 THEN 'released' ELSE reservation_status END,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(status).bind(failure.map(|f|serde_json::to_value(f).unwrap_or(Value::Null).as_str().unwrap_or("persistence_uncertain").to_owned())).bind(release).execute(tx.conn()).await.map_err(|_|DomainError::Internal)?;
        tx.commit().await.map_err(domain_error)
    }
}
impl ModelEffectStore for AppModelStore<'_> {
    async fn prepare_model_effect(
        &self,
        p: ModelEffectPreparation,
    ) -> DomainResult<PreparedModelEffect> {
        let mut tx = self.checked(&p.context).await?;
        let id = crate::governance::stable_id(
            "ai-effect",
            &format!("{}:{}", self.request_id, self.call_key),
        );
        tx.lock_record_key("ai.effect", id)
            .await
            .map_err(domain_error)?;
        let existing = sqlx::query(
            "SELECT intent,fingerprint,status,response FROM app_ai_effects WHERE id=$1",
        )
        .bind(id)
        .fetch_optional(tx.conn())
        .await
        .map_err(|_| DomainError::Internal)?;
        if let Some(row) = existing {
            if row
                .try_get::<Vec<u8>, _>("fingerprint")
                .map_err(|_| DomainError::Internal)?
                != p.fingerprint
            {
                return Err(DomainError::IdempotencyConflict);
            }
            let intent: EffectIntent =
                serde_json::from_value(row.try_get("intent").map_err(|_| DomainError::Internal)?)
                    .map_err(|_| DomainError::Internal)?;
            let status: EffectStatus = serde_json::from_value(json!(
                row.try_get::<String, _>("status")
                    .map_err(|_| DomainError::Internal)?
            ))
            .map_err(|_| DomainError::Internal)?;
            let response: Option<Value> =
                row.try_get("response").map_err(|_| DomainError::Internal)?;
            let response = response
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| DomainError::Internal)?;
            tx.commit().await.map_err(domain_error)?;
            return Ok(PreparedModelEffect {
                intent,
                status,
                data_policy: self.policy.clone(),
                existing_response: response,
            });
        }
        if !p.allow_new_effect {
            return Err(DomainError::Unavailable);
        }
        self.policy.authorize_request(
            &p.request,
            p.input_bytes,
            p.conservative_input_tokens,
            p.registration.retention_seconds,
        )?;
        let token_reservation =
            i64::from(p.conservative_input_tokens) + i64::from(p.request.max_output_tokens);
        tx.reserve_quota("ai_budget_units", p.reservation_units)
            .await
            .map_err(domain_error)?;
        tx.reserve_quota("ai_tokens", token_reservation)
            .await
            .map_err(domain_error)?;
        let intent = EffectIntent {
            id,
            project_id: tx.actor().application_id(),
            job_id: self.claim.id,
            destination_id: p.request.destination_id.clone(),
            fingerprint: p.fingerprint,
            reservation_id: id,
            reserved_units: p.reservation_units,
            request_purpose: p.request.input.purpose,
            request_categories: p.request.input.categories.clone(),
            input_bytes: p.input_bytes,
            conservative_input_tokens: p.conservative_input_tokens,
            max_output_tokens: p.request.max_output_tokens,
            max_response_bytes: self.policy.limits.max_response_bytes,
            deadline_ms: p.request.deadline_ms,
            registration: p.registration,
        };
        sqlx::query("INSERT INTO app_ai_effects(tenant_id,principal_id,id,request_id,call_key,generation,intent,fingerprint,status,reserved_units,reserved_tokens) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'prepared',$9,$10)").bind(tx.actor().tenant_id()).bind(tx.actor().principal_id()).bind(id).bind(self.request_id).bind(&self.call_key).bind(self.claim.generation).bind(serde_json::to_value(&intent).map_err(|_|DomainError::Internal)?).bind(p.fingerprint.to_vec()).bind(p.reservation_units).bind(token_reservation).execute(tx.conn()).await.map_err(|_|DomainError::Internal)?;
        tx.audit("B150","ai.reserve",Some(id),json!({"request_id":self.request_id,"units":p.reservation_units,"tokens":token_reservation,"registration":intent.registration})).await.map_err(domain_error)?;
        tx.commit().await.map_err(domain_error)?;
        Ok(PreparedModelEffect {
            intent,
            status: EffectStatus::Prepared,
            data_policy: self.policy.clone(),
            existing_response: None,
        })
    }
    async fn mark_sending(
        &self,
        context: &ModelEffectContext,
        id: Uuid,
        request: &ModelRequest,
    ) -> DomainResult<()> {
        let mut tx = self.checked(context).await?;
        tx.lock_record_key("ai.effect", id)
            .await
            .map_err(domain_error)?;
        let r=sqlx::query("SELECT intent FROM app_ai_effects WHERE id=$1 AND request_id=$2 AND call_key=$3 AND status='prepared' FOR UPDATE").bind(id).bind(self.request_id).bind(&self.call_key).fetch_optional(tx.conn()).await.map_err(|_|DomainError::Internal)?.ok_or(DomainError::Conflict("effect_already_sent".into()))?;
        let intent: EffectIntent =
            serde_json::from_value(r.try_get("intent").map_err(|_| DomainError::Internal)?)
                .map_err(|_| DomainError::Internal)?;
        self.policy.authorize_request(
            request,
            intent.input_bytes,
            intent.conservative_input_tokens,
            intent.registration.retention_seconds,
        )?;
        sqlx::query("UPDATE app_ai_effects SET status='sending',generation=$2,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(self.claim.generation).execute(tx.conn()).await.map_err(|_|DomainError::Internal)?;
        tx.commit().await.map_err(domain_error)
    }
    async fn settle_effect(
        &self,
        context: &ModelEffectContext,
        id: Uuid,
        response: ModelResponse,
    ) -> DomainResult<EffectStatus> {
        let mut tx = self.checked(context).await?;
        tx.lock_record_key("ai.effect", id)
            .await
            .map_err(domain_error)?;
        let row=sqlx::query("SELECT intent,reserved_units,reserved_tokens,status FROM app_ai_effects WHERE id=$1 AND request_id=$2 AND call_key=$3 FOR UPDATE").bind(id).bind(self.request_id).bind(&self.call_key).fetch_optional(tx.conn()).await.map_err(|_|DomainError::Internal)?.ok_or(DomainError::NotFound)?;
        if row
            .try_get::<String, _>("status")
            .map_err(|_| DomainError::Internal)?
            != "sending"
        {
            return Err(DomainError::Conflict("effect_no_longer_sending".into()));
        }
        let intent: EffectIntent =
            serde_json::from_value(row.try_get("intent").map_err(|_| DomainError::Internal)?)
                .map_err(|_| DomainError::Internal)?;
        if response.destination_id != intent.registration.destination_id
            || response.model != intent.registration.model
            || response.provider != intent.registration.provider
            || response.model_version != intent.registration.model_version
            || response.pricing != intent.registration.pricing
            || response.output.schema_id != intent.registration.output_schema_id
            || response.output.schema_version != intent.registration.output_schema_version
        {
            return Err(DomainError::Forbidden);
        }
        let units = response
            .usage
            .as_ref()
            .map(|u| response.pricing.actual_units(u))
            .transpose()?
            .flatten();
        let tokens = response
            .usage
            .as_ref()
            .and_then(|u| u.input_tokens.zip(u.output_tokens))
            .and_then(|(i, o)| i.checked_add(o));
        let mut settled = false;
        if let (Some(units), Some(tokens)) = (units, tokens)
            && units
                <= row
                    .try_get::<i64, _>("reserved_units")
                    .map_err(|_| DomainError::Internal)?
            && tokens
                <= row
                    .try_get::<i64, _>("reserved_tokens")
                    .map_err(|_| DomainError::Internal)?
        {
            tx.settle_quota(
                "ai_budget_units",
                row.try_get("reserved_units")
                    .map_err(|_| DomainError::Internal)?,
                units,
            )
            .await
            .map_err(domain_error)?;
            tx.settle_quota(
                "ai_tokens",
                row.try_get("reserved_tokens")
                    .map_err(|_| DomainError::Internal)?,
                tokens,
            )
            .await
            .map_err(domain_error)?;
            settled = true;
        }
        sqlx::query("UPDATE app_ai_effects SET status='succeeded',response=$2,actual_units=$3,reservation_status=CASE WHEN $4 THEN 'settled' ELSE 'held' END,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(serde_json::to_value(&response).map_err(|_|DomainError::Internal)?).bind(units).bind(settled).execute(tx.conn()).await.map_err(|_|DomainError::Internal)?;
        tx.audit("B150","ai.receipt",Some(id),json!({"usage":response.usage,"pricing":response.pricing,"actual_units":units,"settled":settled,"cost_kind":"estimate","invoice_verified":false})).await.map_err(domain_error)?;
        tx.commit().await.map_err(domain_error)?;
        Ok(EffectStatus::Succeeded)
    }
    async fn mark_unknown(
        &self,
        c: &ModelEffectContext,
        id: Uuid,
        f: ModelFailureCode,
    ) -> DomainResult<()> {
        self.transition(c, id, "unknown", Some(f), false).await
    }
    async fn release_not_sent(&self, c: &ModelEffectContext, id: Uuid) -> DomainResult<()> {
        self.transition(c, id, "failed", None, true).await
    }
    async fn release_prepared_effect(
        &self,
        c: &ModelEffectContext,
        id: Uuid,
        _: &ModelRequest,
    ) -> DomainResult<()> {
        self.transition(c, id, "cancelled", None, true).await
    }
    async fn get_effect(
        &self,
        actor: Uuid,
        application: Uuid,
        id: Uuid,
    ) -> DomainResult<EffectRecordView> {
        if actor != self.original.principal_id() || application != self.original.application_id() {
            return Err(DomainError::NotFound);
        }
        let mut tx = self.checked(&self.context).await?;
        let r = sqlx::query(
            "SELECT * FROM app_ai_effects WHERE id=$1 AND request_id=$2 AND call_key=$3",
        )
        .bind(id)
        .bind(self.request_id)
        .bind(&self.call_key)
        .fetch_optional(tx.conn())
        .await
        .map_err(|_| DomainError::Internal)?
        .ok_or(DomainError::NotFound)?;
        let intent: EffectIntent =
            serde_json::from_value(r.try_get("intent").map_err(|_| DomainError::Internal)?)
                .map_err(|_| DomainError::Internal)?;
        let response: Option<Value> = r.try_get("response").map_err(|_| DomainError::Internal)?;
        let view = EffectRecordView {
            effect_id: id,
            job_id: self.claim.id,
            project_id: application,
            generation: r.try_get("generation").map_err(|_| DomainError::Internal)?,
            destination: intent.destination_id.clone(),
            status: serde_json::from_value(json!(
                r.try_get::<String, _>("status")
                    .map_err(|_| DomainError::Internal)?
            ))
            .map_err(|_| DomainError::Internal)?,
            intent: intent.clone().into(),
            result: response
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| DomainError::Internal)?,
            reconciliation: None,
            failure_code: r
                .try_get::<Option<String>, _>("failure_code")
                .map_err(|_| DomainError::Internal)?
                .map(|s| serde_json::from_value(json!(s)))
                .transpose()
                .map_err(|_| DomainError::Internal)?,
            reserved_units: intent.reserved_units,
            reservation_status: serde_json::from_value(json!(
                r.try_get::<String, _>("reservation_status")
                    .map_err(|_| DomainError::Internal)?
            ))
            .map_err(|_| DomainError::Internal)?,
            created_at: r.try_get("created_at").map_err(|_| DomainError::Internal)?,
            updated_at: r.try_get("updated_at").map_err(|_| DomainError::Internal)?,
        };
        tx.commit().await.map_err(domain_error)?;
        Ok(view)
    }
}
