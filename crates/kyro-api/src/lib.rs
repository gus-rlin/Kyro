#![forbid(unsafe_code)]

use std::{
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, MatchedPath, Request, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::get,
};
use kyro_domain::{Config, Environment, Error};
use kyro_gateway::{Gateway, GatewayConfig};
use kyro_store::Store;
use serde::Serialize;
use tokio::{sync::Semaphore, time};
use tracing::Instrument;
use uuid::Uuid;

pub mod budgets;
pub mod error;
pub mod events;
pub mod identity;
pub mod jobs;
pub mod projects;

use error::ApiError;
use identity::AuthConfig;

const MAX_HTTP_BODY_BYTES: usize = 256 * 1024;
const MAX_IN_FLIGHT_REQUESTS: usize = 128;
const MAX_SSE_CONNECTIONS: usize = 64;
const REQUEST_DEADLINE: time::Duration = time::Duration::from_secs(30);
const HTTP_METHOD_LABELS: [&str; 8] = [
    "GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "OTHER",
];
const HTTP_STATUS_LABELS: [&str; 6] = ["1xx", "2xx", "3xx", "4xx", "5xx", "other"];
const HTTP_LATENCY_BUCKETS: [(u64, &str); 11] = [
    (5_000_000, "0.005"),
    (10_000_000, "0.01"),
    (25_000_000, "0.025"),
    (50_000_000, "0.05"),
    (100_000_000, "0.1"),
    (250_000_000, "0.25"),
    (500_000_000, "0.5"),
    (1_000_000_000, "1"),
    (2_500_000_000, "2.5"),
    (5_000_000_000, "5"),
    (10_000_000_000, "10"),
];

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub config: Arc<Config>,
    pub auth: Arc<AuthConfig>,
    pub gateway: Arc<Gateway>,
    pub http: Arc<HttpControls>,
}

impl AppState {
    pub fn new(
        store: Store,
        config: Arc<Config>,
        auth: Arc<AuthConfig>,
        gateway: Arc<Gateway>,
    ) -> Self {
        Self {
            store,
            config,
            auth,
            gateway,
            http: Arc::new(HttpControls::default()),
        }
    }
}

pub struct HttpControls {
    request_permits: Arc<Semaphore>,
    sse_permits: Arc<Semaphore>,
    metrics: HttpMetrics,
}

impl Default for HttpControls {
    fn default() -> Self {
        Self {
            request_permits: Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS)),
            sse_permits: Arc::new(Semaphore::new(MAX_SSE_CONNECTIONS)),
            metrics: HttpMetrics::default(),
        }
    }
}

struct HttpMetrics {
    requests: [AtomicU64; HTTP_METHOD_LABELS.len() * HTTP_STATUS_LABELS.len()],
    latency_buckets: [AtomicU64; HTTP_LATENCY_BUCKETS.len()],
    latency_count: AtomicU64,
    latency_sum_nanos: AtomicU64,
}

impl Default for HttpMetrics {
    fn default() -> Self {
        Self {
            requests: std::array::from_fn(|_| AtomicU64::new(0)),
            latency_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            latency_count: AtomicU64::new(0),
            latency_sum_nanos: AtomicU64::new(0),
        }
    }
}

impl HttpMetrics {
    fn observe(&self, method: &axum::http::Method, status: StatusCode, elapsed_nanos: u64) {
        let method_index = method_label_index(method);
        let status_index = status_label_index(status);
        self.requests[method_index * HTTP_STATUS_LABELS.len() + status_index]
            .fetch_add(1, Ordering::Relaxed);
        for (index, (bound, _)) in HTTP_LATENCY_BUCKETS.iter().enumerate() {
            if elapsed_nanos <= *bound {
                self.latency_buckets[index].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        self.latency_sum_nanos
            .fetch_add(elapsed_nanos, Ordering::Relaxed);
    }

    fn render_prometheus(&self) -> String {
        let mut output = String::with_capacity(2048);
        let _ = writeln!(
            output,
            "# HELP kyro_http_requests_total HTTP responses that produced headers."
        );
        let _ = writeln!(output, "# TYPE kyro_http_requests_total counter");
        for (method_index, method) in HTTP_METHOD_LABELS.iter().enumerate() {
            for (status_index, status) in HTTP_STATUS_LABELS.iter().enumerate() {
                let value = self.requests[method_index * HTTP_STATUS_LABELS.len() + status_index]
                    .load(Ordering::Relaxed);
                let _ = writeln!(
                    output,
                    "kyro_http_requests_total{{method=\"{method}\",status_class=\"{status}\"}} {value}"
                );
            }
        }

        let _ = writeln!(
            output,
            "# HELP kyro_http_response_headers_seconds Time until the response headers are produced."
        );
        let _ = writeln!(
            output,
            "# TYPE kyro_http_response_headers_seconds histogram"
        );
        for (index, (_, bound)) in HTTP_LATENCY_BUCKETS.iter().enumerate() {
            let value = self.latency_buckets[index].load(Ordering::Relaxed);
            let _ = writeln!(
                output,
                "kyro_http_response_headers_seconds_bucket{{le=\"{bound}\"}} {value}"
            );
        }
        let count = self.latency_count.load(Ordering::Relaxed);
        let sum_seconds = self.latency_sum_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0;
        let _ = writeln!(
            output,
            "kyro_http_response_headers_seconds_bucket{{le=\"+Inf\"}} {count}"
        );
        let _ = writeln!(
            output,
            "kyro_http_response_headers_seconds_sum {sum_seconds:.9}"
        );
        let _ = writeln!(output, "kyro_http_response_headers_seconds_count {count}");
        output
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Clone)]
struct RequestId(String);

pub fn router(state: AppState) -> Router {
    let body_limit = state.config.max_body_bytes.min(MAX_HTTP_BODY_BYTES);
    let normal_routes = Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/metrics", get(metrics))
        .merge(identity::routes())
        .merge(projects::routes())
        .merge(jobs::routes())
        .merge(budgets::routes())
        .route_layer(middleware::from_fn_with_state(
            state.http.clone(),
            limit_in_flight_requests,
        ))
        .route_layer(middleware::from_fn(request_deadline));
    Router::new()
        .merge(normal_routes)
        .merge(events::routes())
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(middleware::from_fn_with_state(
            state.http.clone(),
            observe_request,
        ))
        .layer(middleware::from_fn(attach_request_id))
        .with_state(state)
}

async fn health_live() -> Json<HealthResponse> {
    Json(HealthResponse { status: "live" })
}

async fn health_ready(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    state.store.check_ready().await.map_err(ApiError::from)?;
    Ok(Json(HealthResponse { status: "ready" }))
}

async fn metrics(State(state): State<AppState>) -> Response {
    metrics_response(&state.http.metrics)
}

fn metrics_response(metrics: &HttpMetrics) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics.render_prometheus(),
    )
        .into_response()
}

async fn request_deadline(request: Request<Body>, next: Next) -> Response {
    match time::timeout(REQUEST_DEADLINE, next.run(request)).await {
        Ok(response) => response,
        Err(_) => ApiError::request_timeout().into_response(),
    }
}

async fn limit_in_flight_requests(
    State(controls): State<Arc<HttpControls>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Ok(_permit) = controls.request_permits.clone().try_acquire_owned() else {
        return ApiError::capacity_limited().into_response();
    };
    next.run(request).await
}

async fn attach_request_id(mut request: Request<Body>, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-request-id"), value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn observe_request(
    State(controls): State<Arc<HttpControls>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_else(|| "unavailable".to_owned());
    let method = safe_http_method(request.method()).to_owned();
    let observed_method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str())
        .unwrap_or("unmatched")
        .to_owned();
    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %method,
        route = %route,
        status = tracing::field::Empty,
        latency_ms = tracing::field::Empty,
    );
    let started = Instant::now();
    let response = next.run(request).instrument(span.clone()).await;
    let status = response.status();
    let elapsed = started.elapsed();
    let latency_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let elapsed_nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
    controls
        .metrics
        .observe(&observed_method, status, elapsed_nanos);
    span.record("status", status.as_u16());
    span.record("latency_ms", latency_ms);
    tracing::info!(
        parent: &span,
        status = status.as_u16(),
        latency_ms,
        "http_response"
    );
    response
}

fn safe_http_method(method: &axum::http::Method) -> &'static str {
    match method.as_str() {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "POST" => "POST",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "OPTIONS" => "OPTIONS",
        "PATCH" => "PATCH",
        "TRACE" => "TRACE",
        "CONNECT" => "CONNECT",
        _ => "OTHER",
    }
}

fn method_label_index(method: &axum::http::Method) -> usize {
    match safe_http_method(method) {
        "GET" => 0,
        "HEAD" => 1,
        "POST" => 2,
        "PUT" => 3,
        "DELETE" => 4,
        "PATCH" => 5,
        "OPTIONS" => 6,
        _ => 7,
    }
}

fn status_label_index(status: StatusCode) -> usize {
    match status.as_u16() / 100 {
        1 => 0,
        2 => 1,
        3 => 2,
        4 => 3,
        5 => 4,
        _ => 5,
    }
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        let terminate = async {
            if let Ok(mut signal) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                let _ = signal.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn connect_store(config: &Config) -> Result<Store, Error> {
    Ok(Store::connect(&config.database_url, config.max_connections)
        .await?
        .with_environment(config.environment))
}

pub fn gateway_config(environment: Environment) -> Result<GatewayConfig, Error> {
    GatewayConfig::for_admission_from_env(environment)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::{Method, StatusCode};

    use super::{HttpControls, HttpMetrics, safe_http_method};

    #[test]
    fn observability_normalizes_unrecognized_client_methods() {
        let method = Method::from_bytes(b"X-CUSTOM-CLIENT-METHOD").expect("valid extension method");
        assert_eq!(safe_http_method(&method), "OTHER");
        assert_eq!(safe_http_method(&Method::POST), "POST");
    }

    #[test]
    fn prometheus_metrics_are_emitted_with_only_bounded_labels() {
        let metrics = HttpMetrics::default();
        metrics.observe(&Method::GET, StatusCode::OK, 20_000_000);
        metrics.observe(&Method::POST, StatusCode::TOO_MANY_REQUESTS, 300_000_000);
        for code in [100, 300, 400, 500] {
            let status = StatusCode::from_u16(code).expect("valid status class example");
            metrics.observe(&Method::GET, status, 1_000_000);
        }
        let hostile_method = Method::from_bytes(b"X-PROJECT-550e8400-e29b-41d4-a716-446655440000")
            .expect("synthetic extension method token");
        metrics.observe(
            &hostile_method,
            StatusCode::from_u16(599).unwrap(),
            50_000_000,
        );

        let output = metrics.render_prometheus();
        assert!(output.contains("kyro_http_requests_total{method=\"GET\",status_class=\"2xx\"} 1"));
        assert!(
            output.contains("kyro_http_requests_total{method=\"POST\",status_class=\"4xx\"} 1")
        );
        assert!(output.contains("kyro_http_response_headers_seconds_bucket{le=\"0.025\"} 5"));
        assert!(
            output.contains("kyro_http_requests_total{method=\"OTHER\",status_class=\"5xx\"} 1")
        );
        assert!(output.contains("kyro_http_response_headers_seconds_count 7"));
        for class in ["1xx", "2xx", "3xx", "4xx", "5xx"] {
            assert!(output.contains(&format!("status_class=\"{class}\"")));
        }
        assert!(!output.contains("route="));
        assert!(!output.contains("project_id"));
        assert!(!output.contains("X-PROJECT-550e8400-e29b-41d4-a716-446655440000"));
        assert!(!output.contains("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[tokio::test]
    async fn metrics_route_returns_prometheus_text() {
        use axum::{body::to_bytes, http::header};

        let controls = Arc::new(HttpControls::default());
        controls
            .metrics
            .observe(&Method::GET, StatusCode::OK, 15_000_000);

        let response = super::metrics_response(&controls.metrics);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let body = to_bytes(response.into_body(), 8192)
            .await
            .expect("bounded metrics body");
        let body = String::from_utf8(body.to_vec()).expect("metrics are UTF-8");
        assert!(body.contains("kyro_http_requests_total"));
        assert!(body.contains("status_class=\"2xx\""));
        assert!(body.contains("kyro_http_response_headers_seconds_count 1"));
    }
}
