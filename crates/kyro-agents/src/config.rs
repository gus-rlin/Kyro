use kyro_domain::{
    Error, Result,
    agents::Role,
    model::{ModelOutputMode, ModelProviderKind},
};
use kyro_gateway::Gateway;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelChoice {
    pub destination_id: String,
    pub model: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub roles: BTreeMap<Role, ModelChoice>,
    /// Only development permits synthetic qualification. Cloud roles must use NVIDIA models.
    pub synthetic: bool,
    pub poll_ms: u64,
}
impl AgentConfig {
    pub fn from_env(
        gateway: &Gateway,
        environment: kyro_domain::Environment,
    ) -> Result<Option<Self>> {
        let Some(path) = std::env::var_os("KYRO_AGENTS_CONFIG_FILE") else {
            return Ok(None);
        };
        let meta = std::fs::symlink_metadata(Path::new(&path)).map_err(|_| Error::Unavailable)?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 16384 {
            return Err(Error::Invalid("invalid_agent_configuration".into()));
        }
        let config: Self =
            serde_json::from_slice(&std::fs::read(path).map_err(|_| Error::Unavailable)?)
                .map_err(|_| Error::Invalid("invalid_agent_configuration".into()))?;
        config.validate(gateway, environment)?;
        Ok(Some(config))
    }
    pub fn validate(&self, gateway: &Gateway, environment: kyro_domain::Environment) -> Result<()> {
        let roles = [
            Role::Orchestrator,
            Role::Pixel,
            Role::Moka,
            Role::Kiwi,
            Role::Biscotte,
            Role::Review,
            Role::Security,
        ];
        if self.roles.len() != roles.len()
            || !(100..=10_000).contains(&self.poll_ms)
            || (self.synthetic && environment != kyro_domain::Environment::Development)
        {
            return Err(Error::Invalid("invalid_agent_configuration".into()));
        }
        let registry = gateway.registry();
        let mut prices = BTreeMap::new();
        let mut currency = None;
        for role in roles {
            let choice = self
                .roles
                .get(&role)
                .ok_or_else(|| Error::Invalid("missing_agent_role".into()))?;
            let item = registry
                .iter()
                .find(|m| {
                    m.registration.destination_id == choice.destination_id
                        && m.registration.model == choice.model
                })
                .ok_or(Error::Unavailable)?;
            let r = &item.registration;
            if r.output_mode != ModelOutputMode::StructuredJson
                || r.protocol != kyro_domain::model::ModelProtocol::Chat
                || r.output_schema_id != "kyro-agent-contract"
                || !matches!(r.output_schema_version.as_str(), "1" | "2")
                || !item.admissible
                || (self.synthetic != (r.provider_kind == ModelProviderKind::Synthetic))
                || (!self.synthetic && !r.model.to_ascii_lowercase().starts_with("nvidia/"))
            {
                return Err(Error::Invalid("unqualified_agent_role".into()));
            }
            let unit = (r.pricing.currency.clone(), r.pricing.unit_scale);
            if currency.as_ref().is_some_and(|old| old != &unit) {
                return Err(Error::Invalid("incomparable_agent_prices".into()));
            }
            currency = Some(unit);
            prices.insert(
                role,
                r.pricing
                    .input_units_per_million_tokens
                    .checked_add(r.pricing.output_units_per_million_tokens)
                    .ok_or(Error::ResourceLimit)?,
            );
        }
        if !self.synthetic {
            let orchestrator = prices[&Role::Orchestrator];
            let comparable: Vec<_> = registry
                .iter()
                .filter(|m| {
                    m.admissible
                        && m.registration.output_mode == ModelOutputMode::StructuredJson
                        && m.registration.output_schema_id == "kyro-agent-contract"
                        && matches!(m.registration.output_schema_version.as_str(), "1" | "2")
                        && m.registration.protocol == kyro_domain::model::ModelProtocol::Chat
                        && m.registration
                            .model
                            .to_ascii_lowercase()
                            .starts_with("nvidia/")
                        && Some(&(
                            m.registration.pricing.currency.clone(),
                            m.registration.pricing.unit_scale,
                        )) == currency.as_ref()
                })
                .map(|m| {
                    m.registration
                        .pricing
                        .input_units_per_million_tokens
                        .saturating_add(m.registration.pricing.output_units_per_million_tokens)
                })
                .collect();
            let max = comparable.iter().max().copied().ok_or(Error::Unavailable)?;
            let min = comparable.iter().min().copied().ok_or(Error::Unavailable)?;
            if orchestrator != max
                || roles
                    .iter()
                    .filter(|r| **r != Role::Orchestrator)
                    .any(|r| prices[r] != min || prices[r] >= orchestrator)
            {
                return Err(Error::Invalid("agent_economic_roles_invalid".into()));
            }
        }
        Ok(())
    }
}
