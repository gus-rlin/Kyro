use super::*;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Deliver {
    adapter_id: Uuid,
    message: crate::notifications::Send,
    #[serde(default)]
    endpoint_id: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    adapter_id: Uuid,
    #[serde(default)]
    phone: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Verify {
    id: Uuid,
    secret: String,
}
pub(super) async fn execute(
    service: &ConnectorService,
    tx: &mut AppTx,
    r: &OperationRequest,
) -> AppResult<Value> {
    match r.action.as_str() {
        "email.send" | "mobile.send" | "push.send" => {
            let i: Deliver = decode(r)?;
            let p = service.admitted(tx, i.adapter_id).await?;
            let call = match (r.component_id.as_str(), &p.configuration) {
                ("B104", Provider::Resend { .. }) => {
                    tx.require_operation("B152", "adapter.call")?;
                    Call::Email { message: i.message }
                }
                ("B105", Provider::Twilio { .. }) => Call::Mobile {
                    message: i.message,
                    endpoint_id: i
                        .endpoint_id
                        .ok_or(AppError::invalid("endpoint_required"))?,
                },
                ("B106", Provider::Ntfy { .. }) => Call::Push {
                    message: i.message,
                    endpoint_id: i
                        .endpoint_id
                        .ok_or(AppError::invalid("endpoint_required"))?,
                },
                _ => return Err(AppError::invalid("delivery_adapter_mismatch")),
            };
            service.prepare(tx, r, p, call).await
        }
        "endpoint.register" => {
            let i: Register = decode(r)?;
            let p = service.admitted(tx, i.adapter_id).await?;
            let mobile = match (r.component_id.as_str(), &p.configuration) {
                ("B105", Provider::Twilio { .. }) => true,
                ("B106", Provider::Ntfy { .. }) => false,
                _ => return Err(AppError::Forbidden),
            };
            let human:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_principals WHERE id=$1 AND status='active' AND account_type='human')").bind(tx.actor().principal_id()).fetch_one(tx.conn()).await?;
            if !human {
                return Err(AppError::Forbidden);
            }
            let quota_id = crate::governance::stable_id(
                "endpoint-admission",
                &tx.actor().principal_id().to_string(),
            );
            tx.lock_record_key("notification.endpoint", quota_id)
                .await?;
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM app_delivery_endpoints WHERE principal_id=$1 AND NOT revoked",
            )
            .bind(tx.actor().principal_id())
            .fetch_one(tx.conn())
            .await?;
            if count >= 5 {
                return Err(AppError::Quota);
            }
            let id = Uuid::new_v4();
            let destination = if mobile {
                i.phone
                    .filter(|p| protocols::phone_valid(p))
                    .ok_or(AppError::invalid("phone_invalid"))?
            } else {
                if i.phone.is_some() {
                    return Err(AppError::invalid("push_destination_is_assigned"));
                }
                let prefix = match &p.configuration {
                    Provider::Ntfy { topic_prefix } => topic_prefix,
                    _ => return Err(AppError::Internal),
                };
                format!("{prefix}_{}", crate::governance::token()?)
            };
            let challenge = Zeroizing::new(crate::governance::token()?);
            let value = json!({"destination":destination,"challenge":challenge.as_str()});
            let cipher = service.cipher.seal(
                &service.context(tx, id, "endpoint"),
                &serde_json::to_vec(&value).map_err(|_| AppError::Internal)?,
            )?;
            sqlx::query("INSERT INTO app_delivery_endpoints(tenant_id,principal_id,id,channel,destination_cipher,challenge_hash,challenge_expires) VALUES($1,$2,$3,$4,$5,$6,clock_timestamp()+interval '5 minutes')").bind(tx.actor().tenant_id()).bind(tx.actor().principal_id()).bind(id).bind(if mobile{"mobile"}else{"push"}).bind(cipher).bind(Sha256::digest(challenge.as_bytes()).to_vec()).execute(tx.conn()).await?;
            let delivery = service
                .prepare(tx, r, p, Call::EndpointChallenge { endpoint_id: id })
                .await?;
            let mut result = json!({"id":id,"version":1,"verified":false,"delivery":delivery});
            if !mobile {
                result["secret_once"] =
                    json!({"subscription_topic":destination,"endpoint":p.endpoint});
            }
            Ok(result)
        }
        "endpoint.verify" => {
            let i: Verify = decode(r)?;
            if i.secret.len() > 128 {
                return Err(AppError::invalid("endpoint_proof_invalid"));
            }
            let row=sqlx::query("SELECT version,verified,challenge_hash,challenge_attempts,destination_cipher FROM app_delivery_endpoints WHERE id=$1 AND channel=$2 AND NOT revoked AND challenge_expires>clock_timestamp() FOR UPDATE").bind(i.id).bind(if r.component_id=="B105"{"mobile"}else{"push"}).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            if row.try_get::<bool, _>("verified")? {
                return Err(AppError::conflict("endpoint_already_verified"));
            }
            let attempts: i32 = row.try_get("challenge_attempts")?;
            if attempts >= 5 {
                return Err(AppError::Forbidden);
            }
            let actual: Vec<u8> = row.try_get("challenge_hash")?;
            let good = Sha256::digest(i.secret.as_bytes()).to_vec() == actual;
            if good {
                let context = service.context(tx, i.id, "endpoint");
                let plain = service
                    .cipher
                    .open(&context, &row.try_get::<Vec<u8>, _>("destination_cipher")?)?;
                let mut value: Value =
                    serde_json::from_slice(&plain).map_err(|_| AppError::Internal)?;
                value
                    .as_object_mut()
                    .ok_or(AppError::Internal)?
                    .remove("challenge");
                let cipher = service.cipher.seal(
                    &context,
                    &serde_json::to_vec(&value).map_err(|_| AppError::Internal)?,
                )?;
                sqlx::query("UPDATE app_delivery_endpoints SET destination_cipher=$2 WHERE id=$1")
                    .bind(i.id)
                    .bind(cipher)
                    .execute(tx.conn())
                    .await?;
            }
            sqlx::query("UPDATE app_delivery_endpoints SET challenge_attempts=challenge_attempts+1,verified=$2,version=version+CASE WHEN $2 THEN 1 ELSE 0 END,challenge_hash=CASE WHEN $2 THEN NULL ELSE challenge_hash END WHERE id=$1").bind(i.id).bind(good).execute(tx.conn()).await?;
            tx.audit(
                &r.component_id,
                "endpoint.verify",
                Some(i.id),
                json!({"verified":good}),
            )
            .await?;
            Ok(
                json!({"id":i.id,"verified":good,"attempts_remaining":4-attempts,"version":row.try_get::<i64,_>("version")?+i64::from(good)}),
            )
        }
        "endpoint.revoke" => {
            let i: AdapterId = decode(r)?;
            let version = r
                .expected_version
                .filter(|v| *v > 0)
                .ok_or(AppError::invalid("expected_version_required"))?;
            let n=sqlx::query("UPDATE app_delivery_endpoints SET revoked=true,version=version+1,challenge_hash=NULL WHERE id=$1 AND version=$2 AND channel=$3 AND NOT revoked").bind(i.id).bind(version).bind(if r.component_id=="B105"{"mobile"}else{"push"}).execute(tx.conn()).await?.rows_affected();
            if n != 1 {
                return Err(AppError::conflict("endpoint_version_conflict"));
            }
            Ok(json!({"id":i.id,"version":version+1,"revoked":true}))
        }
        _ => Err(AppError::NotFound),
    }
}
