//! Tenant collaboration operations (B011–B020).
//!
//! All identity and tenant context comes from the verified `AppTx` actor. The
//! client may name a business resource, but it cannot supply a tenant, role, or
//! principal on whose behalf the operation runs.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use getrandom::fill as random_fill;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{AppError, AppResult, AppTx, OperationRequest, Record};

const MAX_LIST: u32 = 200;
const MAX_ACTIVITY: usize = 50;
const MAX_COMMENT_CHARS: usize = 4_000;
const MAX_SPACE_NAME_CHARS: usize = 120;
const MAX_GROUP_NAME_CHARS: usize = 80;
const MAX_PROFILE_NAME_CHARS: usize = 80;
const MAX_INVITE_HOURS: i64 = 168;
const MAX_SHARE_HOURS: i64 = 168;

/// True only for actions implemented by this collaboration family.
pub fn supports(component_id: &str, action: &str) -> bool {
    match component_id {
        "B011" => matches!(action, "context"),
        "B012" => matches!(action, "create" | "list" | "grant" | "revoke"),
        "B013" => matches!(action, "invite" | "accept" | "revoke" | "remove" | "list"),
        "B014" => matches!(
            action,
            "create" | "list" | "member_add" | "member_remove" | "space_grant" | "space_revoke"
        ),
        "B015" => matches!(
            action,
            "claim" | "transfer" | "delegate" | "delegate_revoke"
        ),
        "B016" => matches!(action, "get" | "update"),
        "B017" => matches!(action, "create" | "list" | "delete"),
        "B018" => matches!(
            action,
            "mention" | "subscribe" | "unsubscribe" | "mentions" | "subscribers"
        ),
        "B019" => matches!(action, "list"),
        "B020" => matches!(action, "create" | "redeem" | "revoke"),
        _ => false,
    }
}

/// Classifies supported actions so the caller can enforce read/write policy.
pub fn is_read(component_id: &str, action: &str) -> bool {
    matches!(
        (component_id, action),
        ("B011", "context")
            | ("B012", "list")
            | ("B013", "list")
            | ("B014", "list")
            | ("B016", "get")
            | ("B017", "list")
            | ("B018", "mentions")
            | ("B018", "subscribers")
            | ("B019", "list")
            | ("B020", "redeem")
    )
}

pub async fn execute(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    if !supports(&request.component_id, &request.action) {
        return Err(AppError::invalid("unsupported_collaboration_action"));
    }

    match (request.component_id.as_str(), request.action.as_str()) {
        ("B011", "context") => tenant_context(tx, decode(&request.payload)?).await,
        ("B012", "create") => space_create(tx, decode(&request.payload)?).await,
        ("B012", "list") => space_list(tx, decode(&request.payload)?).await,
        ("B012", "grant") => space_grant(tx, decode(&request.payload)?).await,
        ("B012", "revoke") => space_revoke(tx, decode(&request.payload)?).await,
        ("B013", "invite") => invitation_create(tx, decode(&request.payload)?).await,
        ("B013", "accept") => invitation_accept(tx, request, decode(&request.payload)?).await,
        ("B013", "revoke") => invitation_revoke(tx, request, decode(&request.payload)?).await,
        ("B013", "remove") => member_remove(tx, decode(&request.payload)?).await,
        ("B013", "list") => member_list(tx, decode(&request.payload)?).await,
        ("B014", "create") => group_create(tx, decode(&request.payload)?).await,
        ("B014", "list") => group_list(tx, decode(&request.payload)?).await,
        ("B014", "member_add") => group_member_add(tx, decode(&request.payload)?).await,
        ("B014", "member_remove") => group_member_remove(tx, decode(&request.payload)?).await,
        ("B014", "space_grant") => group_space_grant(tx, decode(&request.payload)?).await,
        ("B014", "space_revoke") => group_space_revoke(tx, decode(&request.payload)?).await,
        ("B015", "claim") => ownership_claim(tx, decode(&request.payload)?).await,
        ("B015", "transfer") => ownership_transfer(tx, decode(&request.payload)?).await,
        ("B015", "delegate") => ownership_delegate(tx, decode(&request.payload)?).await,
        ("B015", "delegate_revoke") => delegation_revoke(tx, decode(&request.payload)?).await,
        ("B016", "get") => profile_get(tx, decode(&request.payload)?).await,
        ("B016", "update") => profile_update(tx, request, decode(&request.payload)?).await,
        ("B017", "create") => comment_create(tx, decode(&request.payload)?).await,
        ("B017", "list") => comment_list(tx, decode(&request.payload)?).await,
        ("B017", "delete") => comment_delete(tx, decode(&request.payload)?).await,
        ("B018", "mention") => mention_create(tx, decode(&request.payload)?).await,
        ("B018", "subscribe") => subscription_create(tx, decode(&request.payload)?).await,
        ("B018", "unsubscribe") => subscription_remove(tx, decode(&request.payload)?).await,
        ("B018", "mentions") => mention_list(tx, decode(&request.payload)?).await,
        ("B018", "subscribers") => subscription_list(tx, decode(&request.payload)?).await,
        ("B019", "list") => activity_list(tx, decode(&request.payload)?).await,
        ("B020", "create") => share_create(tx, decode(&request.payload)?).await,
        ("B020", "redeem") => share_redeem(tx, decode(&request.payload)?).await,
        ("B020", "revoke") => share_revoke(tx, request, decode(&request.payload)?).await,
        _ => Err(AppError::invalid("unsupported_collaboration_action")),
    }
}

fn decode<T: DeserializeOwned>(value: &Value) -> AppResult<T> {
    serde_json::from_value(value.clone()).map_err(|_| AppError::invalid("invalid_input"))
}

async fn get_record(tx: &mut AppTx, kind: &str, id: Uuid) -> AppResult<Record> {
    tx.get(kind, id).await
}

async fn get_record_for_update(tx: &mut AppTx, kind: &str, id: Uuid) -> AppResult<Record> {
    tx.get_for_update(kind, id).await
}

async fn insert_record(tx: &mut AppTx, kind: &str, data: Value) -> AppResult<Record> {
    tx.insert(kind, Uuid::new_v4(), data).await
}

async fn update_record(
    tx: &mut AppTx,
    request: &OperationRequest,
    record: Record,
    data: Value,
) -> AppResult<Record> {
    if request
        .expected_version
        .is_some_and(|expected| expected != record.version)
    {
        return Err(AppError::conflict("stale_record_version"));
    }
    tx.update(&record.kind, record.id, record.version, data)
        .await
}

async fn list_kind(tx: &mut AppTx, kind: &str, limit: u32) -> AppResult<Vec<Record>> {
    tx.list(kind, limit.min(MAX_LIST), None).await
}

async fn find_kind(
    tx: &mut AppTx,
    kind: &str,
    field: &str,
    value: &Value,
) -> AppResult<Vec<Record>> {
    tx.find(kind, field, value, MAX_LIST).await
}

fn actor_id(tx: &AppTx) -> Uuid {
    tx.actor().principal_id()
}

fn tenant_id(tx: &AppTx) -> Uuid {
    tx.actor().tenant_id()
}

fn has_tenant_admin_role(tx: &AppTx) -> bool {
    tx.actor().roles().contains("owner") || tx.actor().roles().contains("admin")
}

fn require_tenant_admin(tx: &AppTx) -> AppResult<()> {
    if has_tenant_admin_role(tx) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn require_tenant_member(tx: &AppTx) -> AppResult<()> {
    if has_tenant_admin_role(tx)
        || tx.actor().roles().contains("member")
        || tx.actor().roles().contains("collaborator")
    {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn require_owner_or_admin(tx: &AppTx, owner_id: Uuid) -> AppResult<()> {
    if actor_id(tx) == owner_id || has_tenant_admin_role(tx) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

async fn audit(
    tx: &mut AppTx,
    component: &str,
    action: &str,
    resource_id: Option<Uuid>,
    payload: Value,
) -> AppResult<()> {
    tx.audit(component, action, resource_id, payload).await
}

fn record_uuid(record: &Record, field: &str) -> Option<Uuid> {
    record
        .data
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn record_str<'a>(record: &'a Record, field: &str) -> Option<&'a str> {
    record.data.get(field).and_then(Value::as_str)
}

fn record_time(record: &Record, field: &str) -> Option<DateTime<Utc>> {
    record
        .data
        .get(field)
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn checked_name(value: String, max_chars: usize) -> AppResult<String> {
    let normalized = value.trim();
    if normalized.is_empty() || normalized.chars().count() > max_chars {
        return Err(AppError::invalid("invalid_name"));
    }
    Ok(normalized.to_owned())
}

fn opaque_token() -> AppResult<String> {
    let mut bytes = [0_u8; 32];
    random_fill(&mut bytes).map_err(|_| AppError::Unavailable)?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        bytes,
    ))
}

fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn effective_role_name(role: SpaceRole) -> &'static str {
    match role {
        SpaceRole::Viewer => "viewer",
        SpaceRole::Commenter => "commenter",
        SpaceRole::Editor => "editor",
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum SpaceRole {
    Viewer,
    Commenter,
    Editor,
}

impl SpaceRole {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "viewer" => Some(Self::Viewer),
            "commenter" => Some(Self::Commenter),
            "editor" => Some(Self::Editor),
            _ => None,
        }
    }
}

fn role_data(value: SpaceRole) -> &'static str {
    effective_role_name(value)
}

fn json_uuid(value: Uuid) -> Value {
    Value::String(value.to_string())
}

fn resource_space_id(resource: &Record) -> Option<Uuid> {
    record_uuid(resource, "space_id")
}

fn check_resource_identity(kind: &str, id: Uuid) -> AppResult<()> {
    if kind.is_empty() || kind.len() > 64 || kind.starts_with("app_") {
        return Err(AppError::invalid("invalid_resource_kind"));
    }
    let canonical = kind
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if !canonical || id.is_nil() {
        return Err(AppError::invalid("invalid_resource"));
    }
    Ok(())
}

fn html_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

async fn database_now(tx: &mut AppTx) -> AppResult<DateTime<Utc>> {
    sqlx::query_scalar::<_, DateTime<Utc>>("SELECT clock_timestamp()")
        .fetch_one(tx.conn())
        .await
        .map_err(|_| AppError::Internal)
}

async fn ensure_deadline(tx: &mut AppTx, hours: i64, maximum: i64) -> AppResult<DateTime<Utc>> {
    if !(1..=maximum).contains(&hours) {
        return Err(AppError::invalid("invalid_duration"));
    }
    sqlx::query_scalar::<_, DateTime<Utc>>("SELECT clock_timestamp() + ($1 * INTERVAL '1 hour')")
        .bind(hours)
        .fetch_one(tx.conn())
        .await
        .map_err(|_| AppError::Internal)
}

fn actor_roles(tx: &AppTx) -> BTreeSet<String> {
    tx.actor().roles().iter().cloned().collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdInput {
    id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameInput {
    name: String,
}

async fn tenant_context(tx: &mut AppTx, _input: EmptyInput) -> AppResult<Value> {
    // The actor is constructed from the server-verified session inside AppTx.
    // In particular, the request has no tenant_id or role field to override it.
    require_tenant_member(tx)?;
    Ok(json!({
        "tenant_id": tenant_id(tx),
        "principal_id": actor_id(tx),
        "roles": actor_roles(tx),
        "scopes": tx.actor().scopes(),
    }))
}

async fn active_tenant_member(tx: &mut AppTx, principal_id: Uuid) -> AppResult<bool> {
    let rows = find_kind(
        tx,
        "tenant_membership",
        "principal_id",
        &json_uuid(principal_id),
    )
    .await?;
    if let Some(found) = rows
        .into_iter()
        .find(|row| record_uuid(row, "principal_id") == Some(principal_id))
    {
        let row = match tx.get("tenant_membership", found.id).await {
            Ok(row) => row,
            Err(AppError::NotFound) => return Ok(false),
            Err(error) => return Err(error),
        };
        return Ok(record_str(&row, "status") == Some("active"));
    }
    if principal_id == actor_id(tx) {
        return Ok(has_tenant_admin_role(tx)
            || tx.actor().roles().contains("member")
            || tx.actor().roles().contains("collaborator"));
    }
    // Bootstrap members may have an authoritative application role before an
    // invitation record exists. Checking another principal needs its fresh
    // server-side roles, never the current actor's roles. An explicit removed
    // tenant membership above still wins over this bootstrap fallback.
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public.app_memberships WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3 AND status='active' AND role IN ('owner','admin','member','collaborator'))")
        .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(principal_id)
        .fetch_one(tx.conn()).await.map_err(Into::into)
}

async fn require_active_tenant_member(tx: &mut AppTx) -> AppResult<()> {
    if active_tenant_member(tx, actor_id(tx)).await? {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

async fn space_create(tx: &mut AppTx, input: NameInput) -> AppResult<Value> {
    require_tenant_admin(tx)?;
    let name = checked_name(input.name, MAX_SPACE_NAME_CHARS)?;
    let record = insert_record(
        tx,
        "space",
        json!({
            "tenant_id": tenant_id(tx),
            "owner_id": actor_id(tx),
            "name": name,
            "created_at": now(),
            "status": "active"
        }),
    )
    .await?;
    audit(
        tx,
        "B012",
        "create",
        Some(record.id),
        json!({ "space_id": record.id }),
    )
    .await?;
    Ok(json!({ "id": record.id, "name": name, "role": "owner" }))
}

async fn space_list(tx: &mut AppTx, _input: EmptyInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let mut spaces = Vec::new();
    for space in list_kind(tx, "space", MAX_LIST).await? {
        if record_str(&space, "status") != Some("active") {
            continue;
        }
        if let Some(role) = effective_space_role(tx, space.id).await? {
            spaces.push(json!({
                "id": space.id,
                "name": record_str(&space, "name").unwrap_or(""),
                "role": effective_role_name(role),
            }));
        }
    }
    Ok(json!({ "spaces": spaces }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpaceGrantInput {
    space_id: Uuid,
    principal_id: Uuid,
    role: SpaceRole,
}

async fn space_grant(tx: &mut AppTx, input: SpaceGrantInput) -> AppResult<Value> {
    if input.principal_id.is_nil() {
        return Err(AppError::invalid("invalid_principal"));
    }
    if !active_tenant_member(tx, input.principal_id).await? {
        return Err(AppError::NotFound);
    }
    let space = get_record(tx, "space", input.space_id).await?;
    require_owner_or_admin(
        tx,
        record_uuid(&space, "owner_id").ok_or(AppError::Internal)?,
    )?;
    let existing = find_kind(tx, "space_access", "space_id", &json_uuid(space.id)).await?;
    if let Some(mut access) = existing.into_iter().find(|row| {
        record_uuid(row, "principal_id") == Some(input.principal_id)
            && record_str(row, "status") == Some("active")
    }) {
        let mut data = access.data.clone();
        data["role"] = json!(role_data(input.role));
        access = tx
            .update("space_access", access.id, access.version, data)
            .await?;
        audit(
            tx,
            "B012",
            "grant",
            Some(space.id),
            json!({ "principal_id": input.principal_id, "role": role_data(input.role) }),
        )
        .await?;
        return Ok(json!({ "access_id": access.id, "role": role_data(input.role) }));
    }
    let access = insert_record(
        tx,
        "space_access",
        json!({
            "tenant_id": tenant_id(tx),
            "space_id": space.id,
            "principal_id": input.principal_id,
            "role": role_data(input.role),
            "status": "active",
            "created_by": actor_id(tx),
            "created_at": now()
        }),
    )
    .await?;
    audit(
        tx,
        "B012",
        "grant",
        Some(space.id),
        json!({ "principal_id": input.principal_id, "role": role_data(input.role) }),
    )
    .await?;
    Ok(json!({ "access_id": access.id, "role": role_data(input.role) }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpaceAccessInput {
    space_id: Uuid,
    principal_id: Uuid,
}

async fn space_revoke(tx: &mut AppTx, input: SpaceAccessInput) -> AppResult<Value> {
    let space = get_record(tx, "space", input.space_id).await?;
    require_owner_or_admin(
        tx,
        record_uuid(&space, "owner_id").ok_or(AppError::Internal)?,
    )?;
    let rows = find_kind(tx, "space_access", "space_id", &json_uuid(space.id)).await?;
    let Some(access) = rows.into_iter().find(|row| {
        record_uuid(row, "principal_id") == Some(input.principal_id)
            && record_str(row, "status") == Some("active")
    }) else {
        return Ok(json!({ "revoked": false }));
    };
    let mut data = access.data.clone();
    data["status"] = json!("revoked");
    data["revoked_at"] = json!(now());
    tx.update("space_access", access.id, access.version, data)
        .await?;
    audit(
        tx,
        "B012",
        "revoke",
        Some(space.id),
        json!({ "principal_id": input.principal_id }),
    )
    .await?;
    Ok(json!({ "revoked": true }))
}

async fn effective_space_role(tx: &mut AppTx, space_id: Uuid) -> AppResult<Option<SpaceRole>> {
    // AppTx holds the application authority fence throughout this lookup.
    // These checks also run in SQL READ ONLY transactions; row mutation locks
    // are reserved for the commands that actually change an authority record.
    if !active_tenant_member(tx, actor_id(tx)).await? {
        return Ok(None);
    }
    let space = get_record(tx, "space", space_id).await?;
    if record_str(&space, "status") != Some("active") {
        return Ok(None);
    }
    if record_uuid(&space, "owner_id") == Some(actor_id(tx)) {
        return Ok(Some(SpaceRole::Editor));
    }
    let mut best = None;
    let access_rows = find_kind(tx, "space_access", "space_id", &json_uuid(space_id)).await?;
    for found in access_rows {
        let access = match tx.get("space_access", found.id).await {
            Ok(access) => access,
            Err(AppError::NotFound) => continue,
            Err(error) => return Err(error),
        };
        if record_uuid(&access, "principal_id") == Some(actor_id(tx))
            && record_str(&access, "status") == Some("active")
            && let Some(role) = SpaceRole::parse(record_str(&access, "role").unwrap_or(""))
        {
            best = Some(best.map_or(role, |current: SpaceRole| current.max(role)));
        }
    }
    let group_memberships =
        find_kind(tx, "group_member", "principal_id", &json_uuid(actor_id(tx))).await?;
    for found_membership in group_memberships {
        let membership = match tx.get("group_member", found_membership.id).await {
            Ok(membership) => membership,
            Err(AppError::NotFound) => continue,
            Err(error) => return Err(error),
        };
        if record_str(&membership, "status") != Some("active") {
            continue;
        }
        let Some(group_id) = record_uuid(&membership, "group_id") else {
            continue;
        };
        for found_grant in
            find_kind(tx, "group_space_access", "space_id", &json_uuid(space_id)).await?
        {
            let grant = match tx.get("group_space_access", found_grant.id).await {
                Ok(grant) => grant,
                Err(AppError::NotFound) => continue,
                Err(error) => return Err(error),
            };
            if record_uuid(&grant, "group_id") == Some(group_id)
                && record_str(&grant, "status") == Some("active")
                && let Some(role) = SpaceRole::parse(record_str(&grant, "role").unwrap_or(""))
            {
                best = Some(best.map_or(role, |current: SpaceRole| current.max(role)));
            }
        }
    }
    Ok(best)
}

async fn require_space_role(tx: &mut AppTx, space_id: Uuid, minimum: SpaceRole) -> AppResult<()> {
    if effective_space_role(tx, space_id)
        .await?
        .is_some_and(|role| role >= minimum)
    {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum InviteRole {
    Member,
    Collaborator,
}

fn invite_role_name(role: InviteRole) -> &'static str {
    match role {
        InviteRole::Member => "member",
        InviteRole::Collaborator => "collaborator",
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvitationCreateInput {
    target_principal_id: Uuid,
    role: InviteRole,
    expires_in_hours: i64,
}

async fn invitation_create(tx: &mut AppTx, input: InvitationCreateInput) -> AppResult<Value> {
    require_tenant_admin(tx)?;
    if input.target_principal_id.is_nil() || input.target_principal_id == actor_id(tx) {
        return Err(AppError::invalid("invalid_invitation_target"));
    }
    let expires_at = ensure_deadline(tx, input.expires_in_hours, MAX_INVITE_HOURS).await?;
    let token = opaque_token()?;
    let invite = insert_record(
        tx,
        "invitation",
        json!({
            "tenant_id": tenant_id(tx),
            "created_by": actor_id(tx),
            "target_principal_id": input.target_principal_id,
            "role": invite_role_name(input.role),
            "token_hash": token_hash(&token),
            "expires_at": expires_at,
            "created_at": now(),
            "accepted_at": null,
            "accepted_by": null,
            "revoked_at": null
        }),
    )
    .await?;
    audit(
        tx,
        "B013",
        "invite",
        Some(invite.id),
        json!({ "target_principal_id": input.target_principal_id, "role": invite_role_name(input.role) }),
    )
    .await?;
    Ok(json!({ "invitation_id": invite.id, "token": token, "expires_at": expires_at }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvitationAcceptInput {
    token: String,
}

async fn invitation_accept(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: InvitationAcceptInput,
) -> AppResult<Value> {
    if input.token.len() != 43
        || !input
            .token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(AppError::NotFound);
    }
    let hash = token_hash(&input.token);
    let rows = find_kind(tx, "invitation", "token_hash", &json!(hash)).await?;
    let Some(found) = rows
        .into_iter()
        .find(|row| record_str(row, "token_hash") == Some(hash.as_str()))
    else {
        return Err(AppError::NotFound);
    };
    let invitation = get_record_for_update(tx, "invitation", found.id).await?;
    let target = record_uuid(&invitation, "target_principal_id").ok_or(AppError::Internal)?;
    if target != actor_id(tx) {
        return Err(AppError::NotFound);
    }
    let db_now = database_now(tx).await?;
    if record_time(&invitation, "expires_at").is_none_or(|expires| expires <= db_now)
        || record_time(&invitation, "revoked_at").is_some()
        || record_time(&invitation, "accepted_at").is_some()
    {
        return Err(AppError::Conflict("invitation_unavailable"));
    }
    let role = record_str(&invitation, "role").ok_or(AppError::Internal)?;
    if !matches!(role, "member" | "collaborator") {
        return Err(AppError::Forbidden);
    }
    let member_rows = find_kind(
        tx,
        "tenant_membership",
        "principal_id",
        &json_uuid(actor_id(tx)),
    )
    .await?;
    if member_rows.into_iter().any(|row| {
        record_uuid(&row, "principal_id") == Some(actor_id(tx))
            && record_str(&row, "status") == Some("active")
    }) {
        return Err(AppError::Conflict("already_a_tenant_member"));
    }
    let existing_removed = find_kind(
        tx,
        "tenant_membership",
        "principal_id",
        &json_uuid(actor_id(tx)),
    )
    .await?
    .into_iter()
    .find(|row| record_uuid(row, "principal_id") == Some(actor_id(tx)));
    if let Some(existing) = existing_removed {
        let mut data = existing.data.clone();
        data["role"] = json!(role);
        data["status"] = json!("active");
        data["accepted_at"] = json!(now());
        tx.update("tenant_membership", existing.id, existing.version, data)
            .await?;
    } else {
        insert_record(
            tx,
            "tenant_membership",
            json!({
                "tenant_id": tenant_id(tx),
                "principal_id": actor_id(tx),
                "role": role,
                "status": "active",
                "accepted_at": now()
            }),
        )
        .await?;
    }
    let mut invite_data = invitation.data.clone();
    invite_data["accepted_at"] = json!(now());
    invite_data["accepted_by"] = json!(actor_id(tx));
    update_record(tx, request, invitation.clone(), invite_data).await?;
    audit(
        tx,
        "B013",
        "accept",
        Some(found.id),
        json!({ "principal_id": actor_id(tx) }),
    )
    .await?;
    Ok(json!({ "membership": "active", "role": role }))
}

async fn invitation_revoke(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: IdInput,
) -> AppResult<Value> {
    let invitation = get_record_for_update(tx, "invitation", input.id).await?;
    let creator = record_uuid(&invitation, "created_by").ok_or(AppError::Internal)?;
    require_owner_or_admin(tx, creator)?;
    if record_time(&invitation, "accepted_at").is_some() {
        return Err(AppError::Conflict("invitation_already_consumed"));
    }
    if record_time(&invitation, "revoked_at").is_some() {
        return Ok(json!({ "revoked": false }));
    }
    let mut data = invitation.data.clone();
    data["revoked_at"] = json!(now());
    update_record(tx, request, invitation, data).await?;
    audit(tx, "B013", "revoke", Some(input.id), json!({})).await?;
    Ok(json!({ "revoked": true }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberRemoveInput {
    principal_id: Uuid,
}

async fn member_remove(tx: &mut AppTx, input: MemberRemoveInput) -> AppResult<Value> {
    require_tenant_admin(tx)?;
    if input.principal_id == actor_id(tx) {
        return Err(AppError::Forbidden);
    }
    let rows = find_kind(
        tx,
        "tenant_membership",
        "principal_id",
        &json_uuid(input.principal_id),
    )
    .await?;
    let Some(member) = rows.into_iter().find(|row| {
        record_uuid(row, "principal_id") == Some(input.principal_id)
            && record_str(row, "status") == Some("active")
    }) else {
        return Ok(json!({ "removed": false }));
    };
    if record_str(&member, "role") == Some("owner") {
        return Err(AppError::Conflict("cannot_remove_tenant_owner"));
    }
    let mut data = member.data.clone();
    data["status"] = json!("removed");
    data["removed_at"] = json!(now());
    tx.update("tenant_membership", member.id, member.version, data)
        .await?;
    audit(
        tx,
        "B013",
        "remove",
        Some(member.id),
        json!({ "principal_id": input.principal_id }),
    )
    .await?;
    Ok(json!({ "removed": true }))
}

async fn member_list(tx: &mut AppTx, _input: EmptyInput) -> AppResult<Value> {
    require_tenant_admin(tx)?;
    let members = list_kind(tx, "tenant_membership", MAX_LIST)
        .await?
        .into_iter()
        .filter(|row| record_str(row, "status") == Some("active"))
        .map(|row| {
            json!({
                "principal_id": record_uuid(&row, "principal_id"),
                "role": record_str(&row, "role"),
                "accepted_at": row.data.get("accepted_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "members": members }))
}

async fn group_create(tx: &mut AppTx, input: NameInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let name = checked_name(input.name, MAX_GROUP_NAME_CHARS)?;
    let creator = actor_id(tx);
    let group = insert_record(
        tx,
        "collaboration_group",
        json!({
            "tenant_id": tenant_id(tx),
            "name": name,
            "owner_id": creator,
            "created_by": creator,
            "created_at": now(),
            "status": "active"
        }),
    )
    .await?;
    insert_record(
        tx,
        "group_member",
        json!({
            "tenant_id": tenant_id(tx),
            "group_id": group.id,
            "principal_id": creator,
            "status": "active",
            "created_by": creator,
            "created_at": now()
        }),
    )
    .await?;
    audit(
        tx,
        "B014",
        "create",
        Some(group.id),
        json!({ "owner_id": creator }),
    )
    .await?;
    Ok(json!({ "id": group.id, "name": name }))
}

async fn group_list(tx: &mut AppTx, _input: EmptyInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let memberships =
        find_kind(tx, "group_member", "principal_id", &json_uuid(actor_id(tx))).await?;
    let visible_groups = memberships
        .into_iter()
        .filter(|member| record_str(member, "status") == Some("active"))
        .filter_map(|member| record_uuid(&member, "group_id"))
        .collect::<BTreeSet<_>>();
    let groups = list_kind(tx, "collaboration_group", MAX_LIST)
        .await?
        .into_iter()
        .filter(|group| {
            record_str(group, "status") == Some("active")
                && (has_tenant_admin_role(tx)
                    || record_uuid(group, "owner_id") == Some(actor_id(tx))
                    || visible_groups.contains(&group.id))
        })
        .map(|group| json!({ "id": group.id, "name": record_str(&group, "name") }))
        .collect::<Vec<_>>();
    Ok(json!({ "groups": groups }))
}

async fn require_group_manager(tx: &mut AppTx, group_id: Uuid) -> AppResult<Record> {
    let group = get_record(tx, "collaboration_group", group_id).await?;
    if record_str(&group, "status") != Some("active") {
        return Err(AppError::NotFound);
    }
    let owner = record_uuid(&group, "owner_id").ok_or(AppError::Internal)?;
    require_owner_or_admin(tx, owner)?;
    Ok(group)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupMemberInput {
    group_id: Uuid,
    principal_id: Uuid,
}

async fn group_member_add(tx: &mut AppTx, input: GroupMemberInput) -> AppResult<Value> {
    let group = require_group_manager(tx, input.group_id).await?;
    if !active_tenant_member(tx, input.principal_id).await? {
        return Err(AppError::NotFound);
    }
    let members = find_kind(tx, "group_member", "group_id", &json_uuid(group.id)).await?;
    if let Some(existing) = members.into_iter().find(|row| {
        record_uuid(row, "principal_id") == Some(input.principal_id)
            && record_str(row, "status") == Some("active")
    }) {
        return Ok(json!({ "membership_id": existing.id, "added": false }));
    }
    let prior = find_kind(tx, "group_member", "group_id", &json_uuid(group.id))
        .await?
        .into_iter()
        .find(|row| record_uuid(row, "principal_id") == Some(input.principal_id));
    let member = if let Some(prior) = prior {
        let mut data = prior.data.clone();
        data["status"] = json!("active");
        data["created_by"] = json!(actor_id(tx));
        data["created_at"] = json!(now());
        tx.update("group_member", prior.id, prior.version, data)
            .await?
    } else {
        let tenant = tenant_id(tx);
        let creator = actor_id(tx);
        insert_record(
            tx,
            "group_member",
            json!({
                "tenant_id": tenant,
                "group_id": group.id,
                "principal_id": input.principal_id,
                "status": "active",
                "created_by": creator,
                "created_at": now()
            }),
        )
        .await?
    };
    audit(
        tx,
        "B014",
        "member_add",
        Some(group.id),
        json!({ "principal_id": input.principal_id }),
    )
    .await?;
    Ok(json!({ "membership_id": member.id, "added": true }))
}

async fn group_member_remove(tx: &mut AppTx, input: GroupMemberInput) -> AppResult<Value> {
    let group = require_group_manager(tx, input.group_id).await?;
    let members = find_kind(tx, "group_member", "group_id", &json_uuid(group.id)).await?;
    let Some(member) = members.into_iter().find(|row| {
        record_uuid(row, "principal_id") == Some(input.principal_id)
            && record_str(row, "status") == Some("active")
    }) else {
        return Ok(json!({ "removed": false }));
    };
    if input.principal_id == record_uuid(&group, "owner_id").ok_or(AppError::Internal)? {
        return Err(AppError::Conflict("cannot_remove_group_owner"));
    }
    let mut data = member.data.clone();
    data["status"] = json!("removed");
    data["removed_at"] = json!(now());
    tx.update("group_member", member.id, member.version, data)
        .await?;
    audit(
        tx,
        "B014",
        "member_remove",
        Some(group.id),
        json!({ "principal_id": input.principal_id }),
    )
    .await?;
    Ok(json!({ "removed": true }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupSpaceGrantInput {
    group_id: Uuid,
    space_id: Uuid,
    role: SpaceRole,
}

async fn group_space_grant(tx: &mut AppTx, input: GroupSpaceGrantInput) -> AppResult<Value> {
    let group = require_group_manager(tx, input.group_id).await?;
    require_space_role(tx, input.space_id, SpaceRole::Editor).await?;
    let rows = find_kind(
        tx,
        "group_space_access",
        "space_id",
        &json_uuid(input.space_id),
    )
    .await?;
    if let Some(existing) = rows.into_iter().find(|row| {
        record_uuid(row, "group_id") == Some(group.id)
            && record_str(row, "status") == Some("active")
    }) {
        let mut data = existing.data.clone();
        data["role"] = json!(role_data(input.role));
        tx.update("group_space_access", existing.id, existing.version, data)
            .await?;
        audit(
            tx,
            "B014",
            "space_grant",
            Some(input.space_id),
            json!({ "group_id": group.id, "role": role_data(input.role) }),
        )
        .await?;
        return Ok(json!({ "granted": true, "role": role_data(input.role) }));
    }
    let tenant = tenant_id(tx);
    let creator = actor_id(tx);
    insert_record(
        tx,
        "group_space_access",
        json!({
            "tenant_id": tenant,
            "group_id": group.id,
            "space_id": input.space_id,
            "role": role_data(input.role),
            "status": "active",
            "created_by": creator,
            "created_at": now()
        }),
    )
    .await?;
    audit(
        tx,
        "B014",
        "space_grant",
        Some(input.space_id),
        json!({ "group_id": group.id, "role": role_data(input.role) }),
    )
    .await?;
    Ok(json!({ "granted": true, "role": role_data(input.role) }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupSpaceRevokeInput {
    group_id: Uuid,
    space_id: Uuid,
}

async fn group_space_revoke(tx: &mut AppTx, input: GroupSpaceRevokeInput) -> AppResult<Value> {
    let group = require_group_manager(tx, input.group_id).await?;
    require_space_role(tx, input.space_id, SpaceRole::Editor).await?;
    let rows = find_kind(
        tx,
        "group_space_access",
        "space_id",
        &json_uuid(input.space_id),
    )
    .await?;
    let Some(access) = rows.into_iter().find(|row| {
        record_uuid(row, "group_id") == Some(group.id)
            && record_str(row, "status") == Some("active")
    }) else {
        return Ok(json!({ "revoked": false }));
    };
    let mut data = access.data.clone();
    data["status"] = json!("revoked");
    data["revoked_at"] = json!(now());
    tx.update("group_space_access", access.id, access.version, data)
        .await?;
    audit(
        tx,
        "B014",
        "space_revoke",
        Some(input.space_id),
        json!({ "group_id": group.id }),
    )
    .await?;
    Ok(json!({ "revoked": true }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceInput {
    resource_kind: String,
    resource_id: Uuid,
}

async fn load_business_resource(tx: &mut AppTx, input: &ResourceInput) -> AppResult<Record> {
    check_resource_identity(&input.resource_kind, input.resource_id)?;
    get_record(tx, &input.resource_kind, input.resource_id).await
}

async fn effective_resource_role(
    tx: &mut AppTx,
    resource: &Record,
) -> AppResult<Option<SpaceRole>> {
    let Some(space_id) = resource_space_id(resource) else {
        return Ok(None);
    };
    if let Some(role) = effective_space_role(tx, space_id).await? {
        return Ok(Some(role));
    }
    let delegation_rows = find_kind(
        tx,
        "resource_delegation",
        "resource_id",
        &json_uuid(resource.id),
    )
    .await?;
    let mut delegations = Vec::with_capacity(delegation_rows.len());
    for row in delegation_rows {
        match tx.get("resource_delegation", row.id).await {
            Ok(locked) => delegations.push(locked),
            Err(AppError::NotFound) => continue,
            Err(error) => return Err(error),
        }
    }
    let mut best = None;
    let db_now = database_now(tx).await?;
    for delegation in delegations {
        if record_str(&delegation, "resource_kind") != Some(resource.kind.as_str())
            || record_uuid(&delegation, "principal_id") != Some(actor_id(tx))
            || record_str(&delegation, "status") != Some("active")
            || record_time(&delegation, "expires_at").is_none_or(|expiry| expiry <= db_now)
        {
            continue;
        }
        let role = match record_str(&delegation, "scope") {
            Some("read") => Some(SpaceRole::Viewer),
            Some("comment") => Some(SpaceRole::Commenter),
            Some("write") => Some(SpaceRole::Editor),
            _ => None,
        };
        if let Some(role) = role {
            best = Some(best.map_or(role, |current: SpaceRole| current.max(role)));
        }
    }
    Ok(best)
}

async fn require_resource_role(
    tx: &mut AppTx,
    resource: &Record,
    minimum: SpaceRole,
) -> AppResult<()> {
    if effective_resource_role(tx, resource)
        .await?
        .is_some_and(|role| role >= minimum)
    {
        Ok(())
    } else {
        // Treat an invisible resource and an unauthorized resource identically.
        Err(AppError::NotFound)
    }
}

async fn ownership_row(tx: &mut AppTx, resource: &Record) -> AppResult<Option<Record>> {
    let rows = find_kind(tx, "resource_owner", "resource_id", &json_uuid(resource.id)).await?;
    Ok(rows
        .into_iter()
        .find(|row| record_str(row, "resource_kind") == Some(resource.kind.as_str())))
}

async fn resource_owner_id(tx: &mut AppTx, resource: &Record) -> AppResult<Option<Uuid>> {
    if let Some(owner) = ownership_row(tx, resource).await? {
        return Ok(record_uuid(&owner, "owner_id"));
    }
    Ok(record_uuid(resource, "owner_id").or_else(|| record_uuid(resource, "created_by")))
}

async fn ensure_resource_owner(tx: &mut AppTx, resource: &Record) -> AppResult<Uuid> {
    resource_owner_id(tx, resource)
        .await?
        .ok_or(AppError::NotFound)
}

async fn ownership_claim(tx: &mut AppTx, input: ResourceInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let resource = load_business_resource(tx, &input).await?;
    let space_id = resource_space_id(&resource).ok_or(AppError::NotFound)?;
    require_space_role(tx, space_id, SpaceRole::Editor).await?;
    if let Some(owner_id) = resource_owner_id(tx, &resource).await? {
        if owner_id == actor_id(tx) {
            return Ok(json!({ "owner_id": owner_id, "claimed": false }));
        }
        return Err(AppError::Conflict("resource_already_owned"));
    }
    let tenant = tenant_id(tx);
    let owner_id = actor_id(tx);
    insert_record(
        tx,
        "resource_owner",
        json!({
            "tenant_id": tenant,
            "resource_kind": resource.kind,
            "resource_id": resource.id,
            "space_id": space_id,
            "owner_id": owner_id,
            "updated_by": owner_id,
            "updated_at": now()
        }),
    )
    .await?;
    audit(
        tx,
        "B015",
        "claim",
        Some(resource.id),
        json!({ "resource_kind": resource.kind, "space_id": space_id }),
    )
    .await?;
    Ok(json!({ "owner_id": owner_id, "claimed": true }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnershipTransferInput {
    resource_kind: String,
    resource_id: Uuid,
    new_owner_id: Uuid,
}

async fn ownership_transfer(tx: &mut AppTx, input: OwnershipTransferInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    if input.new_owner_id == actor_id(tx) || input.new_owner_id.is_nil() {
        return Err(AppError::invalid("invalid_new_owner"));
    }
    if !active_tenant_member(tx, input.new_owner_id).await? {
        return Err(AppError::NotFound);
    }
    let resource_input = ResourceInput {
        resource_kind: input.resource_kind,
        resource_id: input.resource_id,
    };
    let resource = load_business_resource(tx, &resource_input).await?;
    let space_id = resource_space_id(&resource).ok_or(AppError::NotFound)?;
    require_space_role(tx, space_id, SpaceRole::Editor).await?;
    let old_owner = ensure_resource_owner(tx, &resource).await?;
    if old_owner != actor_id(tx) {
        return Err(AppError::Forbidden);
    }
    if !member_has_space_role(tx, space_id, input.new_owner_id, SpaceRole::Viewer).await? {
        return Err(AppError::Forbidden);
    }
    if let Some(row) = ownership_row(tx, &resource).await? {
        let mut data = row.data.clone();
        data["owner_id"] = json!(input.new_owner_id);
        data["updated_by"] = json!(actor_id(tx));
        data["updated_at"] = json!(now());
        tx.update("resource_owner", row.id, row.version, data)
            .await?;
    } else {
        let tenant = tenant_id(tx);
        let actor = actor_id(tx);
        insert_record(
            tx,
            "resource_owner",
            json!({
                "tenant_id": tenant,
                "resource_kind": resource.kind,
                "resource_id": resource.id,
                "space_id": space_id,
                "owner_id": input.new_owner_id,
                "updated_by": actor,
                "updated_at": now()
            }),
        )
        .await?;
    }
    audit(
        tx,
        "B015",
        "transfer",
        Some(resource.id),
        json!({ "from_owner_id": old_owner, "to_owner_id": input.new_owner_id, "tenant_id": tenant_id(tx), "space_id": space_id }),
    )
    .await?;
    Ok(json!({ "owner_id": input.new_owner_id, "transferred": true }))
}

async fn member_has_space_role(
    tx: &mut AppTx,
    space_id: Uuid,
    principal_id: Uuid,
    minimum: SpaceRole,
) -> AppResult<bool> {
    if !active_tenant_member(tx, principal_id).await? {
        return Ok(false);
    }
    let space = get_record(tx, "space", space_id).await?;
    if record_uuid(&space, "owner_id") == Some(principal_id) {
        return Ok(true);
    }
    let mut best = None;
    let direct_rows = find_kind(tx, "space_access", "space_id", &json_uuid(space_id)).await?;
    for found in direct_rows {
        let access = match tx.get("space_access", found.id).await {
            Ok(access) => access,
            Err(AppError::NotFound) => continue,
            Err(error) => return Err(error),
        };
        if record_uuid(&access, "principal_id") == Some(principal_id)
            && record_str(&access, "status") == Some("active")
            && let Some(role) = SpaceRole::parse(record_str(&access, "role").unwrap_or(""))
        {
            best = Some(best.map_or(role, |current: SpaceRole| current.max(role)));
        }
    }
    let membership_rows =
        find_kind(tx, "group_member", "principal_id", &json_uuid(principal_id)).await?;
    let mut member_groups = BTreeSet::new();
    for found in membership_rows {
        let membership = match tx.get("group_member", found.id).await {
            Ok(membership) => membership,
            Err(AppError::NotFound) => continue,
            Err(error) => return Err(error),
        };
        if record_str(&membership, "status") == Some("active")
            && let Some(group_id) = record_uuid(&membership, "group_id")
        {
            member_groups.insert(group_id);
        }
    }
    let grant_rows = find_kind(tx, "group_space_access", "space_id", &json_uuid(space_id)).await?;
    for found in grant_rows {
        let grant = match tx.get("group_space_access", found.id).await {
            Ok(grant) => grant,
            Err(AppError::NotFound) => continue,
            Err(error) => return Err(error),
        };
        if member_groups.contains(&record_uuid(&grant, "group_id").unwrap_or(Uuid::nil()))
            && record_str(&grant, "status") == Some("active")
            && let Some(role) = SpaceRole::parse(record_str(&grant, "role").unwrap_or(""))
        {
            best = Some(best.map_or(role, |current: SpaceRole| current.max(role)));
        }
    }
    Ok(best.is_some_and(|role| role >= minimum))
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DelegatedScope {
    Read,
    Comment,
    Write,
}

impl DelegatedScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Comment => "comment",
            Self::Write => "write",
        }
    }

    fn role(self) -> SpaceRole {
        match self {
            Self::Read => SpaceRole::Viewer,
            Self::Comment => SpaceRole::Commenter,
            Self::Write => SpaceRole::Editor,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateInput {
    resource_kind: String,
    resource_id: Uuid,
    principal_id: Uuid,
    scope: DelegatedScope,
    expires_in_hours: i64,
}

async fn ownership_delegate(tx: &mut AppTx, input: DelegateInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    if input.principal_id == actor_id(tx) || input.principal_id.is_nil() {
        return Err(AppError::invalid("self_delegation_forbidden"));
    }
    if !active_tenant_member(tx, input.principal_id).await? {
        return Err(AppError::NotFound);
    }
    let resource_input = ResourceInput {
        resource_kind: input.resource_kind,
        resource_id: input.resource_id,
    };
    let resource = load_business_resource(tx, &resource_input).await?;
    let space_id = resource_space_id(&resource).ok_or(AppError::NotFound)?;
    require_space_role(tx, space_id, input.scope.role()).await?;
    let owner = resource_owner_id(tx, &resource).await?;
    if owner != Some(actor_id(tx))
        && !member_has_space_role(tx, space_id, actor_id(tx), SpaceRole::Editor).await?
    {
        return Err(AppError::Forbidden);
    }
    let expires_at = ensure_deadline(tx, input.expires_in_hours, MAX_INVITE_HOURS).await?;
    let tenant = tenant_id(tx);
    let creator = actor_id(tx);
    let delegation = insert_record(
        tx,
        "resource_delegation",
        json!({
            "tenant_id": tenant,
            "resource_kind": resource.kind,
            "resource_id": resource.id,
            "space_id": space_id,
            "principal_id": input.principal_id,
            "scope": input.scope.as_str(),
            "created_by": creator,
            "created_at": now(),
            "expires_at": expires_at,
            "status": "active",
            "revoked_at": null
        }),
    )
    .await?;
    audit(
        tx,
        "B015",
        "delegate",
        Some(resource.id),
        json!({ "principal_id": input.principal_id, "scope": input.scope.as_str(), "space_id": space_id }),
    )
    .await?;
    Ok(
        json!({ "delegation_id": delegation.id, "scope": input.scope.as_str(), "expires_at": expires_at }),
    )
}

async fn delegation_revoke(tx: &mut AppTx, input: IdInput) -> AppResult<Value> {
    let row = get_record_for_update(tx, "resource_delegation", input.id).await?;
    let resource_id = record_uuid(&row, "resource_id").ok_or(AppError::Internal)?;
    let creator = record_uuid(&row, "created_by").ok_or(AppError::Internal)?;
    let resource_kind = record_str(&row, "resource_kind")
        .ok_or(AppError::Internal)?
        .to_owned();
    let resource = get_record(tx, &resource_kind, resource_id).await?;
    let owner = resource_owner_id(tx, &resource).await?;
    if creator != actor_id(tx) && owner != Some(actor_id(tx)) && !has_tenant_admin_role(tx) {
        return Err(AppError::Forbidden);
    }
    if record_str(&row, "status") != Some("active") {
        return Ok(json!({ "revoked": false }));
    }
    let mut data = row.data.clone();
    data["status"] = json!("revoked");
    data["revoked_at"] = json!(now());
    tx.update("resource_delegation", row.id, row.version, data)
        .await?;
    audit(
        tx,
        "B015",
        "delegate_revoke",
        Some(resource_id),
        json!({ "delegation_id": input.id }),
    )
    .await?;
    Ok(json!({ "revoked": true }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileUpdateInput {
    display_name: Option<String>,
    locale: Option<String>,
    time_zone: Option<String>,
    theme: Option<ThemePreferenceInput>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ThemePreferenceInput {
    System,
    Light,
    Dark,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileGetInput {}

fn validate_profile_name(value: String) -> AppResult<String> {
    let normalized = value.trim();
    if normalized.is_empty()
        || normalized.chars().count() > MAX_PROFILE_NAME_CHARS
        || normalized.chars().any(char::is_control)
    {
        return Err(AppError::invalid("invalid_profile_display_name"));
    }
    Ok(normalized.to_owned())
}

fn validate_locale(value: String) -> AppResult<String> {
    let parts = value.split('-').collect::<Vec<_>>();
    let valid = match parts.as_slice() {
        [language] => language.len() == 2 && language.bytes().all(|b| b.is_ascii_lowercase()),
        [language, region] => {
            language.len() == 2
                && language.bytes().all(|b| b.is_ascii_lowercase())
                && region.len() == 2
                && region.bytes().all(|b| b.is_ascii_uppercase())
        }
        _ => false,
    };
    if !valid {
        return Err(AppError::invalid("invalid_profile_locale"));
    }
    Ok(value)
}

async fn profile_get(tx: &mut AppTx, _input: ProfileGetInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    match tx.get("profile", actor_id(tx)).await {
        Ok(profile) => Ok(json!({
            "profile": effective_profile(tx,&profile.data),
            "version": profile.version
        })),
        Err(AppError::NotFound) => {
            Ok(json!({ "profile": effective_profile(tx,&json!({})), "version": null }))
        }
        Err(error) => Err(error),
    }
}

fn effective_profile(tx: &AppTx, data: &Value) -> Value {
    let mut result = serde_json::Map::new();
    for key in ["display_name", "locale", "time_zone", "theme"] {
        if let Some(value) = data.get(key).or_else(|| tx.preference(key)) {
            result.insert(key.into(), value.clone());
        }
    }
    Value::Object(result)
}

async fn profile_update(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: ProfileUpdateInput,
) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    if input.display_name.is_none()
        && input.locale.is_none()
        && input.time_zone.is_none()
        && input.theme.is_none()
    {
        return Err(AppError::invalid("empty_profile_update"));
    }
    let id = actor_id(tx);
    let current = match tx.get_for_update("profile", id).await {
        Ok(profile) => Some(profile),
        Err(AppError::NotFound) => None,
        Err(error) => return Err(error),
    };
    let (data, version) = match current {
        Some(profile) => {
            if request
                .expected_version
                .is_some_and(|expected| expected != profile.version)
            {
                return Err(AppError::conflict("stale_record_version"));
            }
            (profile.data, Some(profile.version))
        }
        None => {
            if request.expected_version.is_some() {
                return Err(AppError::conflict("stale_record_version"));
            }
            (json!({}), None)
        }
    };
    let mut data = data;
    let mut updated_fields = Vec::new();
    if let Some(name) = input.display_name {
        data["display_name"] = json!(validate_profile_name(name)?);
        updated_fields.push("display_name");
    }
    if let Some(locale) = input.locale {
        data["locale"] = json!(validate_locale(locale)?);
        updated_fields.push("locale");
    }
    if let Some(time_zone) = input.time_zone {
        if time_zone.len() > 64 || time_zone.parse::<chrono_tz::Tz>().is_err() {
            return Err(AppError::invalid("invalid_profile_time_zone"));
        }
        data["time_zone"] = json!(time_zone);
        updated_fields.push("time_zone");
    }
    if let Some(theme) = input.theme {
        data["theme"] = json!(theme);
        updated_fields.push("theme");
    }
    let profile = if let Some(version) = version {
        tx.update("profile", id, version, data).await?
    } else {
        tx.insert("profile", id, data).await?
    };
    audit(
        tx,
        "B016",
        "update",
        Some(id),
        json!({ "fields": updated_fields }),
    )
    .await?;
    Ok(json!({
        "profile": effective_profile(tx,&profile.data),
        "version": profile.version
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentCreateInput {
    resource_kind: String,
    resource_id: Uuid,
    text: String,
}

async fn comment_create(tx: &mut AppTx, input: CommentCreateInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let resource_input = ResourceInput {
        resource_kind: input.resource_kind,
        resource_id: input.resource_id,
    };
    let resource = load_business_resource(tx, &resource_input).await?;
    require_resource_role(tx, &resource, SpaceRole::Commenter).await?;
    let text = input.text.trim();
    if text.is_empty() || text.chars().count() > MAX_COMMENT_CHARS || text.contains('\0') {
        return Err(AppError::invalid("invalid_comment"));
    }
    let escaped = html_escape(text);
    let author = actor_id(tx);
    let comment = insert_record(
        tx,
        "comment",
        json!({
            "tenant_id": tenant_id(tx),
            "resource_kind": resource.kind,
            "resource_id": resource.id,
            "space_id": resource_space_id(&resource),
            "author_id": author,
            "text_html": escaped,
            "created_at": now()
        }),
    )
    .await?;
    audit(
        tx,
        "B017",
        "create",
        Some(comment.id),
        json!({ "resource_kind": resource.kind, "resource_id": resource.id }),
    )
    .await?;
    record_activity(tx, "B017", "comment.created", Some(&resource)).await?;
    Ok(
        json!({ "id": comment.id, "text_html": escaped, "created_at": comment.data.get("created_at") }),
    )
}

async fn comment_list(tx: &mut AppTx, input: ResourceInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let resource = load_business_resource(tx, &input).await?;
    require_resource_role(tx, &resource, SpaceRole::Viewer).await?;
    let mut comments = find_kind(tx, "comment", "resource_id", &json_uuid(resource.id))
        .await?
        .into_iter()
        .filter(|row| record_str(row, "resource_kind") == Some(resource.kind.as_str()))
        .collect::<Vec<_>>();
    comments.sort_by(|left, right| {
        record_str(left, "created_at")
            .cmp(&record_str(right, "created_at"))
            .then_with(|| left.id.cmp(&right.id))
    });
    comments.truncate(50);
    let comments = comments
        .into_iter()
        .map(|comment| {
            json!({
                "id": comment.id,
                "author_id": record_uuid(&comment, "author_id"),
                "text_html": record_str(&comment, "text_html"),
                "created_at": comment.data.get("created_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "comments": comments }))
}

async fn comment_delete(tx: &mut AppTx, input: IdInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let comment = get_record_for_update(tx, "comment", input.id).await?;
    let kind = record_str(&comment, "resource_kind")
        .ok_or(AppError::Internal)?
        .to_owned();
    let resource_id = record_uuid(&comment, "resource_id").ok_or(AppError::Internal)?;
    let resource = get_record(tx, &kind, resource_id).await?;
    let author = record_uuid(&comment, "author_id").ok_or(AppError::Internal)?;
    if author != actor_id(tx) {
        require_resource_role(tx, &resource, SpaceRole::Editor).await?;
    }
    tx.delete("comment", comment.id, comment.version).await?;
    audit(
        tx,
        "B017",
        "delete",
        Some(comment.id),
        json!({ "resource_kind": kind, "resource_id": resource_id }),
    )
    .await?;
    record_activity(tx, "B017", "comment.deleted", Some(&resource)).await?;
    Ok(json!({ "deleted": true }))
}

async fn record_activity(
    tx: &mut AppTx,
    component: &str,
    event: &str,
    resource: Option<&Record>,
) -> AppResult<()> {
    let actor = actor_id(tx);
    let tenant = tenant_id(tx);
    let data = if let Some(resource) = resource {
        json!({
            "tenant_id": tenant,
            "actor_id": actor,
            "event": event,
            "component": component,
            "resource_kind": resource.kind,
            "resource_id": resource.id,
            "space_id": resource_space_id(resource),
            "created_at": now()
        })
    } else {
        json!({
            "tenant_id": tenant,
            "actor_id": actor,
            "event": event,
            "component": component,
            "created_at": now()
        })
    };
    insert_record(tx, "activity", data).await?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MentionInput {
    resource_kind: String,
    resource_id: Uuid,
    principal_ids: Vec<Uuid>,
}

fn stable_record_id(parts: &[&str]) -> Uuid {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

async fn mention_create(tx: &mut AppTx, input: MentionInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    if input.principal_ids.len() > 20 || input.principal_ids.iter().any(Uuid::is_nil) {
        return Err(AppError::invalid("invalid_mention_targets"));
    }
    let resource_input = ResourceInput {
        resource_kind: input.resource_kind,
        resource_id: input.resource_id,
    };
    let resource = load_business_resource(tx, &resource_input).await?;
    require_resource_role(tx, &resource, SpaceRole::Commenter).await?;
    let mut targets = input.principal_ids;
    targets.sort_unstable();
    targets.dedup();
    for target in targets {
        if target == actor_id(tx)
            || !active_tenant_member(tx, target).await?
            || !member_can_view_resource(tx, &resource, target).await?
        {
            // A fixed response prevents the caller from probing whether a
            // principal exists or can see the named object.
            continue;
        }
        let id = stable_record_id(&[
            "mention",
            &tenant_id(tx).to_string(),
            &resource.kind,
            &resource.id.to_string(),
            &target.to_string(),
            &actor_id(tx).to_string(),
        ]);
        tx.lock_record_key("mention", id).await?;
        match tx.get("mention", id).await {
            Ok(_) => continue,
            Err(AppError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let tenant = tenant_id(tx);
        let author = actor_id(tx);
        tx.insert(
            "mention",
            id,
            json!({
                "tenant_id": tenant,
                "resource_kind": resource.kind,
                "resource_id": resource.id,
                "space_id": resource_space_id(&resource),
                "principal_id": target,
                "created_by": author,
                "created_at": now(),
                "status": "active"
            }),
        )
        .await?;
    }
    audit(
        tx,
        "B018",
        "mention",
        Some(resource.id),
        json!({ "resource_kind": resource.kind }),
    )
    .await?;
    record_activity(tx, "B018", "collaborator.mentioned", Some(&resource)).await?;
    Ok(json!({ "status": "processed" }))
}

async fn member_can_view_resource(
    tx: &mut AppTx,
    resource: &Record,
    principal_id: Uuid,
) -> AppResult<bool> {
    let Some(space_id) = resource_space_id(resource) else {
        return Ok(false);
    };
    member_has_space_role(tx, space_id, principal_id, SpaceRole::Viewer).await
}

async fn mention_list(tx: &mut AppTx, _input: EmptyInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let rows = find_kind(tx, "mention", "principal_id", &json_uuid(actor_id(tx))).await?;
    let mut mentions = Vec::new();
    for row in rows {
        if record_str(&row, "status") != Some("active") {
            continue;
        }
        let Some(kind) = record_str(&row, "resource_kind") else {
            continue;
        };
        let Some(resource_id) = record_uuid(&row, "resource_id") else {
            continue;
        };
        let Ok(resource) = get_record(tx, kind, resource_id).await else {
            continue;
        };
        if require_resource_role(tx, &resource, SpaceRole::Viewer)
            .await
            .is_ok()
        {
            mentions.push(json!({
                "id": row.id,
                "resource_kind": kind,
                "resource_id": resource_id,
                "created_by": record_uuid(&row, "created_by"),
                "created_at": row.data.get("created_at"),
            }));
        }
    }
    Ok(json!({ "mentions": mentions }))
}

async fn subscription_create(tx: &mut AppTx, input: ResourceInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let resource = load_business_resource(tx, &input).await?;
    require_resource_role(tx, &resource, SpaceRole::Viewer).await?;
    let tenant = tenant_id(tx);
    let principal = actor_id(tx);
    let id = stable_record_id(&[
        "subscription",
        &tenant.to_string(),
        &resource.kind,
        &resource.id.to_string(),
        &principal.to_string(),
    ]);
    tx.lock_record_key("subscription", id).await?;
    let created = match tx.get("subscription", id).await {
        Ok(_) => false,
        Err(AppError::NotFound) => {
            tx.insert(
                "subscription",
                id,
                json!({
                    "tenant_id": tenant,
                    "resource_kind": resource.kind,
                    "resource_id": resource.id,
                    "space_id": resource_space_id(&resource),
                    "principal_id": principal,
                    "status": "active",
                    "created_at": now(),
                    "revoked_at": null
                }),
            )
            .await?;
            true
        }
        Err(error) => return Err(error),
    };
    audit(
        tx,
        "B018",
        "subscribe",
        Some(resource.id),
        json!({ "resource_kind": resource.kind }),
    )
    .await?;
    record_activity(tx, "B018", "object.subscribed", Some(&resource)).await?;
    Ok(json!({ "subscribed": true, "created": created }))
}

async fn subscription_remove(tx: &mut AppTx, input: ResourceInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    check_resource_identity(&input.resource_kind, input.resource_id)?;
    let id = stable_record_id(&[
        "subscription",
        &tenant_id(tx).to_string(),
        &input.resource_kind,
        &input.resource_id.to_string(),
        &actor_id(tx).to_string(),
    ]);
    tx.lock_record_key("subscription", id).await?;
    let row = match tx.get_for_update("subscription", id).await {
        Ok(row) => row,
        Err(AppError::NotFound) => return Ok(json!({ "ok": true, "changed": false })),
        Err(error) => return Err(error),
    };
    if record_str(&row, "status") != Some("active") {
        return Ok(json!({ "ok": true, "changed": false }));
    }
    let mut data = row.data.clone();
    data["status"] = json!("revoked");
    data["revoked_at"] = json!(now());
    tx.update("subscription", row.id, row.version, data).await?;
    Ok(json!({ "ok": true, "changed": true }))
}

async fn subscription_list(tx: &mut AppTx, input: ResourceInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let resource = load_business_resource(tx, &input).await?;
    require_resource_role(tx, &resource, SpaceRole::Viewer).await?;
    let mut principal_ids = Vec::new();
    for row in find_kind(tx, "subscription", "resource_id", &json_uuid(resource.id)).await? {
        let Some(principal_id) = record_uuid(&row, "principal_id") else {
            continue;
        };
        if record_str(&row, "resource_kind") == Some(resource.kind.as_str())
            && record_str(&row, "status") == Some("active")
            && active_tenant_member(tx, principal_id).await?
        {
            principal_ids.push(principal_id);
        }
    }
    principal_ids.sort_unstable();
    principal_ids.dedup();
    Ok(json!({ "principal_ids": principal_ids }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityListInput {
    #[serde(default)]
    after: Option<Uuid>,
    #[serde(default)]
    limit: Option<u32>,
}

async fn activity_list(tx: &mut AppTx, input: ActivityListInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    let limit = input
        .limit
        .unwrap_or(MAX_ACTIVITY as u32)
        .clamp(1, MAX_ACTIVITY as u32);
    let scanned = tx.list("activity", MAX_LIST, input.after).await?;
    let next_after = if scanned.len() == MAX_LIST as usize {
        scanned.last().map(|row| row.id)
    } else {
        None
    };
    let mut visible = Vec::new();
    for row in scanned {
        let resource_kind = record_str(&row, "resource_kind");
        let resource_id = record_uuid(&row, "resource_id");
        match (resource_kind, resource_id) {
            (Some(kind), Some(id)) => {
                let resource = match tx.get(kind, id).await {
                    Ok(resource) => resource,
                    Err(AppError::NotFound) => continue,
                    Err(error) => return Err(error),
                };
                if let Err(error) = require_resource_role(tx, &resource, SpaceRole::Viewer).await {
                    if matches!(error, AppError::NotFound) {
                        continue;
                    }
                    return Err(error);
                }
                visible.push(json!({
                    "id": row.id,
                    "event": record_str(&row, "event"),
                    "actor_id": record_uuid(&row, "actor_id"),
                    "resource_kind": kind,
                    "resource_id": id,
                    "created_at": row.data.get("created_at"),
                }));
            }
            (None, None) => visible.push(json!({
                "id": row.id,
                "event": record_str(&row, "event"),
                "actor_id": record_uuid(&row, "actor_id"),
                "created_at": row.data.get("created_at"),
            })),
            _ => continue,
        }
        if visible.len() >= limit as usize {
            break;
        }
    }
    Ok(json!({ "activity": visible, "next_after": next_after }))
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ShareScope {
    Preview,
}

impl ShareScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareCreateInput {
    resource_kind: String,
    resource_id: Uuid,
    target_principal_id: Uuid,
    scope: ShareScope,
    expires_in_hours: i64,
}

async fn share_create(tx: &mut AppTx, input: ShareCreateInput) -> AppResult<Value> {
    require_active_tenant_member(tx).await?;
    if input.target_principal_id == actor_id(tx) || input.target_principal_id.is_nil() {
        return Err(AppError::invalid("invalid_share_target"));
    }
    if !active_tenant_member(tx, input.target_principal_id).await? {
        return Err(AppError::NotFound);
    }
    let resource_input = ResourceInput {
        resource_kind: input.resource_kind,
        resource_id: input.resource_id,
    };
    let resource = load_business_resource(tx, &resource_input).await?;
    let space_id = resource_space_id(&resource).ok_or(AppError::NotFound)?;
    require_space_role(tx, space_id, SpaceRole::Editor).await?;
    let expires_at = ensure_deadline(tx, input.expires_in_hours, MAX_SHARE_HOURS).await?;
    let token = opaque_token()?;
    let tenant = tenant_id(tx);
    let creator = actor_id(tx);
    let share = insert_record(
        tx,
        "temporary_share",
        json!({
            "tenant_id": tenant,
            "resource_kind": resource.kind,
            "resource_id": resource.id,
            "space_id": space_id,
            "target_principal_id": input.target_principal_id,
            "scope": input.scope.as_str(),
            "token_hash": token_hash(&token),
            "created_by": creator,
            "created_at": now(),
            "expires_at": expires_at,
            "status": "active",
            "revoked_at": null
        }),
    )
    .await?;
    audit(
        tx,
        "B020",
        "create",
        Some(share.id),
        json!({ "resource_kind": resource.kind, "resource_id": resource.id, "target_principal_id": input.target_principal_id, "scope": input.scope.as_str(), "expires_at": expires_at }),
    )
    .await?;
    record_activity(tx, "B020", "share.created", Some(&resource)).await?;
    Ok(
        json!({ "share_id": share.id, "token": token, "scope": input.scope.as_str(), "expires_at": expires_at }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareRedeemInput {
    token: String,
}

async fn share_redeem(tx: &mut AppTx, input: ShareRedeemInput) -> AppResult<Value> {
    if input.token.len() != 43
        || !input
            .token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(AppError::NotFound);
    }
    let hash = token_hash(&input.token);
    let rows = find_kind(tx, "temporary_share", "token_hash", &json!(hash)).await?;
    let Some(found) = rows
        .into_iter()
        .find(|row| record_str(row, "token_hash") == Some(hash.as_str()))
    else {
        return Err(AppError::NotFound);
    };
    let share = get_record(tx, "temporary_share", found.id).await?;
    let db_now = database_now(tx).await?;
    if record_str(&share, "status") != Some("active")
        || record_time(&share, "revoked_at").is_some()
        || record_time(&share, "expires_at").is_none_or(|expiry| expiry <= db_now)
        || record_uuid(&share, "target_principal_id") != Some(actor_id(tx))
    {
        return Err(AppError::NotFound);
    }
    if !active_tenant_member(tx, actor_id(tx)).await? {
        return Err(AppError::NotFound);
    }
    let resource_kind = record_str(&share, "resource_kind")
        .ok_or(AppError::Internal)?
        .to_owned();
    let resource_id = record_uuid(&share, "resource_id").ok_or(AppError::Internal)?;
    let creator = record_uuid(&share, "created_by").ok_or(AppError::Internal)?;
    let resource = get_record(tx, &resource_kind, resource_id).await?;
    let Some(space_id) = resource_space_id(&resource) else {
        return Err(AppError::NotFound);
    };
    if !member_has_space_role(tx, space_id, creator, SpaceRole::Editor).await? {
        return Err(AppError::NotFound);
    }
    let preview = resource
        .data
        .get("share_preview")
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 10_000)
        .unwrap_or("");
    Ok(json!({
        "resource_kind": resource_kind,
        "resource_id": resource_id,
        "scope": record_str(&share, "scope"),
        "preview": preview
    }))
}

async fn share_revoke(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: IdInput,
) -> AppResult<Value> {
    let share = get_record_for_update(tx, "temporary_share", input.id).await?;
    let kind = record_str(&share, "resource_kind")
        .ok_or(AppError::Internal)?
        .to_owned();
    let resource_id = record_uuid(&share, "resource_id").ok_or(AppError::Internal)?;
    let resource = get_record(tx, &kind, resource_id).await?;
    let creator = record_uuid(&share, "created_by").ok_or(AppError::Internal)?;
    let owner = resource_owner_id(tx, &resource).await?;
    if creator != actor_id(tx) && owner != Some(actor_id(tx)) && !has_tenant_admin_role(tx) {
        return Err(AppError::Forbidden);
    }
    if record_str(&share, "status") != Some("active") {
        return Ok(json!({ "revoked": false }));
    }
    let mut data = share.data.clone();
    data["status"] = json!("revoked");
    data["revoked_at"] = json!(now());
    update_record(tx, request, share, data).await?;
    audit(
        tx,
        "B020",
        "revoke",
        Some(input.id),
        json!({ "resource_kind": kind, "resource_id": resource_id }),
    )
    .await?;
    record_activity(tx, "B020", "share.revoked", Some(&resource)).await?;
    Ok(json!({ "revoked": true }))
}
