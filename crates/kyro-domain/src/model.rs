//! Contrats explicites pour les appels de modèles, leurs effets et leur comptabilité.

use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{Error, Result};

pub const MAX_DESTINATION_ID_BYTES: usize = 128;
pub const MAX_MODEL_ID_BYTES: usize = 256;
pub const MAX_REGISTRY_ID_BYTES: usize = 128;
pub const MAX_POLICY_DESTINATIONS: usize = 64;
pub const MAX_POLICY_CATEGORIES: usize = 16;
pub const MAX_POLICY_PURPOSES: usize = 32;
pub const MAX_RETENTION_SECONDS: u32 = 31_536_000;
pub const MAX_INPUT_BYTES: u32 = 1_048_576;
pub const MAX_INPUT_TOKENS: u32 = 262_144;
pub const MAX_OUTPUT_TOKENS: u32 = 32_768;
pub const MAX_DEADLINE_MS: u32 = 120_000;
/// Body provider cap; the persisted effect result must stay below the database's 64 KiB bound.
pub const MAX_RESPONSE_BYTES: u32 = 48_000;
pub const TOKEN_RATE_DENOMINATOR: i64 = 1_000_000;
pub const CONSERVATIVE_TOKEN_OVERHEAD: u32 = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataCategory {
    UserRequest,
    ProjectSpecification,
    ProjectSource,
    Diagnostics,
    EndUserData,
    Secret,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurpose {
    Conversation,
    Planning,
    Generation,
    Review,
    Diagnostics,
    Summarization,
    StructuredExtraction,
    Translation,
    Embedding,
}

/// Données structurées déclarées par l'appelant; elles ne contiennent ni destination URL ni clé.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInput {
    pub purpose: ModelPurpose,
    pub categories: BTreeSet<DataCategory>,
    pub content: Value,
}

/// Destination et modèle sont des identifiants du registre serveur, jamais des URLs.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRequest {
    pub destination_id: String,
    pub model: String,
    pub input: ModelInput,
    pub max_output_tokens: u32,
    /// Délai maximal de cet appel, en millisecondes depuis son admission.
    pub deadline_ms: u32,
}

impl ModelRequest {
    pub fn validate_shape(&self) -> Result<()> {
        if !valid_registry_id(&self.destination_id, MAX_DESTINATION_ID_BYTES) {
            return Err(Error::Invalid("identifiant de destination invalide".into()));
        }
        if !valid_model_id(&self.model) {
            return Err(Error::Invalid("identifiant de modèle invalide".into()));
        }
        if self.input.categories.is_empty() {
            return Err(Error::Invalid(
                "au moins une catégorie de données est requise".into(),
            ));
        }
        if self.max_output_tokens == 0 || self.max_output_tokens > MAX_OUTPUT_TOKENS {
            return Err(Error::ResourceLimit);
        }
        if self.deadline_ms == 0 || self.deadline_ms > MAX_DEADLINE_MS {
            return Err(Error::ResourceLimit);
        }
        reject_recognizable_secrets(&self.input.content)?;
        Ok(())
    }
}

/// Limites finies par projet. La politique par défaut interdit toute destination.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPolicyLimits {
    pub max_input_bytes: u32,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub max_deadline_ms: u32,
    pub max_response_bytes: u32,
    /// Durée maximale de conservation déclarée par le fournisseur; zéro interdit toute rétention.
    pub max_retention_seconds: u32,
}

impl Default for ModelPolicyLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 65_536,
            max_input_tokens: 16_384,
            max_output_tokens: 2_048,
            max_deadline_ms: 30_000,
            max_response_bytes: 48_000,
            max_retention_seconds: 0,
        }
    }
}

impl ModelPolicyLimits {
    pub fn validate(&self) -> Result<()> {
        if self.max_input_bytes == 0
            || self.max_input_bytes > MAX_INPUT_BYTES
            || self.max_input_tokens == 0
            || self.max_input_tokens > MAX_INPUT_TOKENS
            || self.max_output_tokens == 0
            || self.max_output_tokens > MAX_OUTPUT_TOKENS
            || self.max_deadline_ms == 0
            || self.max_deadline_ms > MAX_DEADLINE_MS
            || self.max_response_bytes == 0
            || self.max_response_bytes > MAX_RESPONSE_BYTES
            || self.max_retention_seconds > MAX_RETENTION_SECONDS
        {
            return Err(Error::ResourceLimit);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DataPolicy {
    /// Explicit consent for conversations whose provider retention duration is unknown.
    /// Absent/false keeps the historical fail-closed rule.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_unknown_provider_retention: bool,
    /// Separate, explicit opt-in for agent purposes; historical chat consent never enables them.
    /// An empty set preserves the refusal of providers whose retention is unknown.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub accepted_unknown_retention_purposes: BTreeSet<ModelPurpose>,
    pub allowed_destinations: BTreeSet<String>,
    pub allowed_categories: BTreeSet<DataCategory>,
    pub allowed_purposes: BTreeSet<ModelPurpose>,
    pub limits: ModelPolicyLimits,
}

impl Default for DataPolicy {
    fn default() -> Self {
        Self {
            allow_unknown_provider_retention: false,
            accepted_unknown_retention_purposes: Default::default(),
            allowed_destinations: BTreeSet::new(),
            allowed_categories: BTreeSet::new(),
            allowed_purposes: BTreeSet::new(),
            limits: ModelPolicyLimits::default(),
        }
    }
}

impl DataPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.allowed_destinations.len() > MAX_POLICY_DESTINATIONS
            || self.allowed_categories.len() > MAX_POLICY_CATEGORIES
            || self.allowed_purposes.len() > MAX_POLICY_PURPOSES
            || self
                .allowed_destinations
                .iter()
                .any(|id| !valid_registry_id(id, MAX_DESTINATION_ID_BYTES))
            || self.allowed_categories.contains(&DataCategory::Secret)
            || !self
                .accepted_unknown_retention_purposes
                .is_subset(&self.allowed_purposes)
            || self
                .accepted_unknown_retention_purposes
                .iter()
                .any(|purpose| {
                    !matches!(
                        purpose,
                        ModelPurpose::Planning | ModelPurpose::Generation | ModelPurpose::Review
                    )
                })
        {
            return Err(Error::Invalid("politique de données invalide".into()));
        }
        self.limits.validate()
    }

    pub fn authorize_request(
        &self,
        request: &ModelRequest,
        input_bytes: u32,
        conservative_input_tokens: u32,
        provider_retention_seconds: Option<u32>,
    ) -> Result<()> {
        self.validate()?;
        request.validate_shape()?;

        if request.input.categories.contains(&DataCategory::Secret)
            || !self.allowed_destinations.contains(&request.destination_id)
            || !request.input.categories.is_subset(&self.allowed_categories)
            || !self.allowed_purposes.contains(&request.input.purpose)
        {
            return Err(Error::Forbidden);
        }
        if input_bytes > self.limits.max_input_bytes
            || conservative_input_tokens > self.limits.max_input_tokens
            || request.max_output_tokens > self.limits.max_output_tokens
            || request.deadline_ms > self.limits.max_deadline_ms
        {
            return Err(Error::ResourceLimit);
        }
        match provider_retention_seconds {
            Some(seconds)
                if seconds <= self.limits.max_retention_seconds
                    && seconds <= MAX_RETENTION_SECONDS => {}
            None if (self.allow_unknown_provider_retention
                && request.input.purpose == ModelPurpose::Conversation)
                || self
                    .accepted_unknown_retention_purposes
                    .contains(&request.input.purpose) => {}
            _ => return Err(Error::Forbidden),
        }
        Ok(())
    }
}

/// Tarifs versionnés en unités entières par million de tokens, sans nombres flottants.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PricingSnapshot {
    pub version: String,
    pub effective_date: String,
    pub currency: String,
    pub unit: String,
    pub unit_scale: i64,
    pub input_units_per_million_tokens: i64,
    pub output_units_per_million_tokens: i64,
}

impl PricingSnapshot {
    pub fn validate(&self) -> Result<()> {
        if !valid_registry_id(&self.version, MAX_REGISTRY_ID_BYTES)
            || NaiveDate::parse_from_str(&self.effective_date, "%Y-%m-%d").is_err()
            || self.currency.len() != 3
            || !self.currency.bytes().all(|b| b.is_ascii_uppercase())
            || !valid_registry_id(&self.unit, MAX_REGISTRY_ID_BYTES)
            || self.unit_scale <= 0
            || self.input_units_per_million_tokens < 0
            || self.output_units_per_million_tokens < 0
        {
            return Err(Error::Invalid("instantané tarifaire invalide".into()));
        }
        Ok(())
    }

    /// Réserve avec arrondi supérieur pour ne jamais sous-estimer le plafond demandé.
    pub fn reservation_units(&self, input_tokens: u32, output_tokens: u32) -> Result<i64> {
        self.validate()?;
        let input = cost_component(self.input_units_per_million_tokens, i64::from(input_tokens))?;
        let output = cost_component(
            self.output_units_per_million_tokens,
            i64::from(output_tokens),
        )?;
        Ok(input
            .checked_add(output)
            .ok_or(Error::ResourceLimit)?
            .max(1))
    }

    /// Retourne `None` si l'un des compteurs de tokens nécessaires manque.
    pub fn actual_units(&self, usage: &ModelUsage) -> Result<Option<i64>> {
        self.validate()?;
        usage.validate()?;
        let (Some(input_tokens), Some(output_tokens)) = (usage.input_tokens, usage.output_tokens)
        else {
            return Ok(None);
        };
        let input = cost_component(self.input_units_per_million_tokens, input_tokens)?;
        let output = cost_component(self.output_units_per_million_tokens, output_tokens)?;
        Ok(Some(input.checked_add(output).ok_or(Error::ResourceLimit)?))
    }
}

fn cost_component(rate: i64, tokens: i64) -> Result<i64> {
    if rate < 0 || tokens < 0 {
        return Err(Error::Invalid("compteur ou tarif négatif".into()));
    }
    let numerator = rate
        .checked_mul(tokens)
        .and_then(|value| value.checked_add(TOKEN_RATE_DENOMINATOR - 1))
        .ok_or(Error::ResourceLimit)?;
    Ok(numerator / TOKEN_RATE_DENOMINATOR)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
}

impl ModelUsage {
    pub fn validate(&self) -> Result<()> {
        if self.input_tokens.is_some_and(|value| value < 0)
            || self.output_tokens.is_some_and(|value| value < 0)
            || self.cached_input_tokens.is_some_and(|value| value < 0)
            || matches!((self.cached_input_tokens, self.input_tokens), (Some(cached), Some(input)) if cached > input)
            || (self.cached_input_tokens.is_some() && self.input_tokens.is_none())
        {
            return Err(Error::Invalid("usage fournisseur incohérente".into()));
        }
        Ok(())
    }
}

/// Sortie JSON fermée par une version de schéma connue du registre.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredModelOutput {
    pub schema_id: String,
    pub schema_version: String,
    pub data: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelResponse {
    pub destination_id: String,
    pub provider: String,
    pub model: String,
    pub model_version: Option<String>,
    /// Opaque provider receipt, never an authentication credential. Absent on historical results.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_provider_request_id"
    )]
    pub provider_request_id: Option<String>,
    pub output: StructuredModelOutput,
    /// `None` signifie que le fournisseur n'a pas fourni d'usage vérifiable.
    pub usage: Option<ModelUsage>,
    pub pricing: PricingSnapshot,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationStatus {
    Held,
    Settled,
    Released,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSnapshot {
    pub project_id: Uuid,
    pub configuration_version: i64,
    pub limit_units: i64,
    pub reserved_units: i64,
    pub spent_units: i64,
    pub currency: String,
    pub unit_scale: i64,
}

impl BudgetSnapshot {
    pub fn validate(&self) -> Result<()> {
        if self.limit_units < 0
            || self.configuration_version < 0
            || self.reserved_units < 0
            || self.spent_units < 0
            || self.unit_scale <= 0
            || self.currency.len() != 3
            || !self.currency.bytes().all(|byte| byte.is_ascii_uppercase())
            || self.reserved_units.checked_add(self.spent_units).is_none()
            || self.reserved_units + self.spent_units > self.limit_units
        {
            return Err(Error::Invalid("instantané de budget invalide".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectReconciliationReceipt {
    pub evidence_id: String,
    pub evidence_fingerprint: [u8; 32],
    pub decision: ReconciliationDecisionKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationDecisionKind {
    Processed,
    NotProcessed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectStoredResult {
    pub response: Option<ModelResponse>,
    pub reconciliation: Option<EffectReconciliationReceipt>,
    pub failure_code: Option<ModelFailureCode>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectRecordView {
    pub effect_id: Uuid,
    pub job_id: Uuid,
    pub project_id: Uuid,
    pub generation: i64,
    pub destination: String,
    pub status: EffectStatus,
    pub intent: EffectIntentView,
    pub result: Option<ModelResponse>,
    pub reconciliation: Option<EffectReconciliationReceipt>,
    pub failure_code: Option<ModelFailureCode>,
    pub reserved_units: i64,
    pub reservation_status: ReservationStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectListPage {
    pub items: Vec<EffectRecordView>,
    pub next_before: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectStatus {
    Prepared,
    Sending,
    Succeeded,
    Failed,
    Unknown,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderKind {
    Synthetic,
    Cloud,
}

/// Trusted wire format; historical registrations retain their structured contract.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelOutputMode {
    #[default]
    StructuredJson,
    TextChat,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChatInput {
    pub messages: Vec<ChatMessage>,
    pub context_tokens: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRegistrationSnapshot {
    #[serde(default, skip_serializing_if = "is_structured_mode")]
    pub output_mode: ModelOutputMode,
    #[serde(default, skip_serializing_if = "ModelProtocol::is_chat")]
    pub protocol: ModelProtocol,
    pub destination_id: String,
    pub provider: String,
    pub provider_kind: ModelProviderKind,
    pub model: String,
    pub model_version: Option<String>,
    pub output_schema_id: String,
    pub output_schema_version: String,
    pub output_schema_hash: [u8; 32],
    pub pricing: PricingSnapshot,
    /// `None` means unknown retention; refused unless explicitly accepted by the conversation policy.
    pub retention_seconds: Option<u32>,
}

fn is_structured_mode(mode: &ModelOutputMode) -> bool {
    *mode == ModelOutputMode::StructuredJson
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The protocol belongs to the trusted registration and its effect fingerprint.
/// Omitted chat preserves existing P1 snapshots byte for byte.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProtocol {
    #[default]
    Chat,
    Embeddings,
}
impl ModelProtocol {
    pub fn is_chat(&self) -> bool {
        *self == Self::Chat
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEffectContext {
    pub project_id: Uuid,
    pub actor_id: Uuid,
    pub job_id: Uuid,
    pub source_revision: i64,
    pub generation: i64,
    pub lease_owner: Uuid,
    pub lease_until: DateTime<Utc>,
    /// Job-wide deadline, distinct from the bounded provider-call duration in ModelRequest.
    pub deadline: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEffectPreparation {
    pub context: ModelEffectContext,
    pub request: ModelRequest,
    /// Worker-only gate: when false, the store may return an existing effect but must not create one.
    pub allow_new_effect: bool,
    pub fingerprint: [u8; 32],
    pub input_bytes: u32,
    pub conservative_input_tokens: u32,
    pub reservation_units: i64,
    pub registration: ModelRegistrationSnapshot,
}

/// Snapshot écrit dans `effects.intent`; aucun contenu de prompt ou secret n'y figure.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectIntent {
    pub id: Uuid,
    pub project_id: Uuid,
    pub job_id: Uuid,
    pub destination_id: String,
    pub fingerprint: [u8; 32],
    pub reservation_id: Uuid,
    pub reserved_units: i64,
    pub request_purpose: ModelPurpose,
    pub request_categories: BTreeSet<DataCategory>,
    pub input_bytes: u32,
    pub conservative_input_tokens: u32,
    pub max_output_tokens: u32,
    /// Limite de réponse du projet évaluée au moment de la préparation.
    pub max_response_bytes: u32,
    pub deadline_ms: u32,
    pub registration: ModelRegistrationSnapshot,
}

/// Public projection; the input fingerprint stays in the durable EffectIntent only.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectIntentView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub job_id: Uuid,
    pub destination_id: String,
    pub reservation_id: Uuid,
    pub reserved_units: i64,
    pub request_purpose: ModelPurpose,
    pub request_categories: BTreeSet<DataCategory>,
    pub input_bytes: u32,
    pub conservative_input_tokens: u32,
    pub max_output_tokens: u32,
    /// Limite de réponse du projet évaluée au moment de la préparation.
    pub max_response_bytes: u32,
    pub deadline_ms: u32,
    pub registration: ModelRegistrationSnapshot,
}

impl From<EffectIntent> for EffectIntentView {
    fn from(intent: EffectIntent) -> Self {
        Self {
            id: intent.id,
            project_id: intent.project_id,
            job_id: intent.job_id,
            destination_id: intent.destination_id,
            reservation_id: intent.reservation_id,
            reserved_units: intent.reserved_units,
            request_purpose: intent.request_purpose,
            request_categories: intent.request_categories,
            input_bytes: intent.input_bytes,
            conservative_input_tokens: intent.conservative_input_tokens,
            max_output_tokens: intent.max_output_tokens,
            max_response_bytes: intent.max_response_bytes,
            deadline_ms: intent.deadline_ms,
            registration: intent.registration,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedModelEffect {
    pub intent: EffectIntent,
    pub status: EffectStatus,
    pub data_policy: DataPolicy,
    /// Réponse déjà acquise; évite un nouvel appel quand le worker doit seulement finir son CAS.
    pub existing_response: Option<ModelResponse>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFailureCode {
    TransportUncertain,
    ProviderStatusUncertain,
    InvalidResponse,
    ResponseTooLarge,
    InvalidUsage,
    PersistenceUncertain,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEffectOutcome {
    pub effect_id: Uuid,
    pub status: EffectStatus,
    pub response: Option<ModelResponse>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectReconciliationContext {
    pub project_id: Uuid,
    /// Opérateur autorisé à rapprocher l'effet; distinct de l'auteur du job cible.
    pub actor_id: Uuid,
    pub effect_id: Uuid,
    /// Job cible dérivé du lien d'effet après la prélecture autorisée, jamais fourni par le client.
    pub target_job_id: Uuid,
    /// Révision retournée par le verrou de projet pris avant le job cible.
    pub current_revision: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReconciledEffectDecision {
    Processed { response: ModelResponse },
    NotProcessed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileEffectRequest {
    /// Clé opaque de la preuve; sa répétition identique est idempotente.
    pub evidence_id: String,
    pub decision: ReconciledEffectDecision,
}

impl ReconcileEffectRequest {
    pub fn validate(&self) -> Result<()> {
        if self.evidence_id.trim().is_empty() || self.evidence_id.len() > 256 {
            return Err(Error::Invalid("identifiant de preuve invalide".into()));
        }
        if let ReconciledEffectDecision::Processed { response } = &self.decision {
            let usage = response
                .usage
                .as_ref()
                .ok_or_else(|| Error::Invalid("usage complet absent du rapprochement".into()))?;
            usage.validate()?;
            if usage.input_tokens.is_none() || usage.output_tokens.is_none() {
                return Err(Error::Invalid(
                    "usage complet requis pour rapprocher un effet traité".into(),
                ));
            }
            if response.destination_id.is_empty()
                || response.provider.is_empty()
                || response.model.is_empty()
                || response.output.schema_id.is_empty()
                || response.output.schema_version.is_empty()
            {
                return Err(Error::Invalid("réponse de rapprochement invalide".into()));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectReconcileOutcome {
    pub effect_id: Uuid,
    pub job_id: Uuid,
    pub generation: i64,
    pub status: EffectStatus,
}

/// Port persistant implémenté par `kyro-store`; les transactions finissent avant toute requête HTTP.
#[allow(async_fn_in_trait)]
pub trait ModelEffectStore: Send + Sync {
    /// Only a current worker lease may publish provisional text. Defaults fail closed.
    async fn append_chat_delta(
        &self,
        _context: &ModelEffectContext,
        _effect_id: Uuid,
        _text: &str,
    ) -> Result<()> {
        Err(Error::Unavailable)
    }
    async fn check_chat_active(
        &self,
        _context: &ModelEffectContext,
        _effect_id: Uuid,
    ) -> Result<()> {
        Err(Error::Unavailable)
    }
    async fn prepare_model_effect(
        &self,
        preparation: ModelEffectPreparation,
    ) -> Result<PreparedModelEffect>;
    async fn mark_sending(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        request: &ModelRequest,
    ) -> Result<()>;
    async fn settle_effect(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        response: ModelResponse,
    ) -> Result<EffectStatus>;
    async fn mark_unknown(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        failure: ModelFailureCode,
    ) -> Result<()>;
    async fn release_not_sent(&self, context: &ModelEffectContext, effect_id: Uuid) -> Result<()>;
    async fn release_prepared_effect(
        &self,
        context: &ModelEffectContext,
        effect_id: Uuid,
        request: &ModelRequest,
    ) -> Result<()>;
    async fn get_effect(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        effect_id: Uuid,
    ) -> Result<EffectRecordView>;
}

fn valid_registry_id(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && matches!(value.as_bytes()[0], b'a'..=b'z')
}

fn deserialize_provider_request_id<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    if value
        .as_deref()
        .is_some_and(|id| !valid_provider_request_id(id))
    {
        return Err(serde::de::Error::custom("identifiant fournisseur invalide"));
    }
    Ok(value)
}

pub fn valid_provider_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn valid_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_ID_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':')
        })
}

/// A narrow deny-list, not a complete DLP system. Runs before queue persistence and at execution.
pub fn reject_recognizable_secrets(value: &Value) -> Result<()> {
    let mut pending = vec![value];
    let mut nodes = 0_usize;
    while let Some(value) = pending.pop() {
        nodes += 1;
        if nodes > 16_384 {
            return Err(Error::ResourceLimit);
        }
        match value {
            Value::String(text) => {
                if text.contains("-----BEGIN PRIVATE KEY-----")
                    || text.contains("-----BEGIN RSA PRIVATE KEY-----")
                    || text.contains("-----BEGIN OPENSSH PRIVATE KEY-----")
                    || text
                        .split(|c: char| {
                            !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                        })
                        .any(|word| {
                            (word.starts_with("sk-") && word.len() >= 24)
                                || (word.starts_with("v1.")
                                    && word.len() >= 64
                                    && word.matches('.').count() == 2
                                    && word.bytes().all(|b| {
                                        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')
                                    }))
                        })
                {
                    return Err(Error::Forbidden);
                }
            }
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => {
                for (key, child) in values {
                    if matches!(
                        key.to_ascii_lowercase().as_str(),
                        "api_key"
                            | "apikey"
                            | "access_token"
                            | "private_key"
                            | "client_secret"
                            | "authorization"
                    ) && child.as_str().is_some_and(|text| !text.is_empty())
                    {
                        return Err(Error::Forbidden);
                    }
                    pending.push(child);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizable_secrets_are_refused_without_echoing_content() {
        for content in [
            serde_json::json!({"api_key":"fake-canary-123"}),
            serde_json::json!({"nested":["-----BEGIN PRIVATE KEY-----"]}),
            serde_json::json!({"text":format!("v1.{}.{}", "x".repeat(40), "y".repeat(40))}),
            serde_json::json!({"text":"sk-invented-canary-abcdefghijklmnopqrstuvwxyz"}),
        ] {
            let mut input = request();
            input.input.content = content;
            assert_eq!(input.validate_shape(), Err(Error::Forbidden));
        }
        assert!(request().validate_shape().is_ok());
    }

    fn request() -> ModelRequest {
        ModelRequest {
            destination_id: "synthetic-local".into(),
            model: "synthetic-structured".into(),
            input: ModelInput {
                purpose: ModelPurpose::Planning,
                categories: [DataCategory::UserRequest].into_iter().collect(),
                content: serde_json::json!({ "goal": "test" }),
            },
            max_output_tokens: 32,
            deadline_ms: 1_000,
        }
    }

    fn policy() -> DataPolicy {
        DataPolicy {
            allow_unknown_provider_retention: false,
            accepted_unknown_retention_purposes: Default::default(),
            allowed_destinations: ["synthetic-local".into()].into_iter().collect(),
            allowed_categories: [DataCategory::UserRequest].into_iter().collect(),
            allowed_purposes: [ModelPurpose::Planning].into_iter().collect(),
            limits: ModelPolicyLimits::default(),
        }
    }

    fn pricing() -> PricingSnapshot {
        PricingSnapshot {
            version: "synthetic-1".into(),
            effective_date: "2026-10-03".into(),
            currency: "SYN".into(),
            unit: "synthetic_budget_unit".into(),
            unit_scale: 1,
            input_units_per_million_tokens: 1_000_001,
            output_units_per_million_tokens: 2_000_000,
        }
    }

    #[test]
    fn data_policy_is_deny_all_by_default_and_checks_all_dimensions() {
        let input_bytes = serde_json::to_vec(&request().input).unwrap().len() as u32;
        let tokens = input_bytes + CONSERVATIVE_TOKEN_OVERHEAD;
        assert_eq!(
            DataPolicy::default().authorize_request(&request(), input_bytes, tokens, Some(0)),
            Err(Error::Forbidden)
        );
        assert!(
            policy()
                .authorize_request(&request(), input_bytes, tokens, Some(0))
                .is_ok()
        );
        assert_eq!(
            policy().authorize_request(&request(), input_bytes, tokens, None),
            Err(Error::Forbidden)
        );
    }

    #[test]
    fn unknown_retention_requires_explicit_conversation_consent() {
        let mut policy = policy();
        let mut input = request();
        input.input.purpose = ModelPurpose::Conversation;
        policy.allowed_purposes.insert(ModelPurpose::Conversation);
        assert_eq!(
            policy.authorize_request(&input, 100, 100, None),
            Err(Error::Forbidden)
        );
        assert!(
            !serde_json::to_value(&policy)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("allow_unknown_provider_retention")
        );
        policy.allow_unknown_provider_retention = true;
        assert!(policy.authorize_request(&input, 100, 100, None).is_ok());
        assert_eq!(
            policy.authorize_request(&input, 100, 100, Some(1)),
            Err(Error::Forbidden)
        );
        input.input.purpose = ModelPurpose::Planning;
        assert_eq!(
            policy.authorize_request(&input, 100, 100, None),
            Err(Error::Forbidden)
        );
    }

    #[test]
    fn agent_retention_opt_in_is_scoped_and_never_relabels_retention_as_zero() {
        let mut policy = policy();
        let input = request();
        assert_eq!(
            policy.authorize_request(&input, 100, 100, None),
            Err(Error::Forbidden)
        );
        policy
            .accepted_unknown_retention_purposes
            .insert(ModelPurpose::Planning);
        assert!(policy.authorize_request(&input, 100, 100, None).is_ok());
        assert_eq!(
            policy.authorize_request(&input, 100, 100, Some(1)),
            Err(Error::Forbidden)
        );
        let mut other = input.clone();
        other.input.purpose = ModelPurpose::Generation;
        policy.allowed_purposes.insert(ModelPurpose::Generation);
        assert_eq!(
            policy.authorize_request(&other, 100, 100, None),
            Err(Error::Forbidden)
        );
        other.input.categories.insert(DataCategory::Secret);
        policy
            .accepted_unknown_retention_purposes
            .insert(ModelPurpose::Generation);
        assert_eq!(
            policy.authorize_request(&other, 100, 100, None),
            Err(Error::Forbidden)
        );
        policy
            .accepted_unknown_retention_purposes
            .insert(ModelPurpose::Translation);
        assert!(policy.validate().is_err());
        policy
            .accepted_unknown_retention_purposes
            .remove(&ModelPurpose::Translation);
        policy.allowed_purposes.remove(&ModelPurpose::Planning);
        assert!(policy.validate().is_err());
    }

    #[test]
    fn secret_category_is_rejected_even_if_other_policy_dimensions_match() {
        let mut input = request();
        input.input.categories.insert(DataCategory::Secret);
        let bytes = serde_json::to_vec(&input.input).unwrap().len() as u32;
        let tokens = bytes + CONSERVATIVE_TOKEN_OVERHEAD;
        assert_eq!(
            policy().authorize_request(&input, bytes, tokens, Some(0)),
            Err(Error::Forbidden)
        );
        let mut invalid_policy = policy();
        invalid_policy
            .allowed_categories
            .insert(DataCategory::Secret);
        assert!(matches!(invalid_policy.validate(), Err(Error::Invalid(_))));
    }

    #[test]
    fn prices_round_up_and_missing_usage_stays_unknown() {
        let pricing = pricing();
        assert_eq!(pricing.reservation_units(1, 1).unwrap(), 4);
        let complete = ModelUsage {
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_input_tokens: None,
        };
        assert_eq!(pricing.actual_units(&complete).unwrap(), Some(4));
        let missing = ModelUsage {
            input_tokens: None,
            output_tokens: Some(1),
            cached_input_tokens: None,
        };
        assert_eq!(pricing.actual_units(&missing).unwrap(), None);
    }

    #[test]
    fn prices_reject_overflow_instead_of_wrapping() {
        let mut pricing = pricing();
        pricing.input_units_per_million_tokens = i64::MAX;
        assert_eq!(
            pricing.reservation_units(u32::MAX, 1),
            Err(Error::ResourceLimit)
        );
    }

    #[test]
    fn model_request_does_not_accept_client_controlled_urls_or_unknown_fields() {
        let mut value = serde_json::to_value(request()).unwrap();
        value.as_object_mut().unwrap().insert(
            "base_url".into(),
            serde_json::json!("https://attacker.invalid"),
        );
        assert!(serde_json::from_value::<ModelRequest>(value).is_err());
    }

    #[test]
    fn reported_usage_cannot_be_negative_or_exceed_input() {
        let invalid = ModelUsage {
            input_tokens: Some(2),
            output_tokens: Some(-1),
            cached_input_tokens: Some(3),
        };
        assert!(invalid.validate().is_err());
    }
}
