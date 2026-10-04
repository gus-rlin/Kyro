use std::time::Duration;

use chrono::Utc;
use kyro_domain::{
    Error, Result,
    model::{
        CONSERVATIVE_TOKEN_OVERHEAD, DataPolicy, EffectIntent, EffectStatus, MAX_RESPONSE_BYTES,
        ModelEffectContext, ModelEffectOutcome, ModelEffectPreparation, ModelEffectStore,
        ModelFailureCode, ModelRegistrationSnapshot, ModelRequest, ModelResponse, ModelUsage,
        ReconcileEffectRequest, ReconciledEffectDecision, StructuredModelOutput,
    },
};
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::time::timeout;

use crate::config::{
    Destination, GatewayConfig, RegisteredModel, RegistryModelView, validate_schema_instance,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MODEL_RESPONSE_BODY_BYTES: usize = MAX_RESPONSE_BYTES as usize;

pub struct Gateway {
    config: GatewayConfig,
    client: Client,
}

impl Gateway {
    pub fn new(config: GatewayConfig) -> Result<Self> {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_millis(120_000))
            .pool_max_idle_per_host(2)
            .no_proxy();
        for destination in &config.destinations {
            builder = builder.resolve_to_addrs(&destination.host, &destination.pinned_addresses);
        }
        let client = builder.build().map_err(|_| Error::Unavailable)?;
        Ok(Self { config, client })
    }

    pub fn is_available(&self) -> bool {
        self.config.execution_enabled
            && self
                .config
                .destinations
                .iter()
                .any(|destination| destination.enabled)
    }

    pub fn is_model_available(&self, destination_id: &str, model_id: &str) -> bool {
        self.config.execution_enabled
            && self.config.destinations.iter().any(|destination| {
                destination.id == destination_id
                    && destination.enabled
                    && destination
                        .models
                        .iter()
                        .any(|model| model.registration.model == model_id)
            })
    }

    pub fn registry(&self) -> Vec<RegistryModelView> {
        self.config.registry()
    }

    /// Prévalidation pure à appeler avant d'enregistrer le payload d'un job.
    pub fn validate_request(
        &self,
        request: &ModelRequest,
        policy: &DataPolicy,
    ) -> Result<ModelRegistrationSnapshot> {
        request.validate_shape()?;
        let (destination, model) = self
            .registered_model(&request.destination_id, &request.model)
            .ok_or(Error::Unavailable)?;
        if !destination.admissible {
            return Err(Error::Unavailable);
        }
        let (input_bytes, conservative_input_tokens) = request_measurements(request, model)?;
        policy.authorize_request(
            request,
            input_bytes,
            conservative_input_tokens,
            model.registration.retention_seconds,
        )?;
        let _ = model
            .registration
            .pricing
            .reservation_units(conservative_input_tokens, request.max_output_tokens)?;
        Ok(model.registration.clone())
    }

    /// Valide purement une preuve contre le registre et le snapshot chargés depuis un effet.
    /// L'admission/la réconciliation durable se fait séparément par un job worker.
    pub fn validate_reconciliation(
        &self,
        intent: &EffectIntent,
        request: &ReconcileEffectRequest,
    ) -> Result<()> {
        request.validate()?;
        let registration = &intent.registration;
        if registration.provider_kind != kyro_domain::model::ModelProviderKind::Synthetic {
            return Err(Error::Forbidden);
        }
        let (_, model) = self
            .registered_model(&registration.destination_id, &registration.model)
            .ok_or(Error::Unavailable)?;
        if model.registration != *registration {
            return Err(Error::Conflict(
                "le registre synthétique a changé depuis l'effet".into(),
            ));
        }
        if let ReconciledEffectDecision::Processed { response } = &request.decision {
            validate_reconciled_response(response, registration, model, intent.max_response_bytes)?;
        }
        Ok(())
    }

    /// Réserve et persiste l'intention avant de passer à l'état `sending` et d'ouvrir le socket.
    pub async fn execute_model_effect<S>(
        &self,
        store: &S,
        context: ModelEffectContext,
        request: ModelRequest,
    ) -> Result<ModelEffectOutcome>
    where
        S: ModelEffectStore + Sync,
    {
        if !self.config.execution_enabled {
            return Err(Error::Unavailable);
        }
        request.validate_shape()?;
        let (destination, model) = self
            .registered_model(&request.destination_id, &request.model)
            .ok_or(Error::Unavailable)?;
        let can_send = destination.enabled;
        let input_json = serde_json::to_vec(&request.input)
            .map_err(|_| Error::Invalid("entrée de modèle invalide".into()))?;
        let (input_bytes, conservative_input_tokens) = request_measurements(&request, model)?;
        let request_fingerprint = fingerprint(&request, &model.registration)?;
        let reservation_units = model
            .registration
            .pricing
            .reservation_units(conservative_input_tokens, request.max_output_tokens)?;

        let prepared = store
            .prepare_model_effect(ModelEffectPreparation {
                context: context.clone(),
                request: request.clone(),
                allow_new_effect: can_send,
                fingerprint: request_fingerprint,
                input_bytes,
                conservative_input_tokens,
                reservation_units,
                registration: model.registration.clone(),
            })
            .await?;
        if prepared.status == EffectStatus::Succeeded {
            return Ok(ModelEffectOutcome {
                effect_id: prepared.intent.id,
                status: EffectStatus::Succeeded,
                response: prepared.existing_response,
            });
        }
        if matches!(
            prepared.status,
            EffectStatus::Sending | EffectStatus::Unknown
        ) {
            return Ok(ModelEffectOutcome {
                effect_id: prepared.intent.id,
                status: EffectStatus::Unknown,
                response: None,
            });
        }
        if matches!(
            prepared.status,
            EffectStatus::Failed | EffectStatus::Cancelled
        ) {
            return Ok(ModelEffectOutcome {
                effect_id: prepared.intent.id,
                status: prepared.status,
                response: None,
            });
        }
        if prepared.status != EffectStatus::Prepared {
            return Err(Error::Conflict("état d'effet non exécutable".into()));
        }
        if !can_send {
            return Err(Error::Unavailable);
        }

        let policy_check = prepared.data_policy.authorize_request(
            &request,
            input_bytes,
            conservative_input_tokens,
            model.registration.retention_seconds,
        );
        if let Err(error) = policy_check {
            store
                .release_prepared_effect(&context, prepared.intent.id, &request)
                .await?;
            return Err(error);
        }

        // Le CAS et la réservation sont terminés et validés avant toute requête fournisseur.
        store
            .mark_sending(&context, prepared.intent.id, &request)
            .await?;
        let remaining = (context.deadline - Utc::now()).to_std().unwrap_or_default();
        if remaining.is_zero() {
            store.release_not_sent(&context, prepared.intent.id).await?;
            return Err(Error::Unavailable);
        }
        let deadline = Duration::from_millis(u64::from(request.deadline_ms)).min(remaining);
        let call = self.send_request(
            destination,
            model,
            &request,
            &input_json,
            prepared.data_policy.limits.max_response_bytes,
        );
        let response = match timeout(deadline, call).await {
            Ok(Ok(response)) if Utc::now() < context.deadline => response,
            Ok(Err(SendFailure::DefinitelyNotSent)) => {
                store.release_not_sent(&context, prepared.intent.id).await?;
                return Err(Error::Unavailable);
            }
            Ok(Err(failure)) => {
                store
                    .mark_unknown(&context, prepared.intent.id, failure.code())
                    .await?;
                return Ok(ModelEffectOutcome {
                    effect_id: prepared.intent.id,
                    status: EffectStatus::Unknown,
                    response: None,
                });
            }
            Ok(Ok(_)) | Err(_) => {
                store
                    .mark_unknown(
                        &context,
                        prepared.intent.id,
                        ModelFailureCode::TransportUncertain,
                    )
                    .await?;
                return Ok(ModelEffectOutcome {
                    effect_id: prepared.intent.id,
                    status: EffectStatus::Unknown,
                    response: None,
                });
            }
        };
        let status = store
            .settle_effect(&context, prepared.intent.id, response.clone())
            .await?;
        Ok(ModelEffectOutcome {
            effect_id: prepared.intent.id,
            status,
            response: (status == EffectStatus::Succeeded).then_some(response),
        })
    }

    fn registered_model(
        &self,
        destination_id: &str,
        model_id: &str,
    ) -> Option<(&Destination, &RegisteredModel)> {
        let destination = self
            .config
            .destinations
            .iter()
            .find(|value| value.id == destination_id)?;
        let model = destination
            .models
            .iter()
            .find(|value| value.registration.model == model_id)?;
        Some((destination, model))
    }

    async fn send_request(
        &self,
        destination: &Destination,
        model: &RegisteredModel,
        request: &ModelRequest,
        input_json: &[u8],
        project_max_response_bytes: u32,
    ) -> std::result::Result<ModelResponse, SendFailure> {
        let endpoint = destination
            .base_url
            .join("chat/completions")
            .map_err(|_| SendFailure::DefinitelyNotSent)?;
        let schema = response_schema(model);
        let body = json!({
            "model": model.registration.model,
            "messages": [{
                "role": "user",
                "content": std::str::from_utf8(input_json).map_err(|_| SendFailure::DefinitelyNotSent)?
            }],
            "max_tokens": request.max_output_tokens,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": model.registration.output_schema_id,
                    "strict": true,
                    "schema": schema
                }
            }
        });
        let mut http_request = self.client.post(endpoint);
        if let Some(secret) = &destination.secret {
            http_request = http_request.bearer_auth(secret.expose());
        }
        let response = http_request
            .header(header::CONTENT_TYPE, "application/json")
            .json(&body)
            .timeout(Duration::from_millis(u64::from(request.deadline_ms)))
            .send()
            .await
            .map_err(|error| {
                if error.is_connect() {
                    SendFailure::DefinitelyNotSent
                } else {
                    SendFailure::TransportUncertain
                }
            })?;
        if response.status() != StatusCode::OK {
            return Err(SendFailure::ProviderStatusUncertain);
        }
        let maximum = usize::try_from(
            model
                .limits
                .max_response_bytes
                .min(project_max_response_bytes),
        )
        .unwrap_or(MAX_MODEL_RESPONSE_BODY_BYTES)
        .min(MAX_MODEL_RESPONSE_BODY_BYTES);
        if response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
        {
            return Err(SendFailure::ResponseTooLarge);
        }
        let mut response = response;
        let mut bytes = Vec::with_capacity(maximum.min(16_384));
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| SendFailure::TransportUncertain)?
        {
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > maximum)
            {
                return Err(SendFailure::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        let provider_response: OpenAiResponse =
            serde_json::from_slice(&bytes).map_err(|_| SendFailure::InvalidResponse)?;
        parse_response(provider_response, destination, model)
    }
}

fn fingerprint(
    request: &ModelRequest,
    registration: &kyro_domain::model::ModelRegistrationSnapshot,
) -> Result<[u8; 32]> {
    #[derive(serde::Serialize)]
    struct FingerprintMaterial<'a> {
        request: &'a ModelRequest,
        registration: &'a kyro_domain::model::ModelRegistrationSnapshot,
    }
    let bytes = serde_json::to_vec(&FingerprintMaterial {
        request,
        registration,
    })
    .map_err(|_| Error::Invalid("requête de modèle invalide".into()))?;
    Ok(Sha256::digest(bytes).into())
}

fn request_measurements(request: &ModelRequest, model: &RegisteredModel) -> Result<(u32, u32)> {
    let input_bytes = serde_json::to_vec(&request.input)
        .map_err(|_| Error::Invalid("entrée de modèle invalide".into()))?
        .len();
    if input_bytes > model.limits.max_input_bytes as usize
        || request.max_output_tokens > model.limits.max_output_tokens
        || request.deadline_ms > model.limits.max_deadline_ms
    {
        return Err(Error::ResourceLimit);
    }
    let input_bytes = u32::try_from(input_bytes).map_err(|_| Error::ResourceLimit)?;
    let conservative = input_bytes
        .checked_add(CONSERVATIVE_TOKEN_OVERHEAD)
        .ok_or(Error::ResourceLimit)?;
    if conservative > model.limits.max_input_tokens {
        return Err(Error::ResourceLimit);
    }
    Ok((input_bytes, conservative))
}

fn response_schema(model: &RegisteredModel) -> Value {
    json!({
        "type": "object",
        "properties": {
            "schema_id": { "type": "string", "const": model.registration.output_schema_id, "maxLength": 128 },
            "schema_version": { "type": "string", "const": model.registration.output_schema_version, "maxLength": 128 },
            "data": model.output_schema
        },
        "required": ["schema_id", "schema_version", "data"],
        "additionalProperties": false
    })
}

fn parse_response(
    response: OpenAiResponse,
    destination: &Destination,
    model: &RegisteredModel,
) -> std::result::Result<ModelResponse, SendFailure> {
    if response.choices.len() != 1 {
        return Err(SendFailure::InvalidResponse);
    }
    if response
        .model
        .as_deref()
        .is_some_and(|actual| actual != model.registration.model)
    {
        return Err(SendFailure::InvalidResponse);
    }
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(SendFailure::InvalidResponse)?;
    if choice.finish_reason.as_deref() != Some("stop") {
        return Err(SendFailure::InvalidResponse);
    }
    let structured: StructuredModelOutput =
        serde_json::from_str(&choice.message.content).map_err(|_| SendFailure::InvalidResponse)?;
    if structured.schema_id != model.registration.output_schema_id
        || structured.schema_version != model.registration.output_schema_version
    {
        return Err(SendFailure::InvalidResponse);
    }
    let wrapped = json!({
        "schema_id": structured.schema_id,
        "schema_version": structured.schema_version,
        "data": structured.data
    });
    validate_schema_instance(&response_schema(model), &wrapped)
        .map_err(|_| SendFailure::InvalidResponse)?;
    let usage = response.usage.map(OpenAiUsage::into_usage).transpose()?;
    Ok(ModelResponse {
        destination_id: destination.id.clone(),
        provider: destination.provider.clone(),
        model: model.registration.model.clone(),
        model_version: model.registration.model_version.clone(),
        output: structured,
        usage,
        pricing: model.registration.pricing.clone(),
    })
}

fn validate_reconciled_response(
    response: &ModelResponse,
    registration: &kyro_domain::model::ModelRegistrationSnapshot,
    model: &RegisteredModel,
    max_response_bytes: u32,
) -> Result<()> {
    if response.destination_id != registration.destination_id
        || response.provider != registration.provider
        || response.model != registration.model
        || response.model_version != registration.model_version
        || response.pricing != registration.pricing
        || response.output.schema_id != registration.output_schema_id
        || response.output.schema_version != registration.output_schema_version
    {
        return Err(Error::Invalid(
            "preuve synthétique incompatible avec l'effet".into(),
        ));
    }
    if let Some(usage) = &response.usage {
        usage.validate()?;
    }
    let response_bytes = serde_json::to_vec(response)
        .map_err(|_| Error::Invalid("preuve synthétique invalide".into()))?
        .len();
    if response_bytes > max_response_bytes as usize {
        return Err(Error::ResourceLimit);
    }
    let wrapped = json!({
        "schema_id": response.output.schema_id,
        "schema_version": response.output.schema_version,
        "data": response.output.data
    });
    validate_schema_instance(&response_schema(model), &wrapped)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    model: Option<String>,
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    prompt_tokens_details: Option<OpenAiPromptDetails>,
}

#[derive(Debug, Deserialize)]
struct OpenAiPromptDetails {
    cached_tokens: Option<i64>,
}

impl OpenAiUsage {
    fn into_usage(self) -> std::result::Result<ModelUsage, SendFailure> {
        let usage = ModelUsage {
            input_tokens: self.prompt_tokens,
            output_tokens: self.completion_tokens,
            cached_input_tokens: self
                .prompt_tokens_details
                .and_then(|details| details.cached_tokens),
        };
        usage.validate().map_err(|_| SendFailure::InvalidUsage)?;
        Ok(usage)
    }
}

#[derive(Clone, Copy)]
enum SendFailure {
    DefinitelyNotSent,
    TransportUncertain,
    ProviderStatusUncertain,
    InvalidResponse,
    ResponseTooLarge,
    InvalidUsage,
}

impl SendFailure {
    fn code(self) -> ModelFailureCode {
        match self {
            Self::DefinitelyNotSent => ModelFailureCode::TransportUncertain,
            Self::TransportUncertain => ModelFailureCode::TransportUncertain,
            Self::ProviderStatusUncertain => ModelFailureCode::ProviderStatusUncertain,
            Self::InvalidResponse => ModelFailureCode::InvalidResponse,
            Self::ResponseTooLarge => ModelFailureCode::ResponseTooLarge,
            Self::InvalidUsage => ModelFailureCode::InvalidUsage,
        }
    }
}
