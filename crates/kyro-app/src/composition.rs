//! Compiled declarative node constraints. Values remain JSON data, and every
//! operation still enters the normal permission/RLS/idempotency transaction.
use crate::{AppError, AppResult, OperationRequest};
use kyro_domain::factory::SignedCompositionLock;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfiguration {
    #[serde(default)]
    pub allowed_actions: BTreeSet<String>,
    #[serde(default)]
    pub defaults: BTreeMap<String, String>,
    #[serde(default)]
    pub reference: Option<String>,
}

/// Fixed payload fields shared by catalogue metadata and the compiled runtime.
pub fn default_fields(id: &str) -> &'static [&'static str] {
    match id {
        "B031" | "B032" | "B034" | "B035" | "B036" | "B037" | "B038" | "B039" | "B040" => {
            &["entity"]
        }
        "B043" | "B044" => &["record_kind"],
        "B045" => &["machine_id"],
        "B046" => &["definition_record_id"],
        "B047" => &["policy_id"],
        "B048" => &["target_kind"],
        "B050" => &["schema_id"],
        "B112" | "B113" | "B117" => &["resource_id"],
        "B114" | "B116" => &["slot_id"],
        "B115" | "B119" => &["booking_id"],
        "B118" => &["establishment_id"],
        "B122" => &["product_id", "currency"],
        "B124" => &["quote_id"],
        "B125" | "B127" => &["order_id"],
        "B126" => &["price_id", "connector_id"],
        "B128" => &["payment_id", "connector_id"],
        "B130" => &["product_id", "warehouse_id"],
        "B134" | "B136" => &["project_id"],
        "B135" => &["site_id"],
        _ => &[],
    }
}

/// Syntactic types are enforced before a build. Resource existence and access
/// still belong to the runtime transaction, because deployments have live data.
pub fn validate_fixed_value(field: &str, value: &str) -> AppResult<()> {
    match field {
        "entity" if crate::data::valid_name(value) => Ok(()),
        "record_kind" | "target_kind" => crate::workflow::validate_record_kind(value),
        "currency" if value.len() == 3 && value.bytes().all(|b| b.is_ascii_uppercase()) => Ok(()),
        field if field.ends_with("_id") && Uuid::parse_str(value).is_ok_and(|id| !id.is_nil()) => {
            Ok(())
        }
        _ => Err(AppError::invalid("invalid_fixed_value")),
    }
}
#[derive(Clone)]
pub struct CompositionRuntime {
    lock: SignedCompositionLock,
    configurations: BTreeMap<String, NodeConfiguration>,
}
impl CompositionRuntime {
    pub fn new(lock: SignedCompositionLock) -> AppResult<Self> {
        lock.validate_shape()
            .map_err(|_| AppError::invalid("invalid_compiled_composition"))?;
        let keys: BTreeSet<_> = lock.lock.nodes.keys().cloned().collect();
        if lock.lock.order.iter().cloned().collect::<BTreeSet<_>>() != keys {
            return Err(AppError::invalid("invalid_compiled_order"));
        }
        let mut done = BTreeSet::new();
        let mut configurations = BTreeMap::new();
        for id in &lock.lock.order {
            let node = &lock.lock.nodes[id];
            if !label(id)
                || !node.depends_on.is_subset(&done)
                || !lock.lock.components.contains_key(&node.component_id)
                || node.bindings.keys().any(|port| !label(port))
            {
                return Err(AppError::invalid("invalid_compiled_graph"));
            }
            let config: NodeConfiguration = serde_json::from_value(node.configuration.clone())
                .map_err(|_| AppError::invalid("invalid_node_configuration"))?;
            if config.allowed_actions.len() > 64
                || config.defaults.len() > 32
                || config.allowed_actions.iter().any(|s| !label(s))
                || config
                    .defaults
                    .iter()
                    .any(|(k, v)| !label(k) || !reference(v))
                || config.reference.as_deref().is_some_and(|s| !reference(s))
            {
                return Err(AppError::invalid("invalid_node_configuration"));
            }
            for (field, value) in &config.defaults {
                if !default_fields(&node.component_id).contains(&field.as_str()) {
                    return Err(AppError::invalid("unsupported_fixed_field"));
                }
                validate_fixed_value(field, value)?;
            }
            if let Some(value) = &config.reference {
                validate_fixed_value(
                    if node.component_id == "B031" {
                        "entity"
                    } else {
                        "resource_id"
                    },
                    value,
                )?;
            }
            for (field, target) in &node.bindings {
                if !node.depends_on.contains(target) || !done.contains(target) {
                    return Err(AppError::invalid("invalid_compiled_binding"));
                }
                let target_config: &NodeConfiguration = configurations
                    .get(target)
                    .ok_or(AppError::invalid("invalid_compiled_binding"))?;
                let value = target_config
                    .reference
                    .as_deref()
                    .ok_or(AppError::invalid("binding_reference_missing"))?;
                if !default_fields(&node.component_id).contains(&field.as_str()) {
                    return Err(AppError::invalid("unsupported_fixed_field"));
                }
                validate_fixed_value(field, value)?;
                if config
                    .defaults
                    .get(field)
                    .is_some_and(|fixed| fixed != value)
                {
                    return Err(AppError::invalid("conflicting_node_binding"));
                }
            }
            configurations.insert(id.clone(), config);
            done.insert(id.clone());
        }
        Ok(Self {
            lock,
            configurations,
        })
    }
    pub fn application_id(&self) -> Uuid {
        self.lock.lock.application_id
    }
    pub fn preferences(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.lock.lock.preferences
    }
    pub fn enabled(&self) -> BTreeSet<String> {
        self.lock
            .lock
            .components
            .keys()
            .filter(|s| s[1..].parse::<u16>().is_ok_and(|n| n <= 160))
            .cloned()
            .collect()
    }
    pub fn constrain(
        &self,
        node_id: Option<&str>,
        mut request: OperationRequest,
    ) -> AppResult<OperationRequest> {
        let id = match node_id {
            Some(id) => id,
            None => {
                let mut nodes = self
                    .lock
                    .lock
                    .nodes
                    .iter()
                    .filter(|(_, n)| n.component_id == request.component_id);
                let (id, _) = nodes.next().ok_or(AppError::NotFound)?;
                if nodes.next().is_some() {
                    return Err(AppError::invalid("explicit_node_required"));
                }
                id
            }
        };
        let node = self.lock.lock.nodes.get(id).ok_or(AppError::NotFound)?;
        if node.component_id != request.component_id {
            return Err(AppError::NotFound);
        }
        let config = &self.configurations[id];
        if !config.allowed_actions.is_empty() && !config.allowed_actions.contains(&request.action) {
            return Err(AppError::Forbidden);
        }
        let payload = request
            .payload
            .as_object_mut()
            .ok_or(AppError::invalid("invalid_operation_payload"))?;
        let mut fixed = config.defaults.clone();
        for (port, target) in &node.bindings {
            let value = self.configurations[target]
                .reference
                .clone()
                .ok_or(AppError::invalid("binding_reference_missing"))?;
            if fixed
                .insert(port.clone(), value.clone())
                .is_some_and(|old| old != value)
            {
                return Err(AppError::invalid("conflicting_node_binding"));
            }
        }
        for (key, value) in fixed {
            let value = serde_json::Value::String(value);
            if payload.get(&key).is_some_and(|existing| existing != &value) {
                return Err(AppError::Forbidden);
            }
            payload.insert(key, value);
        }
        Ok(request)
    }
}
fn label(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn reference(v: &str) -> bool {
    !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kyro_domain::{
        Environment,
        factory::{CompositionLock, LockedComponent, LockedNode},
    };
    use serde_json::json;
    fn lock() -> SignedCompositionLock {
        let component = LockedComponent {
            id: "B031".into(),
            version: "0.1.0".into(),
            manifest_digest: "a".repeat(64),
            source_digest: "a".repeat(64),
            migration_digests: BTreeMap::new(),
        };
        SignedCompositionLock {
            signature: "compiled signature fixture".into(),
            lock: CompositionLock {
                schema_version: 1,
                project_id: Uuid::new_v4(),
                application_id: Uuid::new_v4(),
                source_revision: 1,
                environment: Environment::Development,
                preferences: BTreeMap::new(),
                spec_digest: "a".repeat(64),
                catalogue_revision: 1,
                catalogue_digest: "a".repeat(64),
                components: BTreeMap::from([("B031".into(), component)]),
                nodes: BTreeMap::from([(
                    "tickets".into(),
                    LockedNode {
                        component_id: "B031".into(),
                        configuration: json!({"allowed_actions":["get"],"defaults":{"entity":"ticket"},"reference":"ticket"}),
                        depends_on: BTreeSet::new(),
                        bindings: BTreeMap::new(),
                    },
                )]),
                order: vec!["tickets".into()],
                capabilities: BTreeSet::new(),
                toolchain: "rust-1.96.1-linux-x86_64".into(),
            },
        }
    }
    fn request(action: &str, payload: serde_json::Value) -> OperationRequest {
        OperationRequest {
            component_id: "B031".into(),
            action: action.into(),
            payload,
            idempotency_key: "composition-test".into(),
            expected_version: None,
        }
    }
    #[test]
    fn compiled_node_defaults_are_enforced_for_both_routes() {
        let plan = CompositionRuntime::new(lock()).unwrap();
        assert_eq!(
            plan.constrain(None, request("get", json!({"id":"example"})))
                .unwrap()
                .payload["entity"],
            "ticket"
        );
        assert!(
            plan.constrain(Some("tickets"), request("get", json!({"entity":"private"})))
                .is_err()
        );
        assert!(plan.constrain(None, request("create", json!({}))).is_err());
        assert!(
            plan.constrain(Some("missing"), request("get", json!({})))
                .is_err()
        );
    }
    #[test]
    fn compiled_bindings_require_order_references_and_explicit_ambiguous_nodes() {
        let mut lock = lock();
        let mut node = lock.lock.nodes["tickets"].clone();
        node.configuration = json!({});
        node.depends_on.insert("tickets".into());
        node.bindings.insert("entity".into(), "tickets".into());
        lock.lock.nodes.insert("reader".into(), node);
        lock.lock.order.push("reader".into());
        let plan = CompositionRuntime::new(lock.clone()).unwrap();
        assert_eq!(
            plan.constrain(Some("reader"), request("get", json!({})))
                .unwrap()
                .payload["entity"],
            "ticket"
        );
        assert!(plan.constrain(None, request("get", json!({}))).is_err());
        lock.lock.order.swap(0, 1);
        assert!(CompositionRuntime::new(lock).is_err());
    }
    #[test]
    fn compiled_config_rejects_code_unknown_fields_and_missing_binding_output() {
        let mut lock = lock();
        lock.lock.nodes.get_mut("tickets").unwrap().configuration = json!({"rust":"execute"});
        assert!(CompositionRuntime::new(lock).is_err());
    }
}
