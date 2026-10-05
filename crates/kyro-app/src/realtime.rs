//! Bounded SSE and WebSocket transports over the same authorized operations.
use crate::http::{CookieAuthentication, HttpState};
use crate::{Actor, AppError, AppResult, OperationRequest};
use axum::{
    extract::{
        Extension, Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct RealtimeConfig {
    origin: String,
}
impl RealtimeConfig {
    pub fn new(origin: &str, development: bool) -> AppResult<Self> {
        let url =
            url::Url::parse(origin).map_err(|_| AppError::invalid("realtime_origin_invalid"))?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(url.scheme() == "https" || development && url.scheme() == "http" && local)
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || url.origin().ascii_serialization() != origin
        {
            return Err(AppError::invalid("realtime_origin_invalid"));
        }
        Ok(Self {
            origin: origin.into(),
        })
    }
    pub fn from_env() -> AppResult<Option<Self>> {
        match std::env::var("KYRO_APP_UI_ORIGIN") {
            Ok(v) => Ok(Some(Self::new(
                &v,
                std::env::var("KYRO_APP_ENVIRONMENT").as_deref() == Ok("development"),
            )?)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(AppError::invalid("realtime_origin_invalid")),
        }
    }
}
#[derive(Default)]
pub(crate) struct Connections {
    counts: Mutex<BTreeMap<(Uuid, Uuid, Uuid), u8>>,
}
struct Permit {
    limits: Arc<Connections>,
    key: (Uuid, Uuid, Uuid),
}
impl Connections {
    fn acquire(self: &Arc<Self>, actor: &Actor) -> AppResult<Permit> {
        let key = (
            actor.tenant_id(),
            actor.application_id(),
            actor.principal_id(),
        );
        let mut counts = self.counts.lock().map_err(|_| AppError::Internal)?;
        if counts.values().map(|n| usize::from(*n)).sum::<usize>() >= 128
            || counts.get(&key).copied().unwrap_or(0) >= 2
        {
            return Err(AppError::Quota);
        }
        *counts.entry(key).or_default() += 1;
        Ok(Permit {
            limits: self.clone(),
            key,
        })
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut counts) = self.limits.counts.lock()
            && let Some(n) = counts.get_mut(&self.key)
        {
            *n = n.saturating_sub(1);
            if *n == 0 {
                counts.remove(&self.key);
            }
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventsQuery {
    after: Option<String>,
}
pub(crate) async fn events(
    State(state): State<HttpState>,
    Path(app): Path<Uuid>,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> AppResult<Response> {
    if actor.application_id() != app
        || !state.operations.has_component("B107")
        || !state.operations.has_component("B101")
    {
        return Err(AppError::NotFound);
    }
    let header_values = headers.get_all("last-event-id");
    if header_values.iter().count() > 1 {
        return Err(AppError::invalid("notification_cursor_invalid"));
    }
    let supplied = header_values
        .iter()
        .next()
        .map(|h| h.to_str().map(str::to_owned))
        .transpose()
        .map_err(|_| AppError::invalid("notification_cursor_invalid"))?;
    if supplied.is_some() && query.after.is_some() && supplied != query.after {
        return Err(AppError::invalid("notification_cursor_conflict"));
    }
    let after = supplied.or(query.after);
    let mut tx = state.core.begin_read(actor.clone()).await?;
    tx.require_operation("B107", "stream.read")?;
    crate::notifications::feed(&mut tx, after.as_deref(), 1).await?;
    tx.commit().await?;
    let permit = state.connections.acquire(&actor)?;
    let stream_state = EventsState {
        state,
        actor,
        cursor: after,
        permit,
        expires: tokio::time::Instant::now() + Duration::from_secs(900),
        closed: false,
    };
    let stream = futures_util::stream::unfold(stream_state, next_event);
    Ok(Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("alive"),
        )
        .into_response())
}
struct EventsState {
    state: HttpState,
    actor: Actor,
    cursor: Option<String>,
    permit: Permit,
    expires: tokio::time::Instant,
    closed: bool,
}
async fn next_event(mut s: EventsState) -> Option<(Result<Event, Infallible>, EventsState)> {
    if s.closed || tokio::time::Instant::now() >= s.expires {
        return None;
    }
    let _ = &s.permit;
    let mut tx = s.state.core.begin_read(s.actor.clone()).await.ok()?;
    tx.require_operation("B107", "stream.read").ok()?;
    let page = match crate::notifications::feed(&mut tx, s.cursor.as_deref(), 1).await {
        Ok(p) => p,
        Err(AppError::Conflict("notification_history_expired")) => {
            s.closed = true;
            return Some((
                Ok(Event::default()
                    .event("reset-required")
                    .data(r#"{"code":"notification_history_expired","snapshot_required":true}"#)),
                s,
            ));
        }
        Err(_) => return None,
    };
    tx.commit().await.ok()?;
    s.cursor = Some(page["next_cursor"].as_str()?.into());
    if let Some(item) = page["items"].as_array()?.first() {
        let data = serde_json::to_string(item).ok()?;
        let id = s.cursor.clone()?;
        Some((
            Ok(Event::default().event("notification").id(id).data(data)),
            s,
        ))
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Some((Ok(Event::default().comment("poll")), s))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoQuery {}
pub(crate) async fn socket(
    State(state): State<HttpState>,
    Path(app): Path<Uuid>,
    Extension(actor): Extension<Actor>,
    Extension(CookieAuthentication(cookie)): Extension<CookieAuthentication>,
    headers: HeaderMap,
    Query(_): Query<NoQuery>,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    if actor.application_id() != app || !state.operations.has_component("B108") {
        return Err(AppError::NotFound);
    }
    let origin = state
        .realtime
        .as_ref()
        .map(|c| c.origin.as_str())
        .or_else(|| state.identity.as_ref().map(|i| i.config().origin()))
        .ok_or(AppError::Unavailable)?;
    if headers.get_all(header::ORIGIN).iter().count() != 1
        || headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(origin)
    {
        return Err(AppError::Forbidden);
    }
    if headers.get_all("sec-websocket-protocol").iter().count() != 1 {
        return Err(AppError::Forbidden);
    }
    let protocols = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 256)
        .ok_or(AppError::Forbidden)?;
    let protocols: Vec<_> = protocols.split(',').map(str::trim).collect();
    if !protocols.contains(&"kyro.operations.v1") {
        return Err(AppError::Forbidden);
    }
    if cookie {
        let identity = state.identity.as_ref().ok_or(AppError::Unauthorized)?;
        let csrf_cookie = crate::identity::http::cookie(
            &headers,
            &format!("kyro_csrf_{}", identity.config().application_id.simple()),
        )?
        .ok_or(AppError::Forbidden)?;
        let csrf: Vec<_> = protocols
            .iter()
            .filter_map(|p| p.strip_prefix("csrf."))
            .collect();
        if csrf.len() != 1 || csrf[0] != csrf_cookie {
            return Err(AppError::Forbidden);
        }
        state.core.validate_csrf(actor.clone(), csrf[0]).await?;
    }
    let tx = state.core.begin_read(actor.clone()).await?;
    tx.require_operation("B108", "socket.open")?;
    tx.commit().await?;
    let permit = state.connections.acquire(&actor)?;
    Ok(ws
        .max_message_size(16384)
        .max_frame_size(16384)
        .write_buffer_size(1024)
        .max_write_buffer_size(32768)
        .protocols(["kyro.operations.v1"])
        .on_upgrade(move |ws| run_socket(ws, state, actor, permit))
        .into_response())
}
fn allowed(r: &OperationRequest) -> bool {
    matches!(
        (r.component_id.as_str(), r.action.as_str()),
        ("B101", "notification.feed" | "notification.read")
            | (
                "B109",
                "presence.touch" | "presence.list" | "presence.leave"
            )
            | ("B110", "channel.get" | "message.send" | "message.list")
    )
}
async fn run_socket(mut socket: WebSocket, state: HttpState, actor: Actor, _permit: Permit) {
    let expires = tokio::time::Instant::now() + Duration::from_secs(900);
    let mut window = tokio::time::Instant::now();
    let mut messages = 0u16;
    loop {
        if tokio::time::Instant::now() >= expires {
            break;
        }
        let tx = match state.core.begin_read(actor.clone()).await {
            Ok(t) => t,
            Err(_) => break,
        };
        if tx.require_operation("B108", "socket.open").is_err() || tx.commit().await.is_err() {
            break;
        }
        let incoming = match tokio::time::timeout(Duration::from_secs(2), socket.recv()).await {
            Err(_) => {
                continue;
            }
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        if window.elapsed() >= Duration::from_secs(60) {
            window = tokio::time::Instant::now();
            messages = 0;
        }
        messages += 1;
        if messages > 120 {
            break;
        }
        let result = match incoming {
            Message::Text(text) => {
                let parsed: AppResult<OperationRequest> = serde_json::from_str(&text)
                    .map_err(|_| AppError::invalid("invalid_socket_message"));
                match parsed {
                    Ok(r) if allowed(&r) => {
                        state
                            .operations
                            .dispatch_socket(&state.core, actor.clone(), r)
                            .await
                    }
                    Ok(_) => Err(AppError::Forbidden),
                    Err(e) => Err(e),
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => continue,
            _ => break,
        };
        let response = match result {
            Ok(value) => json!({"ok":true,"result":value}),
            Err(AppError::Unauthorized) => break,
            Err(error) => json!({"ok":false,"error":{"code":error.code()}}),
        };
        let value = match serde_json::to_string(&response) {
            Ok(v) if v.len() <= 65536 => v,
            _ => break,
        };
        if !matches!(
            tokio::time::timeout(
                Duration::from_secs(5),
                socket.send(Message::Text(value.into())),
            )
            .await,
            Ok(Ok(()))
        ) {
            break;
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
}
