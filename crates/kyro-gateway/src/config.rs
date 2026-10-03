use std::{
    env, fmt, fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    str::FromStr,
};

use kyro_domain::{
    Environment, Error, Result,
    model::{ModelPolicyLimits, ModelProviderKind, ModelRegistrationSnapshot, PricingSnapshot},
};
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const MAX_REGISTRY_BYTES: usize = 1_048_576;
const MAX_DESTINATIONS: usize = 64;
const MAX_MODELS_PER_DESTINATION: usize = 128;
const MAX_PINNED_ADDRESSES: usize = 16;
const MAX_OUTPUT_SCHEMA_BYTES: usize = 32_768;
const MAX_SCHEMA_DEPTH: usize = 16;
const MAX_SCHEMA_NODES: usize = 512;
const MAX_SCHEMA_PROPERTIES: usize = 128;
const MODEL_API_KEY_ENV: &str = "KYRO_MODEL_API_KEY";
const LOOPBACK_OPT_IN_ENV: &str = "KYRO_MODEL_ALLOW_SYNTHETIC_LOOPBACK";
const REGISTRY_PATH_ENV: &str = "KYRO_MODEL_REGISTRY_PATH";

#[derive(Clone)]
pub(crate) struct SecretValue(String);

impl SecretValue {
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([redacted])")
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RegisteredModel {
    pub(crate) registration: ModelRegistrationSnapshot,
    pub(crate) limits: ModelPolicyLimits,
    pub(crate) output_schema: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct Destination {
    pub(crate) id: String,
    pub(crate) provider: String,
    pub(crate) base_url: Url,
    pub(crate) host: String,
    pub(crate) pinned_addresses: Vec<SocketAddr>,
    pub(crate) secret: Option<SecretValue>,
    pub(crate) admissible: bool,
    pub(crate) enabled: bool,
    pub(crate) disabled_reason: Option<DisabledReason>,
    pub(crate) models: Vec<RegisteredModel>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledReason {
    MissingSecret,
    NotQualified,
    LoopbackNotEnabled,
    ProviderUnavailableInProduction,
}

#[derive(Debug)]
pub struct RegistryModelView {
    pub registration: ModelRegistrationSnapshot,
    pub limits: ModelPolicyLimits,
    pub output_schema: Value,
    /// The trusted registry/policy can admit requests; it does not imply a worker secret exists.
    pub admissible: bool,
    pub enabled: bool,
    pub disabled_reason: Option<DisabledReason>,
}

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub(crate) execution_enabled: bool,
    pub(crate) destinations: Vec<Destination>,
}

#[derive(Clone, Copy)]
enum GatewayMode {
    Admission,
    Execution,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    format_version: u32,
    destinations: Vec<DestinationFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationFile {
    id: String,
    provider: String,
    kind: ModelProviderKind,
    base_url: String,
    allowed_host: String,
    pinned_addresses: Vec<String>,
    secret_ref: Option<String>,
    qualified: bool,
    retention_seconds: Option<u32>,
    models: Vec<ModelFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelFile {
    id: String,
    version: Option<String>,
    output_schema: OutputSchemaFile,
    pricing: PricingSnapshot,
    max_input_bytes: u32,
    max_input_tokens: u32,
    max_output_tokens: u32,
    max_deadline_ms: u32,
    max_response_bytes: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputSchemaFile {
    id: String,
    version: String,
    schema: Value,
}

impl GatewayConfig {
    /// Charge le registre et la clé côté worker uniquement.
    pub fn from_env(environment: Environment) -> Result<Self> {
        Self::from_environment(environment, GatewayMode::Execution)
    }

    /// Charge uniquement les métadonnées fiables pour le préflight API, sans lire de clé API.
    pub fn for_admission_from_env(environment: Environment) -> Result<Self> {
        Self::from_environment(environment, GatewayMode::Admission)
    }

    fn from_environment(environment: Environment, mode: GatewayMode) -> Result<Self> {
        let path = env::var_os(REGISTRY_PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("config/models.example.json"));
        let metadata = fs::metadata(&path).map_err(|_| Error::Unavailable)?;
        if metadata.len() > MAX_REGISTRY_BYTES as u64 {
            return Err(Error::ResourceLimit);
        }
        let registry = fs::read(&path).map_err(|_| Error::Unavailable)?;
        let api_key = match mode {
            GatewayMode::Admission => None,
            GatewayMode::Execution => env::var(MODEL_API_KEY_ENV)
                .ok()
                .filter(|value| !value.is_empty()),
        };
        let loopback_opt_in = env::var(LOOPBACK_OPT_IN_ENV).is_ok_and(|value| value == "1");
        Self::from_registry_json_mode(
            &registry,
            environment,
            loopback_opt_in,
            api_key.as_deref(),
            mode,
        )
    }

    /// Entrée serveur/test pour parser un registre fiable. Les handlers HTTP ne doivent pas recevoir cette valeur.
    pub fn from_registry_json(
        registry_json: &[u8],
        environment: Environment,
        allow_synthetic_loopback: bool,
        api_key: Option<&str>,
    ) -> Result<Self> {
        Self::from_registry_json_mode(
            registry_json,
            environment,
            allow_synthetic_loopback,
            api_key,
            GatewayMode::Execution,
        )
    }

    /// Entrée pure d'admission; aucun secret n'est accepté ni lu.
    pub fn for_admission_from_registry_json(
        registry_json: &[u8],
        environment: Environment,
        allow_synthetic_loopback: bool,
    ) -> Result<Self> {
        Self::from_registry_json_mode(
            registry_json,
            environment,
            allow_synthetic_loopback,
            None,
            GatewayMode::Admission,
        )
    }

    fn from_registry_json_mode(
        registry_json: &[u8],
        environment: Environment,
        allow_synthetic_loopback: bool,
        api_key: Option<&str>,
        mode: GatewayMode,
    ) -> Result<Self> {
        if registry_json.len() > MAX_REGISTRY_BYTES {
            return Err(Error::ResourceLimit);
        }
        if api_key.is_some_and(|value| value.trim().is_empty()) {
            return Err(Error::Invalid("référence de secret vide".into()));
        }
        let file: RegistryFile = serde_json::from_slice(registry_json)
            .map_err(|_| Error::Invalid("registre modèle invalide".into()))?;
        if file.format_version != 1 || file.destinations.len() > MAX_DESTINATIONS {
            return Err(Error::Invalid(
                "version ou nombre d'entrées de registre invalide".into(),
            ));
        }
        if environment == Environment::Production && allow_synthetic_loopback {
            return Err(Error::Invalid(
                "l'option simulateur loopback est interdite en production".into(),
            ));
        }

        let mut destinations = Vec::with_capacity(file.destinations.len());
        for file_destination in file.destinations {
            destinations.push(parse_destination(
                file_destination,
                environment,
                allow_synthetic_loopback,
                api_key,
                mode,
            )?);
        }
        let mut seen_ids = std::collections::BTreeSet::new();
        if destinations
            .iter()
            .any(|destination| !seen_ids.insert(destination.id.clone()))
        {
            return Err(Error::Invalid("identifiant de destination dupliqué".into()));
        }
        Ok(Self {
            execution_enabled: matches!(mode, GatewayMode::Execution),
            destinations,
        })
    }

    pub fn registry(&self) -> Vec<RegistryModelView> {
        self.destinations
            .iter()
            .flat_map(|destination| {
                destination.models.iter().map(|model| RegistryModelView {
                    registration: model.registration.clone(),
                    limits: model.limits.clone(),
                    output_schema: model.output_schema.clone(),
                    admissible: destination.admissible,
                    enabled: destination.enabled,
                    disabled_reason: destination.disabled_reason,
                })
            })
            .collect()
    }
}

fn parse_destination(
    file: DestinationFile,
    environment: Environment,
    allow_synthetic_loopback: bool,
    api_key: Option<&str>,
    mode: GatewayMode,
) -> Result<Destination> {
    if !valid_id(&file.id)
        || !valid_id(&file.provider)
        || !valid_host(&file.allowed_host)
        || file.models.is_empty()
        || file.models.len() > MAX_MODELS_PER_DESTINATION
        || file.pinned_addresses.is_empty()
        || file.pinned_addresses.len() > MAX_PINNED_ADDRESSES
    {
        return Err(Error::Invalid("destination du registre invalide".into()));
    }
    let base_url =
        Url::parse(&file.base_url).map_err(|_| Error::Invalid("URL registre invalide".into()))?;
    let host = base_url
        .host_str()
        .ok_or_else(|| Error::Invalid("hôte de destination absent".into()))?
        .to_ascii_lowercase();
    let is_loopback = file.kind == ModelProviderKind::Synthetic
        && is_loopback_host(&host)
        && file.pinned_addresses.iter().all(|address| {
            SocketAddr::from_str(address).is_ok_and(|address| address.ip().is_loopback())
        });
    let scheme_is_allowed = base_url.scheme() == "https"
        || (base_url.scheme() == "http"
            && file.kind == ModelProviderKind::Synthetic
            && is_loopback
            && environment == Environment::Development
            && allow_synthetic_loopback);
    if !scheme_is_allowed
        || host != file.allowed_host.to_ascii_lowercase()
        || !base_url.username().is_empty()
        || base_url.password().is_some()
        || base_url.query().is_some()
        || base_url.fragment().is_some()
        || !base_url.path().ends_with('/')
    {
        return Err(Error::Invalid("URL ou hôte hors registre autorisé".into()));
    }
    let expected_port = base_url
        .port_or_known_default()
        .ok_or_else(|| Error::Invalid("port de destination absent".into()))?;
    let pinned_addresses = file
        .pinned_addresses
        .iter()
        .map(|address| {
            let parsed = SocketAddr::from_str(address)
                .map_err(|_| Error::Invalid("adresse épinglée invalide".into()))?;
            if parsed.port() != expected_port
                || (is_loopback && !parsed.ip().is_loopback())
                || (!is_loopback && !is_public_ip(parsed.ip()))
            {
                return Err(Error::Invalid("adresse épinglée hors périmètre".into()));
            }
            Ok(parsed)
        })
        .collect::<Result<Vec<_>>>()?;
    if file.kind == ModelProviderKind::Synthetic
        && (environment == Environment::Production || !allow_synthetic_loopback || !is_loopback)
    {
        return Err(Error::Invalid(
            "simulateur interdit dans cet environnement".into(),
        ));
    }
    if file.kind == ModelProviderKind::Cloud && is_loopback {
        return Err(Error::Invalid(
            "destination cloud loopback interdite".into(),
        ));
    }
    if file
        .retention_seconds
        .is_some_and(|seconds| seconds > kyro_domain::model::MAX_RETENTION_SECONDS)
    {
        return Err(Error::ResourceLimit);
    }

    let secret = match file.secret_ref.as_deref() {
        Some(reference) if reference == "env:KYRO_MODEL_API_KEY" => {
            api_key.map(|key| SecretValue(key.to_owned()))
        }
        Some(_) => return Err(Error::Invalid("référence de secret non autorisée".into())),
        None => None,
    };
    let admissible = file.qualified && file.secret_ref.is_some();
    let disabled_reason = if !file.qualified {
        Some(DisabledReason::NotQualified)
    } else if matches!(mode, GatewayMode::Execution) && secret.is_none() {
        Some(DisabledReason::MissingSecret)
    } else {
        None
    };
    let enabled = matches!(mode, GatewayMode::Execution) && admissible && secret.is_some();
    let destination_id = file.id.clone();
    let provider = file.provider.clone();
    let destination_kind = file.kind;
    let retention_seconds = file.retention_seconds;
    let models = file
        .models
        .into_iter()
        .map(|model| {
            parse_model(
                &destination_id,
                &provider,
                destination_kind,
                retention_seconds,
                model,
            )
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Destination {
        id: file.id,
        provider: file.provider,
        base_url,
        host,
        pinned_addresses,
        secret,
        admissible,
        enabled,
        disabled_reason,
        models,
    })
}

fn parse_model(
    destination_id: &str,
    provider: &str,
    provider_kind: ModelProviderKind,
    retention_seconds: Option<u32>,
    model: ModelFile,
) -> Result<RegisteredModel> {
    if !valid_id(&model.id)
        || model
            .version
            .as_deref()
            .is_some_and(|value| !valid_version(value))
        || !valid_id(&model.output_schema.id)
        || !valid_version(&model.output_schema.version)
    {
        return Err(Error::Invalid("métadonnées modèle invalides".into()));
    }
    validate_output_schema(&model.output_schema.schema)?;
    model.pricing.validate()?;
    let schema_bytes = serde_json::to_vec(&model.output_schema.schema)
        .map_err(|_| Error::Invalid("schéma de sortie invalide".into()))?;
    let limits = ModelPolicyLimits {
        max_input_bytes: model.max_input_bytes,
        max_input_tokens: model.max_input_tokens,
        max_output_tokens: model.max_output_tokens,
        max_deadline_ms: model.max_deadline_ms,
        max_response_bytes: model.max_response_bytes,
        max_retention_seconds: retention_seconds.unwrap_or(0),
    };
    limits.validate()?;
    Ok(RegisteredModel {
        registration: ModelRegistrationSnapshot {
            destination_id: destination_id.to_owned(),
            provider: provider.to_owned(),
            provider_kind,
            model: model.id,
            model_version: model.version,
            output_schema_id: model.output_schema.id,
            output_schema_version: model.output_schema.version,
            output_schema_hash: Sha256::digest(schema_bytes).into(),
            pricing: model.pricing,
            retention_seconds,
        },
        limits,
        output_schema: model.output_schema.schema,
    })
}

/// Autorise un sous-ensemble borné de JSON Schema sans références ni validation à coût non borné.
pub(crate) fn validate_output_schema(schema: &Value) -> Result<()> {
    if serde_json::to_vec(schema)
        .map_err(|_| Error::Invalid("schéma de sortie invalide".into()))?
        .len()
        > MAX_OUTPUT_SCHEMA_BYTES
    {
        return Err(Error::ResourceLimit);
    }
    let mut nodes = 0;
    validate_schema_node(schema, 0, &mut nodes, true)
}

pub(crate) fn validate_schema_instance(schema: &Value, instance: &Value) -> Result<()> {
    validate_instance_node(schema, instance, 0)
}

fn validate_instance_node(schema: &Value, instance: &Value, depth: usize) -> Result<()> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(Error::Invalid(
            "sortie au-delà de la profondeur du schéma".into(),
        ));
    }
    let definition = schema.as_object().ok_or_else(|| Error::Internal)?;
    if let Some(expected) = definition.get("const") {
        if expected != instance {
            return Err(Error::Invalid("sortie incompatible avec le schéma".into()));
        }
    }
    if let Some(values) = definition.get("enum").and_then(Value::as_array) {
        if !values.contains(instance) {
            return Err(Error::Invalid("sortie incompatible avec le schéma".into()));
        }
    }
    let valid_type = match definition.get("type").and_then(Value::as_str) {
        Some("object") => instance.is_object(),
        Some("array") => instance.is_array(),
        Some("string") => instance.is_string(),
        Some("number") => instance.is_number(),
        Some("integer") => instance
            .as_number()
            .is_some_and(|number| number.is_i64() || number.is_u64()),
        Some("boolean") => instance.is_boolean(),
        Some("null") => instance.is_null(),
        _ => false,
    };
    if !valid_type {
        return Err(Error::Invalid(
            "type de sortie incompatible avec le schéma".into(),
        ));
    }
    match definition.get("type").and_then(Value::as_str) {
        Some("object") => {
            let value = instance
                .as_object()
                .ok_or_else(|| Error::Invalid("sortie objet requise".into()))?;
            let properties = definition
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| Error::Internal)?;
            if value.len() != properties.len() {
                return Err(Error::Invalid("propriétés de sortie incorrectes".into()));
            }
            for (name, child_schema) in properties {
                let child = value
                    .get(name)
                    .ok_or_else(|| Error::Invalid("propriété de sortie manquante".into()))?;
                validate_instance_node(child_schema, child, depth + 1)?;
            }
        }
        Some("array") => {
            let value = instance
                .as_array()
                .ok_or_else(|| Error::Invalid("sortie tableau requise".into()))?;
            let maximum = definition
                .get("maxItems")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let minimum = definition
                .get("minItems")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            if value.len() < minimum || value.len() > maximum {
                return Err(Error::Invalid(
                    "taille de tableau de sortie invalide".into(),
                ));
            }
            let item_schema = definition.get("items").ok_or_else(|| Error::Internal)?;
            for item in value {
                validate_instance_node(item_schema, item, depth + 1)?;
            }
        }
        Some("string") => {
            let value = instance.as_str().ok_or_else(|| Error::Internal)?;
            let length = value.chars().count();
            let maximum = definition
                .get("maxLength")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let minimum = definition
                .get("minLength")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            if length < minimum || length > maximum {
                return Err(Error::Invalid("longueur de sortie invalide".into()));
            }
        }
        Some("number" | "integer") => {
            let number = instance.as_f64().ok_or_else(|| Error::Internal)?;
            if definition
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|minimum| number < minimum)
                || definition
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_some_and(|maximum| number > maximum)
            {
                return Err(Error::Invalid(
                    "valeur numérique de sortie hors limites".into(),
                ));
            }
        }
        Some("boolean" | "null") => {}
        _ => return Err(Error::Internal),
    }
    Ok(())
}

fn validate_schema_node(schema: &Value, depth: usize, nodes: &mut usize, root: bool) -> Result<()> {
    *nodes = nodes.checked_add(1).ok_or(Error::ResourceLimit)?;
    if depth > MAX_SCHEMA_DEPTH || *nodes > MAX_SCHEMA_NODES {
        return Err(Error::ResourceLimit);
    }
    let object = schema
        .as_object()
        .ok_or_else(|| Error::Invalid("définition de schéma invalide".into()))?;
    const ALLOWED: &[&str] = &[
        "type",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "const",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minimum",
        "maximum",
        "description",
    ];
    if object.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(Error::Invalid(
            "mot-clé de schéma non pris en charge".into(),
        ));
    }
    if object
        .get("description")
        .is_some_and(|value| !value.as_str().is_some_and(|text| text.len() <= 512))
    {
        return Err(Error::Invalid("description de schéma invalide".into()));
    }
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Invalid("type JSON Schema requis".into()))?;
    if !matches!(
        kind,
        "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
    ) || (root && kind != "object")
    {
        return Err(Error::Invalid("type JSON Schema non pris en charge".into()));
    }
    if let Some(values) = object.get("enum") {
        let values = values
            .as_array()
            .filter(|values| !values.is_empty() && values.len() <= MAX_SCHEMA_PROPERTIES)
            .ok_or_else(|| Error::Invalid("énumération de schéma invalide".into()))?;
        for value in values {
            *nodes = nodes.checked_add(1).ok_or(Error::ResourceLimit)?;
            if *nodes > MAX_SCHEMA_NODES {
                return Err(Error::ResourceLimit);
            }
            let _ = value;
        }
    }
    if object.get("const").is_some() {
        *nodes = nodes.checked_add(1).ok_or(Error::ResourceLimit)?;
        if *nodes > MAX_SCHEMA_NODES {
            return Err(Error::ResourceLimit);
        }
    }
    for bound in ["minLength", "maxLength", "minItems", "maxItems"] {
        if object
            .get(bound)
            .is_some_and(|value| value.as_u64().is_none_or(|value| value > 1_000_000))
        {
            return Err(Error::Invalid("borne de schéma invalide".into()));
        }
    }
    if object.get("minimum").is_some_and(|value| {
        !value
            .as_number()
            .is_some_and(|number| number.is_f64() || number.is_i64() || number.is_u64())
    }) || object.get("maximum").is_some_and(|value| {
        !value
            .as_number()
            .is_some_and(|number| number.is_f64() || number.is_i64() || number.is_u64())
    }) {
        return Err(Error::Invalid("borne numérique de schéma invalide".into()));
    }
    if let (Some(minimum), Some(maximum)) = (object.get("minimum"), object.get("maximum")) {
        if minimum
            .as_f64()
            .zip(maximum.as_f64())
            .is_some_and(|(min, max)| min > max)
        {
            return Err(Error::Invalid("bornes numériques inversées".into()));
        }
    }
    if kind == "object" {
        let properties = object
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::Invalid("propriétés d'objet requises".into()))?;
        if properties.is_empty() || properties.len() > MAX_SCHEMA_PROPERTIES {
            return Err(Error::ResourceLimit);
        }
        if object.get("additionalProperties") != Some(&Value::Bool(false)) {
            return Err(Error::Invalid(
                "additionalProperties doit être false".into(),
            ));
        }
        let required = object
            .get("required")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Invalid("liste required requise".into()))?;
        if required.len() != properties.len()
            || required.iter().any(|entry| {
                entry
                    .as_str()
                    .is_none_or(|key| !properties.contains_key(key))
            })
        {
            return Err(Error::Invalid("required doit fermer l'objet".into()));
        }
        for property in properties.values() {
            validate_schema_node(property, depth + 1, nodes, false)?;
        }
    } else if object.contains_key("properties")
        || object.contains_key("required")
        || object.contains_key("additionalProperties")
    {
        return Err(Error::Invalid(
            "clés object appliquées à un autre type".into(),
        ));
    }
    if kind == "array" {
        let items = object
            .get("items")
            .ok_or_else(|| Error::Invalid("schéma items requis".into()))?;
        if object.get("maxItems").and_then(Value::as_u64).is_none() {
            return Err(Error::Invalid("maxItems borné requis".into()));
        }
        validate_schema_node(items, depth + 1, nodes, false)?;
    } else if object.contains_key("items")
        || object.contains_key("minItems")
        || object.contains_key("maxItems")
    {
        return Err(Error::Invalid(
            "clés array appliquées à un autre type".into(),
        ));
    }
    if kind == "string" {
        if object.get("maxLength").and_then(Value::as_u64).is_none() {
            return Err(Error::Invalid("maxLength borné requis".into()));
        }
    } else if object.contains_key("minLength") || object.contains_key("maxLength") {
        return Err(Error::Invalid(
            "clés string appliquées à un autre type".into(),
        ));
    }
    if !matches!(kind, "number" | "integer")
        && (object.contains_key("minimum") || object.contains_key("maximum"))
    {
        return Err(Error::Invalid(
            "clés numériques appliquées à un autre type".into(),
        ));
    }
    Ok(())
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && matches!(value.as_bytes()[0], b'a'..=b'z')
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
}

fn valid_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
        && !value.contains("..")
        && !value.starts_with('.')
        && !value.ends_with('.')
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !address.is_private()
        && !address.is_loopback()
        && !address.is_link_local()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_broadcast()
        && a != 0
        && a < 224
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 192 && (b == 0 || b == 168))
        && !(a == 192 && b == 88 && c == 99)
        && !(a == 198 && (b == 18 || b == 19 || b == 51))
        && !(a == 203 && b == 0 && c == 113)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = address.segments();
    (segments[0] & 0xe000) == 0x2000
        && !address.is_loopback()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_unique_local()
        && !address.is_unicast_link_local()
        && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
        && !(segments[0] == 0x2001 && (segments[1] == 0x0db8 || segments[1] == 0x0010))
        && segments[0] != 0x2002
        && segments[0] != 0x3fff
}
