//! Flux SSE privé, reprenable et borné par connexion.

use std::{collections::VecDeque, convert::Infallible, time::Duration};

use axum::{
    Router,
    extract::State,
    http::HeaderMap,
    response::{
        IntoResponse,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::get,
};
use futures_util::stream;
use kyro_domain::Event;
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedSemaphorePermit;
use uuid::Uuid;

use crate::{
    AppState,
    error::{ApiError, ApiPath, ApiQuery},
    identity::{self, AuthActor},
};

const EVENT_BATCH_SIZE: usize = 100;
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/projects/{project_id}/events", get(stream_events))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsQuery {
    after: Option<String>,
}

#[derive(Serialize)]
struct PublicEvent<'a> {
    sequence: i64,
    kind: &'a str,
    payload: &'a serde_json::Value,
    created_at: &'a chrono::DateTime<chrono::Utc>,
}

struct StreamState {
    app: AppState,
    actor: AuthActor,
    project_id: Uuid,
    cursor: i64,
    pending: VecDeque<Event>,
    closed: bool,
    _permit: OwnedSemaphorePermit,
}

async fn stream_events(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<EventsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let cursor = parse_initial_cursor(project_id, &headers, query.after.as_deref())?;
    identity::authorize_project(&state, &actor, project_id, "read").await?;
    let page = state
        .store
        .read_event_page(
            actor.actor_id,
            project_id,
            cursor,
            i64::try_from(EVENT_BATCH_SIZE).map_err(|_| ApiError::internal())?,
        )
        .await
        .map_err(ApiError::from)?;
    validate_cursor_bounds(cursor, page.earliest_retained, page.latest_sequence)?;
    if page.events.len() > EVENT_BATCH_SIZE {
        return Err(ApiError::internal());
    }
    let permit = state
        .http
        .sse_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::capacity_limited())?;

    let stream_state = StreamState {
        app: state,
        actor,
        project_id,
        cursor,
        pending: page.events.into(),
        closed: false,
        _permit: permit,
    };
    let events = stream::unfold(stream_state, next_event);

    Ok(Sse::new(events).keep_alive(
        KeepAlive::new()
            .interval(KEEP_ALIVE_INTERVAL)
            .text("keep-alive"),
    ))
}

async fn next_event(mut state: StreamState) -> Option<(Result<SseEvent, Infallible>, StreamState)> {
    if state.closed {
        return None;
    }
    loop {
        // Recheck both session and project grant before every poll or buffered event.
        state.actor = identity::revalidate_actor(&state.app, &state.actor)
            .await
            .ok()?;
        identity::authorize_project(&state.app, &state.actor, state.project_id, "read")
            .await
            .ok()?;

        if let Some(event) = state.pending.pop_front() {
            if event.project_id != state.project_id
                || !event_sequence_follows_cursor(state.cursor, event.sequence)
            {
                // An event outside the project or behind the cursor indicates an inconsistent
                // store result. Stop rather than duplicate or mis-scope an event.
                let payload = r#"{"code":"event_history_expired","snapshot_required":true}"#;
                let reset = SseEvent::default().event("reset-required").data(payload);
                state.closed = true;
                return Some((Ok(reset), state));
            }
            state.cursor = event.sequence;
            let data = serde_json::to_string(&PublicEvent {
                sequence: event.sequence,
                kind: &event.kind,
                payload: &event.payload,
                created_at: &event.created_at,
            })
            .ok()?;
            let id = format!("{}:{}", state.project_id, event.sequence);
            let output = SseEvent::default().event("project-event").id(id).data(data);
            return Some((Ok(output), state));
        }

        // Bounds and visible rows come from one DB snapshot. A project sequence is
        // global, so hidden events can create gaps without indicating a purge.
        let page = state
            .app
            .store
            .read_event_page(
                state.actor.actor_id,
                state.project_id,
                state.cursor,
                i64::try_from(EVENT_BATCH_SIZE).ok()?,
            )
            .await
            .ok()?;
        if page.events.len() > EVENT_BATCH_SIZE {
            return None;
        }
        let reset_code =
            if cursor_history_expired(state.cursor, page.earliest_retained, page.latest_sequence) {
                Some("event_history_expired")
            } else if cursor_is_future(state.cursor, page.latest_sequence) {
                Some("event_cursor_future")
            } else {
                None
            };
        if let Some(code) = reset_code {
            let payload = format!(r#"{{"code":"{code}","snapshot_required":true}}"#);
            let reset = SseEvent::default().event("reset-required").data(payload);
            state.closed = true;
            return Some((Ok(reset), state));
        }
        state.pending = page.events.into();
        if state.pending.is_empty() {
            tokio::time::sleep(POLL_INTERVAL).await;
            return Some((Ok(SseEvent::default().comment("poll")), state));
        }
    }
}

fn validate_cursor_bounds(
    cursor: i64,
    earliest_retained: Option<i64>,
    latest_sequence: i64,
) -> Result<(), ApiError> {
    if cursor_is_future(cursor, latest_sequence) {
        return Err(ApiError::event_cursor_future());
    }
    if cursor_history_expired(cursor, earliest_retained, latest_sequence) {
        return Err(ApiError::event_history_expired());
    }
    Ok(())
}

fn cursor_is_future(cursor: i64, project_sequence: i64) -> bool {
    cursor > project_sequence
}

fn cursor_history_expired(
    cursor: i64,
    earliest_retained: Option<i64>,
    latest_sequence: i64,
) -> bool {
    match earliest_retained {
        Some(first) => cursor < first.saturating_sub(1),
        None => cursor < latest_sequence,
    }
}

fn event_sequence_follows_cursor(cursor: i64, event_sequence: i64) -> bool {
    event_sequence > cursor
}

fn parse_initial_cursor(
    project_id: Uuid,
    headers: &HeaderMap,
    query_cursor: Option<&str>,
) -> Result<i64, ApiError> {
    let mut values = headers.get_all("last-event-id").iter();
    let header_cursor = values.next().map(|value| {
        value
            .to_str()
            .map_err(|_| ApiError::event_cursor_invalid())
            .and_then(|value| parse_cursor(project_id, value))
    });
    if values.next().is_some() {
        return Err(ApiError::event_cursor_invalid());
    }
    let header_cursor = header_cursor.transpose()?;
    let query_cursor = query_cursor
        .map(|value| parse_cursor(project_id, value))
        .transpose()?;

    match (header_cursor, query_cursor) {
        (Some(header), Some(query)) if header != query => Err(ApiError::event_cursor_invalid()),
        (Some(header), _) => Ok(header),
        (_, Some(query)) => Ok(query),
        (None, None) => Ok(0),
    }
}

fn parse_cursor(project_id: Uuid, value: &str) -> Result<i64, ApiError> {
    if value.len() > 64 {
        return Err(ApiError::event_cursor_invalid());
    }
    let (cursor_project, sequence) = value
        .split_once(':')
        .ok_or_else(ApiError::event_cursor_invalid)?;
    let cursor_project =
        Uuid::parse_str(cursor_project).map_err(|_| ApiError::event_cursor_invalid())?;
    if cursor_project != project_id {
        return Err(ApiError::event_cursor_project_mismatch());
    }
    if sequence.is_empty() || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ApiError::event_cursor_invalid());
    }
    sequence
        .parse::<i64>()
        .ok()
        .filter(|sequence| *sequence >= 0)
        .ok_or_else(ApiError::event_cursor_invalid)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use uuid::Uuid;

    use super::{
        cursor_history_expired, cursor_is_future, event_sequence_follows_cursor, parse_cursor,
        parse_initial_cursor, validate_cursor_bounds,
    };

    #[test]
    fn cursor_is_project_scoped_and_parses_nonnegative_sequence() {
        let project = Uuid::new_v4();
        assert_eq!(parse_cursor(project, &format!("{project}:0")).unwrap(), 0);
        assert_eq!(parse_cursor(project, &format!("{project}:9")).unwrap(), 9);
        assert_eq!(
            parse_cursor(project, &format!("{}:9", Uuid::new_v4()))
                .unwrap_err()
                .status(),
            axum::http::StatusCode::CONFLICT
        );
        assert!(parse_cursor(project, &format!("{project}:-1")).is_err());
        assert!(parse_cursor(project, "bad:1").is_err());
    }

    #[test]
    fn header_and_query_cursors_must_match() {
        let project = Uuid::new_v4();
        let mut headers = HeaderMap::new();
        headers.insert(
            "last-event-id",
            HeaderValue::from_str(&format!("{project}:4")).unwrap(),
        );
        assert_eq!(
            parse_initial_cursor(project, &headers, Some(&format!("{project}:4"))).unwrap(),
            4
        );
        assert!(parse_initial_cursor(project, &headers, Some(&format!("{project}:5"))).is_err());
        assert_eq!(
            parse_initial_cursor(project, &HeaderMap::new(), None).unwrap(),
            0
        );
    }

    #[test]
    fn physical_bounds_detect_pruned_prefix_and_entire_history() {
        assert!(cursor_history_expired(3, Some(5), 8));
        assert!(!cursor_history_expired(4, Some(5), 8));
        assert!(!cursor_history_expired(0, Some(1), 8));
        assert!(cursor_history_expired(7, None, 8));
        assert!(!cursor_history_expired(8, None, 8));
        assert!(cursor_is_future(9, 8));
    }

    #[test]
    fn snapshot_and_hidden_only_sequences_are_valid_cursors() {
        // An event from another environment can advance the global sequence without
        // adding a row to this environment's visible event stream.
        assert!(!cursor_history_expired(10, Some(1), 11));
        assert!(!cursor_is_future(10, 11));

        // A project snapshot carries the global high-water mark, which may be newer
        // than this runtime environment's last visible event.
        assert!(!cursor_history_expired(11, Some(1), 11));
        assert!(!cursor_is_future(11, 11));

        // Hidden-only rows remain in physical history, so no visible events is
        // different from a globally purged event log.
        assert!(!cursor_history_expired(11, Some(9), 11));
    }

    #[test]
    fn initial_cursor_uses_atomic_physical_bounds_and_global_sequence() {
        assert!(validate_cursor_bounds(8, Some(5), 8).is_ok());
        assert_eq!(
            validate_cursor_bounds(3, Some(5), 8).unwrap_err().status(),
            axum::http::StatusCode::GONE
        );
        assert_eq!(
            validate_cursor_bounds(9, Some(5), 8).unwrap_err().status(),
            axum::http::StatusCode::CONFLICT
        );
        assert_eq!(
            validate_cursor_bounds(4, None, 8).unwrap_err().status(),
            axum::http::StatusCode::GONE
        );
        assert!(validate_cursor_bounds(8, None, 8).is_ok());
    }

    #[test]
    fn visible_event_sequences_may_skip_events_from_other_environments() {
        assert!(event_sequence_follows_cursor(4, 7));
        assert!(!event_sequence_follows_cursor(7, 7));
        assert!(!event_sequence_follows_cursor(7, 6));
    }
}
