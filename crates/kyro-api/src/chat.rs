//! Private provisional output; reconnecting never launches inference.
use crate::{
    AppState,
    error::{ApiError, ApiPath, ApiQuery},
    identity::{self, AuthActor},
};
use axum::{
    Router,
    extract::State,
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
};
use futures_util::stream;
use kyro_domain::{
    model::ModelPurpose,
    task::{JobPayloadSummary, JobResult},
};
use serde::Deserialize;
use serde_json::json;
use std::{collections::VecDeque, convert::Infallible, time::Duration};
use tokio::sync::OwnedSemaphorePermit;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/projects/{project_id}/jobs/{job_id}/stream",
        get(stream_chat),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    after: Option<String>,
}
struct StreamState {
    app: AppState,
    actor: AuthActor,
    project: Uuid,
    job: Uuid,
    generation: i64,
    cursor: i64,
    pending: VecDeque<Event>,
    closed: bool,
    _permit: OwnedSemaphorePermit,
}

fn cursor(value: Option<&str>, job: Uuid, generation: i64) -> Result<i64, ApiError> {
    let Some(value) = value else {
        return Ok(0);
    };
    if value.len() > 100 {
        return Err(ApiError::bad_request());
    }
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[0] != job.to_string()
        || parts[1].parse::<i64>().ok() != Some(generation)
    {
        return Err(ApiError::bad_request());
    }
    parts[2]
        .parse::<i64>()
        .ok()
        .filter(|n| (0..=1024).contains(n))
        .ok_or_else(ApiError::bad_request)
}

async fn stream_chat(
    State(app): State<AppState>,
    actor: AuthActor,
    ApiPath((project, job)): ApiPath<(Uuid, Uuid)>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<Query>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let permit = app
        .http
        .sse_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::capacity_limited())?;
    identity::authorize_project(&app, &actor, project, "read").await?;
    let current = app
        .store
        .get_job(actor.actor_id, project, job)
        .await
        .map_err(ApiError::from)?;
    if current.actor_id != actor.actor_id
        || !matches!(
            current.payload,
            JobPayloadSummary::ModelCall {
                purpose: ModelPurpose::Conversation,
                ..
            }
        )
    {
        return Err(ApiError::not_found());
    }
    let after = headers
        .get("last-event-id")
        .map(|h| h.to_str().map_err(|_| ApiError::bad_request()))
        .transpose()?
        .or(query.after.as_deref());
    let cursor = cursor(after, job, current.generation)?;
    let state = StreamState {
        app,
        actor,
        project,
        job,
        generation: current.generation,
        cursor,
        pending: VecDeque::new(),
        closed: false,
        _permit: permit,
    };
    Ok(Sse::new(stream::unfold(state, next)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

async fn next(mut state: StreamState) -> Option<(Result<Event, Infallible>, StreamState)> {
    loop {
        if state.closed {
            return None;
        }
        state.actor = identity::revalidate_actor(&state.app, &state.actor)
            .await
            .ok()?;
        identity::authorize_project(&state.app, &state.actor, state.project, "read")
            .await
            .ok()?;
        if let Some(event) = state.pending.pop_front() {
            return Some((Ok(event), state));
        }
        let job = state
            .app
            .store
            .get_job(state.actor.actor_id, state.project, state.job)
            .await
            .ok()?;
        if job.actor_id != state.actor.actor_id {
            return None;
        }
        if job.generation != state.generation {
            if state.cursor != 0 {
                return None;
            }
            state.generation = job.generation;
        }
        let chunks = state
            .app
            .store
            .read_chat_chunks(
                state.actor.actor_id,
                state.project,
                state.job,
                state.generation,
                state.cursor,
            )
            .await
            .ok()?;
        if !chunks.is_empty() {
            for chunk in chunks {
                if chunk.index != state.cursor + 1 {
                    return None;
                }
                state.cursor = chunk.index;
                state.pending.push_back(
                    Event::default()
                        .event("delta")
                        .id(format!(
                            "{}:{}:{}",
                            state.job, state.generation, chunk.index
                        ))
                        .data(json!({"text":chunk.text}).to_string()),
                );
            }
            continue;
        }
        if job.status.is_terminal() {
            let mut data = json!({"status":job.status,"error_code":job.error_code});
            let effect_id = if let Some(JobResult::ModelCall { effect_id, .. }) = job.result {
                Some(effect_id)
            } else {
                state
                    .app
                    .store
                    .chat_effect_id(state.actor.actor_id, state.project, state.job)
                    .await
                    .ok()?
            };
            // Cancellation intentionally clears Job.result. Its accounting effect may still be unknown.
            if let Some(effect_id) = effect_id {
                let effect = state
                    .app
                    .store
                    .get_effect(state.actor.actor_id, state.project, effect_id)
                    .await
                    .ok()?;
                data["effect_status"] = json!(effect.status);
                if let Some(response) = effect.result {
                    data["output"] = response.output.data;
                    data["usage"] = json!(response.usage);
                }
            }
            state.closed = true;
            return Some((
                Ok(Event::default().event("complete").data(data.to_string())),
                state,
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_is_job_and_generation_scoped() {
        let job = Uuid::new_v4();
        assert_eq!(cursor(None, job, 1).unwrap(), 0);
        assert_eq!(cursor(Some(&format!("{job}:1:3")), job, 1).unwrap(), 3);
        assert!(cursor(Some(&format!("{job}:2:3")), job, 1).is_err());
        assert!(cursor(Some(&format!("{job}:1:-1")), job, 1).is_err());
    }
}
