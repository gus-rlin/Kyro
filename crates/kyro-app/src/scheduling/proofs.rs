//! Short lived, server issued attendance proofs. Only their SHA-256 is stored.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub(super) async fn issue(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        booking_id: Uuid,
        expires_in_minutes: u16,
    }
    let body: Input = decode(input)?;
    if !(1..=60).contains(&body.expires_in_minutes) {
        return Err(AppError::invalid("attendance_proof_lifetime_invalid"));
    }
    let row=sqlx::query("SELECT b.principal_id,b.version,b.status,r.owner_id FROM app_sched_bookings b JOIN app_sched_slots s ON s.id=b.slot_id JOIN app_sched_resources r ON r.id=s.resource_id WHERE b.id=$1 FOR UPDATE OF b")
        .bind(body.booking_id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if row.try_get::<Uuid, _>("principal_id")? != principal(tx)
        && !can_manage_owner(tx, row.try_get("owner_id")?)
    {
        return Err(AppError::NotFound);
    }
    if row.try_get::<String, _>("status")? != "confirmed" {
        return Err(AppError::conflict("booking_not_attendable"));
    }
    sqlx::query("DELETE FROM app_sched_attendance_proofs WHERE booking_id=$1 AND expires_at<=clock_timestamp()")
        .bind(body.booking_id).execute(tx.conn()).await?;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_sched_attendance_proofs WHERE booking_id=$1")
            .bind(body.booking_id)
            .fetch_one(tx.conn())
            .await?;
    if count >= 5 {
        return Err(AppError::Quota);
    }
    let mut raw = Zeroizing::new([0u8; 32]);
    getrandom::fill(raw.as_mut()).map_err(|_| AppError::Unavailable)?;
    let token = URL_SAFE_NO_PAD.encode(raw.as_slice());
    let hash = Sha256::digest(raw.as_slice()).to_vec();
    let id = Uuid::new_v4();
    let expires:DateTime<Utc>=sqlx::query_scalar("INSERT INTO app_sched_attendance_proofs(tenant_id,id,token_hash,booking_id,booking_version,issued_by,expires_at) VALUES($1,$2,$3,$4,$5,$6,clock_timestamp()+make_interval(mins=>$7)) RETURNING expires_at")
        .bind(tenant(tx)).bind(id).bind(hash).bind(body.booking_id).bind(row.try_get::<i64,_>("version")?).bind(principal(tx)).bind(i32::from(body.expires_in_minutes)).fetch_one(tx.conn()).await?;
    tx.audit(
        "B119",
        "issue_ticket",
        Some(id),
        json!({"booking_id":body.booking_id,"expires_at":expires}),
    )
    .await?;
    Ok(
        json!({"id":id,"booking_id":body.booking_id,"token":token,"expires_at":expires,"format":"opaque_sha256_v1"}),
    )
}

pub(super) async fn consume(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        token: String,
    }
    let body: Input = decode(input)?;
    let raw = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(&body.token)
            .map_err(|_| AppError::NotFound)?,
    );
    if raw.len() != 32 || URL_SAFE_NO_PAD.encode(raw.as_slice()) != body.token {
        return Err(AppError::NotFound);
    }
    let hash = Sha256::digest(raw.as_slice()).to_vec();
    let row=sqlx::query("SELECT id,booking_id,booking_version FROM app_sched_attendance_proofs WHERE token_hash=$1 AND consumed_at IS NULL AND expires_at>clock_timestamp() FOR UPDATE")
        .bind(hash).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let booking: Uuid = row.try_get("booking_id")?;
    let source =
        sqlx::query("SELECT principal_id,version,status FROM app_sched_bookings WHERE id=$1")
            .bind(booking)
            .fetch_optional(tx.conn())
            .await?
            .ok_or(AppError::NotFound)?;
    if source.try_get::<i64, _>("version")? != row.try_get::<i64, _>("booking_version")?
        || source.try_get::<String, _>("status")? != "confirmed"
    {
        return Err(AppError::NotFound);
    }
    if source.try_get::<Uuid, _>("principal_id")? != principal(tx)
        && !tx.actor().roles().iter().any(|r| {
            matches!(
                r.as_str(),
                "attendance.scan" | "scheduling.manage" | "admin" | "owner"
            )
        })
    {
        return Err(AppError::Forbidden);
    }
    let member: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM app_memberships WHERE principal_id=$1 AND status='active')",
    )
    .bind(source.try_get::<Uuid, _>("principal_id")?)
    .fetch_one(tx.conn())
    .await?;
    if !member {
        return Err(AppError::NotFound);
    }
    let mut result = attendance_inner(
        tx,
        &json!({"booking_id":booking}),
        true,
        Some(row.try_get("booking_version")?),
    )
    .await?;
    let id: Uuid = row.try_get("id")?;
    let changed=sqlx::query("UPDATE app_sched_attendance_proofs SET consumed_at=clock_timestamp() WHERE id=$1 AND consumed_at IS NULL AND expires_at>clock_timestamp()").bind(id).execute(tx.conn()).await?.rows_affected();
    if changed != 1 {
        return Err(AppError::NotFound);
    }
    tx.audit(
        "B119",
        "consume_ticket",
        Some(id),
        json!({"booking_id":booking}),
    )
    .await?;
    result["proof_id"] = json!(id);
    Ok(result)
}
