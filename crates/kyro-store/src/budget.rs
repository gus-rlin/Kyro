use chrono::{DateTime, Duration, Utc};
use kyro_domain::{
    Action, Error, Result,
    model::{
        BudgetSnapshot, CONSERVATIVE_TOKEN_OVERHEAD, DataPolicy, EffectIntent, EffectListPage,
        EffectReconcileOutcome, EffectReconciliationContext, EffectReconciliationReceipt,
        EffectRecordView, EffectStatus, EffectStoredResult, ModelEffectContext,
        ModelEffectPreparation, ModelEffectStore, ModelFailureCode, ModelProviderKind,
        ModelRequest, ModelResponse, ReconcileEffectRequest, ReconciledEffectDecision,
        ReconciliationDecisionKind, ReservationStatus,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::Store;

const RESERVATION_RETENTION_SECONDS: i64 = 86_400;
const MAX_EFFECT_PAGE_SIZE: i64 = 100;
const MAX_EFFECT_RESULT_JSON_BYTES: usize = 60_000;

#[derive(Clone, Debug)]
struct BudgetRow {
    limit_units: i64,
    reserved_units: i64,
    spent_units: i64,
    currency: String,
    unit_scale: i64,
}

#[derive(Clone, Debug)]
struct EffectRow {
    id: Uuid,
    job_id: Uuid,
    project_id: Uuid,
    generation: i64,
    destination: String,
    fingerprint: [u8; 32],
    status: EffectStatus,
    intent: EffectIntent,
    result: Option<EffectStoredResult>,
    reservation_id: Uuid,
    reservation_units: i64,
    reservation_status: ReservationStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Store {
    pub async fn model_dispatch_ready(&self) -> Result<bool> {
        let enabled: Option<bool> = sqlx::query_scalar(
            "SELECT external_sends_enabled FROM public.runtime_control WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        enabled.ok_or(Error::Unavailable)
    }

    pub async fn get_budget(&self, actor_id: Uuid, project_id: Uuid) -> Result<BudgetSnapshot> {
        let mut transaction = self.begin_actor(actor_id).await?;
        Store::authorize_budget_or_manage(&mut *transaction, actor_id, project_id).await?;
        let row = sqlx::query(
            "SELECT project_id, configuration_version, limit_units, reserved_units, spent_units, currency, unit_scale \
             FROM public.project_budgets WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or(Error::NotFound)?;
        let snapshot = budget_snapshot(&row)?;
        snapshot.validate()?;
        transaction.commit().await.map_err(database_error)?;
        Ok(snapshot)
    }

    /// Modifie le plafond par CAS indépendant de la révision AppSpec.
    pub async fn update_budget(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        expected_configuration_version: i64,
        limit_units: i64,
        currency: String,
        unit_scale: i64,
    ) -> Result<BudgetSnapshot> {
        validate_budget_update(limit_units, &currency, unit_scale)?;
        let mut transaction = self.begin_actor(actor_id).await?;
        sqlx::query("SELECT current_revision FROM public.kyro_lock_project_for_actor($1, ARRAY['budget', 'manage']::TEXT[])")
            .bind(project_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            .ok_or(Error::NotFound)?;
        let row = sqlx::query(
            "SELECT project_id, configuration_version, limit_units, reserved_units, spent_units, currency, unit_scale \
             FROM public.project_budgets WHERE project_id = $1 FOR UPDATE",
        )
        .bind(project_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or(Error::NotFound)?;
        Store::authorize_budget_or_manage(&mut *transaction, actor_id, project_id).await?;
        let current = budget_snapshot(&row)?;
        if current.configuration_version != expected_configuration_version {
            return Err(Error::StaleBudgetVersion {
                expected: expected_configuration_version,
                current: current.configuration_version,
            });
        }
        let committed = current
            .reserved_units
            .checked_add(current.spent_units)
            .ok_or(Error::ResourceLimit)?;
        if limit_units < committed {
            return Err(Error::BudgetExceeded);
        }
        let denomination_changes = current.currency != currency || current.unit_scale != unit_scale;
        if denomination_changes && (current.reserved_units != 0 || current.spent_units != 0) {
            return Err(Error::Conflict(
                "la devise et l'échelle ne changent qu'avec un compte vide".into(),
            ));
        }
        let configuration_changed = current.limit_units != limit_units
            || current.currency != currency
            || current.unit_scale != unit_scale;
        let next_configuration_version = if configuration_changed {
            current
                .configuration_version
                .checked_add(1)
                .ok_or(Error::ResourceLimit)?
        } else {
            current.configuration_version
        };
        let changed = sqlx::query(
            "UPDATE public.project_budgets \
             SET limit_units = $3, currency = $4, unit_scale = $5, \
                 configuration_version = $6, updated_at = now() \
             WHERE project_id = $1 AND configuration_version = $2",
        )
        .bind(project_id)
        .bind(expected_configuration_version)
        .bind(limit_units)
        .bind(&currency)
        .bind(unit_scale)
        .bind(next_configuration_version)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        require_one_row(changed)?;
        Store::append_event(
            &mut *transaction,
            project_id,
            "budget.updated",
            json!({
                "units": limit_units
            }),
        )
        .await?;
        let updated = BudgetSnapshot {
            project_id,
            configuration_version: next_configuration_version,
            limit_units,
            reserved_units: current.reserved_units,
            spent_units: current.spent_units,
            currency,
            unit_scale,
        };
        updated.validate()?;
        transaction.commit().await.map_err(database_error)?;
        Ok(updated)
    }

    pub async fn get_effect(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        effect_id: Uuid,
    ) -> Result<EffectRecordView> {
        let mut transaction = self.begin_actor(actor_id).await?;
        Store::authorize_in(
            &mut *transaction,
            actor_id,
            project_id,
            Action::Read.as_str(),
        )
        .await?;
        let row = load_effect_view(&mut *transaction, project_id, Some(effect_id)).await?;
        transaction.commit().await.map_err(database_error)?;
        row.ok_or(Error::NotFound)
    }

    pub async fn list_effects(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        limit: i64,
        before: Option<Uuid>,
    ) -> Result<EffectListPage> {
        if !(1..=MAX_EFFECT_PAGE_SIZE).contains(&limit) {
            return Err(Error::Invalid("taille de page d'effets invalide".into()));
        }
        let mut transaction = self.begin_actor(actor_id).await?;
        Store::authorize_in(
            &mut *transaction,
            actor_id,
            project_id,
            Action::Read.as_str(),
        )
        .await?;
        let rows = sqlx::query(
            "SELECT e.id, e.job_id, e.project_id, e.generation, e.destination, e.fingerprint, \
                    e.status, e.intent, e.result, e.created_at, e.updated_at, \
                    r.id AS reservation_id, r.units AS reservation_units, r.status AS reservation_status \
             FROM public.effects e \
             JOIN public.budget_reservations r ON r.effect_id = e.id \
             WHERE e.project_id = $1 \
               AND ($2::uuid IS NULL OR (e.created_at, e.id) < ( \
                    SELECT c.created_at, c.id FROM public.effects c \
                    WHERE c.id = $2 AND c.project_id = $1)) \
             ORDER BY e.created_at DESC, e.id DESC LIMIT $3",
        )
        .bind(project_id)
        .bind(before)
        .bind(limit + 1)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let has_more = rows.len() > limit as usize;
        let mut items = rows
            .into_iter()
            .take(limit as usize)
            .map(effect_view)
            .collect::<Result<Vec<_>>>()?;
        let next_before = if has_more {
            items.last().map(|item| item.effect_id)
        } else {
            None
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(EffectListPage {
            items: std::mem::take(&mut items),
            next_before,
        })
    }

    async fn authorize_budget_or_manage(
        connection: &mut PgConnection,
        actor_id: Uuid,
        project_id: Uuid,
    ) -> Result<()> {
        Store::authorize_any_in(
            connection,
            actor_id,
            project_id,
            &[Action::Budget.as_str(), Action::Manage.as_str()],
        )
        .await
    }

    async fn begin_accounting(
        &self,
        job_id: Uuid,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        sqlx::query("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT set_config('kyro.environment', $1, true)")
            .bind(self.environment.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        crate::queue::set_accounting_job_context(&mut *transaction, job_id).await?;
        Ok(transaction)
    }

    async fn read_effect_for_job(
        connection: &mut PgConnection,
        job_id: Uuid,
    ) -> Result<Option<EffectRow>> {
        let row = sqlx::query(
            "SELECT e.id, e.job_id, e.project_id, e.generation, e.destination, e.fingerprint, \
                    e.status, e.intent, e.result, e.created_at, e.updated_at, \
                    r.id AS reservation_id, r.units AS reservation_units, r.status AS reservation_status \
             FROM public.effects e \
             JOIN public.budget_reservations r ON r.effect_id = e.id \
             WHERE e.job_id = $1 FOR UPDATE OF e, r",
        )
        .bind(job_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(database_error)?;
        row.as_ref().map(effect_row).transpose()
    }

    async fn lock_billing_effect(
        connection: &mut PgConnection,
        context: &ModelEffectContext,
        effect_id: Uuid,
    ) -> Result<EffectRow> {
        crate::queue::set_accounting_job_context(connection, context.job_id).await?;
        let locked_project =
            sqlx::query("SELECT project_id FROM public.kyro_lock_project_for_job($1, false)")
                .bind(context.job_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(database_error)?
                .ok_or(Error::NotFound)?;
        let locked_project_id: Uuid = locked_project
            .try_get("project_id")
            .map_err(|_| Error::Internal)?;
        if locked_project_id != context.project_id {
            return Err(Error::Forbidden);
        }
        let job = sqlx::query(
            "SELECT actor_id, project_id FROM public.jobs WHERE id = $1 AND project_id = $2 FOR UPDATE",
        )
        .bind(context.job_id)
        .bind(context.project_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(database_error)?
        .ok_or(Error::NotFound)?;
        let actor_id: Uuid = job.try_get("actor_id").map_err(|_| Error::Internal)?;
        if actor_id != context.actor_id {
            return Err(Error::Forbidden);
        }
        let row = sqlx::query(
            "SELECT e.id, e.job_id, e.project_id, e.generation, e.destination, e.fingerprint, \
                    e.status, e.intent, e.result, e.created_at, e.updated_at, \
                    r.id AS reservation_id, r.units AS reservation_units, r.status AS reservation_status \
             FROM public.effects e \
             JOIN public.budget_reservations r ON r.effect_id = e.id \
             WHERE e.id = $1 AND e.job_id = $2 AND e.project_id = $3 FOR UPDATE OF e, r",
        )
        .bind(effect_id)
        .bind(context.job_id)
        .bind(context.project_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(database_error)?
        .ok_or(Error::NotFound)?;
        let effect = effect_row(&row)?;
        if effect.generation != context.generation
            || effect.job_id != context.job_id
            || effect.project_id != context.project_id
        {
            return Err(Error::Conflict("contexte d'effet périmé".into()));
        }
        Ok(effect)
    }

    async fn budget_for_update(
        connection: &mut PgConnection,
        project_id: Uuid,
    ) -> Result<BudgetRow> {
        let row = sqlx::query(
            "SELECT limit_units, reserved_units, spent_units, currency, unit_scale \
             FROM public.project_budgets WHERE project_id = $1 FOR UPDATE",
        )
        .bind(project_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(database_error)?
        .ok_or(Error::NotFound)?;
        Ok(BudgetRow {
            limit_units: row.try_get("limit_units").map_err(|_| Error::Internal)?,
            reserved_units: row.try_get("reserved_units").map_err(|_| Error::Internal)?,
            spent_units: row.try_get("spent_units").map_err(|_| Error::Internal)?,
            currency: row.try_get("currency").map_err(|_| Error::Internal)?,
            unit_scale: row.try_get("unit_scale").map_err(|_| Error::Internal)?,
        })
    }

    async fn release_held_reservation(
        connection: &mut PgConnection,
        effect: &EffectRow,
    ) -> Result<()> {
        if effect.reservation_status != ReservationStatus::Held {
            return Err(Error::Conflict("réservation non retenue".into()));
        }
        let budget = Self::budget_for_update(connection, effect.project_id).await?;
        let new_reserved = budget
            .reserved_units
            .checked_sub(effect.reservation_units)
            .ok_or(Error::Internal)?;
        let budget_rows = sqlx::query(
            "UPDATE public.project_budgets SET reserved_units = $2, updated_at = now() \
             WHERE project_id = $1 AND reserved_units >= $3",
        )
        .bind(effect.project_id)
        .bind(new_reserved)
        .bind(effect.reservation_units)
        .execute(&mut *connection)
        .await
        .map_err(database_error)?
        .rows_affected();
        require_one_row(budget_rows)?;
        let reservation_rows = sqlx::query(
            "UPDATE public.budget_reservations SET status = 'released', updated_at = now() \
             WHERE id = $1 AND effect_id = $2 AND status = 'held'",
        )
        .bind(effect.reservation_id)
        .bind(effect.id)
        .execute(&mut *connection)
        .await
        .map_err(database_error)?
        .rows_affected();
        require_one_row(reservation_rows)?;
        Ok(())
    }

    async fn append_effect_event(
        connection: &mut PgConnection,
        effect: &EffectRow,
        kind: &str,
        extra: Value,
    ) -> Result<()> {
        let mut payload = json!({
            "effect_id": effect.id,
            "job_id": effect.job_id,
            "reservation_id": effect.reservation_id
        });
        if let (Some(target), Some(source)) = (payload.as_object_mut(), extra.as_object()) {
            for (key, value) in source {
                if !matches!(
                    key.as_str(),
                    "status" | "generation" | "attempts" | "units" | "error_code" | "revision"
                ) {
                    return Err(Error::Internal);
                }
                match key.as_str() {
                    "status"
                        if !value.as_str().is_some_and(|status| {
                            matches!(
                                status,
                                "prepared"
                                    | "sending"
                                    | "succeeded"
                                    | "failed"
                                    | "unknown"
                                    | "cancelled"
                            )
                        }) =>
                    {
                        return Err(Error::Internal);
                    }
                    "error_code"
                        if !value.as_str().is_some_and(|code| {
                            matches!(
                                code,
                                "transport_uncertain"
                                    | "provider_status_uncertain"
                                    | "invalid_response"
                                    | "response_too_large"
                                    | "invalid_usage"
                                    | "persistence_uncertain"
                            )
                        }) =>
                    {
                        return Err(Error::Internal);
                    }
                    "units" | "generation" | "attempts" | "revision"
                        if !value.as_i64().is_some_and(|number| number >= 0) =>
                    {
                        return Err(Error::Internal);
                    }
                    _ => {}
                }
            }
            target.extend(source.clone());
        }
        Store::append_event(connection, effect.project_id, kind, payload).await?;
        Ok(())
    }

    async fn prepare_effect(
        &self,
        preparation: ModelEffectPreparation,
    ) -> Result<kyro_domain::model::PreparedModelEffect> {
        validate_preparation(&preparation)?;
        let context = &preparation.context;
        let mut transaction = self.begin_actor(context.actor_id).await?;
        crate::queue::lock_validate_model_job_tx(
            self,
            &mut *transaction,
            context,
            &preparation.request,
        )
        .await?;
        crate::queue::set_accounting_job_context(&mut *transaction, context.job_id).await?;

        let policy_value: Value =
            sqlx::query_scalar("SELECT data_policy FROM public.projects WHERE id = $1")
                .bind(context.project_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(database_error)?
                .ok_or(Error::NotFound)?;
        let data_policy = decode_data_policy(policy_value)?;
        let input_bytes = serde_json::to_vec(&preparation.request.input)
            .map_err(|_| Error::Invalid("entrée de modèle invalide".into()))?
            .len();
        let input_bytes = u32::try_from(input_bytes).map_err(|_| Error::ResourceLimit)?;
        if input_bytes != preparation.input_bytes {
            return Err(Error::Invalid("empreinte d'entrée incohérente".into()));
        }
        data_policy.authorize_request(
            &preparation.request,
            input_bytes,
            preparation.conservative_input_tokens,
            preparation.registration.retention_seconds,
        )?;
        let recalculated = preparation.registration.pricing.reservation_units(
            preparation.conservative_input_tokens,
            preparation.request.max_output_tokens,
        )?;
        if recalculated != preparation.reservation_units {
            return Err(Error::Invalid("réservation tarifaire incohérente".into()));
        }

        if let Some(effect) = Self::read_effect_for_job(&mut *transaction, context.job_id).await? {
            if effect.project_id != context.project_id
                || effect.fingerprint != preparation.fingerprint
                || effect.intent.registration != preparation.registration
            {
                return Err(Error::Conflict(
                    "un effet distinct est déjà lié à ce job".into(),
                ));
            }
            if effect.status == EffectStatus::Prepared {
                if effect.reservation_status != ReservationStatus::Held {
                    return Err(Error::Internal);
                }
                let affected = sqlx::query(
                    "UPDATE public.effects SET generation = $2, updated_at = now() \
                     WHERE id = $1 AND status = 'prepared'",
                )
                .bind(effect.id)
                .bind(context.generation)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?
                .rows_affected();
                require_one_row(affected)?;
            }
            let response = if effect.status == EffectStatus::Succeeded {
                Some(
                    effect
                        .result
                        .as_ref()
                        .and_then(|result| result.response.clone())
                        .ok_or(Error::Internal)?,
                )
            } else {
                None
            };
            transaction.commit().await.map_err(database_error)?;
            return Ok(kyro_domain::model::PreparedModelEffect {
                intent: effect.intent,
                status: effect.status,
                data_policy,
                existing_response: response,
            });
        }

        // A worker missing its server-side key may reuse a durable effect, but may not
        // create an intent or reservation that it cannot send.
        if !preparation.allow_new_effect {
            return Err(Error::Unavailable);
        }

        let external_sends_enabled: bool = sqlx::query_scalar(
            "SELECT external_sends_enabled FROM public.runtime_control WHERE id = 1",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or(Error::Unavailable)?;
        if !external_sends_enabled {
            return Err(Error::Unavailable);
        }
        let effect_id = Uuid::new_v4();
        let reservation_id = Uuid::new_v4();
        let intent = EffectIntent {
            id: effect_id,
            project_id: context.project_id,
            job_id: context.job_id,
            destination_id: preparation.request.destination_id.clone(),
            fingerprint: preparation.fingerprint,
            reservation_id,
            reserved_units: preparation.reservation_units,
            request_purpose: preparation.request.input.purpose,
            request_categories: preparation.request.input.categories.clone(),
            input_bytes: preparation.input_bytes,
            conservative_input_tokens: preparation.conservative_input_tokens,
            max_output_tokens: preparation.request.max_output_tokens,
            max_response_bytes: data_policy.limits.max_response_bytes,
            deadline_ms: preparation.request.deadline_ms,
            registration: preparation.registration.clone(),
        };
        let intent_value = serialize_json(&intent)?;
        if serde_json::to_vec(&intent_value)
            .map_err(|_| Error::Internal)?
            .len()
            > 32_768
        {
            return Err(Error::ResourceLimit);
        }
        sqlx::query(
            "INSERT INTO public.effects \
             (id, job_id, project_id, generation, destination, fingerprint, intent, status, result, reservation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'prepared', NULL, $8)",
        )
        .bind(effect_id)
        .bind(context.job_id)
        .bind(context.project_id)
        .bind(context.generation)
        .bind(&preparation.request.destination_id)
        .bind(preparation.fingerprint.to_vec())
        .bind(intent_value)
        .bind(reservation_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO public.budget_reservations \
             (id, effect_id, job_id, project_id, idempotency_key, units, status, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 'held', $7)",
        )
        .bind(reservation_id)
        .bind(effect_id)
        .bind(context.job_id)
        .bind(context.project_id)
        .bind(effect_id.to_string())
        .bind(preparation.reservation_units)
        .bind(Utc::now() + Duration::seconds(RESERVATION_RETENTION_SECONDS))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        // The accounting policy exposes a budget only through an existing
        // effect for this already-authorized job. Create both linked rows first
        // in this transaction; any pricing/quota denial rolls them back before
        // an intent becomes durable or an external request can be sent.
        let budget = Self::budget_for_update(&mut *transaction, context.project_id).await?;
        if budget.currency != preparation.registration.pricing.currency
            || budget.unit_scale != preparation.registration.pricing.unit_scale
        {
            return Err(Error::Conflict(
                "devise du tarif différente du compte".into(),
            ));
        }
        let total = budget
            .reserved_units
            .checked_add(budget.spent_units)
            .and_then(|value| value.checked_add(preparation.reservation_units))
            .ok_or(Error::ResourceLimit)?;
        if total > budget.limit_units {
            return Err(Error::BudgetExceeded);
        }
        let updated_reserved = budget
            .reserved_units
            .checked_add(preparation.reservation_units)
            .ok_or(Error::ResourceLimit)?;
        let reserve_rows = sqlx::query(
            "UPDATE public.project_budgets SET reserved_units = $2, updated_at = now() \
             WHERE project_id = $1 AND reserved_units = $3",
        )
        .bind(context.project_id)
        .bind(updated_reserved)
        .bind(budget.reserved_units)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if reserve_rows != 1 {
            return Err(Error::Conflict(
                "budget changé pendant la réservation".into(),
            ));
        }
        Store::append_event(
            &mut *transaction,
            context.project_id,
            "effect.prepared",
            json!({
                "effect_id": effect_id,
                "job_id": context.job_id,
                "reservation_id": reservation_id,
                "units": preparation.reservation_units
            }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(kyro_domain::model::PreparedModelEffect {
            intent,
            status: EffectStatus::Prepared,
            data_policy,
            existing_response: None,
        })
    }

    async fn mark_effect_sending(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        request: &ModelRequest,
    ) -> Result<()> {
        let mut transaction = self.begin_actor(context.actor_id).await?;
        crate::queue::lock_validate_model_job_tx(self, &mut *transaction, context, request).await?;
        crate::queue::set_accounting_job_context(&mut *transaction, context.job_id).await?;
        let enabled: bool = sqlx::query_scalar(
            "SELECT external_sends_enabled FROM public.runtime_control WHERE id = 1",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or(Error::Unavailable)?;
        if !enabled {
            return Err(Error::Unavailable);
        }
        let row = Self::read_effect_for_job(&mut transaction, context.job_id)
            .await?
            .ok_or(Error::NotFound)?;
        if row.id != effect_id || row.generation != context.generation {
            return Err(Error::Conflict("contexte d'effet périmé".into()));
        }
        if row.status != EffectStatus::Prepared || row.reservation_status != ReservationStatus::Held
        {
            return Err(Error::Conflict(
                "l'effet n'est pas prêt à être envoyé".into(),
            ));
        }
        let policy_value: Value =
            sqlx::query_scalar("SELECT data_policy FROM public.projects WHERE id = $1")
                .bind(context.project_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(database_error)?
                .ok_or(Error::NotFound)?;
        let policy = decode_data_policy(policy_value)?;
        let policy_check = policy
            .authorize_request(
                request,
                row.intent.input_bytes,
                row.intent.conservative_input_tokens,
                row.intent.registration.retention_seconds,
            )
            .and_then(|()| {
                if row.intent.max_response_bytes > policy.limits.max_response_bytes {
                    Err(Error::ResourceLimit)
                } else {
                    Ok(())
                }
            });
        if let Err(error) = policy_check {
            Self::release_held_reservation(&mut *transaction, &row).await?;
            let affected = sqlx::query(
                "UPDATE public.effects SET status = 'cancelled', result = $2, updated_at = now() \
                 WHERE id = $1 AND status = 'prepared'",
            )
            .bind(effect_id)
            .bind(serialize_json(&EffectStoredResult {
                response: None,
                reconciliation: None,
                failure_code: None,
            })?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            require_one_row(affected.rows_affected())?;
            Self::append_effect_event(
                &mut *transaction,
                &row,
                "effect.cancelled",
                json!({ "status": "cancelled" }),
            )
            .await?;
            transaction.commit().await.map_err(database_error)?;
            return Err(error);
        }
        let affected = sqlx::query(
            "UPDATE public.effects SET status = 'sending', updated_at = now() \
             WHERE id = $1 AND job_id = $2 AND generation = $3 AND status = 'prepared'",
        )
        .bind(effect_id)
        .bind(context.job_id)
        .bind(context.generation)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        require_one_row(affected)?;
        Store::append_event(
            &mut *transaction,
            context.project_id,
            "effect.sending",
            json!({ "effect_id": effect_id, "job_id": context.job_id, "status": "sending" }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    async fn mark_effect_unknown(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        failure: ModelFailureCode,
    ) -> Result<()> {
        let mut transaction = self.begin_accounting(context.job_id).await?;
        let mut effect = Self::lock_billing_effect(&mut transaction, context, effect_id).await?;
        if effect.status == EffectStatus::Unknown
            && effect.reservation_status == ReservationStatus::Held
        {
            transaction.commit().await.map_err(database_error)?;
            return Ok(());
        }
        if effect.status != EffectStatus::Sending
            || effect.reservation_status != ReservationStatus::Held
        {
            return Err(Error::Conflict(
                "l'effet ne peut pas devenir incertain".into(),
            ));
        }
        effect.status = EffectStatus::Unknown;
        let result = EffectStoredResult {
            response: None,
            reconciliation: None,
            failure_code: Some(failure),
        };
        let affected = sqlx::query(
            "UPDATE public.effects SET status = 'unknown', result = $2, updated_at = now() \
             WHERE id = $1 AND status = 'sending'",
        )
        .bind(effect_id)
        .bind(serialize_json(&result)?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        require_one_row(affected)?;
        Self::append_effect_event(
            &mut transaction,
            &effect,
            "effect.unknown",
            json!({ "error_code": model_failure_code(failure) }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    async fn release_not_sent_effect(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
    ) -> Result<()> {
        let mut transaction = self.begin_accounting(context.job_id).await?;
        let mut effect = Self::lock_billing_effect(&mut transaction, context, effect_id).await?;
        if !matches!(effect.status, EffectStatus::Sending | EffectStatus::Unknown)
            || effect.reservation_status != ReservationStatus::Held
        {
            return Err(Error::Conflict(
                "seul un envoi non démarré se libère".into(),
            ));
        }
        Self::release_held_reservation(&mut *transaction, &effect).await?;
        effect.status = EffectStatus::Failed;
        let affected = sqlx::query(
            "UPDATE public.effects SET status = 'failed', result = $2, updated_at = now() \
             WHERE id = $1 AND status IN ('sending', 'unknown')",
        )
        .bind(effect_id)
        .bind(serialize_json(&EffectStoredResult {
            response: None,
            reconciliation: None,
            failure_code: None,
        })?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        require_one_row(affected)?;
        Self::append_effect_event(
            &mut transaction,
            &effect,
            "effect.failed",
            json!({ "status": "failed" }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    async fn release_prepared(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        request: &ModelRequest,
    ) -> Result<()> {
        let mut transaction = self.begin_actor(context.actor_id).await?;
        crate::queue::lock_validate_model_job_tx(self, &mut *transaction, context, request).await?;
        crate::queue::set_accounting_job_context(&mut *transaction, context.job_id).await?;
        let mut effect = Self::read_effect_for_job(&mut *transaction, context.job_id)
            .await?
            .ok_or(Error::NotFound)?;
        if effect.id != effect_id
            || effect.generation != context.generation
            || effect.status != EffectStatus::Prepared
        {
            return Err(Error::Conflict(
                "seul un effet préparé peut être libéré".into(),
            ));
        }
        Self::release_held_reservation(&mut *transaction, &effect).await?;
        effect.status = EffectStatus::Cancelled;
        let affected = sqlx::query(
            "UPDATE public.effects SET status = 'cancelled', result = $2, updated_at = now() \
             WHERE id = $1 AND status = 'prepared'",
        )
        .bind(effect_id)
        .bind(serialize_json(&EffectStoredResult {
            response: None,
            reconciliation: None,
            failure_code: None,
        })?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        require_one_row(affected)?;
        Self::append_effect_event(
            &mut transaction,
            &effect,
            "effect.cancelled",
            json!({ "status": "cancelled" }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    async fn settle_model_effect(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        response: ModelResponse,
    ) -> Result<EffectStatus> {
        let mut transaction = self.begin_accounting(context.job_id).await?;
        let mut effect = Self::lock_billing_effect(&mut transaction, context, effect_id).await?;
        validate_response_snapshot(
            &response,
            &effect.intent.registration.pricing,
            Some(&effect.intent),
        )?;
        if effect.status == EffectStatus::Succeeded {
            if effect
                .result
                .as_ref()
                .and_then(|result| result.response.as_ref())
                == Some(&response)
            {
                transaction.commit().await.map_err(database_error)?;
                return Ok(EffectStatus::Succeeded);
            }
            return Err(Error::Conflict(
                "l'effet possède déjà un autre résultat".into(),
            ));
        }
        if !matches!(effect.status, EffectStatus::Sending | EffectStatus::Unknown)
            || effect.reservation_status != ReservationStatus::Held
        {
            return Err(Error::Conflict(
                "l'effet ne peut pas être comptabilisé depuis cet état".into(),
            ));
        }
        let stored_value = serialize_json(&EffectStoredResult {
            response: Some(response.clone()),
            reconciliation: None,
            failure_code: None,
        })?;
        if serde_json::to_vec(&stored_value)
            .map_err(|_| Error::Internal)?
            .len()
            > MAX_EFFECT_RESULT_JSON_BYTES
        {
            effect.status = EffectStatus::Unknown;
            let failure = EffectStoredResult {
                response: None,
                reconciliation: None,
                failure_code: Some(ModelFailureCode::ResponseTooLarge),
            };
            let affected = sqlx::query(
                "UPDATE public.effects SET status = 'unknown', result = $2, updated_at = now() \
                 WHERE id = $1 AND status IN ('sending', 'unknown')",
            )
            .bind(effect_id)
            .bind(serialize_json(&failure)?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?
            .rows_affected();
            require_one_row(affected)?;
            Self::append_effect_event(
                &mut *transaction,
                &effect,
                "effect.unknown",
                json!({ "error_code": "response_too_large" }),
            )
            .await?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(EffectStatus::Unknown);
        }
        let usage_result = settle_usage(&mut *transaction, &effect, &response).await;
        if matches!(
            usage_result,
            Err(Error::BudgetExceeded | Error::ResourceLimit)
        ) {
            effect.status = EffectStatus::Unknown;
            let stored = EffectStoredResult {
                response: None,
                reconciliation: None,
                failure_code: Some(ModelFailureCode::InvalidUsage),
            };
            let affected = sqlx::query(
                "UPDATE public.effects SET status = 'unknown', result = $2, updated_at = now() \
                 WHERE id = $1 AND status IN ('sending', 'unknown')",
            )
            .bind(effect_id)
            .bind(serialize_json(&stored)?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?
            .rows_affected();
            require_one_row(affected)?;
            Self::append_effect_event(
                &mut *transaction,
                &effect,
                "effect.unknown",
                json!({ "error_code": "invalid_usage" }),
            )
            .await?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(EffectStatus::Unknown);
        }
        let actual_units = usage_result?;
        let Some(actual_units) = actual_units else {
            effect.status = EffectStatus::Unknown;
            let stored = EffectStoredResult {
                response: None,
                reconciliation: None,
                failure_code: Some(ModelFailureCode::InvalidUsage),
            };
            let affected = sqlx::query(
                "UPDATE public.effects SET status = 'unknown', result = $2, updated_at = now() \
                 WHERE id = $1 AND status IN ('sending', 'unknown')",
            )
            .bind(effect_id)
            .bind(serialize_json(&stored)?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?
            .rows_affected();
            require_one_row(affected)?;
            Self::append_effect_event(
                &mut *transaction,
                &effect,
                "effect.unknown",
                json!({ "error_code": "invalid_usage" }),
            )
            .await?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(EffectStatus::Unknown);
        };
        effect.status = EffectStatus::Succeeded;
        let affected = sqlx::query(
            "UPDATE public.effects SET status = 'succeeded', result = $2, updated_at = now() \
             WHERE id = $1 AND status IN ('sending', 'unknown')",
        )
        .bind(effect_id)
        .bind(stored_value)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        require_one_row(affected)?;
        Self::append_effect_event(
            &mut *transaction,
            &effect,
            "effect.succeeded",
            json!({ "status": "succeeded", "units": actual_units }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(EffectStatus::Succeeded)
    }
}

/// Reconcile an effect inside the reconciliation-command transaction.
/// The caller must hold the project and target-job locks and set `accounting_job_id`
/// to `context.target_job_id`; it must also authorize the command lease and operator.
pub(crate) async fn reconcile_model_effect_in(
    connection: &mut PgConnection,
    context: &EffectReconciliationContext,
    request: ReconcileEffectRequest,
    validate_registry: impl FnOnce(&EffectIntent) -> Result<()> + Send,
) -> Result<EffectReconcileOutcome> {
    request.validate()?;
    if context.current_revision < 0 || context.actor_id.is_nil() {
        return Err(Error::Invalid("contexte de rapprochement invalide".into()));
    }
    let row = sqlx::query(
        "SELECT e.id, e.job_id, e.project_id, e.generation, e.destination, e.fingerprint, \
                e.status, e.intent, e.result, e.created_at, e.updated_at, \
                r.id AS reservation_id, r.units AS reservation_units, r.status AS reservation_status \
         FROM public.effects e \
         JOIN public.budget_reservations r ON r.effect_id = e.id \
         WHERE e.id = $1 AND e.project_id = $2 AND e.job_id = $3 FOR UPDATE OF e, r",
    )
    .bind(context.effect_id)
    .bind(context.project_id)
    .bind(context.target_job_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(database_error)?
    .ok_or(Error::NotFound)?;
    let mut effect = effect_row(&row)?;
    if effect.project_id != context.project_id || effect.job_id != context.target_job_id {
        return Err(Error::Conflict("effet lié à un autre job".into()));
    }
    if effect.intent.registration.provider_kind != ModelProviderKind::Synthetic {
        return Err(Error::Forbidden);
    }

    // Lock budget after effect/reservation, before the queue finalizer checks current grants.
    let _budget = Store::budget_for_update(connection, effect.project_id).await?;
    Store::authorize_budget_or_manage(connection, context.actor_id, context.project_id).await?;
    // Validate the immutable, locked intent against the worker's current registry
    // before a receipt, ledger entry or reservation transition is written.
    validate_registry(&effect.intent)?;
    let decision_kind = match &request.decision {
        ReconciledEffectDecision::Processed { response } => {
            validate_response_snapshot(
                response,
                &effect.intent.registration.pricing,
                Some(&effect.intent),
            )?;
            ReconciliationDecisionKind::Processed
        }
        ReconciledEffectDecision::NotProcessed => ReconciliationDecisionKind::NotProcessed,
    };
    let evidence_bytes =
        serde_json::to_vec(&request).map_err(|_| Error::Invalid("preuve invalide".into()))?;
    let evidence_fingerprint: [u8; 32] = Sha256::digest(evidence_bytes).into();
    if let Some(receipt) = effect
        .result
        .as_ref()
        .and_then(|result| result.reconciliation.as_ref())
    {
        if receipt.evidence_id != request.evidence_id
            || receipt.evidence_fingerprint != evidence_fingerprint
            || receipt.decision != decision_kind
        {
            return Err(Error::Conflict(
                "un rapprochement différent a déjà été enregistré".into(),
            ));
        }
        let outcome = EffectReconcileOutcome {
            effect_id: effect.id,
            job_id: effect.job_id,
            generation: effect.generation,
            status: effect.status,
        };
        crate::queue::reconcile_model_effect_tx(connection, context, &outcome).await?;
        return Ok(outcome);
    }
    if effect.status != EffectStatus::Unknown
        || effect.reservation_status != ReservationStatus::Held
    {
        return Err(Error::Conflict(
            "seul un effet incertain non rapproché est recevable".into(),
        ));
    }

    let receipt = EffectReconciliationReceipt {
        evidence_id: request.evidence_id,
        evidence_fingerprint,
        decision: decision_kind,
    };
    let (status, response, failure_code) = match request.decision {
        ReconciledEffectDecision::Processed { response } => {
            let usage = response.usage.as_ref().ok_or_else(|| {
                Error::Invalid("un rapprochement traité exige un usage complet".into())
            })?;
            usage.validate()?;
            if usage.input_tokens.is_none() || usage.output_tokens.is_none() {
                return Err(Error::Invalid(
                    "un rapprochement traité exige un usage complet".into(),
                ));
            }
            let _actual_units = settle_usage(connection, &effect, &response)
                .await?
                .ok_or_else(|| Error::Invalid("usage complet non comptabilisable".into()))?;
            (EffectStatus::Succeeded, Some(response), None)
        }
        ReconciledEffectDecision::NotProcessed => {
            Store::release_held_reservation(connection, &effect).await?;
            (EffectStatus::Failed, None, None)
        }
    };
    effect.status = status;
    let stored_value = serialize_json(&EffectStoredResult {
        response,
        reconciliation: Some(receipt),
        failure_code,
    })?;
    if serde_json::to_vec(&stored_value)
        .map_err(|_| Error::Internal)?
        .len()
        > MAX_EFFECT_RESULT_JSON_BYTES
    {
        return Err(Error::ResourceLimit);
    }
    let affected = sqlx::query(
        "UPDATE public.effects SET status = $2, result = $3, updated_at = now() \
         WHERE id = $1 AND job_id = $4 AND project_id = $5 AND status = 'unknown'",
    )
    .bind(effect.id)
    .bind(effect_status_str(status))
    .bind(stored_value)
    .bind(context.target_job_id)
    .bind(context.project_id)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?
    .rows_affected();
    require_one_row(affected)?;
    let outcome = EffectReconcileOutcome {
        effect_id: effect.id,
        job_id: effect.job_id,
        generation: effect.generation,
        status,
    };
    Store::append_effect_event(
        connection,
        &effect,
        "effect.reconciled",
        json!({ "status": effect_status_str(status) }),
    )
    .await?;
    crate::queue::reconcile_model_effect_tx(connection, context, &outcome).await?;
    Ok(outcome)
}

/// Release only a prepared effect after its owning job has been locked and terminalized.
/// Callers must lock project, then job, before invoking this helper.
pub(crate) async fn release_prepared_effect_accounting(
    connection: &mut PgConnection,
    job_id: Uuid,
    project_id: Uuid,
) -> Result<bool> {
    crate::queue::set_accounting_job_context(connection, job_id).await?;
    let Some(row) = sqlx::query(
        "SELECT e.id, e.job_id, e.project_id, e.generation, e.destination, e.fingerprint, \
                e.status, e.intent, e.result, e.created_at, e.updated_at, \
                r.id AS reservation_id, r.units AS reservation_units, r.status AS reservation_status \
         FROM public.effects e \
         JOIN public.budget_reservations r ON r.effect_id = e.id \
         WHERE e.job_id = $1 AND e.project_id = $2 FOR UPDATE OF e, r",
    )
    .bind(job_id)
    .bind(project_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(database_error)?
    else {
        return Ok(false);
    };
    let mut effect = effect_row(&row)?;
    if effect.status != EffectStatus::Prepared {
        return Ok(false);
    }
    Store::release_held_reservation(connection, &effect).await?;
    effect.status = EffectStatus::Cancelled;
    let effect_rows = sqlx::query(
        "UPDATE public.effects SET status = 'cancelled', result = $2, updated_at = now() \
         WHERE id = $1 AND status = 'prepared'",
    )
    .bind(effect.id)
    .bind(serialize_json(&EffectStoredResult {
        response: None,
        reconciliation: None,
        failure_code: None,
    })?)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;
    require_one_row(effect_rows.rows_affected())?;
    Store::append_effect_event(
        connection,
        &effect,
        "effect.cancelled",
        json!({ "status": "cancelled" }),
    )
    .await?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn validate_budget_update(limit_units: i64, currency: &str, unit_scale: i64) -> Result<()> {
    if limit_units < 0
        || unit_scale <= 0
        || currency.len() != 3
        || !currency.bytes().all(|byte| byte.is_ascii_uppercase())
    {
        return Err(Error::Invalid("dénomination de budget invalide".into()));
    }
    Ok(())
}

fn budget_snapshot(row: &PgRow) -> Result<BudgetSnapshot> {
    Ok(BudgetSnapshot {
        project_id: row.try_get("project_id").map_err(|_| Error::Internal)?,
        configuration_version: row
            .try_get("configuration_version")
            .map_err(|_| Error::Internal)?,
        limit_units: row.try_get("limit_units").map_err(|_| Error::Internal)?,
        reserved_units: row.try_get("reserved_units").map_err(|_| Error::Internal)?,
        spent_units: row.try_get("spent_units").map_err(|_| Error::Internal)?,
        currency: row.try_get("currency").map_err(|_| Error::Internal)?,
        unit_scale: row.try_get("unit_scale").map_err(|_| Error::Internal)?,
    })
}

async fn load_effect_view(
    connection: &mut PgConnection,
    project_id: Uuid,
    effect_id: Option<Uuid>,
) -> Result<Option<EffectRecordView>> {
    let Some(effect_id) = effect_id else {
        return Ok(None);
    };
    let row = sqlx::query(
        "SELECT e.id, e.job_id, e.project_id, e.generation, e.destination, e.fingerprint, \
                e.status, e.intent, e.result, e.created_at, e.updated_at, \
                r.id AS reservation_id, r.units AS reservation_units, r.status AS reservation_status \
         FROM public.effects e \
         JOIN public.budget_reservations r ON r.effect_id = e.id \
         WHERE e.project_id = $1 AND e.id = $2",
    )
    .bind(project_id)
    .bind(effect_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(database_error)?;
    row.map(effect_view).transpose()
}

fn effect_view(row: PgRow) -> Result<EffectRecordView> {
    let effect = effect_row(&row)?;
    Ok(EffectRecordView {
        effect_id: effect.id,
        job_id: effect.job_id,
        project_id: effect.project_id,
        generation: effect.generation,
        destination: effect.destination,
        status: effect.status,
        intent: effect.intent.into(),
        result: effect
            .result
            .as_ref()
            .and_then(|result| result.response.clone()),
        reconciliation: effect
            .result
            .as_ref()
            .and_then(|result| result.reconciliation.clone()),
        failure_code: effect.result.and_then(|result| result.failure_code),
        reserved_units: effect.reservation_units,
        reservation_status: effect.reservation_status,
        created_at: effect.created_at,
        updated_at: effect.updated_at,
    })
}

fn effect_row(row: &PgRow) -> Result<EffectRow> {
    let fingerprint: Vec<u8> = row.try_get("fingerprint").map_err(|_| Error::Internal)?;
    let fingerprint: [u8; 32] = fingerprint.try_into().map_err(|_| Error::Internal)?;
    let intent_value: Value = row.try_get("intent").map_err(|_| Error::Internal)?;
    let intent: EffectIntent = serde_json::from_value(intent_value).map_err(|_| Error::Internal)?;
    let result_value: Option<Value> = row.try_get("result").map_err(|_| Error::Internal)?;
    let result = result_value
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| Error::Internal)?;
    let id: Uuid = row.try_get("id").map_err(|_| Error::Internal)?;
    let reservation_id: Uuid = row.try_get("reservation_id").map_err(|_| Error::Internal)?;
    if intent.id != id
        || intent.reservation_id != reservation_id
        || intent.fingerprint != fingerprint
    {
        return Err(Error::Internal);
    }
    Ok(EffectRow {
        id,
        job_id: row.try_get("job_id").map_err(|_| Error::Internal)?,
        project_id: row.try_get("project_id").map_err(|_| Error::Internal)?,
        generation: row.try_get("generation").map_err(|_| Error::Internal)?,
        destination: row.try_get("destination").map_err(|_| Error::Internal)?,
        fingerprint,
        status: parse_effect_status(
            &row.try_get::<String, _>("status")
                .map_err(|_| Error::Internal)?,
        )?,
        intent,
        result,
        reservation_id,
        reservation_units: row
            .try_get("reservation_units")
            .map_err(|_| Error::Internal)?,
        reservation_status: parse_reservation_status(
            &row.try_get::<String, _>("reservation_status")
                .map_err(|_| Error::Internal)?,
        )?,
        created_at: row.try_get("created_at").map_err(|_| Error::Internal)?,
        updated_at: row.try_get("updated_at").map_err(|_| Error::Internal)?,
    })
}

fn parse_effect_status(value: &str) -> Result<EffectStatus> {
    match value {
        "prepared" => Ok(EffectStatus::Prepared),
        "sending" => Ok(EffectStatus::Sending),
        "succeeded" => Ok(EffectStatus::Succeeded),
        "failed" => Ok(EffectStatus::Failed),
        "unknown" => Ok(EffectStatus::Unknown),
        "cancelled" => Ok(EffectStatus::Cancelled),
        _ => Err(Error::Internal),
    }
}

fn effect_status_str(value: EffectStatus) -> &'static str {
    match value {
        EffectStatus::Prepared => "prepared",
        EffectStatus::Sending => "sending",
        EffectStatus::Succeeded => "succeeded",
        EffectStatus::Failed => "failed",
        EffectStatus::Unknown => "unknown",
        EffectStatus::Cancelled => "cancelled",
    }
}

fn model_failure_code(value: ModelFailureCode) -> &'static str {
    match value {
        ModelFailureCode::TransportUncertain => "transport_uncertain",
        ModelFailureCode::ProviderStatusUncertain => "provider_status_uncertain",
        ModelFailureCode::InvalidResponse => "invalid_response",
        ModelFailureCode::ResponseTooLarge => "response_too_large",
        ModelFailureCode::InvalidUsage => "invalid_usage",
        ModelFailureCode::PersistenceUncertain => "persistence_uncertain",
    }
}

fn parse_reservation_status(value: &str) -> Result<ReservationStatus> {
    match value {
        "held" => Ok(ReservationStatus::Held),
        "settled" => Ok(ReservationStatus::Settled),
        "released" => Ok(ReservationStatus::Released),
        _ => Err(Error::Internal),
    }
}

fn serialize_json<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| Error::Internal)
}

fn validate_preparation(preparation: &ModelEffectPreparation) -> Result<()> {
    preparation.request.validate_shape()?;
    preparation.registration.pricing.validate()?;
    if preparation.request.destination_id != preparation.registration.destination_id
        || preparation.request.model != preparation.registration.model
        || preparation.reservation_units <= 0
        || preparation.input_bytes == 0
        || preparation.conservative_input_tokens
            < preparation
                .input_bytes
                .checked_add(CONSERVATIVE_TOKEN_OVERHEAD)
                .ok_or(Error::ResourceLimit)?
        || preparation.conservative_input_tokens > kyro_domain::model::MAX_INPUT_TOKENS
    {
        return Err(Error::Invalid("préparation d'effet incohérente".into()));
    }
    // Historical callers counted input bytes +64. The gateway now includes its entire
    // provider envelope, schema, and (for Nebius) the catalog context ceiling.
    let expected_bytes = serde_json::to_vec(&preparation.request.input)
        .map_err(|_| Error::Invalid("entrée de modèle invalide".into()))?;
    if expected_bytes.len() != preparation.input_bytes as usize {
        return Err(Error::Invalid("taille d'entrée incohérente".into()));
    }
    let expected_reservation = preparation.registration.pricing.reservation_units(
        preparation.conservative_input_tokens,
        preparation.request.max_output_tokens,
    )?;
    if expected_reservation != preparation.reservation_units {
        return Err(Error::Invalid("réservation calculée invalide".into()));
    }
    #[derive(serde::Serialize)]
    struct FingerprintMaterial<'a> {
        request: &'a ModelRequest,
        registration: &'a kyro_domain::model::ModelRegistrationSnapshot,
    }
    let fingerprint_bytes = serde_json::to_vec(&FingerprintMaterial {
        request: &preparation.request,
        registration: &preparation.registration,
    })
    .map_err(|_| Error::Invalid("requête de modèle invalide".into()))?;
    let fingerprint: [u8; 32] = Sha256::digest(fingerprint_bytes).into();
    if fingerprint != preparation.fingerprint {
        return Err(Error::Invalid("empreinte de requête incorrecte".into()));
    }
    Ok(())
}

fn validate_response_snapshot(
    response: &ModelResponse,
    pricing: &kyro_domain::model::PricingSnapshot,
    intent: Option<&EffectIntent>,
) -> Result<()> {
    if response.pricing != *pricing {
        return Err(Error::Invalid(
            "tarif de réponse différent du registre".into(),
        ));
    }
    if let Some(usage) = &response.usage {
        usage.validate()?;
    }
    if let Some(intent) = intent {
        let registration = &intent.registration;
        if response.destination_id != registration.destination_id
            || response.provider != registration.provider
            || response.model != registration.model
            || response.model_version != registration.model_version
            || response.output.schema_id != registration.output_schema_id
            || response.output.schema_version != registration.output_schema_version
        {
            return Err(Error::Invalid("réponse différente de l'intention".into()));
        }
        let response_bytes = serde_json::to_vec(response)
            .map_err(|_| Error::Invalid("réponse modèle invalide".into()))?
            .len();
        if intent.max_response_bytes == 0 || response_bytes > intent.max_response_bytes as usize {
            return Err(Error::ResourceLimit);
        }
    }
    Ok(())
}

fn settlement_counters(
    budget: &BudgetRow,
    reservation_units: i64,
    actual_units: i64,
) -> Result<(i64, i64)> {
    let extra = actual_units.saturating_sub(reservation_units).max(0);
    let budget_total_with_extra = budget
        .reserved_units
        .checked_add(budget.spent_units)
        .and_then(|total| total.checked_add(extra))
        .ok_or(Error::ResourceLimit)?;
    if budget_total_with_extra > budget.limit_units {
        return Err(Error::BudgetExceeded);
    }
    // Release this hold only; other effects keep their complete reserves.
    let reserved_units = budget
        .reserved_units
        .checked_sub(reservation_units)
        .filter(|units| *units >= 0)
        .ok_or(Error::Internal)?;
    let spent_units = budget
        .spent_units
        .checked_add(actual_units)
        .ok_or(Error::ResourceLimit)?;
    if reserved_units
        .checked_add(spent_units)
        .is_none_or(|total| total > budget.limit_units)
    {
        return Err(Error::BudgetExceeded);
    }
    Ok((reserved_units, spent_units))
}

/// Règle atomique la réserve et la dépense lorsque l'usage est connu; `None` conserve le hold.
async fn settle_usage(
    connection: &mut PgConnection,
    effect: &EffectRow,
    response: &ModelResponse,
) -> Result<Option<i64>> {
    let Some(usage) = response.usage.as_ref() else {
        return Ok(None);
    };
    let Some(actual_units) = effect.intent.registration.pricing.actual_units(usage)? else {
        return Ok(None);
    };
    let budget = Store::budget_for_update(connection, effect.project_id).await?;
    let pricing = &effect.intent.registration.pricing;
    if budget.currency != pricing.currency || budget.unit_scale != pricing.unit_scale {
        return Err(Error::Conflict(
            "dénomination du compte modifiée pendant l'effet".into(),
        ));
    }
    let (reserved_units, spent_units) =
        settlement_counters(&budget, effect.reservation_units, actual_units)?;
    let budget_rows = sqlx::query(
        "UPDATE public.project_budgets SET reserved_units = $2, spent_units = $3, updated_at = now() \
         WHERE project_id = $1",
    )
    .bind(effect.project_id)
    .bind(reserved_units)
    .bind(spent_units)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?
    .rows_affected();
    require_one_row(budget_rows)?;
    let reservation_rows = sqlx::query(
        "UPDATE public.budget_reservations SET status = 'settled', updated_at = now() \
         WHERE id = $1 AND effect_id = $2 AND status = 'held'",
    )
    .bind(effect.reservation_id)
    .bind(effect.id)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?
    .rows_affected();
    require_one_row(reservation_rows)?;
    let metadata = json!({
        "usage": usage,
        "pricing_version": pricing.version,
        "effective_date": pricing.effective_date,
        "currency": pricing.currency,
        "unit_scale": pricing.unit_scale
    });
    sqlx::query(
        "INSERT INTO public.usage_ledger \
         (id, project_id, job_id, reservation_id, units, kind, provider, model, metadata, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, 'settlement', $6, $7, $8, now())",
    )
    .bind(Uuid::new_v4())
    .bind(effect.project_id)
    .bind(effect.job_id)
    .bind(effect.reservation_id)
    .bind(actual_units)
    .bind(&response.provider)
    .bind(&response.model)
    .bind(metadata)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;
    Ok(Some(actual_units))
}

fn decode_data_policy(value: Value) -> Result<DataPolicy> {
    let policy: DataPolicy = serde_json::from_value(value).map_err(|_| Error::Internal)?;
    policy.validate()?;
    Ok(policy)
}

pub(crate) fn database_error(error: sqlx::Error) -> Error {
    match error {
        sqlx::Error::Database(database_error) => match database_error.code().as_deref() {
            Some("57014" | "55P03") => Error::Unavailable,
            Some("P0002") => Error::NotFound,
            Some("42501") => Error::Forbidden,
            Some("40001") => Error::Conflict("concurrent database change".into()),
            Some("23505") => Error::Conflict("resource already exists".into()),
            Some("23514") => Error::Invalid("request violates a data constraint".into()),
            _ => Error::Internal,
        },
        sqlx::Error::RowNotFound => Error::NotFound,
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => {
            Error::Unavailable
        }
        _ => Error::Internal,
    }
}

fn require_one_row(rows: u64) -> Result<()> {
    if rows == 1 {
        Ok(())
    } else {
        Err(Error::Conflict("concurrent state change".into()))
    }
}

impl ModelEffectStore for Store {
    async fn append_chat_delta(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        text: &str,
    ) -> Result<()> {
        self.persist_chat_delta(context, effect_id, text).await
    }
    async fn check_chat_active(&self, context: &ModelEffectContext, effect_id: Uuid) -> Result<()> {
        self.chat_active(context, effect_id).await
    }
    async fn prepare_model_effect(
        &self,
        preparation: ModelEffectPreparation,
    ) -> Result<kyro_domain::model::PreparedModelEffect> {
        self.prepare_effect(preparation).await
    }

    async fn mark_sending(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        request: &ModelRequest,
    ) -> Result<()> {
        self.mark_effect_sending(context, effect_id, request).await
    }

    async fn settle_effect(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        response: ModelResponse,
    ) -> Result<EffectStatus> {
        self.settle_model_effect(context, effect_id, response).await
    }

    async fn mark_unknown(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        failure: ModelFailureCode,
    ) -> Result<()> {
        self.mark_effect_unknown(context, effect_id, failure).await
    }

    async fn release_not_sent(&self, context: &ModelEffectContext, effect_id: Uuid) -> Result<()> {
        self.release_not_sent_effect(context, effect_id).await
    }

    async fn release_prepared_effect(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        request: &ModelRequest,
    ) -> Result<()> {
        self.release_prepared(context, effect_id, request).await
    }

    async fn get_effect(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        effect_id: Uuid,
    ) -> Result<EffectRecordView> {
        Store::get_effect(self, actor_id, project_id, effect_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settlement_releases_only_its_hold_for_smaller_equal_and_larger_usage() {
        let budget = BudgetRow {
            limit_units: 100,
            reserved_units: 30,
            spent_units: 7,
            currency: "SYN".into(),
            unit_scale: 1,
        };
        // The other concurrent effect owns twenty units throughout settlement.
        for actual in [4, 10, 12] {
            assert_eq!(
                settlement_counters(&budget, 10, actual).unwrap(),
                (20, 7 + actual)
            );
        }
        let tight = BudgetRow {
            limit_units: 38,
            ..budget.clone()
        };
        assert!(matches!(
            settlement_counters(&tight, 10, 12),
            Err(Error::BudgetExceeded)
        ));
        assert!(matches!(
            settlement_counters(&budget, 31, 1),
            Err(Error::Internal)
        ));
    }
}
