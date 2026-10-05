//! Fixed routing for admitted Rust blocks. A missing block/action is an error.
use crate::{
    AppError, AppResult, AppTx, OperationDispatcher, OperationFuture, OperationHandler,
    OperationRequest,
};
use std::collections::BTreeSet;

include!("operation_names.rs");

#[derive(Clone, Copy)]
enum Family {
    Collaboration,
    Governance,
    Jobs,
    Data,
    Workflow,
    Documents,
    Search,
    Notifications,
    Scheduling,
    Commerce,
    Business,
    Analytics,
}

fn family(component: &str) -> AppResult<Family> {
    let n = component
        .strip_prefix('B')
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or(AppError::invalid("invalid_component"))?;
    if component.len() != 4 {
        return Err(AppError::invalid("invalid_component"));
    }
    match n {
        11..=20 => Ok(Family::Collaboration),
        21..=30 => Ok(Family::Governance),
        51..=60 => Ok(Family::Jobs),
        31..=40 => Ok(Family::Data),
        41..=50 => Ok(Family::Workflow),
        81..=90 => Ok(Family::Documents),
        91 | 93 => Ok(Family::Search),
        101..=103 | 107..=110 => Ok(Family::Notifications),
        111..=120 => Ok(Family::Scheduling),
        121..=130 => Ok(Family::Commerce),
        131..=140 => Ok(Family::Business),
        141..=150 => Ok(Family::Analytics),
        _ => Err(AppError::NotFound),
    }
}

fn supports(f: Family, id: &str, action: &str) -> bool {
    match f {
        Family::Jobs => crate::jobs::supports(id, action),
        Family::Governance => crate::governance::supports(id, action),
        Family::Collaboration => crate::collaboration::supports(id, action),
        Family::Data => crate::data::supports(id, action),
        Family::Workflow => crate::workflow::supports(id, action),
        Family::Documents => crate::documents::supports(id, action),
        Family::Search => crate::search::supports(id, action),
        Family::Notifications => crate::notifications::supports(id, action),
        Family::Scheduling => crate::scheduling::supports(id, action),
        Family::Commerce => crate::commerce::supports(id, action),
        Family::Business => crate::business::supports(id, action),
        Family::Analytics => crate::analytics::supports(id, action),
    }
}
fn is_read(f: Family, id: &str, action: &str) -> bool {
    match f {
        Family::Jobs => crate::jobs::is_read(id, action),
        Family::Governance => crate::governance::is_read(id, action),
        Family::Collaboration => crate::collaboration::is_read(id, action),
        Family::Data => crate::data::is_read(id, action),
        Family::Workflow => crate::workflow::is_read(id, action),
        Family::Documents => crate::documents::is_read(id, action),
        Family::Search => crate::search::is_read(id, action),
        Family::Notifications => crate::notifications::is_read(id, action),
        Family::Scheduling => crate::scheduling::is_read(id, action),
        Family::Commerce => crate::commerce::is_read(id, action),
        Family::Business => crate::business::is_read(id, action),
        Family::Analytics => crate::analytics::is_read(id, action),
    }
}

impl OperationHandler for Family {
    fn execute<'a>(&'a self, tx: &'a mut AppTx, req: OperationRequest) -> OperationFuture<'a> {
        Box::pin(async move {
            match self {
                Family::Jobs => crate::jobs::execute(tx, &req).await,
                Family::Governance => crate::governance::execute(tx, &req).await,
                Family::Collaboration => crate::collaboration::execute(tx, &req).await,
                Family::Data => crate::data::execute(tx, &req).await,
                Family::Workflow => crate::workflow::execute(tx, &req).await,
                Family::Documents => crate::documents::execute(tx, &req).await,
                Family::Search => crate::search::execute(tx, &req).await,
                Family::Notifications => crate::notifications::execute(tx, &req).await,
                Family::Scheduling => crate::scheduling::execute(tx, &req).await,
                Family::Commerce => crate::commerce::execute(tx, &req).await,
                Family::Business => crate::business::execute(tx, &req).await,
                Family::Analytics => crate::analytics::execute(tx, &req).await,
            }
        })
    }
}
#[derive(Clone)]
struct CommerceWithConnectors(std::sync::Arc<crate::connectors::ConnectorService>);
impl OperationHandler for CommerceWithConnectors {
    fn execute<'a>(&'a self, tx: &'a mut AppTx, req: OperationRequest) -> OperationFuture<'a> {
        Box::pin(async move {
            let mut result = crate::commerce::execute(tx, &req).await?;
            if let Some(id) = self.0.attach_commerce_effect(tx, &req, &result).await? {
                result["connector_call_id"] = serde_json::json!(id);
            }
            Ok(result)
        })
    }
}

#[derive(Clone)]
struct SchedulingWithConnectors(std::sync::Arc<crate::connectors::ConnectorService>);

#[derive(Clone)]
struct JobsWithConnectors(std::sync::Arc<crate::connectors::ConnectorService>);
impl OperationHandler for JobsWithConnectors {
    fn execute<'a>(&'a self, tx: &'a mut AppTx, req: OperationRequest) -> OperationFuture<'a> {
        Box::pin(async move {
            if matches!(req.component_id.as_str(), "B058" | "B059") {
                self.0.http_operation(tx, &req).await
            } else {
                crate::jobs::execute(tx, &req).await
            }
        })
    }
}
impl OperationHandler for SchedulingWithConnectors {
    fn execute<'a>(&'a self, tx: &'a mut AppTx, req: OperationRequest) -> OperationFuture<'a> {
        Box::pin(async move { crate::scheduling::execute_with_connectors(tx, &req, &self.0).await })
    }
}

pub fn builtins(enabled: &BTreeSet<String>) -> AppResult<OperationDispatcher> {
    builtins_with_identity(enabled, None)
}

/// Actual closed action contracts; catalogue metadata uses the same predicates
/// as registration, without constructing providers or receiving credentials.
pub fn component_actions(id: &str) -> AppResult<BTreeSet<String>> {
    let external = crate::connectors::actions(id)
        .iter()
        .chain(crate::ai::actions(id).iter())
        .chain(crate::identity::actions(id).iter())
        .map(|s| s.to_string())
        .collect::<BTreeSet<_>>();
    if !external.is_empty() {
        return Ok(external);
    }
    let family = family(id)?;
    let actions = ACTION_NAMES
        .iter()
        .chain(crate::notifications::ACTIONS.iter())
        .chain(crate::analytics::ACTIONS.iter())
        .copied()
        .filter(|action| supports(family, id, action))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if actions.is_empty() {
        return Err(AppError::NotFound);
    }
    Ok(actions)
}

/// Catalogue effects follow the same read/command classification as dispatch.
pub fn component_is_read(id: &str, action: &str) -> AppResult<bool> {
    if !component_actions(id)?.contains(action) {
        return Err(AppError::NotFound);
    }
    if !crate::connectors::actions(id).is_empty() {
        return Ok(crate::connectors::is_read(action));
    }
    if !crate::ai::actions(id).is_empty() {
        return Ok(matches!(action, "proposal.get" | "dataset.inspect"));
    }
    if !crate::identity::actions(id).is_empty() {
        return Ok(crate::identity::is_read(id, action));
    }
    Ok(is_read(family(id)?, id, action))
}

pub fn builtins_with_identity(
    enabled: &BTreeSet<String>,
    identity: Option<&std::sync::Arc<crate::identity::IdentityService>>,
) -> AppResult<OperationDispatcher> {
    builtins_with_services(enabled, identity, None)
}
pub fn builtins_with_services(
    enabled: &BTreeSet<String>,
    identity: Option<&std::sync::Arc<crate::identity::IdentityService>>,
    ai: Option<&std::sync::Arc<crate::ai::AiService>>,
) -> AppResult<OperationDispatcher> {
    builtins_with_connectors(enabled, identity, ai, None)
}

pub fn builtins_with_connectors(
    enabled: &BTreeSet<String>,
    identity: Option<&std::sync::Arc<crate::identity::IdentityService>>,
    ai: Option<&std::sync::Arc<crate::ai::AiService>>,
    connectors: Option<&std::sync::Arc<crate::connectors::ConnectorService>>,
) -> AppResult<OperationDispatcher> {
    if enabled.len() > 139 {
        return Err(AppError::invalid("too_many_components"));
    }
    let mut dispatcher = OperationDispatcher::new();
    for id in enabled {
        if !crate::connectors::actions(id).is_empty() {
            if connectors.is_none() {
                return Err(AppError::Unavailable);
            }
            continue;
        }
        if !crate::ai::actions(id).is_empty() {
            if ai.is_none() {
                return Err(AppError::Unavailable);
            }
            continue;
        }
        if !crate::identity::actions(id).is_empty() {
            if identity.is_none() {
                return Err(AppError::Unavailable);
            }
            continue;
        }
        let family = family(id)?;
        let mut count = 0;
        for action in ACTION_NAMES
            .iter()
            .chain(crate::notifications::ACTIONS.iter())
            .chain(crate::analytics::ACTIONS.iter())
            .copied()
            .filter(|action| supports(family, id, action))
        {
            count += 1;
            let permission = format!("{id}.execute");
            if matches!(family, Family::Jobs)
                && let Some(connectors) = connectors
            {
                let handler = JobsWithConnectors(connectors.clone());
                if is_read(family, id, action) {
                    dispatcher.register_read(id, action, permission, handler)?;
                } else {
                    dispatcher.register_command(id, action, permission, handler)?;
                }
                continue;
            }
            if matches!(family, Family::Scheduling)
                && let Some(connectors) = connectors
            {
                let handler = SchedulingWithConnectors(connectors.clone());
                if is_read(family, id, action) {
                    dispatcher.register_read(id, action, permission, handler)?;
                } else {
                    dispatcher.register_command(id, action, permission, handler)?;
                }
                continue;
            }
            if matches!(family, Family::Commerce)
                && let Some(connectors) = connectors
            {
                let handler = CommerceWithConnectors(connectors.clone());
                if is_read(family, id, action) {
                    dispatcher.register_read(id, action, permission, handler)?;
                } else {
                    dispatcher.register_command(id, action, permission, handler)?;
                }
                continue;
            }
            if is_read(family, id, action) {
                dispatcher.register_read(id, action, permission, family)?;
            } else {
                dispatcher.register_command(id, action, permission, family)?;
            }
        }
        if count == 0 {
            return Err(AppError::NotFound);
        }
    }
    if let Some(identity) = identity {
        identity.register(&mut dispatcher, enabled)?;
    }
    if let Some(ai) = ai {
        ai.register(&mut dispatcher, enabled)?;
    }
    if let Some(connectors) = connectors {
        connectors.register(&mut dispatcher, enabled)?;
    }
    Ok(dispatcher)
}

pub fn actions(id: &str) -> AppResult<Vec<&'static str>> {
    if !crate::connectors::actions(id).is_empty() {
        return Ok(crate::connectors::actions(id).to_vec());
    }
    if !crate::ai::actions(id).is_empty() {
        return Ok(crate::ai::actions(id).to_vec());
    }
    if !crate::identity::actions(id).is_empty() {
        return Ok(crate::identity::actions(id).to_vec());
    }
    let f = family(id)?;
    Ok(ACTION_NAMES
        .iter()
        .chain(crate::notifications::ACTIONS.iter())
        .chain(crate::analytics::ACTIONS.iter())
        .copied()
        .filter(|a| supports(f, id, a))
        .collect())
}
