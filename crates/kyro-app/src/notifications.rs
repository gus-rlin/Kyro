//! Recipient-bound templates, private feeds, channels and expiring presence.
use crate::{AppError, AppResult, AppTx, OperationRequest, Record};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, postgres::PgRow};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const ACTIONS: &[&str] = &[
    "notification.send",
    "notification.feed",
    "notification.read",
    "notification.prune",
    "message_template.define",
    "message_template.approve",
    "message_template.revoke",
    "message_template.get",
    "preferences.set",
    "preferences.get",
    "stream.read",
    "socket.open",
    "presence.touch",
    "presence.list",
    "presence.leave",
    "channel.create",
    "channel.member",
    "channel.get",
    "message.send",
    "message.list",
];
pub fn supports(id: &str, a: &str) -> bool {
    match id {
        "B101" => matches!(
            a,
            "notification.send" | "notification.feed" | "notification.read" | "notification.prune"
        ),
        "B102" => matches!(
            a,
            "message_template.define"
                | "message_template.approve"
                | "message_template.revoke"
                | "message_template.get"
        ),
        "B103" => matches!(a, "preferences.set" | "preferences.get"),
        "B107" => a == "stream.read",
        "B108" => a == "socket.open",
        "B109" => matches!(a, "presence.touch" | "presence.list" | "presence.leave"),
        "B110" => matches!(
            a,
            "channel.create" | "channel.member" | "channel.get" | "message.send" | "message.list"
        ),
        _ => false,
    }
}
pub fn is_read(_id: &str, a: &str) -> bool {
    matches!(
        a,
        "notification.feed"
            | "message_template.get"
            | "preferences.get"
            | "stream.read"
            | "socket.open"
            | "presence.list"
            | "channel.get"
            | "message.list"
    )
}
fn decode<T: DeserializeOwned>(r: &OperationRequest) -> AppResult<T> {
    serde_json::from_value(r.payload.clone())
        .map_err(|_| AppError::invalid("invalid_notification_input"))
}
fn expected(r: &OperationRequest) -> AppResult<i64> {
    r.expected_version
        .filter(|v| *v > 0)
        .ok_or(AppError::invalid("expected_version_required"))
}
fn manager(tx: &AppTx) -> AppResult<()> {
    crate::governance::admin(tx)?;
    tx.require_elevated()
}
fn text(s: &str, max: usize) -> AppResult<()> {
    if s.trim().is_empty() || s.len() > max || s.contains('\0') {
        Err(AppError::invalid("invalid_message_text"))
    } else {
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum VariableType {
    String,
    Integer,
    Boolean,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Template {
    subject: String,
    body: String,
    variables: BTreeMap<String, VariableType>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Define {
    id: Uuid,
    definition: Template,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateId {
    id: Uuid,
    version: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    pub kind: String,
    pub id: Uuid,
    pub version: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Send {
    pub recipient_id: Uuid,
    pub source: Source,
    pub template_id: Uuid,
    pub template_version: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    pub internal: bool,
    pub email: bool,
    pub mobile: bool,
    pub push: bool,
    pub frequency: Frequency,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Frequency {
    Immediate,
    Daily,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            internal: true,
            email: false,
            mobile: false,
            push: false,
            frequency: Frequency::Immediate,
        }
    }
}
pub(crate) async fn preferences(tx: &mut AppTx, recipient: Uuid) -> AppResult<Preferences> {
    let id = crate::governance::stable_id("notification.preferences", &recipient.to_string());
    match tx.get("notification.preferences", id).await {
        Ok(r) => serde_json::from_value(r.data).map_err(|_| AppError::Internal),
        Err(AppError::NotFound) => Ok(Preferences::default()),
        Err(e) => Err(e),
    }
}
fn placeholders(s: &str) -> AppResult<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    let mut rest = s;
    while let Some(at) = rest.find("{{") {
        if rest[..at].contains("}}") {
            return Err(AppError::invalid("invalid_message_template"));
        }
        let tail = &rest[at + 2..];
        let end = tail
            .find("}}")
            .ok_or(AppError::invalid("invalid_message_template"))?;
        let key = &tail[..end];
        if key.is_empty()
            || key.len() > 64
            || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            return Err(AppError::invalid("invalid_message_variable"));
        }
        result.insert(key.into());
        rest = &tail[end + 2..];
    }
    if rest.contains("}}") {
        return Err(AppError::invalid("invalid_message_template"));
    }
    Ok(result)
}
fn validate_template(t: &Template) -> AppResult<()> {
    text(&t.subject, 200)?;
    text(&t.body, 16384)?;
    if t.subject.contains(['\r', '\n']) || t.variables.is_empty() || t.variables.len() > 32 {
        return Err(AppError::invalid("invalid_message_template"));
    }
    let keys = placeholders(&t.subject)?
        .union(&placeholders(&t.body)?)
        .cloned()
        .collect::<BTreeSet<_>>();
    if keys != t.variables.keys().cloned().collect() {
        return Err(AppError::invalid("message_variables_mismatch"));
    }
    Ok(())
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn render(t: &Template, data: &Value) -> AppResult<Value> {
    validate_template(t)?;
    let mut values = BTreeMap::new();
    for (key, ty) in &t.variables {
        let v = data.get(key).ok_or(AppError::NotFound)?;
        let value = match ty {
            VariableType::String => v.as_str().map(str::to_owned),
            VariableType::Integer => v.as_i64().map(|v| v.to_string()),
            VariableType::Boolean => v.as_bool().map(|v| v.to_string()),
        }
        .ok_or(AppError::invalid("message_variable_type"))?;
        if value.len() > 4096 || value.contains('\0') {
            return Err(AppError::invalid("message_variable_limit"));
        }
        values.insert(key.clone(), value);
    }
    let interpolate = |source: &str| -> AppResult<String> {
        let mut result = String::new();
        let mut rest = source;
        while let Some(at) = rest.find("{{") {
            result.push_str(&rest[..at]);
            let tail = &rest[at + 2..];
            let end = tail.find("}}").ok_or(AppError::Internal)?;
            result.push_str(values.get(&tail[..end]).ok_or(AppError::Internal)?);
            rest = &tail[end + 2..];
        }
        result.push_str(rest);
        Ok(result)
    };
    let subject = interpolate(&t.subject)?;
    let body = interpolate(&t.body)?;
    if subject.len() > 300 || subject.contains(['\r', '\n']) || body.len() > 16384 {
        return Err(AppError::invalid("rendered_message_limit"));
    }
    Ok(
        json!({"subject":subject,"text":body,"html":format!("<p>{}</p>",escape(&body).replace('\n',"<br>"))}),
    )
}
pub(crate) fn projection_hash(r: &Record) -> AppResult<Vec<u8>> {
    Ok(Sha256::digest(serde_json::to_vec(r).map_err(|_| AppError::Internal)?).to_vec())
}
pub(crate) async fn prepare_message(tx: &mut AppTx, i: &Send) -> AppResult<(Record, Value)> {
    if i.source.id.is_nil()
        || i.recipient_id.is_nil()
        || i.source.version < 1
        || i.template_version < 1
    {
        return Err(AppError::invalid("message_source_invalid"));
    }
    if i.source.kind.starts_with("data.") {
        tx.require_operation("B031", "get")?;
    }
    crate::governance::projected_resource(tx, &i.source.kind, i.source.id).await?;
    let projected =
        crate::governance::recipient_projection(tx, i.recipient_id, &i.source.kind, i.source.id)
            .await?;
    if projected.version != i.source.version {
        return Err(AppError::conflict("notification_source_changed"));
    }
    let row=sqlx::query("SELECT definition,definition_hash FROM app_notification_templates WHERE id=$1 AND version=$2 AND approved AND NOT revoked").bind(i.template_id).bind(i.template_version).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let value: Value = row.try_get("definition")?;
    let hash: Vec<u8> = row.try_get("definition_hash")?;
    if Sha256::digest(serde_json::to_vec(&value).map_err(|_| AppError::Internal)?)[..] != hash[..] {
        return Err(AppError::conflict("template_hash_changed"));
    }
    let template: Template = serde_json::from_value(value).map_err(|_| AppError::Internal)?;
    let rendered = render(&template, &projected.data)?;
    Ok((projected, rendered))
}
async fn visible(tx: &mut AppTx, row: &PgRow) -> AppResult<bool> {
    let recipient: Uuid = row.try_get("recipient_id")?;
    if recipient != tx.actor().principal_id() {
        return Ok(false);
    }
    let kind: String = row.try_get("source_kind")?;
    let id: Uuid = row.try_get("source_id")?;
    let current = match crate::governance::recipient_projection(tx, recipient, &kind, id).await {
        Ok(r) => r,
        Err(AppError::NotFound | AppError::Forbidden) => return Ok(false),
        Err(e) => return Err(e),
    };
    let hash: Vec<u8> = row.try_get("projection_hash")?;
    let approved:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_notification_templates WHERE id=$1 AND version=$2 AND approved AND NOT revoked)").bind(row.try_get::<Uuid,_>("template_id")?).bind(row.try_get::<i64,_>("template_version")?).fetch_one(tx.conn()).await?;
    Ok(approved
        && current.version == row.try_get::<i64, _>("source_version")?
        && projection_hash(&current)? == hash)
}
fn public_message(row: &PgRow) -> AppResult<Value> {
    Ok(
        json!({"id":row.try_get::<Uuid,_>("id")?,"sequence":row.try_get::<i64,_>("sequence")?,"message":row.try_get::<Value,_>("rendered")?,"read_at":row.try_get::<Option<DateTime<Utc>>,_>("read_at")?,"created_at":row.try_get::<DateTime<Utc>,_>("created_at")?}),
    )
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default)]
    after: Option<String>,
    #[serde(default = "page_size")]
    limit: i64,
}
fn page_size() -> i64 {
    50
}
pub fn cursor(actor: &crate::Actor, seq: i64) -> String {
    format!("{}:{}:{seq}", actor.application_id(), actor.principal_id())
}
pub fn parse_cursor(actor: &crate::Actor, value: Option<&str>) -> AppResult<i64> {
    match value {
        None => Ok(0),
        Some(s) => {
            if s.len() > 120 {
                return Err(AppError::invalid("notification_cursor_invalid"));
            }
            let (scope, n) = s
                .rsplit_once(':')
                .ok_or(AppError::invalid("notification_cursor_invalid"))?;
            if scope != format!("{}:{}", actor.application_id(), actor.principal_id()) {
                return Err(AppError::NotFound);
            }
            n.parse::<i64>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or(AppError::invalid("notification_cursor_invalid"))
        }
    }
}
pub(crate) async fn feed(tx: &mut AppTx, after: Option<&str>, limit: i64) -> AppResult<Value> {
    tx.require_operation("B101", "notification.feed")?;
    if !(1..=50).contains(&limit) {
        return Err(AppError::invalid("notification_page_limit"));
    }
    let after = parse_cursor(tx.actor(), after)?;
    // Locking is unnecessary for readers: bounds and candidate rows are one SQL snapshot.
    let rows=sqlx::query("SELECT h.sequence AS latest,h.purged_through,n.* FROM app_notification_heads h LEFT JOIN LATERAL(SELECT * FROM app_notifications n WHERE n.recipient_id=h.recipient_id AND n.sequence>$1 ORDER BY n.sequence LIMIT $2) n ON true WHERE h.recipient_id=$3").bind(after).bind(limit).bind(tx.actor().principal_id()).fetch_all(tx.conn()).await?;
    let mut latest = 0;
    let mut advanced = after;
    let mut items = vec![];
    for row in rows {
        latest = row.try_get("latest")?;
        if after < row.try_get::<i64, _>("purged_through")? && after != 0 {
            return Err(AppError::conflict("notification_history_expired"));
        }
        if row.try_get::<Option<Uuid>, _>("id")?.is_none() {
            continue;
        }
        let seq: i64 = row.try_get("sequence")?;
        advanced = advanced.max(seq);
        if row.try_get::<DateTime<Utc>, _>("expires_at")? > Utc::now() && visible(tx, &row).await? {
            items.push(public_message(&row)?);
        }
    }
    if after > latest {
        return Err(AppError::invalid("notification_cursor_future"));
    }
    Ok(json!({"items":items,"next_cursor":cursor(tx.actor(),advanced),"has_more":advanced<latest}))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelCreate {
    name: String,
    members: BTreeSet<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    channel_id: Uuid,
    principal_id: Uuid,
    active: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelId {
    channel_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatMessage {
    channel_id: Uuid,
    body: String,
    #[serde(default)]
    attachments: Vec<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelPage {
    channel_id: Uuid,
    #[serde(default)]
    after: i64,
    #[serde(default = "page_size")]
    limit: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Presence {
    channel_id: Uuid,
    status: String,
    #[serde(default = "presence_ttl")]
    ttl_seconds: i64,
}
fn presence_ttl() -> i64 {
    30
}
pub(crate) async fn channel_member(tx: &mut AppTx, id: Uuid) -> AppResult<Record> {
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_channel_members WHERE channel_id=$1 AND principal_id=$2 AND active)").bind(id).bind(tx.actor().principal_id()).fetch_one(tx.conn()).await?;
    if !exists {
        return Err(AppError::NotFound);
    }
    tx.get("notification.channel", id).await
}
async fn active_member(tx: &mut AppTx, principal: Uuid) -> AppResult<()> {
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_memberships m JOIN app_principals p ON p.tenant_id=m.tenant_id AND p.id=m.principal_id WHERE m.principal_id=$1 AND m.status='active' AND p.status='active' AND p.account_type='human')").bind(principal).fetch_one(tx.conn()).await?;
    if exists {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}
pub async fn execute(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    match r.action.as_str() {
        "message_template.define" => {
            manager(tx)?;
            let i: Define = decode(r)?;
            validate_template(&i.definition)?;
            if i.id.is_nil() {
                return Err(AppError::invalid("template_id_invalid"));
            }
            tx.lock_record_key("notification.template", i.id).await?;
            let current: i64 = sqlx::query_scalar(
                "SELECT COALESCE(max(version),0) FROM app_notification_templates WHERE id=$1",
            )
            .bind(i.id)
            .fetch_one(tx.conn())
            .await?;
            if r.expected_version.unwrap_or(0) != current {
                return Err(AppError::conflict("message_template_version_conflict"));
            }
            let version = current.checked_add(1).ok_or(AppError::Quota)?;
            let value = serde_json::to_value(i.definition).map_err(|_| AppError::Internal)?;
            let hash = Sha256::digest(serde_json::to_vec(&value).map_err(|_| AppError::Internal)?)
                .to_vec();
            sqlx::query("INSERT INTO app_notification_templates(tenant_id,id,version,definition,definition_hash,created_by) VALUES($1,$2,$3,$4,$5,$6)").bind(tx.actor().tenant_id()).bind(i.id).bind(version).bind(value).bind(hash).bind(tx.actor().principal_id()).execute(tx.conn()).await?;
            tx.audit(
                &r.component_id,
                "message_template.define",
                Some(i.id),
                json!({"version":version}),
            )
            .await?;
            Ok(json!({"id":i.id,"version":version,"approved":false}))
        }
        "message_template.approve" | "message_template.revoke" => {
            manager(tx)?;
            let i: TemplateId = decode(r)?;
            let approved = r.action == "message_template.approve";
            let changed=sqlx::query("UPDATE app_notification_templates SET approved=$3,revoked=NOT $3,approved_by=$4 WHERE id=$1 AND version=$2 AND (NOT $3 OR created_by<>$4) AND (approved<>$3 OR revoked=$3)").bind(i.id).bind(i.version).bind(approved).bind(tx.actor().principal_id()).execute(tx.conn()).await?;
            if changed.rows_affected() != 1 {
                return Err(AppError::conflict("template_approval_conflict"));
            }
            tx.audit(
                &r.component_id,
                &r.action,
                Some(i.id),
                json!({"version":i.version}),
            )
            .await?;
            Ok(json!({"id":i.id,"version":i.version,"approved":approved}))
        }
        "message_template.get" => {
            manager(tx)?;
            let i: TemplateId = decode(r)?;
            let row=sqlx::query("SELECT definition,approved,revoked FROM app_notification_templates WHERE id=$1 AND version=$2").bind(i.id).bind(i.version).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            Ok(
                json!({"id":i.id,"version":i.version,"definition":row.try_get::<Value,_>("definition")?,"approved":row.try_get::<bool,_>("approved")?,"revoked":row.try_get::<bool,_>("revoked")?}),
            )
        }
        "preferences.get" => {
            let _: crate::notifications::Empty = decode(r)?;
            Ok(
                serde_json::to_value(preferences(tx, tx.actor().principal_id()).await?)
                    .map_err(|_| AppError::Internal)?,
            )
        }
        "preferences.set" => {
            let i: Preferences = decode(r)?;
            let id = crate::governance::stable_id(
                "notification.preferences",
                &tx.actor().principal_id().to_string(),
            );
            tx.lock_record_key("notification.preferences", id).await?;
            let value = serde_json::to_value(i).map_err(|_| AppError::Internal)?;
            let record = match tx.get("notification.preferences", id).await {
                Ok(_old) => {
                    tx.update("notification.preferences", id, expected(r)?, value)
                        .await?
                }
                Err(AppError::NotFound) => {
                    if r.expected_version.is_some() {
                        return Err(AppError::Conflict("preferences_version_conflict"));
                    }
                    tx.insert("notification.preferences", id, value).await?
                }
                Err(e) => return Err(e),
            };
            Ok(json!({"version":record.version,"preferences":record.data}))
        }
        "notification.send" => {
            let i: Send = decode(r)?;
            let (projection, rendered) = prepare_message(tx, &i).await?;
            if !preferences(tx, i.recipient_id).await?.internal {
                return Ok(json!({"state":"suppressed","reason":"recipient_preferences"}));
            }
            tx.reserve_quota("notifications", 1).await?;
            let sequence:i64=sqlx::query_scalar("INSERT INTO app_notification_heads(tenant_id,recipient_id,sequence) VALUES($1,$2,1) ON CONFLICT(tenant_id,application_id,recipient_id) DO UPDATE SET sequence=app_notification_heads.sequence+1 RETURNING sequence").bind(tx.actor().tenant_id()).bind(i.recipient_id).fetch_one(tx.conn()).await?;
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO app_notifications(tenant_id,id,recipient_id,sequence,source_kind,source_id,source_version,projection_hash,template_id,template_version,rendered,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,clock_timestamp()+interval '30 days')").bind(tx.actor().tenant_id()).bind(id).bind(i.recipient_id).bind(sequence).bind(i.source.kind).bind(i.source.id).bind(i.source.version).bind(projection_hash(&projection)?).bind(i.template_id).bind(i.template_version).bind(rendered).execute(tx.conn()).await?;
            tx.settle_quota("notifications", 1, 1).await?;
            tx.audit(
                &r.component_id,
                "notification.send",
                Some(id),
                json!({"recipient":i.recipient_id,"template":i.template_id}),
            )
            .await?;
            Ok(json!({"id":id,"state":"delivered_internal"}))
        }
        "notification.feed" | "stream.read" => {
            let i: Page = decode(r)?;
            feed(tx, i.after.as_deref(), i.limit).await
        }
        "socket.open" => {
            let _: Empty = decode(r)?;
            Ok(
                json!({"protocol":"kyro.operations.v1","max_message_bytes":16384,"max_connections_per_principal":2,"allowed_components":["B101","B109","B110"]}),
            )
        }
        "notification.read" => {
            let i: Id = decode(r)?;
            let row = sqlx::query(
                "SELECT * FROM app_notifications WHERE id=$1 AND expires_at>clock_timestamp()",
            )
            .bind(i.id)
            .fetch_optional(tx.conn())
            .await?
            .ok_or(AppError::NotFound)?;
            if !visible(tx, &row).await? {
                return Err(AppError::NotFound);
            }
            sqlx::query("UPDATE app_notifications SET read_at=COALESCE(read_at,clock_timestamp()) WHERE id=$1").bind(i.id).execute(tx.conn()).await?;
            Ok(json!({"id":i.id,"read":true}))
        }
        "notification.prune" => {
            let _: Empty = decode(r)?;
            let count:i64=sqlx::query_scalar("WITH d AS(DELETE FROM app_notifications WHERE id IN(SELECT id FROM app_notifications WHERE expires_at<=clock_timestamp() ORDER BY sequence LIMIT 100) RETURNING sequence), h AS(UPDATE app_notification_heads SET purged_through=GREATEST(purged_through,COALESCE((SELECT max(sequence) FROM d),0)) WHERE recipient_id=$1) SELECT count(*) FROM d").bind(tx.actor().principal_id()).fetch_one(tx.conn()).await?;
            if count > 0 {
                sqlx::query("UPDATE app_quotas SET used_value=used_value-$1 WHERE quota_key='notifications'").bind(count).execute(tx.conn()).await?;
            }
            Ok(json!({"removed":count}))
        }
        "channel.create" => {
            let mut i: ChannelCreate = decode(r)?;
            text(&i.name, 200)?;
            if i.members.len() > 63 {
                return Err(AppError::invalid("channel_member_limit"));
            }
            i.members.insert(tx.actor().principal_id());
            for p in &i.members {
                active_member(tx, *p).await?;
            }
            let id = Uuid::new_v4();
            let c = tx
                .insert(
                    "notification.channel",
                    id,
                    json!({"name":i.name,"owner_id":tx.actor().principal_id()}),
                )
                .await?;
            for p in i.members {
                sqlx::query("INSERT INTO app_channel_members(tenant_id,channel_id,principal_id) VALUES($1,$2,$3)").bind(tx.actor().tenant_id()).bind(id).bind(p).execute(tx.conn()).await?;
            }
            Ok(serde_json::to_value(c).map_err(|_| AppError::Internal)?)
        }
        "channel.member" => {
            let i: Member = decode(r)?;
            let c = channel_member(tx, i.channel_id).await?;
            if c.data["owner_id"] != tx.actor().principal_id().to_string() {
                return Err(AppError::Forbidden);
            }
            if i.principal_id == tx.actor().principal_id() {
                return Err(AppError::invalid("channel_owner_removal"));
            }
            if i.active {
                active_member(tx, i.principal_id).await?;
            }
            let version = expected(r)?;
            if c.version != version {
                return Err(AppError::conflict("channel_version_conflict"));
            }
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM app_channel_members WHERE channel_id=$1 AND active",
            )
            .bind(i.channel_id)
            .fetch_one(tx.conn())
            .await?;
            if i.active && count >= 64 {
                return Err(AppError::Quota);
            }
            sqlx::query("INSERT INTO app_channel_members(tenant_id,channel_id,principal_id,active) VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id,application_id,channel_id,principal_id) DO UPDATE SET active=$4").bind(tx.actor().tenant_id()).bind(i.channel_id).bind(i.principal_id).bind(i.active).execute(tx.conn()).await?;
            let updated = tx
                .update("notification.channel", i.channel_id, version, c.data)
                .await?;
            tx.audit(
                &r.component_id,
                "channel.member",
                Some(i.channel_id),
                json!({"principal":i.principal_id,"active":i.active}),
            )
            .await?;
            Ok(json!({"id":i.channel_id,"version":updated.version}))
        }
        "channel.get" => {
            let i: ChannelId = decode(r)?;
            let c = channel_member(tx, i.channel_id).await?;
            let members:Vec<Uuid>=sqlx::query_scalar("SELECT principal_id FROM app_channel_members m JOIN app_principals p ON p.tenant_id=m.tenant_id AND p.id=m.principal_id WHERE m.channel_id=$1 AND m.active AND p.status='active' AND EXISTS(SELECT 1 FROM app_memberships a WHERE a.application_id=m.application_id AND a.principal_id=m.principal_id AND a.status='active') ORDER BY principal_id").bind(i.channel_id).fetch_all(tx.conn()).await?;
            Ok(json!({"channel":c,"members":members}))
        }
        "message.send" => {
            let i: ChatMessage = decode(r)?;
            channel_member(tx, i.channel_id).await?;
            text(&i.body, 8192)?;
            if i.attachments.len() > 4
                || i.attachments.iter().collect::<BTreeSet<_>>().len() != i.attachments.len()
            {
                return Err(AppError::invalid("message_attachment_limit"));
            }
            for attachment in &i.attachments {
                document_available(tx, *attachment).await?;
            }
            tx.reserve_quota("notifications", 1).await?;
            let id = Uuid::new_v4();
            let sequence:i64=sqlx::query_scalar("INSERT INTO app_channel_messages(tenant_id,id,channel_id,author_id,body,attachments) VALUES($1,$2,$3,$4,$5,$6) RETURNING sequence").bind(tx.actor().tenant_id()).bind(id).bind(i.channel_id).bind(tx.actor().principal_id()).bind(i.body).bind(i.attachments).fetch_one(tx.conn()).await?;
            tx.settle_quota("notifications", 1, 1).await?;
            tx.audit(
                &r.component_id,
                "message.send",
                Some(id),
                json!({"channel":i.channel_id}),
            )
            .await?;
            Ok(json!({"id":id,"sequence":sequence}))
        }
        "message.list" => {
            let i: ChannelPage = decode(r)?;
            channel_member(tx, i.channel_id).await?;
            if i.after < 0 || !(1..=50).contains(&i.limit) {
                return Err(AppError::invalid("channel_page_invalid"));
            }
            let rows=sqlx::query("SELECT * FROM app_channel_messages WHERE channel_id=$1 AND sequence>$2 ORDER BY sequence LIMIT $3").bind(i.channel_id).bind(i.after).bind(i.limit).fetch_all(tx.conn()).await?;
            let mut items = vec![];
            let mut after = i.after;
            for row in rows {
                after = row.try_get("sequence")?;
                let attachments: Vec<Uuid> = row.try_get("attachments")?;
                let mut available = vec![];
                for id in attachments {
                    match document_available(tx, id).await {
                        Ok(()) => available.push(id),
                        Err(AppError::NotFound | AppError::Forbidden) => {}
                        Err(e) => return Err(e),
                    }
                }
                items.push(json!({"id":row.try_get::<Uuid,_>("id")?,"author_id":row.try_get::<Uuid,_>("author_id")?,"body":row.try_get::<String,_>("body")?,"attachments":available,"sequence":after,"created_at":row.try_get::<DateTime<Utc>,_>("created_at")?}));
            }
            Ok(json!({"items":items,"next_after":after}))
        }
        "presence.touch" => {
            let i: Presence = decode(r)?;
            channel_member(tx, i.channel_id).await?;
            if !(5..=60).contains(&i.ttl_seconds)
                || !matches!(i.status.as_str(), "available" | "busy" | "away")
            {
                return Err(AppError::invalid("presence_invalid"));
            }
            let expires = Utc::now() + Duration::seconds(i.ttl_seconds);
            sqlx::query("INSERT INTO app_presence(tenant_id,channel_id,principal_id,status,expires_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant_id,application_id,channel_id,principal_id) DO UPDATE SET status=$4,expires_at=$5").bind(tx.actor().tenant_id()).bind(i.channel_id).bind(tx.actor().principal_id()).bind(i.status).bind(expires).execute(tx.conn()).await?;
            Ok(json!({"expires_at":expires}))
        }
        "presence.list" => {
            let i: ChannelId = decode(r)?;
            channel_member(tx, i.channel_id).await?;
            let rows=sqlx::query("SELECT p.principal_id,p.status,p.expires_at FROM app_presence p JOIN app_channel_members m ON m.tenant_id=p.tenant_id AND m.application_id=p.application_id AND m.channel_id=p.channel_id AND m.principal_id=p.principal_id JOIN app_principals a ON a.tenant_id=p.tenant_id AND a.id=p.principal_id WHERE p.channel_id=$1 AND m.active AND a.status='active' AND p.expires_at>clock_timestamp() AND EXISTS(SELECT 1 FROM app_memberships am WHERE am.application_id=p.application_id AND am.principal_id=p.principal_id AND am.status='active') ORDER BY p.principal_id LIMIT 64").bind(i.channel_id).fetch_all(tx.conn()).await?;
            let items=rows.into_iter().map(|r|Ok(json!({"principal_id":r.try_get::<Uuid,_>("principal_id")?,"status":r.try_get::<String,_>("status")?,"expires_at":r.try_get::<DateTime<Utc>,_>("expires_at")?}))).collect::<AppResult<Vec<_>>>()?;
            Ok(json!({"items":items}))
        }
        "presence.leave" => {
            let i: ChannelId = decode(r)?;
            channel_member(tx, i.channel_id).await?;
            sqlx::query("DELETE FROM app_presence WHERE channel_id=$1 AND principal_id=$2")
                .bind(i.channel_id)
                .bind(tx.actor().principal_id())
                .execute(tx.conn())
                .await?;
            Ok(json!({"left":true}))
        }
        _ => Err(AppError::NotFound),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
async fn document_available(tx: &mut AppTx, id: Uuid) -> AppResult<()> {
    tx.require_operation("B083", "download")?;
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_documents d WHERE d.id=$1 AND d.kind='file' AND d.state='clean' AND (d.owner_id=$2 OR EXISTS(SELECT 1 FROM app_document_acl a WHERE a.document_id=d.id AND a.principal_id=$2 AND a.permission IN ('read','write','share') AND a.revoked_at IS NULL)))").bind(id).bind(tx.actor().principal_id()).fetch_one(tx.conn()).await?;
    if exists {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_template_escapes_values_and_rejects_header_injection() {
        let t = Template {
            subject: "Hello {{name}}".into(),
            body: "{{name}}\nCount {{n}}".into(),
            variables: BTreeMap::from([
                ("name".into(), VariableType::String),
                ("n".into(), VariableType::Integer),
            ]),
        };
        let r = render(&t, &json!({"name":"<script>x</script>","n":1})).unwrap();
        assert!(r["html"].as_str().unwrap().contains("&lt;script&gt;"));
        assert!(render(&t, &json!({"name":"x\r\nbcc:y","n":1})).is_err());
        assert!(render(&t, &json!({"name":"x","n":"1"})).is_err());
        assert!(placeholders("{{ name }}").is_err());
    }
}
