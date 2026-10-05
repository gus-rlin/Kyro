use crate::{
    Result,
    catalogue::SignedCatalogue,
    crypto::{Purpose, Signer, Trust},
    digest, fail,
};
use kyro_domain::{
    Environment,
    factory::{CompositionLock, LockedComponent, LockedNode, SignedCompositionLock},
    spec::AppSpec,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone)]
pub struct Permit {
    pub project_id: Uuid,
    pub application_id: Uuid,
    pub source_revision: i64,
    pub environment: Environment,
    pub capabilities: BTreeSet<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Properties {
    version: String,
    #[serde(default)]
    configuration: serde_json::Map<String, Value>,
    #[serde(default)]
    depends_on: BTreeSet<String>,
    #[serde(default)]
    bindings: BTreeMap<String, String>,
}
pub fn resolve(
    spec: &AppSpec,
    catalogue: &SignedCatalogue,
    trust: &Trust,
    permit: &Permit,
) -> Result<CompositionLock> {
    spec.validate()
        .map_err(|_| fail("appspec_shape_invalid", "/"))?;
    catalogue.verify(trust, 1)?;
    if permit.project_id.is_nil()
        || permit.application_id.is_nil()
        || permit.source_revision < 0
        || permit.capabilities.iter().any(|c| !crate::label(c))
        || spec.nodes.is_empty()
    {
        return Err(fail("composition_context_invalid", "/context"));
    }
    // Preferences are P1 data. P2 accepts only explicit deployment preferences.
    if spec
        .preferences
        .keys()
        .any(|key| !matches!(key.as_str(), "locale" | "time_zone"))
    {
        return Err(fail("appspec_preference_unsupported", "/preferences"));
    }
    if spec
        .preferences
        .get("locale")
        .is_some_and(|v| !matches!(v.as_str(), Some("fr-FR" | "en-US")))
        || spec
            .preferences
            .get("time_zone")
            .is_some_and(|v| !matches!(v.as_str(), Some("Europe/Paris" | "UTC")))
    {
        return Err(fail("appspec_preference_invalid", "/preferences"));
    }
    let mut components = BTreeMap::new();
    let mut nodes = BTreeMap::new();
    let mut required = BTreeSet::new();
    for node in &spec.nodes {
        let path = format!("/nodes/{}", node.id);
        if !crate::label(&node.id) {
            return Err(fail("node_id_invalid", path));
        }
        let props: Properties = serde_json::from_value(Value::Object(node.properties.clone()))
            .map_err(|_| fail("node_properties_invalid", &path))?;
        let manifest = catalogue.admitted(&node.kind, &props.version)?;
        manifest
            .configuration
            .validate(&Value::Object(props.configuration.clone()))
            .map_err(|_| fail("node_configuration_invalid", &path))?;
        if let Some(defaults) = props
            .configuration
            .get("defaults")
            .and_then(Value::as_object)
        {
            for (field, value) in defaults {
                kyro_app::composition::validate_fixed_value(
                    field,
                    value.as_str().ok_or_else(|| {
                        fail(
                            "node_fixed_value_invalid",
                            format!("{path}/configuration/defaults/{field}"),
                        )
                    })?,
                )
                .map_err(|_| {
                    fail(
                        "node_fixed_value_invalid",
                        format!("{path}/configuration/defaults/{field}"),
                    )
                })?;
            }
        }
        if !manifest.capabilities.is_subset(&permit.capabilities) {
            return Err(fail("component_capability_refused", path));
        }
        if props.depends_on.len() > 32
            || props.bindings.len() > 16
            || props.depends_on.contains(&node.id)
        {
            return Err(fail("node_dependencies_invalid", path));
        }
        for (name, port) in &manifest.ports {
            if port.required && !props.bindings.contains_key(name) {
                return Err(fail(
                    "required_port_unbound",
                    format!("{path}/bindings/{name}"),
                ));
            }
        }
        if props
            .bindings
            .keys()
            .any(|p| !manifest.ports.contains_key(p))
        {
            return Err(fail("unknown_port", format!("{path}/bindings")));
        }
        required.extend(manifest.capabilities.iter().cloned());
        let locked = LockedComponent {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            manifest_digest: digest(manifest)?,
            source_digest: manifest.source_digest.clone(),
            migration_digests: manifest.migration_digests.clone(),
        };
        if components
            .get(&manifest.id)
            .is_some_and(|existing: &LockedComponent| existing.version != locked.version)
        {
            return Err(fail("multiple_component_versions", path));
        }
        components.insert(manifest.id.clone(), locked);
        let mut deps = props.depends_on;
        deps.extend(props.bindings.values().cloned());
        nodes.insert(
            node.id.clone(),
            LockedNode {
                component_id: node.kind.clone(),
                configuration: Value::Object(props.configuration),
                depends_on: deps,
                bindings: props.bindings,
            },
        );
    }
    for (id, component) in &components {
        let m = catalogue.admitted(id, &component.version)?;
        for (dependency, versions) in &m.dependencies {
            if !components
                .get(dependency)
                .is_some_and(|d| versions.contains(&d.version))
            {
                return Err(fail(
                    "component_dependency_missing",
                    format!("/components/{id}/dependencies/{dependency}"),
                ));
            }
        }
    }
    for (id, node) in &nodes {
        let m = catalogue.admitted(&node.component_id, &components[&node.component_id].version)?;
        for dependency in &node.depends_on {
            if !nodes.contains_key(dependency) {
                return Err(fail(
                    "node_reference_missing",
                    format!("/nodes/{id}/depends_on"),
                ));
            }
        }
        for (port, target) in &node.bindings {
            let target = &nodes[target];
            let contract = &m.ports[port];
            let output = catalogue.admitted(
                &target.component_id,
                &components[&target.component_id].version,
            )?;
            if target.component_id != contract.component_id
                || output.output_type.as_deref() != Some(&contract.data_type)
            {
                return Err(fail(
                    "port_type_incompatible",
                    format!("/nodes/{id}/bindings/{port}"),
                ));
            }
        }
    }
    let mut remaining: BTreeSet<String> = nodes.keys().cloned().collect();
    let mut done = BTreeSet::new();
    let mut order = vec![];
    while !remaining.is_empty() {
        let ready: Vec<_> = remaining
            .iter()
            .filter(|id| nodes[*id].depends_on.is_subset(&done))
            .cloned()
            .collect();
        if ready.is_empty() {
            return Err(fail("composition_cycle", "/nodes"));
        }
        for id in ready {
            remaining.remove(&id);
            done.insert(id.clone());
            order.push(id);
        }
    }
    let resolved = CompositionLock {
        schema_version: 1,
        project_id: permit.project_id,
        application_id: permit.application_id,
        source_revision: permit.source_revision,
        environment: permit.environment,
        preferences: spec
            .preferences
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        spec_digest: digest(spec)?,
        catalogue_revision: catalogue.catalogue.revision,
        catalogue_digest: digest(&catalogue.catalogue)?,
        components,
        nodes,
        order,
        capabilities: required,
        toolchain: "rust-1.96.1-linux-x86_64".into(),
    };
    // Semantic acceptance must agree with the exact compiled runtime contract,
    // including node defaults and bound references, before any source is built.
    kyro_app::composition::CompositionRuntime::new(SignedCompositionLock {
        lock: resolved.clone(),
        signature: "structural-validation".into(),
    })
    .map_err(|_| fail("runtime_composition_invalid", "/nodes"))?;
    Ok(resolved)
}
pub fn seal(lock: CompositionLock, signer: &Signer) -> Result<SignedCompositionLock> {
    if signer.purpose() != &Purpose::Composition {
        return Err(fail("composer_signer_required", "/signer"));
    }
    let signature = signer.sign(&lock)?;
    Ok(SignedCompositionLock { lock, signature })
}
pub fn verify(
    lock: &SignedCompositionLock,
    spec: &AppSpec,
    catalogue: &SignedCatalogue,
    trust: &Trust,
    permit: &Permit,
) -> Result<()> {
    lock.validate_shape()
        .map_err(|_| fail("lock_shape_invalid", "/lock"))?;
    trust.verify(Purpose::Composition, &lock.lock, &lock.signature)?;
    let resolved = resolve(spec, catalogue, trust, permit)?;
    if resolved != lock.lock {
        return Err(fail("lock_context_or_catalogue_changed", "/lock"));
    }
    Ok(())
}
