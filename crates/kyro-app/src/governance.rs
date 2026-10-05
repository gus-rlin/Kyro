//! Fixed access rules and bounded governance commands (B021–B030).
use crate::{AppError, AppResult, AppTx, OperationRequest, Record};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub fn supports(id: &str, action: &str) -> bool {
    match id {
        "B021" => matches!(action, "policy.set" | "access.decide"),
        "B022" => matches!(
            action,
            "capability.issue" | "capability.consume" | "capability.revoke"
        ),
        "B023" => matches!(
            action,
            "secret_ref.bind" | "secret_ref.inspect" | "secret_ref.revoke"
        ),
        "B024" => action == "rate.consume",
        "B025" => action == "input.validate",
        "B026" => action == "record.project",
        "B027" => matches!(action, "audit.read" | "audit.verify"),
        "B028" => matches!(action, "retention.set" | "purge.records" | "purge.status"),
        "B029" => matches!(action, "personal.export" | "personal.download"),
        "B030" => matches!(action, "quota.inspect" | "quota.set"),
        _ => false,
    }
}
pub fn is_read(id: &str, action: &str) -> bool {
    matches!(
        (id, action),
        ("B021", "access.decide")
            | ("B023", "secret_ref.inspect")
            | ("B025", "input.validate")
            | ("B026", "record.project")
            | ("B027", "audit.read" | "audit.verify")
            | ("B028", "purge.status")
            | ("B030", "quota.inspect")
    )
}
fn decode<T: DeserializeOwned>(req: &OperationRequest) -> AppResult<T> {
    serde_json::from_value(req.payload.clone())
        .map_err(|_| AppError::invalid("invalid_governance_input"))
}
pub(crate) fn admin(tx: &AppTx) -> AppResult<()> {
    if tx
        .actor()
        .roles()
        .iter()
        .any(|r| matches!(r.as_str(), "owner" | "admin" | "security.admin"))
    {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}
pub(crate) fn stable_id(namespace: &str, value: &str) -> Uuid {
    let hash = Sha256::digest(format!("{namespace}:{value}"));
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash[..16]);
    Uuid::from_bytes(bytes)
}
pub(crate) fn token() -> AppResult<String> {
    use base64::Engine;
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| AppError::Internal)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}
pub(crate) fn validate_shape(value: &Value) -> AppResult<()> {
    fn visit(value: &Value, depth: usize, count: &mut usize) -> AppResult<()> {
        *count += 1;
        if depth > 16 || *count > 4096 {
            return Err(AppError::invalid("input_structure_limit"));
        }
        match value {
            Value::Object(map) => {
                if map.len() > 128 {
                    return Err(AppError::invalid("input_cardinality_limit"));
                }
                for (key, v) in map {
                    if key.len() > 128 || key.chars().any(char::is_control) {
                        return Err(AppError::invalid("input_key_invalid"));
                    }
                    visit(v, depth + 1, count)?;
                }
            }
            Value::Array(array) => {
                if array.len() > 1000 {
                    return Err(AppError::invalid("input_cardinality_limit"));
                }
                for v in array {
                    visit(v, depth + 1, count)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(value, 0, &mut 0)
}
pub(crate) fn is_one_time_secret_field(key: &str) -> bool {
    matches!(
        key,
        "secret_once" | "token" | "access_token" | "refresh_token" | "auth_token" | "session_token"
    )
}

pub(crate) fn redact_one_time(mut v: Value) -> Value {
    fn redact(v: &mut Value) -> bool {
        match v {
            Value::Object(map) => {
                let before = map.len();
                map.retain(|key, _| !is_one_time_secret_field(key));
                let mut changed = map.len() != before;
                for child in map.values_mut() {
                    changed |= redact(child);
                }
                changed
            }
            Value::Array(array) => {
                let mut changed = false;
                for child in array {
                    changed |= redact(child);
                }
                changed
            }
            _ => false,
        }
    }
    if redact(&mut v)
        && let Some(map) = v.as_object_mut()
    {
        map.insert("secret_not_replayed".into(), json!(true));
    }
    v
}
pub(crate) fn masked(v: Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "password"
                            | "password_hash"
                            | "secret_once"
                            | "token_hash"
                            | "secret"
                            | "access_token"
                            | "refresh_token"
                            | "session_token"
                            | "admin_fields"
                            | "private_notes"
                            | "definition"
                    )
                })
                .map(|(k, v)| (k, masked(v)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(masked).collect()),
        v => v,
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceInput {
    kind: String,
    id: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    kind: String,
    action: String,
    owner: bool,
    roles: BTreeSet<String>,
    fields: BTreeMap<String, BTreeSet<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionInput {
    resource: ResourceInput,
    action: String,
}
async fn policy(tx: &mut AppTx, kind: &str, action: &str) -> AppResult<Policy> {
    let r = tx
        .get(
            "security.policy",
            stable_id("policy", &format!("{kind}:{action}")),
        )
        .await?;
    serde_json::from_value(r.data).map_err(|_| AppError::Internal)
}
pub(crate) async fn permitted(
    tx: &mut AppTx,
    kind: &str,
    id: Uuid,
    action: &str,
) -> AppResult<bool> {
    let p = match policy(tx, kind, action).await {
        Ok(p) => p,
        Err(AppError::NotFound) => return Ok(false),
        Err(e) => return Err(e),
    };
    let owner:Option<Uuid>=sqlx::query_scalar("SELECT created_by FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind=$3 AND id=$4").bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(kind).bind(id).fetch_optional(tx.conn()).await?;
    Ok(owner.is_some_and(|owner| {
        (p.owner && owner == tx.actor().principal_id()) || !p.roles.is_disjoint(tx.actor().roles())
    }))
}

pub(crate) async fn projected_resource(tx: &mut AppTx, kind: &str, id: Uuid) -> AppResult<Record> {
    if !permitted(tx, kind, id, "read").await? {
        return Err(AppError::NotFound);
    }
    let p = policy(tx, kind, "read").await?;
    let mut record = tx.get(kind, id).await?;
    if let Some(map) = record.data.as_object_mut() {
        map.retain(|key, _| {
            p.fields
                .get(key)
                .is_some_and(|roles| !roles.is_disjoint(tx.actor().roles()))
        });
    }
    record.data = masked(record.data);
    Ok(record)
}

/// Restricts enumeration before LIMIT/aggregation, then the caller selects only
/// these allowed fields in SQL. Authority changes cannot pass the AppTx fence.
pub(crate) async fn analytics_read_scope(
    tx: &mut AppTx,
    kind: &str,
    fields: &BTreeSet<String>,
) -> AppResult<(bool, bool)> {
    if !kind.starts_with("data.") {
        return Err(AppError::NotFound);
    }
    tx.require_operation("B031", "get")?;
    let p = policy(tx, kind, "read").await.map_err(|e| {
        if e == AppError::NotFound {
            AppError::Forbidden
        } else {
            e
        }
    })?;
    for field in fields {
        if p.fields
            .get(field)
            .is_none_or(|r| r.is_disjoint(tx.actor().roles()))
            || masked(json!({field:true})).get(field).is_none()
        {
            return Err(AppError::Forbidden);
        }
    }
    let roles = !p.roles.is_disjoint(tx.actor().roles());
    if !p.owner && !roles {
        return Err(AppError::Forbidden);
    }
    Ok((p.owner, roles))
}

/// Projects a resource for an active application member without forging an Actor.
/// Used only for recipient-bound notifications; the caller must also be allowed
/// to read the source. Stored messages are checked against this projection again.
pub(crate) async fn recipient_projection(
    tx: &mut AppTx,
    recipient: Uuid,
    kind: &str,
    id: Uuid,
) -> AppResult<Record> {
    let roles: Vec<String> = sqlx::query_scalar("SELECT m.role FROM app_memberships m JOIN app_principals p ON p.tenant_id=m.tenant_id AND p.id=m.principal_id WHERE m.tenant_id=$1 AND m.application_id=$2 AND m.principal_id=$3 AND m.status='active' AND p.status='active' AND p.account_type='human'")
        .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(recipient).fetch_all(tx.conn()).await?;
    if roles.is_empty() {
        return Err(AppError::NotFound);
    }
    let roles: BTreeSet<String> = roles.into_iter().collect();
    let p = policy(tx, kind, "read").await.map_err(|e| {
        if e == AppError::NotFound {
            AppError::NotFound
        } else {
            e
        }
    })?;
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT created_by FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind=$3 AND id=$4")
        .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(kind).bind(id).fetch_optional(tx.conn()).await?;
    if !owner.is_some_and(|owner| (p.owner && owner == recipient) || !p.roles.is_disjoint(&roles)) {
        return Err(AppError::NotFound);
    }
    let mut record = tx.get(kind, id).await?;
    if let Some(map) = record.data.as_object_mut() {
        map.retain(|key, _| {
            p.fields
                .get(key)
                .is_some_and(|allowed| !allowed.is_disjoint(&roles))
        });
    }
    record.data = masked(record.data);
    if kind.starts_with("data.") {
        record.data = crate::data::readable_for_index(tx, &record).await?;
    }
    Ok(record)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityInput {
    resource: ResourceInput,
    action: String,
    environment: String,
    expires_at: DateTime<Utc>,
    uses: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityConsume {
    id: Uuid,
    secret: String,
    resource: ResourceInput,
    action: String,
    environment: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretRef {
    adapter_id: Uuid,
    vault_reference: Uuid,
    purposes: BTreeSet<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuotaInput {
    key: String,
    limit: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default)]
    after: i64,
    #[serde(default = "page_size")]
    limit: i64,
    #[serde(default)]
    checkpoint: Option<String>,
}
fn page_size() -> i64 {
    100
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Retention {
    kind: String,
    days: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Purge {
    kind: String,
    #[serde(default)]
    after: Option<Uuid>,
    #[serde(default = "purge_size")]
    limit: u32,
}
fn purge_size() -> u32 {
    50
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Download {
    id: Uuid,
    secret: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn output(r: &Record) -> Value {
    json!({"id":r.id,"version":r.version,"data":r.data})
}

pub async fn execute(tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
    match (req.component_id.as_str(), req.action.as_str()) {
        ("B021", "policy.set") => {
            admin(tx)?;
            tx.require_elevated()?;
            let i: Policy = decode(req)?;
            if i.kind.len() > 128
                || i.action.len() > 80
                || i.roles.len() > 32
                || i.fields.len() > 128
                || i.fields.values().any(|roles| roles.len() > 32)
            {
                return Err(AppError::invalid("invalid_policy"));
            }
            let id = stable_id("policy", &format!("{}:{}", i.kind, i.action));
            tx.lock_record_key("security.policy", id).await?;
            let data = serde_json::to_value(i).map_err(|_| AppError::Internal)?;
            let r = match tx.get("security.policy", id).await {
                Ok(_r) => {
                    tx.update(
                        "security.policy",
                        id,
                        req.expected_version
                            .ok_or(AppError::invalid("expected_version_required"))?,
                        data,
                    )
                    .await?
                }
                Err(AppError::NotFound) => tx.insert("security.policy", id, data).await?,
                Err(e) => return Err(e),
            };
            Ok(output(&r))
        }
        ("B021", "access.decide") => {
            let i: DecisionInput = decode(req)?;
            Ok(json!({"allowed":permitted(tx,&i.resource.kind,i.resource.id,&i.action).await?}))
        }
        ("B022", "capability.issue") => {
            let i: CapabilityInput = decode(req)?;
            if !matches!(
                i.environment.as_str(),
                "development" | "test" | "production"
            ) || i.uses == 0
                || i.uses > 100
                || i.expires_at <= Utc::now()
                || i.expires_at > Utc::now() + Duration::minutes(15)
            {
                return Err(AppError::invalid("invalid_capability"));
            }
            if !permitted(tx, &i.resource.kind, i.resource.id, &i.action).await? {
                return Err(AppError::Forbidden);
            }
            let secret = token()?;
            let r=tx.insert("security.capability",Uuid::new_v4(),json!({"principal_id":tx.actor().principal_id(),"kind":i.resource.kind,"resource_id":i.resource.id,"action":i.action,"environment":i.environment,"expires_at":i.expires_at,"remaining":i.uses,"revoked":false,"token_hash":hex(&Sha256::digest(secret.as_bytes()))})).await?;
            Ok(json!({"id":r.id,"secret_once":secret,"expires_at":i.expires_at}))
        }
        ("B022", "capability.consume") => {
            let i: CapabilityConsume = decode(req)?;
            let mut r = tx.get_for_update("security.capability", i.id).await?;
            if r.data["principal_id"] != json!(tx.actor().principal_id())
                || r.data["revoked"] != false
                || r.data["kind"] != i.resource.kind
                || r.data["resource_id"] != json!(i.resource.id)
                || r.data["action"] != i.action
                || r.data["environment"] != i.environment
                || r.data["token_hash"] != hex(&Sha256::digest(i.secret.as_bytes()))
            {
                return Err(AppError::Forbidden);
            }
            let expires = DateTime::parse_from_rfc3339(
                r.data["expires_at"].as_str().ok_or(AppError::Internal)?,
            )
            .map_err(|_| AppError::Internal)?;
            let remaining = r.data["remaining"].as_u64().ok_or(AppError::Internal)?;
            if expires <= Utc::now()
                || remaining == 0
                || !permitted(tx, &i.resource.kind, i.resource.id, &i.action).await?
            {
                return Err(AppError::Forbidden);
            }
            r.data["remaining"] = json!(remaining - 1);
            tx.update(&r.kind, r.id, r.version, r.data).await?;
            Ok(json!({"authorized":true,"remaining":remaining-1}))
        }
        ("B022", "capability.revoke") => {
            let i: Id = decode(req)?;
            let mut r = tx.get_for_update("security.capability", i.id).await?;
            if r.data["principal_id"] != json!(tx.actor().principal_id()) {
                admin(tx)?;
            }
            r.data["revoked"] = json!(true);
            tx.update(&r.kind, r.id, r.version, r.data).await?;
            Ok(json!({"revoked":true}))
        }
        ("B023", "secret_ref.bind") => {
            admin(tx)?;
            tx.require_elevated()?;
            let i: SecretRef = decode(req)?;
            tx.get("integration.adapter", i.adapter_id).await?;
            if i.vault_reference.is_nil() || i.purposes.is_empty() || i.purposes.len() > 16 {
                return Err(AppError::invalid("invalid_secret_reference"));
            }
            let r=tx.insert("secret_ref",Uuid::new_v4(),json!({"adapter_id":i.adapter_id,"vault_reference":i.vault_reference,"purposes":i.purposes,"revoked":false})).await?;
            Ok(output(&r))
        }
        ("B023", "secret_ref.inspect") => {
            admin(tx)?;
            let i: Id = decode(req)?;
            Ok(output(&tx.get("secret_ref", i.id).await?))
        }
        ("B023", "secret_ref.revoke") => {
            admin(tx)?;
            let i: Id = decode(req)?;
            let mut r = tx.get_for_update("secret_ref", i.id).await?;
            r.data["revoked"] = json!(true);
            let r = tx.update(&r.kind, r.id, r.version, r.data).await?;
            Ok(output(&r))
        }
        ("B024", "rate.consume") => {
            let _: Empty = decode(req)?;
            // The dispatcher has already committed the rate admission. A
            // second debit here would halve this block's published allowance.
            Ok(json!({"accepted":true}))
        }
        ("B025", "input.validate") => {
            validate_shape(&req.payload)?;
            Ok(json!({"valid":true,"depth_limit":16,"node_limit":4096}))
        }
        ("B026", "record.project") => {
            let i: DecisionInput = decode(req)?;
            if !permitted(tx, &i.resource.kind, i.resource.id, &i.action).await? {
                return Err(AppError::NotFound);
            }
            let p = policy(tx, &i.resource.kind, &i.action).await?;
            let mut r = tx.get(&i.resource.kind, i.resource.id).await?;
            if let Some(map) = r.data.as_object_mut() {
                map.retain(|key, _| {
                    p.fields
                        .get(key)
                        .is_some_and(|roles| !roles.is_disjoint(tx.actor().roles()))
                });
            }
            r.data = masked(r.data);
            Ok(output(&r))
        }
        ("B027", "audit.read" | "audit.verify") => {
            admin(tx)?;
            let i: Page = decode(req)?;
            if i.after < 0 || !(1..=200).contains(&i.limit) {
                return Err(AppError::invalid("invalid_audit_page"));
            }
            let anchor: Option<Vec<u8>> = if i.after == 0 {
                Some(vec![0; 32])
            } else {
                sqlx::query_scalar("SELECT entry_hash FROM app_events WHERE tenant_id=$1 AND application_id=$2 AND chain_index=$3").bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(i.after).fetch_optional(tx.conn()).await?
            };
            if i.checkpoint
                .as_ref()
                .is_some_and(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(AppError::invalid("invalid_audit_checkpoint"));
            }
            let rows=sqlx::query("SELECT id,chain_index,previous_hash,entry_hash,component_id,action,created_at,entry_hash=sha256(convert_to(jsonb_build_object('id',id,'sequence',sequence,'tenant',tenant_id,'application',application_id,'actor',actor_principal_id,'component',component_id,'action',action,'resource',resource_id,'event_type',event_type,'payload',payload,'created_at',created_at,'previous',encode(previous_hash,'hex'),'index',chain_index)::text,'UTF8')) AS valid FROM app_events WHERE tenant_id=$1 AND application_id=$2 AND chain_index>$3 ORDER BY chain_index LIMIT $4")
   .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(i.after).bind(i.limit).fetch_all(tx.conn()).await?;
            let mut valid = anchor.is_some()
                && i.checkpoint.as_ref().is_none_or(|expected| {
                    anchor
                        .as_ref()
                        .is_some_and(|hash| hex(hash) == expected.to_ascii_lowercase())
                });
            let mut previous = anchor.clone();
            let mut expected = i.after + 1;
            let mut items = Vec::new();
            for r in rows {
                let index: i64 = r.try_get("chain_index")?;
                let prev: Vec<u8> = r.try_get("previous_hash")?;
                let hash: Vec<u8> = r.try_get("entry_hash")?;
                valid &= r.try_get::<bool, _>("valid")?
                    && index == expected
                    && previous.as_ref().is_some_and(|p| *p == prev);
                expected = index + 1;
                previous = Some(hash.clone());
                items.push(json!({"id":r.try_get::<Uuid,_>("id")?,"index":index,"component":r.try_get::<String,_>("component_id")?,"action":r.try_get::<String,_>("action")?,"hash":hex(&hash),"created_at":r.try_get::<DateTime<Utc>,_>("created_at")?}));
            }
            Ok(
                json!({"valid":valid,"scope":"page","items":items,"after":expected-1,"anchor":anchor.map(|p|hex(&p)),"checkpoint":previous.map(|p|hex(&p)),"trusted_checkpoint_checked":i.checkpoint.is_some(),"requires_trusted_checkpoint":true}),
            )
        }
        ("B028", "retention.set") => {
            admin(tx)?;
            tx.require_elevated()?;
            let i: Retention = decode(req)?;
            if !(1..=3650).contains(&i.days) || !i.kind.starts_with("data.") {
                return Err(AppError::invalid("invalid_retention_policy"));
            }
            let id = stable_id("retention", &i.kind);
            tx.lock_record_key("security.retention", id).await?;
            let data = json!({"kind":i.kind,"days":i.days,"copies":["app_records","app_record_history","app_data_drafts","app_data_relationships","app_data_unique_values","app_data_imports","app_search_sources","app_search_chunks","app_ai_requests","app_ai_effects.response","app_analytics_facts","app_analytics_snapshots","app_analytics_alerts","app_analytics_exports","app_analytics_reports","app_notifications","app_connector_calls.request_cipher","app_private_exports","app_jobs.private_payloads","app_idempotency.response"],"audit_policy":"retain_identifiers_and_hashes","idempotency_policy":"retain_keys_redact_replies","unknown_effect_policy":"retain_accounting_without_private_response","external_copy_policy":"adapter_delete_receipt_required","backup_policy":"expires_with_backup_retention"});
            let r = match tx.get("security.retention", id).await {
                Ok(r) => {
                    tx.update(
                        &r.kind,
                        r.id,
                        req.expected_version
                            .ok_or(AppError::invalid("expected_version_required"))?,
                        data,
                    )
                    .await?
                }
                Err(AppError::NotFound) => tx.insert("security.retention", id, data).await?,
                Err(e) => return Err(e),
            };
            Ok(output(&r))
        }
        ("B028", "purge.records") => {
            admin(tx)?;
            tx.require_elevated()?;
            let i: Purge = decode(req)?;
            if !(1..=100).contains(&i.limit) || !i.kind.starts_with("data.") {
                return Err(AppError::invalid("invalid_purge_scope"));
            }
            let result: Value = sqlx::query_scalar("SELECT app_purge_data_records($1,$2,$3)")
                .bind(i.kind)
                .bind(i.after)
                .bind(i.limit as i32)
                .fetch_one(tx.conn())
                .await?;
            tx.audit(
                "B028",
                "purge.records",
                None,
                json!({"count":result["count"],"scope":"local","backup_expiry_required":true}),
            )
            .await?;
            Ok(result)
        }
        ("B028", "purge.status") => {
            admin(tx)?;
            let i: ResourceInput = decode(req)?;
            let r = tx
                .get("security.retention", stable_id("retention", &i.kind))
                .await?;
            Ok(output(&r))
        }
        ("B029", "personal.export") => {
            let _: Empty = decode(req)?;
            tx.require_elevated()?;
            let rows=sqlx::query("SELECT id,kind,version,data FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND created_by=$3 AND kind NOT LIKE 'security.%' AND kind NOT LIKE 'integration.%' AND kind NOT LIKE 'commerce.%' AND kind NOT LIKE 'workflow.%' AND kind NOT LIKE 'b140.%' AND kind<>'secret_ref' ORDER BY id LIMIT 1001")
   .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(tx.actor().principal_id()).fetch_all(tx.conn()).await?;
            if rows.len() > 1000 {
                return Err(AppError::Quota);
            }
            let mut items = Vec::new();
            for r in rows {
                items.push(json!({"id":r.try_get::<Uuid,_>("id")?,"kind":r.try_get::<String,_>("kind")?,"data":masked(r.try_get::<Value,_>("data")?)}));
            }
            let bytes = serde_json::to_vec(&items).map_err(|_| AppError::Internal)?;
            if bytes.len() > 1048576 {
                return Err(AppError::Quota);
            }
            let secret = token()?;
            let id = Uuid::new_v4();
            let expires = Utc::now() + Duration::minutes(10);
            sqlx::query("INSERT INTO app_private_exports(tenant_id,application_id,id,principal_id,token_hash,expires_at,content) VALUES($1,$2,$3,$4,$5,$6,$7)")
   .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(id).bind(tx.actor().principal_id()).bind(Sha256::digest(secret.as_bytes()).to_vec()).bind(expires).bind(bytes).execute(tx.conn()).await?;
            Ok(
                json!({"id":id,"secret_once":secret,"expires_at":expires,"record_count":items.len()}),
            )
        }
        ("B029", "personal.download") => {
            let i: Download = decode(req)?;
            let content:Vec<u8>=sqlx::query_scalar("UPDATE app_private_exports SET downloaded_at=clock_timestamp() WHERE tenant_id=$1 AND application_id=$2 AND id=$3 AND principal_id=$4 AND token_hash=$5 AND expires_at>clock_timestamp() AND downloaded_at IS NULL RETURNING content")
   .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(i.id).bind(tx.actor().principal_id()).bind(Sha256::digest(i.secret.as_bytes()).to_vec()).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            serde_json::from_slice::<Value>(&content)
                .map(|items| json!({"items":items}))
                .map_err(|_| AppError::Internal)
        }
        ("B030", "quota.inspect") => {
            admin(tx)?;
            let i: Key = decode(req)?;
            let r=sqlx::query("SELECT limit_value,used_value,reserved_value FROM app_quotas WHERE tenant_id=$1 AND application_id=$2 AND quota_key=$3").bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(&i.key).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            Ok(
                json!({"key":i.key,"limit":r.try_get::<i64,_>("limit_value")?,"used":r.try_get::<i64,_>("used_value")?,"reserved":r.try_get::<i64,_>("reserved_value")?}),
            )
        }
        ("B030", "quota.set") => {
            admin(tx)?;
            tx.require_elevated()?;
            let i: QuotaInput = decode(req)?;
            if i.key.is_empty() || i.key.len() > 128 || i.limit < 0 {
                return Err(AppError::invalid("invalid_quota"));
            }
            let result=sqlx::query("INSERT INTO app_quotas(tenant_id,application_id,quota_key,limit_value) VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id,application_id,quota_key) DO UPDATE SET limit_value=EXCLUDED.limit_value WHERE app_quotas.used_value+app_quotas.reserved_value<=EXCLUDED.limit_value")
   .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(&i.key).bind(i.limit).execute(tx.conn()).await?;
            if result.rows_affected() != 1 {
                return Err(AppError::conflict("quota_below_committed_usage"));
            }
            Ok(json!({"key":i.key,"limit":i.limit}))
        }
        _ => Err(AppError::NotFound),
    }
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) async fn consume_rate(tx: &mut AppTx) -> AppResult<()> {
    let accepted: bool = sqlx::query_scalar("SELECT app_consume_rate()")
        .fetch_one(tx.conn())
        .await?;
    if accepted {
        Ok(())
    } else {
        Err(AppError::Quota)
    }
}
