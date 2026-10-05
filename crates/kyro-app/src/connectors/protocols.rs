use super::*;
use crate::exchange::{ControlledResponse, RestrictedMethod, RestrictedRequest};
use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};

pub(super) fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn email_valid(s: &str) -> bool {
    let Some((local, host)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !host.is_empty()
        && s.len() <= 254
        && host.contains('.')
        && !s.chars().any(|c| {
            c.is_control() || c.is_whitespace() || matches!(c, '<' | '>' | ',' | ';' | '"')
        })
}
pub(super) fn phone_valid(s: &str) -> bool {
    s.starts_with('+') && (8..=16).contains(&s.len()) && s[1..].bytes().all(|c| c.is_ascii_digit())
}
pub(super) async fn file(tx: &mut AppTx, id: Uuid, version: i64) -> AppResult<(Vec<u8>, Vec<u8>)> {
    tx.require_operation("B083", "download")?;
    let row=sqlx::query("SELECT d.version,v.content,v.sha256 FROM app_documents d JOIN app_document_versions v ON v.tenant_id=d.tenant_id AND v.application_id=d.application_id AND v.document_id=d.id AND v.version=d.content_version WHERE d.id=$1 AND d.kind='file' AND d.state='clean' AND (d.owner_id=$2 OR EXISTS(SELECT 1 FROM app_document_acl a WHERE a.document_id=d.id AND a.principal_id=$2 AND a.permission IN ('read','write','share') AND a.revoked_at IS NULL))").bind(id).bind(tx.actor().principal_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if row.try_get::<i64, _>("version")? != version {
        return Err(AppError::conflict("connector_source_changed"));
    }
    let content: Vec<u8> = row.try_get("content")?;
    let hash: Vec<u8> = row.try_get("sha256")?;
    if Sha256::digest(&content).to_vec() != hash {
        return Err(AppError::conflict("document_hash_changed"));
    }
    Ok((content, hash))
}
pub(super) async fn payment(
    tx: &mut AppTx,
    kind: &str,
    id: Uuid,
    adapter: Uuid,
) -> AppResult<crate::Record> {
    let (component, action) = match kind {
        "commerce.payment" => ("B125", "get_payment"),
        "commerce.subscription" => ("B126", "get_subscription"),
        "commerce.refund" => ("B128", "get_refund"),
        _ => return Err(AppError::NotFound),
    };
    tx.require_operation(component, action)?;
    let req = OperationRequest {
        component_id: component.into(),
        action: action.into(),
        payload: json!({"id":id}),
        idempotency_key: String::new(),
        expected_version: None,
    };
    crate::commerce::execute(tx, &req).await?;
    let r = tx.get(kind, id).await?;
    if r.data["connector_id"] != json!(adapter) {
        return Err(AppError::Forbidden);
    }
    Ok(r)
}
pub(super) async fn endpoint(
    service: &ConnectorService,
    tx: &mut AppTx,
    recipient: Uuid,
    id: Uuid,
    channel: &str,
    challenge: bool,
) -> AppResult<(i64, Value)> {
    let row=if challenge{if recipient!=tx.actor().principal_id(){return Err(AppError::Forbidden);}sqlx::query("SELECT version,destination_cipher FROM app_delivery_endpoints WHERE id=$1 AND channel=$2 AND NOT revoked AND NOT verified AND challenge_expires>clock_timestamp() AND challenge_attempts<5").bind(id).bind(channel).fetch_optional(tx.conn()).await?}
 else{sqlx::query("SELECT version,destination_cipher FROM app_delivery_endpoint($1,$2) WHERE channel=$3").bind(recipient).bind(id).bind(channel).fetch_optional(tx.conn()).await?}.ok_or(AppError::NotFound)?;
    let context = format!(
        "{}/{}/{}/{id}/endpoint",
        tx.actor().tenant_id(),
        tx.actor().application_id(),
        recipient
    );
    let plain = service
        .cipher
        .open(&context, &row.try_get::<Vec<u8>, _>("destination_cipher")?)?;
    Ok((
        row.try_get("version")?,
        serde_json::from_slice(&plain).map_err(|_| AppError::Internal)?,
    ))
}
pub(super) async fn validate(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
) -> AppResult<(Option<Vec<u8>>, Option<i64>)> {
    let mut binding = validate_contract(service, tx, p, c).await?;
    if let Some(provider) = p.oauth_provider_id {
        let (_, hash) =
            super::oauth::access(service, tx, provider, &p.required_oauth_scopes).await?;
        binding.0 = Some(
            Sha256::digest(
                serde_json::to_vec(&json!({"source":binding.0,"oauth":hex(&hash)}))
                    .map_err(|_| AppError::Internal)?,
            )
            .to_vec(),
        );
    }
    Ok(binding)
}
async fn validate_contract(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
) -> AppResult<(Option<Vec<u8>>, Option<i64>)> {
    match (c, &p.configuration) {
        (Call::PostgresSnapshot {}, Provider::Postgres { settings }) => {
            super::postgres::validate_source(tx, settings)
                .await
                .map(|hash| (Some(hash), None))
        }
        (_, Provider::OAuth { .. }) => super::oauth::validate(service, tx, p, c).await,
        (
            Call::ObjectPut {
                object_id,
                document_id,
                version,
            },
            Provider::S3 { .. },
        ) => {
            if object_id.is_nil() {
                return Err(AppError::invalid("object_id_invalid"));
            }
            let (content, hash) = file(tx, *document_id, *version).await?;
            if content.len() > p.max_request_bytes {
                return Err(AppError::invalid("object_size_limit"));
            }
            Ok((Some(hash), None))
        }
        (Call::ObjectGet { object_id } | Call::ObjectDelete { object_id }, Provider::S3 { .. }) => {
            if object_id.is_nil() {
                return Err(AppError::invalid("object_id_invalid"));
            }
            Ok((None, None))
        }
        (Call::Email { message }, Provider::Resend { .. }) => {
            let (projection, _) = crate::notifications::prepare_message(tx, message).await?;
            let email: Option<String> = sqlx::query_scalar("SELECT app_verified_email($1)")
                .bind(message.recipient_id)
                .fetch_one(tx.conn())
                .await?;
            if !email.is_some_and(|e| email_valid(&e)) {
                return Err(AppError::NotFound);
            }
            if !crate::notifications::preferences(tx, message.recipient_id)
                .await?
                .email
            {
                return Err(AppError::Forbidden);
            }
            Ok((
                Some(crate::notifications::projection_hash(&projection)?),
                None,
            ))
        }
        (
            Call::Mobile {
                message,
                endpoint_id,
            },
            Provider::Twilio { .. },
        )
        | (
            Call::Push {
                message,
                endpoint_id,
            },
            Provider::Ntfy { .. },
        ) => {
            let (projection, rendered) = crate::notifications::prepare_message(tx, message).await?;
            let mobile = matches!(c, Call::Mobile { .. });
            let prefs = crate::notifications::preferences(tx, message.recipient_id).await?;
            if !(if mobile { prefs.mobile } else { prefs.push }) {
                return Err(AppError::Forbidden);
            }
            if rendered["text"]
                .as_str()
                .is_none_or(|s| s.len() > if mobile { 1600 } else { 4096 })
            {
                return Err(AppError::invalid("delivery_message_limit"));
            }
            let (v, _) = endpoint(
                service,
                tx,
                message.recipient_id,
                *endpoint_id,
                if mobile { "mobile" } else { "push" },
                false,
            )
            .await?;
            Ok((
                Some(crate::notifications::projection_hash(&projection)?),
                Some(v),
            ))
        }
        (
            Call::EndpointChallenge { endpoint_id },
            Provider::Twilio { .. } | Provider::Ntfy { .. },
        ) => {
            let channel = if matches!(p.configuration, Provider::Twilio { .. }) {
                "mobile"
            } else {
                "push"
            };
            let (v, _) = endpoint(
                service,
                tx,
                tx.actor().principal_id(),
                *endpoint_id,
                channel,
                true,
            )
            .await?;
            Ok((None, Some(v)))
        }
        (Call::Geocode { address }, Provider::Nominatim { .. }) => {
            if !bounded(address, 256) {
                return Err(AppError::invalid("geocode_address_invalid"));
            }
            Ok((None, None))
        }
        (Call::Rest { operation, input }, Provider::Rest { operations, .. }) => {
            let definition = operations.get(operation).ok_or(AppError::NotFound)?;
            definition.input.validate(input)?;
            if definition.effect {
                tx.require_elevated()?;
            }
            Ok((None, None))
        }
        (
            Call::ControlledHttp {
                operation,
                input,
                source_record,
                sign_webhook,
            },
            Provider::Rest { operations, .. },
        ) => {
            let definition = operations.get(operation).ok_or(AppError::NotFound)?;
            definition.input.validate(input)?;
            if definition.effect || definition.method != "GET" {
                tx.require_elevated()?;
            }
            if *sign_webhook {
                let reference = p.secret_ref.ok_or(AppError::Forbidden)?;
                if definition.method != "POST" || p.oauth_provider_id.is_some() {
                    return Err(AppError::Forbidden);
                }
                // The signing purpose is separate from bearer authentication.
                service
                    .vault
                    .resolve(tx, p.id, reference, "webhook.send")
                    .await?;
            }
            let hash = if let Some(source) = source_record {
                let projected =
                    crate::governance::projected_resource(tx, &source.kind, source.id).await?;
                if projected.version != source.version {
                    return Err(AppError::conflict("http_source_changed"));
                }
                Some(crate::notifications::projection_hash(&projected)?)
            } else {
                None
            };
            Ok((hash, None))
        }
        (Call::Mcp { tool, arguments }, Provider::Mcp { tools, .. }) => {
            let definition = tools.get(tool).ok_or(AppError::NotFound)?;
            definition.input.validate(arguments)?;
            if definition.effect {
                tx.require_elevated()?;
            }
            Ok((None, None))
        }
        (Call::CalendarCreate { source }, Provider::GoogleCalendar { .. }) => {
            let r = crate::governance::projected_resource(tx, &source.kind, source.id).await?;
            if source.version != r.version {
                return Err(AppError::conflict("connector_source_changed"));
            }
            calendar_data(&r.data)?;
            Ok((Some(crate::notifications::projection_hash(&r)?), None))
        }
        (Call::CalendarRead { from, until }, Provider::GoogleCalendar { .. }) => {
            if from >= until || *until - *from > chrono::Duration::days(31) {
                return Err(AppError::invalid("calendar_period_invalid"));
            }
            Ok((None, None))
        }
        (
            Call::CalendarBookingExport { .. } | Call::CalendarSyncRead { .. },
            Provider::GoogleCalendar { .. },
        ) => Ok((Some(super::calendar::validate(tx, p, c).await?), None)),
        (Call::PaymentIntent { payment_id }, Provider::Stripe { .. }) => {
            let r = payment(tx, "commerce.payment", *payment_id, p.id).await?;
            if r.data["status"] != "pending" {
                return Err(AppError::conflict("payment_not_pending"));
            }
            let order_id = r.data["order_id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or(AppError::Internal)?;
            let order = sqlx::query(
                "SELECT status,total_minor,currency FROM app_commerce_orders WHERE id=$1",
            )
            .bind(order_id)
            .fetch_optional(tx.conn())
            .await?
            .ok_or(AppError::NotFound)?;
            if order.try_get::<String, _>("status")? != "awaiting_payment"
                || r.data["amount_minor"] != json!(order.try_get::<i64, _>("total_minor")?)
                || r.data["currency"] != json!(order.try_get::<String, _>("currency")?)
            {
                return Err(AppError::conflict("order_not_payable"));
            }
            Ok((Some(crate::notifications::projection_hash(&r)?), None))
        }
        (
            Call::Subscription { subscription_id },
            Provider::Stripe {
                customers, prices, ..
            },
        ) => {
            let r = payment(tx, "commerce.subscription", *subscription_id, p.id).await?;
            let price = Uuid::parse_str(r.data["price_id"].as_str().ok_or(AppError::Internal)?)
                .map_err(|_| AppError::Internal)?;
            if r.data["status"] != "pending"
                || !customers.contains_key(&tx.actor().principal_id())
                || !prices.contains_key(&price)
            {
                return Err(AppError::invalid("stripe_subscription_mapping_missing"));
            }
            Ok((Some(crate::notifications::projection_hash(&r)?), None))
        }
        (Call::Refund { refund_id }, Provider::Stripe { .. }) => {
            let r = payment(tx, "commerce.refund", *refund_id, p.id).await?;
            if r.data["status"] != "pending" {
                return Err(AppError::conflict("refund_not_pending"));
            }
            Ok((Some(crate::notifications::projection_hash(&r)?), None))
        }
        _ => Err(AppError::invalid("connector_contract_mismatch")),
    }
}
fn calendar_data(v: &Value) -> AppResult<(String, DateTime<Utc>, DateTime<Utc>)> {
    let summary = v["summary"]
        .as_str()
        .filter(|s| bounded(s, 500))
        .ok_or(AppError::invalid("calendar_summary_invalid"))?;
    let parse = |k: &str| {
        DateTime::parse_from_rfc3339(
            v[k].as_str()
                .ok_or(AppError::invalid("calendar_time_invalid"))?,
        )
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| AppError::invalid("calendar_time_invalid"))
    };
    let start = parse("starts_at")?;
    let end = parse("ends_at")?;
    if start >= end || end - start > chrono::Duration::days(7) {
        return Err(AppError::invalid("calendar_interval_invalid"));
    }
    Ok((summary.into(), start, end))
}
fn method(s: &str) -> AppResult<RestrictedMethod> {
    match s {
        "GET" => Ok(RestrictedMethod::Get),
        "POST" => Ok(RestrictedMethod::Post),
        "PUT" => Ok(RestrictedMethod::Put),
        "PATCH" => Ok(RestrictedMethod::Patch),
        "DELETE" => Ok(RestrictedMethod::Delete),
        _ => Err(AppError::invalid("connector_method_invalid")),
    }
}
fn join(p: &Profile, path: &str) -> AppResult<url::Url> {
    let base = url::Url::parse(&p.endpoint).map_err(|_| AppError::Internal)?;
    let target = base.join(path).map_err(|_| AppError::Internal)?;
    if target.origin() != base.origin() {
        return Err(AppError::Forbidden);
    }
    Ok(target)
}
fn json_body(v: &Value) -> AppResult<Vec<u8>> {
    serde_json::to_vec(v).map_err(|_| AppError::Internal)
}
pub(super) fn mcp_request(
    p: &Profile,
    id: Uuid,
    method: &str,
    params: Value,
) -> AppResult<RestrictedRequest> {
    let mut params = params.as_object().cloned().ok_or(AppError::Internal)?;
    params.insert("_meta".into(),json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"kyro-rust","version":"0.1.0"},"io.modelcontextprotocol/clientCapabilities":{}}));
    let body = json_body(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
    let mut headers = vec![
        ("content-type".into(), "application/json".into()),
        (
            "accept".into(),
            "application/json, text/event-stream".into(),
        ),
        ("mcp-protocol-version".into(), "2026-07-28".into()),
        ("mcp-method".into(), method.into()),
    ];
    if let Some(name) = params.get("name").and_then(Value::as_str) {
        headers.push(("mcp-name".into(), name.into()));
    }
    Ok(RestrictedRequest {
        method: RestrictedMethod::Post,
        url: url::Url::parse(&p.endpoint).map_err(|_| AppError::Internal)?,
        headers,
        body,
    })
}
pub(super) async fn build(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
    id: Uuid,
) -> AppResult<RestrictedRequest> {
    let mut r = RestrictedRequest {
        method: RestrictedMethod::Post,
        url: url::Url::parse(&p.endpoint).map_err(|_| AppError::Internal)?,
        headers: vec![("content-type".into(), "application/json".into())],
        body: vec![],
    };
    match (c, &p.configuration) {
        (_, Provider::OAuth { .. }) => return super::oauth::build(service, tx, p, c).await,
        (
            Call::ObjectPut {
                object_id,
                document_id,
                version,
            },
            Provider::S3 { bucket, prefix, .. },
        ) => {
            let (bytes, _) = file(tx, *document_id, *version).await?;
            r.method = RestrictedMethod::Put;
            r.url = object_url(p, bucket, prefix, tx.actor(), *object_id)?;
            r.body = bytes;
            r.headers = vec![
                ("content-type".into(), "application/octet-stream".into()),
                ("if-none-match".into(), "*".into()),
            ];
        }
        (
            Call::ObjectGet { object_id } | Call::ObjectDelete { object_id },
            Provider::S3 { bucket, prefix, .. },
        ) => {
            r.method = if matches!(c, Call::ObjectGet { .. }) {
                RestrictedMethod::Get
            } else {
                RestrictedMethod::Delete
            };
            r.url = object_url(p, bucket, prefix, tx.actor(), *object_id)?;
            r.headers.clear();
        }
        (Call::Email { message }, Provider::Resend { from }) => {
            let (_, v) = crate::notifications::prepare_message(tx, message).await?;
            let email: Option<String> = sqlx::query_scalar("SELECT app_verified_email($1)")
                .bind(message.recipient_id)
                .fetch_one(tx.conn())
                .await?;
            let to = email.filter(|s| email_valid(s)).ok_or(AppError::NotFound)?;
            r.url = join(p, "/emails")?;
            r.body = json_body(
                &json!({"from":from,"to":[to],"subject":v["subject"],"text":v["text"],"html":v["html"]}),
            )?;
            r.headers.push(("idempotency-key".into(), id.to_string()));
        }
        (
            Call::Mobile {
                message,
                endpoint_id,
            },
            Provider::Twilio { account_sid, from },
        ) => {
            let (_, v) = crate::notifications::prepare_message(tx, message).await?;
            let (_, destination) = endpoint(
                service,
                tx,
                message.recipient_id,
                *endpoint_id,
                "mobile",
                false,
            )
            .await?;
            r.url = join(
                p,
                &format!("/2010-04-01/Accounts/{account_sid}/Messages.json"),
            )?;
            r.headers = vec![(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )];
            r.body = form(&[
                ("From", from.to_owned()),
                (
                    "To",
                    destination["destination"]
                        .as_str()
                        .ok_or(AppError::Internal)?
                        .into(),
                ),
                ("Body", v["text"].as_str().ok_or(AppError::Internal)?.into()),
            ]);
        }
        (
            Call::Push {
                message,
                endpoint_id,
            },
            Provider::Ntfy { .. },
        ) => {
            let (_, v) = crate::notifications::prepare_message(tx, message).await?;
            let (_, destination) = endpoint(
                service,
                tx,
                message.recipient_id,
                *endpoint_id,
                "push",
                false,
            )
            .await?;
            r.body = json_body(
                &json!({"topic":destination["destination"],"title":v["subject"],"message":v["text"]}),
            )?;
        }
        (
            Call::EndpointChallenge { endpoint_id },
            Provider::Twilio { account_sid, .. }
            | Provider::Ntfy {
                topic_prefix: account_sid,
            },
        ) => {
            let mobile = matches!(p.configuration, Provider::Twilio { .. });
            let (_, v) = endpoint(
                service,
                tx,
                tx.actor().principal_id(),
                *endpoint_id,
                if mobile { "mobile" } else { "push" },
                true,
            )
            .await?;
            let text = format!(
                "Kyro verification: {}",
                v["challenge"].as_str().ok_or(AppError::Internal)?
            );
            if mobile {
                r.url = join(
                    p,
                    &format!("/2010-04-01/Accounts/{account_sid}/Messages.json"),
                )?;
                let from = match &p.configuration {
                    Provider::Twilio { from, .. } => from,
                    _ => return Err(AppError::Internal),
                };
                r.headers = vec![(
                    "content-type".into(),
                    "application/x-www-form-urlencoded".into(),
                )];
                r.body = form(&[
                    ("From", from.clone()),
                    (
                        "To",
                        v["destination"].as_str().ok_or(AppError::Internal)?.into(),
                    ),
                    ("Body", text),
                ]);
            } else {
                r.body = json_body(
                    &json!({"topic":v["destination"],"message":text,"title":"Kyro verification"}),
                )?;
            }
        }
        (Call::Geocode { address }, Provider::Nominatim { user_agent }) => {
            r.method = RestrictedMethod::Get;
            r.url = join(p, "/search")?;
            r.url
                .query_pairs_mut()
                .append_pair("q", address)
                .append_pair("format", "jsonv2")
                .append_pair("addressdetails", "0")
                .append_pair("limit", "1");
            r.headers = vec![("user-agent".into(), user_agent.clone())];
        }
        (
            Call::Rest { operation, input }
            | Call::ControlledHttp {
                operation, input, ..
            },
            Provider::Rest { operations, .. },
        ) => {
            let definition = operations.get(operation).ok_or(AppError::NotFound)?;
            r.method = method(&definition.method)?;
            r.url = join(p, &definition.path)?;
            if r.method == RestrictedMethod::Get {
                let map = input
                    .as_object()
                    .ok_or(AppError::invalid("rest_get_arguments_invalid"))?;
                for (k, v) in map {
                    if matches!(v, Value::Object(_) | Value::Array(_) | Value::Null) {
                        return Err(AppError::invalid("rest_query_argument_invalid"));
                    }
                    r.url.query_pairs_mut().append_pair(
                        k,
                        &v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string()),
                    );
                }
            } else {
                r.body = json_body(input)?;
                r.headers.push(("idempotency-key".into(), id.to_string()));
            }
        }
        (Call::Mcp { tool, arguments }, Provider::Mcp { tools, .. }) => {
            r = mcp_request(
                p,
                id,
                "tools/call",
                json!({"name":tool,"arguments":arguments}),
            )?;
            for (key, header) in &tools.get(tool).ok_or(AppError::NotFound)?.mirrored_headers {
                let v = &arguments[key];
                let Some(value) = mirrored_value(v)? else {
                    continue;
                };
                r.headers.push((format!("mcp-param-{header}"), value));
            }
        }
        (Call::CalendarCreate { source }, Provider::GoogleCalendar { calendar_id }) => {
            let v = crate::governance::projected_resource(tx, &source.kind, source.id).await?;
            let (summary, start, end) = calendar_data(&v.data)?;
            r.url = calendar_url(p, calendar_id)?;
            r.body = json_body(
                &json!({"id":id.simple().to_string(),"summary":summary,"start":{"dateTime":start.to_rfc3339()},"end":{"dateTime":end.to_rfc3339()},"extendedProperties":{"private":{"kyroOrigin":id,"kyroSource":source.id,"kyroVersion":source.version}}}),
            )?;
        }
        (Call::CalendarRead { from, until }, Provider::GoogleCalendar { calendar_id }) => {
            r.method = RestrictedMethod::Get;
            r.url = calendar_url(p, calendar_id)?;
            r.url
                .query_pairs_mut()
                .append_pair("timeMin", &from.to_rfc3339())
                .append_pair("timeMax", &until.to_rfc3339())
                .append_pair("maxResults", "100")
                .append_pair("singleEvents", "true");
        }
        (
            Call::CalendarBookingExport { .. } | Call::CalendarSyncRead { .. },
            Provider::GoogleCalendar { .. },
        ) => {
            return super::calendar::build(tx, p, c).await;
        }
        (Call::PaymentIntent { payment_id }, Provider::Stripe { api_version, .. }) => {
            let v = payment(tx, "commerce.payment", *payment_id, p.id).await?;
            r.url = join(p, "/v1/payment_intents")?;
            r.body = form(&[
                ("amount", v.data["amount_minor"].to_string()),
                (
                    "currency",
                    v.data["currency"]
                        .as_str()
                        .ok_or(AppError::Internal)?
                        .to_ascii_lowercase(),
                ),
                ("metadata[kyro_payment]", payment_id.to_string()),
                ("automatic_payment_methods[enabled]", "true".into()),
            ]);
            stripe_headers(&mut r, *payment_id, api_version);
        }
        (
            Call::Subscription { subscription_id },
            Provider::Stripe {
                customers,
                prices,
                api_version,
            },
        ) => {
            let v = payment(tx, "commerce.subscription", *subscription_id, p.id).await?;
            let price = Uuid::parse_str(v.data["price_id"].as_str().ok_or(AppError::Internal)?)
                .map_err(|_| AppError::Internal)?;
            r.url = join(p, "/v1/subscriptions")?;
            r.body = form(&[
                (
                    "customer",
                    customers
                        .get(&tx.actor().principal_id())
                        .ok_or(AppError::Forbidden)?
                        .clone(),
                ),
                (
                    "items[0][price]",
                    prices.get(&price).ok_or(AppError::NotFound)?.clone(),
                ),
                ("metadata[kyro_subscription]", subscription_id.to_string()),
            ]);
            stripe_headers(&mut r, *subscription_id, api_version);
        }
        (Call::Refund { refund_id }, Provider::Stripe { api_version, .. }) => {
            let v = payment(tx, "commerce.refund", *refund_id, p.id).await?;
            let payment_id =
                Uuid::parse_str(v.data["payment_id"].as_str().ok_or(AppError::Internal)?)
                    .map_err(|_| AppError::Internal)?;
            let original = payment(tx, "commerce.payment", payment_id, p.id).await?;
            r.url = join(p, "/v1/refunds")?;
            r.body = form(&[
                (
                    "payment_intent",
                    original.data["provider_reference"]
                        .as_str()
                        .filter(|s| s.starts_with("pi_"))
                        .ok_or(AppError::NotFound)?
                        .into(),
                ),
                ("amount", v.data["amount_minor"].to_string()),
                ("metadata[kyro_refund]", refund_id.to_string()),
            ]);
            stripe_headers(&mut r, *refund_id, api_version);
        }
        _ => return Err(AppError::invalid("connector_contract_mismatch")),
    }
    if r.body.len() > p.max_request_bytes {
        return Err(AppError::invalid("connector_request_limit"));
    }
    Ok(r)
}
fn stripe_headers(r: &mut RestrictedRequest, id: Uuid, version: &str) {
    r.headers = vec![
        (
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        ),
        ("stripe-version".into(), version.into()),
        ("idempotency-key".into(), id.to_string()),
    ];
}
fn mirrored_value(v: &Value) -> AppResult<Option<String>> {
    Ok(match v {
        Value::Null => None,
        Value::String(s)
            if s.is_ascii()
                && s.bytes().all(|b| (32..127).contains(&b))
                && s.trim() == s
                && !(s.starts_with("=?base64?") && s.ends_with("?=")) =>
        {
            Some(s.clone())
        }
        Value::String(s) => Some(format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(s)
        )),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n)
            if n.as_i64()
                .is_some_and(|n| n.unsigned_abs() <= 9_007_199_254_740_991) =>
        {
            Some(n.to_string())
        }
        _ => return Err(AppError::invalid("mcp_header_value_invalid")),
    })
}
pub(super) fn form(values: &[(&str, String)]) -> Vec<u8> {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in values {
        serializer.append_pair(k, v);
    }
    serializer.finish().into_bytes()
}
fn object_url(
    p: &Profile,
    bucket: &str,
    prefix: &str,
    actor: &Actor,
    id: Uuid,
) -> AppResult<url::Url> {
    let mut u = url::Url::parse(&p.endpoint).map_err(|_| AppError::Internal)?;
    u.set_path(&format!(
        "/{bucket}/{prefix}/{}/{}/{}/{id}",
        actor.tenant_id(),
        actor.application_id(),
        actor.principal_id()
    ));
    Ok(u)
}
pub(super) fn calendar_url(p: &Profile, calendar: &str) -> AppResult<url::Url> {
    let mut u = join(p, "/calendar/v3/calendars/")?;
    u.path_segments_mut()
        .map_err(|_| AppError::Internal)?
        .pop_if_empty()
        .push(calendar)
        .push("events");
    Ok(u)
}
pub(super) async fn authorize(
    service: &ConnectorService,
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
    r: &mut RestrictedRequest,
) -> AppResult<()> {
    // Refresh credentials on a resumed MCP call without duplicate authorization.
    r.headers.retain(|(k, _)| {
        !k.eq_ignore_ascii_case("authorization")
            && !k.eq_ignore_ascii_case("x-amz-date")
            && !k.eq_ignore_ascii_case("x-amz-content-sha256")
    });
    if matches!(
        c,
        Call::ControlledHttp {
            sign_webhook: true,
            ..
        }
    ) {
        r.headers.retain(|(k, _)| {
            !k.eq_ignore_ascii_case("x-kyro-timestamp")
                && !k.eq_ignore_ascii_case("x-kyro-signature")
        });
        let secret = service
            .vault
            .resolve(
                tx,
                p.id,
                p.secret_ref.ok_or(AppError::Forbidden)?,
                "webhook.send",
            )
            .await?;
        let timestamp = Utc::now().timestamp();
        r.headers
            .push(("x-kyro-timestamp".into(), timestamp.to_string()));
        r.headers.push((
            "x-kyro-signature".into(),
            crate::exchange::webhook_signature(&secret, timestamp, &r.body)?,
        ));
        return Ok(());
    }
    if matches!(p.configuration, Provider::OAuth { .. }) {
        return super::oauth::authorize_client(service, tx, p, r).await;
    }
    if let Some(provider) = p.oauth_provider_id {
        let (access, _) =
            super::oauth::access(service, tx, provider, &p.required_oauth_scopes).await?;
        r.headers.push((
            "authorization".into(),
            format!("Bearer {}", access.as_str()),
        ));
        return Ok(());
    }
    let Some(reference) = p.secret_ref else {
        return Ok(());
    };
    let secret = service
        .vault
        .resolve(tx, p.id, reference, "connector.send")
        .await?;
    match &p.configuration {
        Provider::S3 { region, .. } => {
            let credentials: Value =
                serde_json::from_slice(&secret).map_err(|_| AppError::Unavailable)?;
            let access = credentials["access_key"]
                .as_str()
                .filter(|s| label(s))
                .ok_or(AppError::Unavailable)?;
            let secret = credentials["secret_key"]
                .as_str()
                .filter(|s| bounded(s, 128))
                .ok_or(AppError::Unavailable)?;
            s3_sign(r, region, access, secret, Utc::now())?;
        }
        Provider::Twilio { account_sid, .. } => {
            let token = std::str::from_utf8(&secret).map_err(|_| AppError::Unavailable)?;
            if !bounded(token, 4096) {
                return Err(AppError::Unavailable);
            }
            r.headers.push((
                "authorization".into(),
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{account_sid}:{token}"))
                ),
            ));
        }
        _ => {
            let token = std::str::from_utf8(&secret).map_err(|_| AppError::Unavailable)?;
            if !bounded(token, 4096) {
                return Err(AppError::Unavailable);
            }
            r.headers
                .push(("authorization".into(), format!("Bearer {token}")));
        }
    }
    Ok(())
}
pub(super) fn s3_sign(
    r: &mut RestrictedRequest,
    region: &str,
    access: &str,
    secret: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    fn mac(key: &[u8], bytes: &[u8]) -> AppResult<Vec<u8>> {
        let mut h = Hmac::<Sha256>::new_from_slice(key).map_err(|_| AppError::Internal)?;
        h.update(bytes);
        Ok(h.finalize().into_bytes().to_vec())
    }
    let date = now.format("%Y%m%d").to_string();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let host = r.url.host_str().ok_or(AppError::Internal)?;
    let host = match r.url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.into(),
    };
    let hash = hex(&Sha256::digest(&r.body));
    let verb = match r.method {
        RestrictedMethod::Get => "GET",
        RestrictedMethod::Put => "PUT",
        RestrictedMethod::Delete => "DELETE",
        _ => return Err(AppError::Internal),
    };
    if r.url.query().is_some() {
        return Err(AppError::invalid("s3_query_not_supported"));
    }
    let mut signed = BTreeMap::from([
        ("host".to_string(), host),
        ("x-amz-content-sha256".to_string(), hash.clone()),
        ("x-amz-date".to_string(), stamp.clone()),
    ]);
    for (name, value) in &r.headers {
        if name.eq_ignore_ascii_case("authorization") {
            continue;
        }
        let normalized = value.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
        if signed
            .insert(name.to_ascii_lowercase(), normalized)
            .is_some()
        {
            return Err(AppError::invalid("s3_header_duplicate"));
        }
    }
    let names = signed.keys().cloned().collect::<Vec<_>>().join(";");
    let headers = signed
        .iter()
        .map(|(k, v)| format!("{k}:{v}\n"))
        .collect::<String>();
    let canonical = format!("{verb}\n{}\n\n{headers}\n{names}\n{hash}", r.url.path());
    let scope = format!("{date}/{region}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let day = Zeroizing::new(mac(format!("AWS4{secret}").as_bytes(), date.as_bytes())?);
    let region_key = Zeroizing::new(mac(&day, region.as_bytes())?);
    let service = Zeroizing::new(mac(&region_key, b"s3")?);
    let key = Zeroizing::new(mac(&service, b"aws4_request")?);
    let signature = hex(&mac(&key, to_sign.as_bytes())?);
    r.headers.extend([("x-amz-date".into(),stamp),("x-amz-content-sha256".into(),hash),("authorization".into(),format!("AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={names}, Signature={signature}"))]);
    Ok(())
}
pub(super) fn mcp_response(response: &ControlledResponse, id: Uuid) -> AppResult<Value> {
    let v: Value = if response
        .content_type
        .as_deref()
        .is_some_and(|t| t.starts_with("text/event-stream"))
    {
        let text = std::str::from_utf8(&response.body)
            .map_err(|_| AppError::invalid("mcp_response_invalid"))?;
        let mut final_result = None;
        let mut frames = 0;
        for frame in text.replace("\r\n", "\n").split("\n\n") {
            frames += 1;
            if frames > 64 {
                return Err(AppError::invalid("mcp_event_limit"));
            }
            let data = frame
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            let v: Value =
                serde_json::from_str(&data).map_err(|_| AppError::invalid("mcp_event_invalid"))?;
            if v["id"] == json!(id) {
                if final_result.is_some() {
                    return Err(AppError::invalid("mcp_duplicate_result"));
                }
                final_result = Some(v);
            } else if !matches!(
                v["method"].as_str(),
                Some("notifications/progress" | "notifications/message")
            ) {
                return Err(AppError::invalid("mcp_server_intent_denied"));
            }
        }
        final_result.ok_or(AppError::invalid("mcp_result_missing"))?
    } else {
        serde_json::from_slice(&response.body)
            .map_err(|_| AppError::invalid("mcp_response_invalid"))?
    };
    if v["jsonrpc"] != "2.0"
        || v["id"] != json!(id)
        || v.get("error").is_some()
        || v["result"].get("inputRequests").is_some()
    {
        return Err(AppError::invalid("mcp_result_invalid"));
    }
    Ok(v["result"].clone())
}
pub(super) fn result(
    p: &Profile,
    c: &Call,
    response: &ControlledResponse,
    id: Uuid,
) -> AppResult<Value> {
    if !(200..300).contains(&response.status) {
        return Err(AppError::invalid("connector_provider_rejected"));
    }
    if matches!(c, Call::CalendarBookingExport { .. }) {
        return super::calendar::export_receipt(response);
    }
    if matches!(p.configuration, Provider::OAuth { .. }) {
        return if matches!(c, Call::OAuthRevoke { .. }) {
            Ok(json!({"accepted":true}))
        } else {
            serde_json::from_slice(&response.body)
                .map_err(|_| AppError::invalid("oauth_response_invalid"))
        };
    }
    if matches!(c, Call::ObjectGet { .. }) {
        return Ok(
            json!({"content_base64":base64::engine::general_purpose::STANDARD.encode(&response.body),"sha256":hex(&Sha256::digest(&response.body)),"size":response.body.len()}),
        );
    }
    if matches!(c, Call::ObjectPut { .. } | Call::ObjectDelete { .. }) {
        return Ok(
            json!({"accepted":true,"status":response.status,"object_scope":"principal_private"}),
        );
    }
    let v: Value = if matches!(c, Call::Mcp { .. }) {
        mcp_response(response, id)?
    } else {
        serde_json::from_slice(&response.body)
            .map_err(|_| AppError::invalid("connector_response_invalid"))?
    };
    crate::governance::validate_shape(&v)?;
    match (c, &p.configuration) {
        (Call::Email { .. }, Provider::Resend { .. }) => {
            let id = v["id"]
                .as_str()
                .filter(|s| bounded(s, 200))
                .ok_or(AppError::invalid("email_receipt_invalid"))?;
            Ok(json!({"provider_id":id,"accepted":true,"delivery_confirmed":false}))
        }
        (Call::Mobile { .. } | Call::EndpointChallenge { .. }, Provider::Twilio { .. }) => {
            let sid = v["sid"]
                .as_str()
                .filter(|s| s.starts_with("SM") && s.len() == 34)
                .ok_or(AppError::invalid("mobile_receipt_invalid"))?;
            if !matches!(
                v["status"].as_str(),
                Some("queued" | "accepted" | "sending" | "sent" | "delivered")
            ) {
                return Err(AppError::invalid("mobile_status_invalid"));
            }
            Ok(
                json!({"provider_id":sid,"accepted":true,"delivery_confirmed":v["status"]=="delivered"}),
            )
        }
        (Call::Push { .. } | Call::EndpointChallenge { .. }, Provider::Ntfy { .. }) => {
            let id = v["id"]
                .as_str()
                .filter(|s| bounded(s, 100))
                .ok_or(AppError::invalid("push_receipt_invalid"))?;
            if v["event"] != "message" {
                return Err(AppError::invalid("push_event_invalid"));
            }
            Ok(json!({"provider_id":id,"accepted":true,"delivery_confirmed":false}))
        }
        (Call::Geocode { .. }, Provider::Nominatim { .. }) => {
            let hits = v
                .as_array()
                .filter(|a| a.len() <= 1)
                .ok_or(AppError::invalid("geocode_response_invalid"))?;
            let mut items = vec![];
            for hit in hits {
                let parse = |k: &str| {
                    hit[k]
                        .as_str()
                        .and_then(|s| s.parse::<f64>().ok())
                        .filter(|n| n.is_finite())
                        .ok_or(AppError::invalid("geocode_coordinates_invalid"))
                };
                let lat = parse("lat")?;
                let lon = parse("lon")?;
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    return Err(AppError::invalid("geocode_coordinates_invalid"));
                }
                items.push(json!({"latitude":lat,"longitude":lon}));
            }
            Ok(
                json!({"items":items,"attribution":{"label":"© OpenStreetMap contributors",
                "url":"https://www.openstreetmap.org/copyright","licence":"ODbL-1.0",
                "licence_url":"https://opendatacommons.org/licenses/odbl/1-0/","display_required":true}}),
            )
        }
        (
            Call::Rest { operation, .. } | Call::ControlledHttp { operation, .. },
            Provider::Rest { operations, .. },
        ) => {
            operations
                .get(operation)
                .ok_or(AppError::NotFound)?
                .output
                .validate(&v)?;
            Ok(crate::governance::masked(v))
        }
        (Call::Mcp { tool, .. }, Provider::Mcp { tools, .. }) => {
            if v["isError"] == true {
                return Err(AppError::invalid("mcp_tool_error"));
            }
            let structured = v
                .get("structuredContent")
                .ok_or(AppError::invalid("mcp_structured_result_required"))?;
            tools
                .get(tool)
                .ok_or(AppError::NotFound)?
                .output
                .validate(structured)?;
            Ok(
                json!({"structured_content":crate::governance::masked(structured.clone()),"trusted_as_instruction":false}),
            )
        }
        (Call::CalendarCreate { .. }, Provider::GoogleCalendar { .. }) => {
            if v["id"] != id.simple().to_string() {
                return Err(AppError::invalid("calendar_receipt_invalid"));
            }
            Ok(json!({"provider_id":v["id"],"accepted":true,"origin_marker":id}))
        }
        (
            Call::CalendarRead { .. } | Call::CalendarSyncRead { .. },
            Provider::GoogleCalendar { .. },
        ) => super::calendar::read_receipt(&v),
        (Call::PaymentIntent { payment_id }, Provider::Stripe { .. }) => {
            let reference = v["id"]
                .as_str()
                .filter(|s| s.starts_with("pi_") && bounded(s, 200))
                .ok_or(AppError::invalid("payment_receipt_invalid"))?;
            if v["metadata"]["kyro_payment"] != payment_id.to_string() {
                return Err(AppError::invalid("payment_source_receipt_mismatch"));
            }
            Ok(
                json!({"provider_id":reference,"status":v["status"],"amount_minor":v["amount"],"currency":v["currency"],"payment_confirmed":false}),
            )
        }
        (Call::Subscription { subscription_id }, Provider::Stripe { .. }) => {
            let reference = v["id"]
                .as_str()
                .filter(|s| s.starts_with("sub_") && bounded(s, 200))
                .ok_or(AppError::invalid("subscription_receipt_invalid"))?;
            if v["metadata"]["kyro_subscription"] != subscription_id.to_string() {
                return Err(AppError::invalid("subscription_source_receipt_mismatch"));
            }
            let items = v["items"]["data"]
                .as_array()
                .filter(|a| a.len() == 1)
                .ok_or(AppError::invalid("subscription_items_receipt_mismatch"))?;
            Ok(
                json!({"provider_id":reference,"customer":v["customer"],"price":items[0]["price"]["id"],"subscription_confirmed":false}),
            )
        }
        (Call::Refund { refund_id }, Provider::Stripe { .. }) => {
            let reference = v["id"]
                .as_str()
                .filter(|s| s.starts_with("re_") && bounded(s, 200))
                .ok_or(AppError::invalid("refund_receipt_invalid"))?;
            if v["metadata"]["kyro_refund"] != refund_id.to_string() {
                return Err(AppError::invalid("refund_source_receipt_mismatch"));
            }
            Ok(
                json!({"provider_id":reference,"amount_minor":v["amount"],"currency":v["currency"],"payment_intent":v["payment_intent"],"refund_confirmed":false}),
            )
        }
        _ => Err(AppError::invalid("connector_response_mismatch")),
    }
}

pub(super) async fn validate_receipt(
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
    receipt: &Value,
) -> AppResult<()> {
    check_receipt(tx, p, c, receipt).await?;
    let resource = financial_resource(c);
    if let Some((kind, id)) = resource {
        let mut record = tx.get_for_update(kind, id).await?;
        let reference = receipt["provider_id"]
            .as_str()
            .ok_or(AppError::invalid("provider_reference_missing"))?;
        if !record.data["provider_reference"].is_null()
            && record.data["provider_reference"] != reference
        {
            return Err(AppError::conflict("provider_reference_changed"));
        }
        if record.data["provider_reference"].is_null() {
            record.data["provider_reference"] = json!(reference);
            tx.update(kind, id, record.version, record.data).await?;
        }
    }
    Ok(())
}

pub(super) fn financial_resource(c: &Call) -> Option<(&'static str, Uuid)> {
    match c {
        Call::PaymentIntent { payment_id } => Some(("commerce.payment", *payment_id)),
        Call::Subscription { subscription_id } => Some(("commerce.subscription", *subscription_id)),
        Call::Refund { refund_id } => Some(("commerce.refund", *refund_id)),
        _ => None,
    }
}

pub(super) async fn check_receipt(
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
    receipt: &Value,
) -> AppResult<()> {
    match (c, &p.configuration) {
        (Call::PaymentIntent { payment_id }, Provider::Stripe { .. })
        | (
            Call::Refund {
                refund_id: payment_id,
            },
            Provider::Stripe { .. },
        ) => {
            let refund = matches!(c, Call::Refund { .. });
            let record = payment(
                tx,
                if refund {
                    "commerce.refund"
                } else {
                    "commerce.payment"
                },
                *payment_id,
                p.id,
            )
            .await?;
            let currency = record.data["currency"]
                .as_str()
                .ok_or(AppError::Internal)?
                .to_ascii_lowercase();
            if receipt["amount_minor"] != record.data["amount_minor"]
                || receipt["currency"] != currency
            {
                return Err(AppError::invalid("financial_receipt_amount_mismatch"));
            }
            if refund {
                let original = Uuid::parse_str(
                    record.data["payment_id"]
                        .as_str()
                        .ok_or(AppError::Internal)?,
                )
                .map_err(|_| AppError::Internal)?;
                let original = payment(tx, "commerce.payment", original, p.id).await?;
                if receipt["payment_intent"] != original.data["provider_reference"] {
                    return Err(AppError::invalid("refund_original_receipt_mismatch"));
                }
            } else if !matches!(
                receipt["status"].as_str(),
                Some(
                    "requires_payment_method"
                        | "requires_confirmation"
                        | "requires_action"
                        | "processing"
                        | "requires_capture"
                        | "canceled"
                        | "succeeded"
                )
            ) {
                return Err(AppError::invalid("payment_receipt_status_invalid"));
            }
        }
        (
            Call::Subscription { subscription_id },
            Provider::Stripe {
                customers, prices, ..
            },
        ) => {
            let record = payment(tx, "commerce.subscription", *subscription_id, p.id).await?;
            let price =
                Uuid::parse_str(record.data["price_id"].as_str().ok_or(AppError::Internal)?)
                    .map_err(|_| AppError::Internal)?;
            if receipt["customer"].as_str()
                != customers
                    .get(&tx.actor().principal_id())
                    .map(String::as_str)
                || receipt["price"].as_str() != prices.get(&price).map(String::as_str)
            {
                return Err(AppError::invalid("subscription_receipt_binding_mismatch"));
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sigv4_matches_published_aws_vector_and_signs_conditional_headers() {
        // Public AWS documentation example, not an installation credential.
        // https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sig-v4-header-based-auth.html
        let mut request = RestrictedRequest {
            method: RestrictedMethod::Get,
            url: url::Url::parse("https://examplebucket.s3.amazonaws.com/test.txt").unwrap(),
            headers: vec![("range".into(), "bytes=0-9".into())],
            body: vec![],
        };
        let now = DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        s3_sign(
            &mut request,
            "us-east-1",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            now,
        )
        .unwrap();
        assert!(
            request
                .headers
                .iter()
                .find(|(k, _)| k == "authorization")
                .unwrap()
                .1
                .ends_with(
                    "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
                )
        );
        request.headers.retain(|(k, _)| k == "range");
        request.headers.push(("if-none-match".into(), "*".into()));
        s3_sign(
            &mut request,
            "us-east-1",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            now,
        )
        .unwrap();
        assert!(
            request
                .headers
                .iter()
                .find(|(k, _)| k == "authorization")
                .unwrap()
                .1
                .contains("SignedHeaders=host;if-none-match;range;x-amz-content-sha256;x-amz-date")
        );
    }
    #[test]
    fn mcp_mirror_encoding_is_unambiguous_and_refuses_unsafe_integers() {
        assert_eq!(mirrored_value(&json!(null)).unwrap(), None);
        assert_eq!(mirrored_value(&json!(true)).unwrap(), Some("true".into()));
        assert_eq!(
            mirrored_value(&json!("plain")).unwrap(),
            Some("plain".into())
        );
        assert_eq!(
            mirrored_value(&json!("=?base64?literal?=")).unwrap(),
            Some(format!(
                "=?base64?{}?=",
                base64::engine::general_purpose::STANDARD.encode("=?base64?literal?=")
            ))
        );
        assert!(mirrored_value(&json!(9007199254740992_i64)).is_err());
        assert!(mirrored_value(&json!(1.5)).is_err());
    }
}
