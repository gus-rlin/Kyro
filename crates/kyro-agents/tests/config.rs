//! Admission metadata fixture; no claim about real provider availability or prices.
use kyro_agents::{AgentConfig, ModelChoice};
use kyro_domain::{Environment, agents::Role};
use kyro_gateway::{Gateway, GatewayConfig};
use serde_json::json;
use std::collections::BTreeMap;
fn gateway() -> Gateway {
    let models:Vec<_>=[("nvidia/low",1), ("nvidia/middle",5), ("nvidia/high",10)].into_iter().map(|(id,price)|json!({
        "id":id,"version":null,"output_schema":{"id":"kyro-agent-contract","version":"1","schema":{"type":"object","properties":{"contract":{"type":"string","maxLength":32000}},"required":["contract"],"additionalProperties":false}},
        "pricing":{"version":"metadata-fixture","effective_date":"2026-10-06","currency":"USD","unit":"micro_usd","unit_scale":1000000,"input_units_per_million_tokens":price,"output_units_per_million_tokens":price},
        "max_input_bytes":65536,"max_input_tokens":131072,"max_output_tokens":8192,"max_deadline_ms":120000,"max_response_bytes":48000,
    })).collect();
    let registry = json!({"format_version":1,"destinations":[{"id":"cloud-metadata-fixture","provider":"nebius","kind":"cloud","base_url":"https://api.tokenfactory.nebius.com/v1/","allowed_host":"api.tokenfactory.nebius.com","pinned_addresses":["213.239.161.19:443"],"secret_ref":"file:KYRO_MODEL_API_KEY_FILE","qualified":true,"retention_seconds":0,
        "nebius":{"json_schema":true,"bounded_completion":true,"retention_evidence":"https://nebius.com/synthetic-metadata-fixture-not-a-real-attestation","wire_overhead_tokens":1024,"context_tokens":131072},"models":models}]});
    Gateway::new(
        GatewayConfig::for_admission_from_registry_json(
            &serde_json::to_vec(&registry).unwrap(),
            Environment::Development,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}
fn config() -> AgentConfig {
    let roles = [
        Role::Orchestrator,
        Role::Pixel,
        Role::Moka,
        Role::Kiwi,
        Role::Biscotte,
        Role::Review,
        Role::Security,
    ]
    .into_iter()
    .map(|role| {
        (
            role,
            ModelChoice {
                destination_id: "cloud-metadata-fixture".into(),
                model: if role == Role::Orchestrator {
                    "nvidia/high"
                } else {
                    "nvidia/low"
                }
                .into(),
            },
        )
    })
    .collect::<BTreeMap<_, _>>();
    AgentConfig {
        roles,
        synthetic: false,
        poll_ms: 1000,
    }
}
#[test]
fn admission_without_a_provider_key_selects_most_expensive_and_cheapest_retained_models() {
    let gateway = gateway();
    let mut cfg = config();
    assert!(cfg.validate(&gateway, Environment::Development).is_ok());
    assert!(!gateway.is_model_available("cloud-metadata-fixture", "nvidia/high"));
    cfg.roles.get_mut(&Role::Review).unwrap().model = "nvidia/middle".into();
    assert!(cfg.validate(&gateway, Environment::Development).is_err());
    cfg.roles.get_mut(&Role::Review).unwrap().model = "nvidia/low".into();
    cfg.roles.get_mut(&Role::Orchestrator).unwrap().model = "nvidia/middle".into();
    assert!(cfg.validate(&gateway, Environment::Development).is_err());
    cfg.roles.remove(&Role::Biscotte);
    assert!(cfg.validate(&gateway, Environment::Development).is_err());
}
