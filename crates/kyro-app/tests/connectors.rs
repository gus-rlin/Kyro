#![cfg(feature = "test-support")]
mod support;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    response::Response,
};
use base64::Engine;
use chrono::Utc;
use kyro_app::{
    Actor, AppError, OperationDispatcher, OperationRequest,
    connectors::{ConnectorService, McpTool, OAuthProvider, Profile, Provider, RestOperation},
    contract::Schema,
    jobs::JobClaim,
    vault::{SecretBinding, SecretVault},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use support::*;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone)]
struct Observed {
    path: String,
    method: String,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}
#[derive(Clone, Default)]
struct ProviderState {
    calls: Arc<Mutex<Vec<Observed>>>,
    tool: Arc<Mutex<Option<Value>>>,
    gate: Arc<tokio::sync::Notify>,
    arrived: Arc<tokio::sync::Notify>,
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    wrong_amount: Arc<std::sync::atomic::AtomicBool>,
    wrong_scope: Arc<std::sync::atomic::AtomicBool>,
    expanded_on_refresh: Arc<std::sync::atomic::AtomicBool>,
    authorization_codes: Arc<Mutex<BTreeMap<String, (String, String)>>>,
    refresh: Arc<Mutex<Option<String>>>,
    token_counter: Arc<std::sync::atomic::AtomicUsize>,
    calendar_events: Arc<Mutex<BTreeMap<String, Value>>>,
    calendar_partial: Arc<std::sync::atomic::AtomicBool>,
    calendar_counter: Arc<std::sync::atomic::AtomicUsize>,
    calendar_gate: Arc<std::sync::atomic::AtomicBool>,
}
async fn provider(State(state): State<ProviderState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 1048576).await.unwrap();
    let path = parts.uri.path().to_owned();
    state.calls.lock().await.push(Observed {
        path: parts.uri.to_string(),
        method: parts.method.to_string(),
        headers: parts.headers.clone(),
        body: body.to_vec(),
    });
    if path == "/slow" {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    if path == "/gate" {
        state.arrived.notify_one();
        state.gate.notified().await;
    }
    if path == "/redirect" {
        return Response::builder()
            .status(302)
            .header("location", "/goods")
            .body(Body::empty())
            .unwrap();
    }
    if path.starts_with("/calendar/v3/calendars/") {
        let mut events = state.calendar_events.lock().await;
        let value = if parts.method == "GET" {
            json!({"items":events.values().cloned().collect::<Vec<_>>(),"timeZone":"UTC","nextPageToken":if state.calendar_partial.load(std::sync::atomic::Ordering::SeqCst){Some("synthetic-page-two")}else{None}})
        } else {
            let event = path.rsplit('/').next().unwrap();
            if parts.method == "PUT" || parts.method == "DELETE" {
                let previous = events.get(event).unwrap();
                if parts.headers["if-match"].to_str().unwrap() != previous["etag"].as_str().unwrap()
                {
                    return Response::builder().status(412).body(Body::empty()).unwrap();
                }
            }
            if parts.method == "DELETE" {
                events.remove(event);
                return Response::builder().status(204).body(Body::empty()).unwrap();
            }
            let mut value: Value = serde_json::from_slice(&body).unwrap();
            let version = state
                .calendar_counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            value["etag"] = json!(format!("\"synthetic-etag-{version}\""));
            events.insert(value["id"].as_str().unwrap().into(), value.clone());
            value
        };
        drop(events);
        if parts.method == "GET"
            && state
                .calendar_gate
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            state.arrived.notify_one();
            state.gate.notified().await;
        }
        return Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap();
    }
    if path.starts_with("/synthetic-bucket/") {
        let mut objects = state.objects.lock().await;
        return match parts.method.as_str() {
            "PUT" if objects.contains_key(&path) => {
                Response::builder().status(412).body(Body::empty()).unwrap()
            }
            "PUT" => {
                assert_eq!(parts.headers["if-none-match"], "*");
                objects.insert(path, body.to_vec());
                Response::new(Body::empty())
            }
            "GET" => match objects.get(&path) {
                Some(b) => Response::new(Body::from(b.clone())),
                None => Response::builder().status(404).body(Body::empty()).unwrap(),
            },
            "DELETE" => {
                objects.remove(&path);
                Response::builder().status(204).body(Body::empty()).unwrap()
            }
            _ => panic!("unexpected synthetic object operation"),
        };
    }
    if path == "/authorize" {
        let query: BTreeMap<_, _> =
            url::form_urlencoded::parse(parts.uri.query().unwrap().as_bytes())
                .into_owned()
                .collect();
        assert_eq!(query["code_challenge_method"], "S256");
        let code = format!("synthetic-code-{}", Uuid::new_v4());
        state.authorization_codes.lock().await.insert(
            code.clone(),
            (
                query["code_challenge"].clone(),
                query["redirect_uri"].clone(),
            ),
        );
        let mut redirect = url::Url::parse(&query["redirect_uri"]).unwrap();
        redirect
            .query_pairs_mut()
            .append_pair("code", &code)
            .append_pair("state", &query["state"])
            .append_pair("iss", "https://issuer.example.test");
        return Response::builder()
            .status(302)
            .header("location", redirect.as_str())
            .body(Body::empty())
            .unwrap();
    }
    if path == "/revoke" {
        let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&body).into_owned().collect();
        assert_eq!(Some(&fields["token"]), state.refresh.lock().await.as_ref());
        *state.refresh.lock().await = None;
        return Response::new(Body::empty());
    }
    let value = match path.as_str() {
        "/token" => {
            let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&body).into_owned().collect();
            assert!(
                parts.headers["authorization"]
                    .to_str()
                    .unwrap()
                    .starts_with("Basic ")
            );
            if fields["grant_type"] == "authorization_code" {
                let (challenge, redirect) = state
                    .authorization_codes
                    .lock()
                    .await
                    .remove(&fields["code"])
                    .unwrap();
                use sha2::{Digest, Sha256};
                assert_eq!(
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(Sha256::digest(fields["code_verifier"].as_bytes())),
                    challenge
                );
                assert_eq!(fields["redirect_uri"], redirect);
            } else {
                assert_eq!(fields["grant_type"], "refresh_token");
                assert_eq!(
                    Some(&fields["refresh_token"]),
                    state.refresh.lock().await.as_ref()
                );
            }
            let n = state
                .token_counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let refresh = format!("synthetic-refresh-{n}");
            *state.refresh.lock().await = Some(refresh.clone());
            json!({"access_token":format!("synthetic-access-{n}"),"refresh_token":refresh,"token_type":"Bearer","expires_in":3600,"scope":if state.wrong_scope.load(std::sync::atomic::Ordering::SeqCst){"https://www.googleapis.com/auth/calendar.events admin.root"}else if fields["grant_type"]=="refresh_token" && state.expanded_on_refresh.load(std::sync::atomic::Ordering::SeqCst){"https://www.googleapis.com/auth/calendar.events synthetic.optional"}else{"https://www.googleapis.com/auth/calendar.events"}})
        }
        "/mcp" => {
            let call: Value = serde_json::from_slice(&body).unwrap();
            let result = if call["method"] == "tools/list" {
                json!({"tools":[state.tool.lock().await.clone().unwrap()]})
            } else {
                json!({"structuredContent":{"answer":"synthetic response"},"isError":false})
            };
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from(format!(
                    "data: {}\n\n",
                    json!({"jsonrpc":"2.0","id":call["id"],"result":result})
                )))
                .unwrap();
        }
        "/emails" => json!({"id":"synthetic-email-1"}),
        "/v1/payment_intents" => {
            let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&body).into_owned().collect();
            json!({"id":"pi_synthetic","status":"requires_payment_method","amount":if state.wrong_amount.load(std::sync::atomic::Ordering::SeqCst){1}else{fields["amount"].parse::<i64>().unwrap()},"currency":fields["currency"],"metadata":{"kyro_payment":fields["metadata[kyro_payment]"]}})
        }
        "/v1/subscriptions" => {
            let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&body).into_owned().collect();
            json!({"id":"sub_synthetic","status":"incomplete","customer":if state.wrong_amount.load(std::sync::atomic::Ordering::SeqCst){"cus_foreign"}else{&fields["customer"]},"items":{"data":[{"price":{"id":fields["items[0][price]"]}}]},"metadata":{"kyro_subscription":fields["metadata[kyro_subscription]"]}})
        }
        "/v1/refunds" => {
            let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&body).into_owned().collect();
            json!({"id":"re_synthetic","status":"pending","amount":fields["amount"].parse::<i64>().unwrap(),"currency":"eur","payment_intent":fields["payment_intent"],"metadata":{"kyro_refund":fields["metadata[kyro_refund]"]}})
        }
        "/search" => json!([{"lat":"48.85","lon":"2.35","display_name":"synthetic location"}]),
        "/push" => json!({"id":"synthetic-push-1","event":"message"}),
        p if p.ends_with("/Messages.json") => {
            json!({"sid":"SM00000000000000000000000000000000","status":"queued"})
        }
        p if p.ends_with("/events") => {
            if parts.method == "POST" {
                json!({"id":serde_json::from_slice::<Value>(&body).unwrap()["id"]})
            } else {
                json!({"items":[]})
            }
        }
        _ => json!({"answer":"synthetic response"}),
    };
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}
fn string() -> Schema {
    Schema::String {
        max_length: 256,
        values: BTreeSet::new(),
    }
}
fn object(fields: &[(&str, Schema)], required: &[&str]) -> Schema {
    Schema::Object {
        properties: fields
            .iter()
            .map(|(k, s)| (k.to_string(), s.clone()))
            .collect(),
        required: required.iter().map(|s| s.to_string()).collect(),
        additional: false,
    }
}
fn rest(path: &str) -> Provider {
    Provider::Rest {
        contract_version: "v1".into(),
        operations: BTreeMap::from([(
            "lookup".into(),
            RestOperation {
                method: "POST".into(),
                path: path.into(),
                input: object(&[("name", string())], &["name"]),
                output: object(&[("answer", string())], &["answer"]),
                effect: false,
            },
        )]),
    }
}
struct Harness {
    f: Fixture,
    service: Arc<ConnectorService>,
    dispatcher: OperationDispatcher,
    worker: Actor,
    provider: ProviderState,
    profiles: Vec<Profile>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Harness {
    async fn new(configurations: Vec<Provider>) -> Self {
        let f = Fixture::new().await;
        Self::with_fixture(f, configurations).await
    }
    async fn with_fixture(f: Fixture, configurations: Vec<Provider>) -> Self {
        let provider_state = ProviderState::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .fallback(provider)
            .with_state(provider_state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut profiles = vec![];
        let mut bindings = vec![];
        for mut configuration in configurations {
            if let Provider::OAuth { settings } = &mut configuration {
                settings.authorization_endpoint =
                    format!("http://provider.test:{}/authorize", address.port());
                settings.revocation_endpoint =
                    format!("http://provider.test:{}/revoke", address.port());
            }
            let id = Uuid::new_v4();
            let reference_id = Uuid::new_v4();
            let record_id = Uuid::new_v4();
            let endpoint = format!(
                "http://provider.test:{}{}",
                address.port(),
                match configuration {
                    Provider::Mcp { .. } => "/mcp",
                    Provider::Ntfy { .. } => "/push",
                    Provider::OAuth { .. } => "/token",
                    _ => "",
                }
            );
            let timeout_ms = if matches!(&configuration, Provider::Rest { operations, .. } if operations.values().any(|o|o.path=="/slow"))
            {
                100
            } else {
                1000
            };
            let minimum_interval_ms = if matches!(&configuration, Provider::Nominatim { .. }) {
                1000
            } else {
                0
            };
            let mut profile = Profile {
                tenant_id: f.actor.tenant_id(),
                application_id: f.actor.application_id(),
                id,
                endpoint,
                allowed_hosts: BTreeSet::from(["provider.test".into()]),
                roles: BTreeSet::from(["admin".into()]),
                secret_ref: Some(record_id),
                oauth_provider_id: None,
                required_oauth_scopes: BTreeSet::new(),
                configuration,
                max_request_bytes: 65536,
                max_response_bytes: 65536,
                timeout_ms,
                minimum_interval_ms,
                estimated_units_per_call: 7,
                currency: "EUR".into(),
                unit_scale: 1000000,
                tariff_date: Utc::now().date_naive(),
            };
            if matches!(&profile.configuration, Provider::GoogleCalendar { .. })
                && let Some(provider) = profiles
                    .iter()
                    .find(|p: &&Profile| matches!(p.configuration, Provider::OAuth { .. }))
            {
                profile.oauth_provider_id = Some(provider.id);
                profile.secret_ref = None;
                profile.required_oauth_scopes =
                    BTreeSet::from(["https://www.googleapis.com/auth/calendar.events".into()]);
            }
            let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
            let purposes = if matches!(profile.configuration, Provider::Rest { .. }) {
                BTreeSet::from(["connector.send".to_string(), "webhook.send".to_string()])
            } else {
                BTreeSet::from(["connector.send".to_string()])
            };
            tx.insert("secret_ref", record_id, json!({"adapter_id":id,"vault_reference":reference_id,"purposes":purposes,"revoked":false})).await.unwrap();
            tx.commit().await.unwrap();
            let secret = if matches!(profile.configuration, Provider::S3 { .. }) {
                json!({"access_key":"SYNTHETIC","secret_key":"public-synthetic-only-s3-secret-32"})
                    .to_string()
            } else {
                "public-synthetic-only-connector-secret-32".into()
            };
            bindings.push(SecretBinding {
                tenant_id: profile.tenant_id,
                application_id: profile.application_id,
                adapter_id: id,
                reference_id,
                purposes,
                secret_base64: base64::engine::general_purpose::STANDARD.encode(secret),
            });
            profiles.push(profile);
        }
        let service = ConnectorService::new_for_test(
            profiles.clone(),
            Arc::new(SecretVault::new(bindings).unwrap()),
            [19; 32],
            BTreeMap::from([("provider.test".into(), address)]),
        )
        .unwrap();
        let enabled = BTreeSet::from_iter(
            (11..=60)
                .chain(81..=93)
                .filter(|i| *i != 92)
                .chain(101..=106)
                .chain(111..=140)
                .chain([151, 152, 153, 154, 155, 156, 157, 160])
                .map(|i| format!("B{i:03}")),
        );
        let dispatcher =
            kyro_app::operations::builtins_with_connectors(&enabled, None, None, Some(&service))
                .unwrap();
        let (worker, _) = Fixture::session(
            &f.admin,
            &f.core,
            f.actor.tenant_id(),
            f.actor.application_id(),
            Uuid::new_v4(),
            &["jobs.worker"],
        )
        .await;
        sqlx::query("DELETE FROM app_role_permissions WHERE tenant_id=$1 AND application_id=$2 AND role='jobs.worker'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
        sqlx::query("INSERT INTO app_role_permissions(tenant_id,application_id,role,permission) VALUES($1,$2,'jobs.worker','B054.execute')").bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
        let h = Self {
            f,
            service,
            dispatcher,
            worker,
            provider: provider_state,
            profiles,
            task,
        };
        for p in &h.profiles {
            h.op(
                &h.f.actor,
                component(&p.configuration),
                "adapter.activate",
                json!({"id":p.id}),
                None,
            )
            .await
            .unwrap();
        }
        h
    }
    async fn op(
        &self,
        actor: &Actor,
        component: &str,
        action: &str,
        payload: Value,
        version: Option<i64>,
    ) -> Result<Value, AppError> {
        self.dispatcher
            .dispatch(
                &self.f.core,
                actor.clone(),
                OperationRequest {
                    component_id: component.into(),
                    action: action.into(),
                    payload,
                    expected_version: version,
                    idempotency_key: Uuid::new_v4().to_string(),
                },
            )
            .await
    }
    async fn invoke(&self, index: usize, specification: Value) -> Value {
        self.op(
            &self.f.actor,
            component(&self.profiles[index].configuration),
            "adapter.call",
            json!({"adapter_id":self.profiles[index].id,"specification":specification}),
            None,
        )
        .await
        .unwrap()
    }
    async fn claim(&self) -> JobClaim {
        let v = self
            .op(
                &self.worker,
                "B054",
                "outbox.claim",
                json!({"effects_only":true}),
                None,
            )
            .await
            .unwrap();
        assert_eq!(v["claimed"], true);
        JobClaim {
            id: id(&v),
            lease_id: Uuid::parse_str(v["lease_id"].as_str().unwrap()).unwrap(),
            generation: v["generation"].as_i64().unwrap(),
        }
    }
    async fn send(&self) -> Value {
        kyro_app::connectors::send_claimed(
            &self.f.core,
            self.worker.clone(),
            self.claim().await,
            &self.service,
        )
        .await
        .unwrap()
    }
}
fn component(p: &Provider) -> &'static str {
    match p {
        Provider::Postgres { .. } => "B158",
        Provider::OAuth { .. } => "B156",
        Provider::Rest { .. } => "B157",
        Provider::Mcp { .. } => "B160",
        Provider::S3 { .. } => "B151",
        Provider::Resend { .. } => "B152",
        Provider::Twilio { .. } => "B105",
        Provider::Ntfy { .. } => "B106",
        Provider::Stripe { .. } => "B153",
        Provider::GoogleCalendar { .. } => "B154",
        Provider::Nominatim { .. } => "B155",
    }
}

async fn calendar_booking(h: &Harness) -> (Uuid, Uuid) {
    use chrono::{Datelike, Days, TimeZone};
    let day = Utc::now()
        .date_naive()
        .checked_add_days(Days::new(1))
        .unwrap();
    let establishment = h
        .op(
            &h.f.actor,
            "B111",
            "create_establishment",
            json!({"name":"Synthetic calendar site","timezone":"UTC"}),
            None,
        )
        .await
        .unwrap();
    let resource=h.op(&h.f.actor,"B111","create_resource",json!({"establishment_id":id(&establishment),"category":"service","name":"Synthetic service","capacity":3}),None).await.unwrap();
    let availability=h.op(&h.f.actor,"B112","set_availability",json!({"resource_id":id(&resource),"weekday":day.weekday().num_days_from_monday(),"timezone":"UTC","local_start":"09:00:00","local_end":"18:00:00","valid_from":day,"valid_until":day}),None).await.unwrap();
    let mut slots = vec![];
    for hour in [10, 12] {
        slots.push(h.op(&h.f.actor,"B113","create_slot",json!({"resource_id":id(&resource),"availability_id":id(&availability),"starts_at":Utc.from_utc_datetime(&day.and_hms_opt(hour,0,0).unwrap()),"ends_at":Utc.from_utc_datetime(&day.and_hms_opt(hour+1,0,0).unwrap()),"capacity":3}),None).await.unwrap());
    }
    let booking = h
        .op(
            &h.f.actor,
            "B114",
            "reserve",
            json!({"slot_id":id(&slots[0]),"units":1}),
            None,
        )
        .await
        .unwrap();
    (
        Uuid::parse_str(booking["booking_id"].as_str().unwrap()).unwrap(),
        id(&slots[1]),
    )
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and synthetic HTTP provider"]
async fn scheduling_calendar_bridge_exports_updates_deletes_and_imports_complete_snapshots() {
    let h = Harness::new(vec![Provider::GoogleCalendar {
        calendar_id: "private@example.test".into(),
    }])
    .await;
    let (booking, next_slot) = calendar_booking(&h).await;
    let conn=h.op(&h.f.actor,"B120","connect_calendar",json!({"provider":"google_calendar","account_ref":"Synthetic calendar label","connector_id":h.profiles[0].id,"scopes":["calendar.read","calendar.write"]}),None).await.unwrap();
    let connection = Uuid::parse_str(conn["connection_id"].as_str().unwrap()).unwrap();
    let input = json!({"connection_id":connection,"booking_id":booking});
    let first = h
        .op(
            &h.f.actor,
            "B120",
            "prepare_calendar_export",
            input.clone(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(first["network_called"], false);
    assert_eq!(
        h.op(
            &h.f.actor,
            "B120",
            "prepare_calendar_export",
            input.clone(),
            None
        )
        .await
        .unwrap()["connector_call_id"],
        first["connector_call_id"]
    );
    assert!(
        h.op(
            &h.f.actor,
            "B120",
            "claim_calendar_outbox",
            json!({"connection_id":connection}),
            None
        )
        .await
        .is_err()
    );
    assert!(h.op(&h.f.actor,"B154","adapter.call",json!({"adapter_id":h.profiles[0].id,"specification":{"kind":"calendar_booking_export","outbox_id":first["outbox_id"]}}),None).await.is_err());
    assert_eq!(h.send().await["state"], "delivered");
    assert_eq!(
        h.op(
            &h.f.actor,
            "B154",
            "adapter.result",
            json!({"id":first["connector_call_id"]}),
            None
        )
        .await
        .unwrap()["state"],
        "delivered"
    );
    assert_eq!(
        h.op(
            &h.f.actor,
            "B120",
            "get_calendar_status",
            json!({"connection_id":connection}),
            None
        )
        .await
        .unwrap()["outbox"]["succeeded"],
        1
    );
    let remote = booking.simple().to_string();
    assert_eq!(
        h.provider.calendar_events.lock().await[&remote]["extendedProperties"]["private"]["kyroOrigin"],
        conn["origin_marker"]
    );
    h.op(
        &h.f.actor,
        "B115",
        "reschedule_booking",
        json!({"booking_id":booking,"new_slot_id":next_slot}),
        None,
    )
    .await
    .unwrap();
    h.op(
        &h.f.actor,
        "B120",
        "prepare_calendar_export",
        input.clone(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(h.send().await["state"], "delivered");
    assert_eq!(h.provider.calendar_events.lock().await.len(), 1);
    let day = Utc::now().date_naive() + chrono::Days::new(1);
    let from = chrono::TimeZone::from_utc_datetime(&Utc, &day.and_hms_opt(0, 0, 0).unwrap());
    let until = from + chrono::Duration::days(1);
    h.provider.calendar_events.lock().await.insert("external-synthetic".into(),json!({"id":"external-synthetic","etag":"\"external-v1\"","summary":"Imported synthetic event","start":{"dateTime":from+chrono::Duration::hours(14)},"end":{"dateTime":from+chrono::Duration::hours(15)}}));
    let refresh = json!({"connection_id":connection,"from":from,"until":until});
    h.op(
        &h.f.actor,
        "B120",
        "refresh_calendar",
        refresh.clone(),
        None,
    )
    .await
    .unwrap();
    let read = h.send().await;
    assert_eq!(read["result"]["imported_or_updated"], 1);
    assert_eq!(read["result"]["echoes_ignored"], 1);
    let listed = h
        .op(
            &h.f.actor,
            "B120",
            "list_calendar_events",
            json!({"connection_id":connection,"limit":1}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        listed["items"][0]["external_event_id"],
        "external-synthetic"
    );
    assert_eq!(listed["items"][0]["trusted_as_instruction"], false);
    h.op(
        &h.f.actor,
        "B120",
        "refresh_calendar",
        refresh.clone(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(h.send().await["result"]["imported_or_updated"], 0);
    h.provider
        .calendar_events
        .lock()
        .await
        .remove("external-synthetic");
    h.provider
        .calendar_partial
        .store(true, std::sync::atomic::Ordering::SeqCst);
    h.op(
        &h.f.actor,
        "B120",
        "refresh_calendar",
        refresh.clone(),
        None,
    )
    .await
    .unwrap();
    assert!(
        kyro_app::connectors::send_claimed(
            &h.f.core,
            h.worker.clone(),
            h.claim().await,
            &h.service
        )
        .await
        .is_err()
    );
    assert_eq!(
        h.op(
            &h.f.actor,
            "B120",
            "list_calendar_events",
            json!({"connection_id":connection}),
            None
        )
        .await
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    h.provider
        .calendar_partial
        .store(false, std::sync::atomic::Ordering::SeqCst);
    h.op(&h.f.actor, "B120", "refresh_calendar", refresh, None)
        .await
        .unwrap();
    assert_eq!(h.send().await["result"]["removed"], 1);
    h.op(
        &h.f.actor,
        "B115",
        "cancel_booking",
        json!({"booking_id":booking}),
        None,
    )
    .await
    .unwrap();
    h.op(&h.f.actor, "B120", "prepare_calendar_export", input, None)
        .await
        .unwrap();
    assert_eq!(h.send().await["state"], "delivered");
    assert!(
        !h.provider
            .calendar_events
            .lock()
            .await
            .contains_key(&remote)
    );
    let methods: Vec<_> = h
        .provider
        .calls
        .lock()
        .await
        .iter()
        .filter(|c| c.path.contains("/calendar/"))
        .map(|c| c.method.clone())
        .collect();
    assert_eq!(
        methods,
        vec!["POST", "PUT", "GET", "GET", "GET", "GET", "DELETE"]
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and synthetic HTTP provider"]
async fn calendar_export_refuses_changed_booking_and_revoked_adapter_before_send() {
    let h = Harness::new(vec![Provider::GoogleCalendar {
        calendar_id: "private@example.test".into(),
    }])
    .await;
    let (booking, next) = calendar_booking(&h).await;
    let conn=h.op(&h.f.actor,"B120","connect_calendar",json!({"provider":"google_calendar","account_ref":"Synthetic","connector_id":h.profiles[0].id,"scopes":["calendar.write"]}),None).await.unwrap();
    let input = json!({"connection_id":conn["connection_id"],"booking_id":booking});
    h.op(
        &h.f.actor,
        "B120",
        "prepare_calendar_export",
        input.clone(),
        None,
    )
    .await
    .unwrap();
    h.op(
        &h.f.actor,
        "B115",
        "reschedule_booking",
        json!({"booking_id":booking,"new_slot_id":next}),
        None,
    )
    .await
    .unwrap();
    assert!(
        kyro_app::connectors::send_claimed(
            &h.f.core,
            h.worker.clone(),
            h.claim().await,
            &h.service
        )
        .await
        .is_err()
    );
    assert!(h.provider.calls.lock().await.is_empty());
    assert_eq!(
        h.op(
            &h.f.actor,
            "B120",
            "get_calendar_status",
            json!({"connection_id":conn["connection_id"]}),
            None
        )
        .await
        .unwrap()["outbox"]["unknown"],
        1
    );
    assert!(
        h.op(&h.f.actor, "B120", "prepare_calendar_export", input, None)
            .await
            .is_err()
    );
    let (booking, _) = calendar_booking(&h).await;
    h.op(
        &h.f.actor,
        "B120",
        "prepare_calendar_export",
        json!({"connection_id":conn["connection_id"],"booking_id":booking}),
        None,
    )
    .await
    .unwrap();
    h.op(
        &h.f.actor,
        "B154",
        "adapter.deactivate",
        json!({"id":h.profiles[0].id}),
        Some(1),
    )
    .await
    .unwrap();
    assert!(
        kyro_app::connectors::send_claimed(
            &h.f.core,
            h.worker.clone(),
            h.claim().await,
            &h.service
        )
        .await
        .is_err()
    );
    assert!(h.provider.calls.lock().await.is_empty());
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and synthetic HTTP provider"]
async fn older_calendar_snapshot_cannot_overwrite_a_newer_requested_generation() {
    use chrono::{Days, TimeZone};
    let h = Harness::new(vec![Provider::GoogleCalendar {
        calendar_id: "private@example.test".into(),
    }])
    .await;
    let conn=h.op(&h.f.actor,"B120","connect_calendar",json!({"provider":"google_calendar","account_ref":"Synthetic","connector_id":h.profiles[0].id,"scopes":["calendar.read"]}),None).await.unwrap();
    let day = Utc::now()
        .date_naive()
        .checked_add_days(Days::new(1))
        .unwrap();
    let from = Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap());
    let mut event = json!({"id":"external-synthetic","etag":"\"v1\"","summary":"old snapshot","start":{"dateTime":from+chrono::Duration::hours(8)},"end":{"dateTime":from+chrono::Duration::hours(9)}});
    h.provider
        .calendar_events
        .lock()
        .await
        .insert("external-synthetic".into(), event.clone());
    let input = json!({"connection_id":conn["connection_id"],"from":from,"until":from+chrono::Duration::days(1)});
    h.op(&h.f.actor, "B120", "refresh_calendar", input.clone(), None)
        .await
        .unwrap();
    h.provider
        .calendar_gate
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let claim = h.claim().await;
    let core = h.f.core.clone();
    let worker = h.worker.clone();
    let service = h.service.clone();
    let send = tokio::spawn(async move {
        kyro_app::connectors::send_claimed(&core, worker, claim, &service).await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        h.provider.arrived.notified(),
    )
    .await
    .unwrap();
    h.op(&h.f.actor, "B120", "refresh_calendar", input, None)
        .await
        .unwrap();
    event["etag"] = json!("\"v2\"");
    event["summary"] = json!("new snapshot");
    h.provider
        .calendar_events
        .lock()
        .await
        .insert("external-synthetic".into(), event);
    h.provider
        .calendar_gate
        .store(false, std::sync::atomic::Ordering::SeqCst);
    h.provider.gate.notify_one();
    assert!(send.await.unwrap().is_err());
    assert!(
        h.op(
            &h.f.actor,
            "B120",
            "list_calendar_events",
            json!({"connection_id":conn["connection_id"]}),
            None
        )
        .await
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.send().await["state"], "delivered");
    let listed = h
        .op(
            &h.f.actor,
            "B120",
            "list_calendar_events",
            json!({"connection_id":conn["connection_id"]}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(listed["items"][0]["title"], "new snapshot");
    assert_eq!(listed["items"][0]["remote_version"], "\"v2\"");
    assert_eq!(h.provider.calls.lock().await.len(), 2);
}

async fn message(h: &Harness) -> Value {
    h.op(&h.f.actor,"B036","migrate",json!({"entity":"memo","version":1,"definition":{"fields":{"name":{"type":"string","required":true}}}}),None).await.unwrap();
    h.op(&h.f.actor,"B021","policy.set",json!({"kind":"data.memo","action":"read","owner":true,"roles":["admin"],"fields":{"name":["admin"]}}),None).await.unwrap();
    let record = h
        .op(
            &h.f.actor,
            "B031",
            "create",
            json!({"entity":"memo","values":{"name":"Synthetic <memo>"}}),
            None,
        )
        .await
        .unwrap();
    let template = Uuid::new_v4();
    h.op(&h.f.actor,"B102","message_template.define",json!({"id":template,"definition":{"subject":"Memo {{name}}","body":"Visible {{name}}","variables":{"name":"string"}}}),None).await.unwrap();
    let (approver, _) = Fixture::session(
        &h.f.admin,
        &h.f.core,
        h.f.actor.tenant_id(),
        h.f.actor.application_id(),
        Uuid::new_v4(),
        &["admin"],
    )
    .await;
    h.op(
        &approver,
        "B102",
        "message_template.approve",
        json!({"id":template,"version":1}),
        None,
    )
    .await
    .unwrap();
    json!({"recipient_id":h.f.actor.principal_id(),"source":{"kind":"data.memo","id":id(&record),"version":1},"template_id":template,"template_version":1})
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn email_mobile_push_use_verified_destinations_templates_and_live_preferences() {
    let h = Harness::new(vec![
        Provider::Resend {
            from: "sender@example.test".into(),
        },
        Provider::Twilio {
            account_sid: "AC00000000000000000000000000000000".into(),
            from: "+33600000000".into(),
        },
        Provider::Ntfy {
            topic_prefix: "private".into(),
        },
    ])
    .await;
    let m = message(&h).await;
    sqlx::query("INSERT INTO app_local_credentials(tenant_id,application_id,principal_id,email,email_verified,password_hash) VALUES($1,$2,$3,'recipient@example.test',true,$4)").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).bind(h.f.actor.principal_id()).bind("synthetic-only-not-used-for-authentication-credential-hash").execute(&h.f.admin).await.unwrap();
    let email = json!({"adapter_id":h.profiles[0].id,"message":m});
    assert_eq!(
        h.op(&h.f.actor, "B104", "email.send", email.clone(), None)
            .await,
        Err(AppError::Forbidden)
    );
    h.op(
        &h.f.actor,
        "B103",
        "preferences.set",
        json!({"internal":true,"email":true,"mobile":true,"push":true,"frequency":"immediate"}),
        None,
    )
    .await
    .unwrap();
    h.op(&h.f.actor, "B104", "email.send", email.clone(), None)
        .await
        .unwrap();
    assert_eq!(h.send().await["state"], "delivered");
    let captures = h.provider.calls.lock().await;
    let body: Value = serde_json::from_slice(&captures[0].body).unwrap();
    assert_eq!(body["to"], json!(["recipient@example.test"]));
    assert!(body["html"].as_str().unwrap().contains("&lt;memo&gt;"));
    assert!(
        captures[0].headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer ")
    );
    drop(captures);
    for (index, block, phone) in [(1, "B105", Some("+33600000001")), (2, "B106", None)] {
        let mut payload = json!({"adapter_id":h.profiles[index].id});
        if let Some(p) = phone {
            payload["phone"] = json!(p);
        }
        let registered = h
            .op(&h.f.actor, block, "endpoint.register", payload, None)
            .await
            .unwrap();
        assert!(registered.get("challenge").is_none());
        h.send().await;
        let captures = h.provider.calls.lock().await;
        let last = captures.last().unwrap();
        let challenge = if block == "B105" {
            let pairs: BTreeMap<_, _> = url::form_urlencoded::parse(&last.body)
                .into_owned()
                .collect();
            assert_eq!(pairs["To"], phone.unwrap());
            pairs["Body"]
                .strip_prefix("Kyro verification: ")
                .unwrap()
                .to_string()
        } else {
            let body: Value = serde_json::from_slice(&last.body).unwrap();
            assert_eq!(
                body["topic"],
                registered["secret_once"]["subscription_topic"]
            );
            body["message"]
                .as_str()
                .unwrap()
                .strip_prefix("Kyro verification: ")
                .unwrap()
                .to_string()
        };
        drop(captures);
        let bad = h
            .op(
                &h.f.actor,
                block,
                "endpoint.verify",
                json!({"id":id(&registered),"secret":"wrong-proof"}),
                None,
            )
            .await
            .unwrap();
        assert_eq!(bad["verified"], false);
        assert_eq!(bad["attempts_remaining"], 4);
        let verified = h
            .op(
                &h.f.actor,
                block,
                "endpoint.verify",
                json!({"id":id(&registered),"secret":challenge}),
                None,
            )
            .await
            .unwrap();
        assert_eq!(verified["verified"], true);
        assert_eq!(verified["version"], 2);
        assert!(
            h.op(
                &h.f.actor,
                block,
                "endpoint.verify",
                json!({"id":id(&registered),"secret":challenge}),
                None
            )
            .await
            .is_err()
        );
        h.op(
            &h.f.actor,
            block,
            if block == "B105" {
                "mobile.send"
            } else {
                "push.send"
            },
            json!({"adapter_id":h.profiles[index].id,"message":m,"endpoint_id":id(&registered)}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(h.send().await["state"], "delivered");
        let pending=h.op(&h.f.actor,block,if block=="B105"{"mobile.send"}else{"push.send"},json!({"adapter_id":h.profiles[index].id,"message":m,"endpoint_id":id(&registered)}),None).await.unwrap();
        h.op(
            &h.f.actor,
            block,
            "endpoint.revoke",
            json!({"id":id(&registered)}),
            Some(2),
        )
        .await
        .unwrap();
        let count = h.provider.calls.lock().await.len();
        let claim = h.claim().await;
        assert!(
            kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), claim, &h.service)
                .await
                .is_err()
        );
        assert_eq!(h.provider.calls.lock().await.len(), count);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM app_connector_calls WHERE id=$1")
                .bind(id(&pending))
                .fetch_one(&h.f.admin)
                .await
                .unwrap(),
            "unknown"
        );
    }
    assert_eq!(h.provider.calls.lock().await.len(), 5);
    h.op(
        &h.f.actor,
        "B103",
        "preferences.set",
        json!({"internal":true,"email":false,"mobile":false,"push":false,"frequency":"immediate"}),
        Some(1),
    )
    .await
    .unwrap();
    assert!(
        h.op(&h.f.actor, "B104", "email.send", email, None)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn revocation_after_socket_open_keeps_receipt_unknown_and_never_resends() {
    let h = Arc::new(Harness::new(vec![rest("/gate")]).await);
    let queued = h
        .invoke(
            0,
            json!({"kind":"rest","operation":"lookup","input":{"name":"gate"}}),
        )
        .await;
    let claim = h.claim().await;
    let runner = h.clone();
    let send = tokio::spawn(async move {
        kyro_app::connectors::send_claimed(
            &runner.f.core,
            runner.worker.clone(),
            claim,
            &runner.service,
        )
        .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        h.provider.arrived.notified(),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id=$1 AND principal_id=$2").bind(h.f.actor.tenant_id()).bind(h.f.actor.principal_id()).execute(&h.f.admin).await.unwrap();
    h.provider.gate.notify_one();
    assert!(send.await.unwrap().is_err());
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM app_connector_calls WHERE id=$1")
            .bind(id(&queued))
            .fetch_one(&h.f.admin)
            .await
            .unwrap(),
        "unknown"
    );
    assert_eq!(
        h.op(
            &h.worker,
            "B054",
            "outbox.claim",
            json!({"effects_only":true}),
            None
        )
        .await
        .unwrap()["claimed"],
        false
    );
    assert_eq!(h.provider.calls.lock().await.len(), 1);
}

async fn order(h: &Harness, sku: &str) -> Uuid {
    let p = h
        .op(
            &h.f.actor,
            "B121",
            "create",
            json!({"sku":sku,"name":"Synthetic product","inventory_tracked":false}),
            None,
        )
        .await
        .unwrap();
    h.op(&h.f.actor, "B121", "publish", json!({"id":id(&p)}), Some(1))
        .await
        .unwrap();
    h.op(&h.f.actor,"B122","set_price",json!({"product_id":id(&p),"currency":"EUR","amount_minor":2500,"interval_unit":"one_time","interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}),None).await.unwrap();
    let quote = h
        .op(
            &h.f.actor,
            "B123",
            "quote",
            json!({"currency":"EUR","items":[{"product_id":id(&p),"quantity":1}]}),
            None,
        )
        .await
        .unwrap();
    id(&h
        .op(
            &h.f.actor,
            "B124",
            "create",
            json!({"quote_id":id(&quote)}),
            None,
        )
        .await
        .unwrap())
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn cancelled_order_never_opens_its_prepared_payment_and_claimed_payment_blocks_cancel() {
    let h = Harness::new(vec![Provider::Stripe {
        api_version: "2025-09-30.clover".into(),
        customers: BTreeMap::new(),
        prices: BTreeMap::new(),
    }])
    .await;
    let oid = order(&h, "cancel-before-payment").await;
    let payment = h
        .op(
            &h.f.actor,
            "B125",
            "create_intent",
            json!({"order_id":oid,"connector_id":h.profiles[0].id}),
            None,
        )
        .await
        .unwrap();
    h.op(
        &h.f.actor,
        "B124",
        "cancel",
        json!({"order_id":oid}),
        Some(1),
    )
    .await
    .unwrap();
    let claim = h.claim().await;
    let sent =
        kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), claim, &h.service).await;
    assert!(
        sent.is_err(),
        "a cancelled payment must be refused before transport"
    );
    assert!(h.provider.calls.lock().await.is_empty());
    let state = h
        .op(
            &h.f.actor,
            "B125",
            "get_payment",
            json!({"id":id(&payment)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(state["data"]["status"], "cancelled");

    let oid = order(&h, "cancel-after-claim").await;
    h.op(
        &h.f.actor,
        "B125",
        "create_intent",
        json!({"order_id":oid,"connector_id":h.profiles[0].id}),
        None,
    )
    .await
    .unwrap();
    let claim = h.claim().await;
    assert_eq!(
        h.op(
            &h.f.actor,
            "B124",
            "cancel",
            json!({"order_id":oid}),
            Some(1)
        )
        .await,
        Err(AppError::Conflict(
            "payment_delivery_requires_reconciliation"
        ))
    );
    assert!(h.provider.calls.lock().await.is_empty());
    assert_eq!(
        kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), claim, &h.service)
            .await
            .unwrap()["state"],
        "delivered"
    );
    assert_eq!(
        h.op(
            &h.f.actor,
            "B124",
            "cancel",
            json!({"order_id":oid}),
            Some(1)
        )
        .await,
        Err(AppError::Conflict(
            "payment_delivery_requires_reconciliation"
        ))
    );
    assert_eq!(h.provider.calls.lock().await.len(), 1);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn payment_outbox_is_atomic_unique_amount_bound_and_does_not_confirm_payment() {
    let h = Harness::new(vec![Provider::Stripe {
        api_version: "2025-09-30.clover".into(),
        customers: BTreeMap::new(),
        prices: BTreeMap::new(),
    }])
    .await;
    let oid = order(&h, "payment-test").await;
    let payment = h
        .op(
            &h.f.actor,
            "B125",
            "create_intent",
            json!({"order_id":oid,"connector_id":h.profiles[0].id}),
            None,
        )
        .await
        .unwrap();
    let again = h
        .op(
            &h.f.actor,
            "B125",
            "create_intent",
            json!({"order_id":oid,"connector_id":h.profiles[0].id}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(again, payment);
    let call = Uuid::parse_str(payment["connector_call_id"].as_str().unwrap()).unwrap();
    assert_eq!(h.send().await["state"], "delivered");
    let captures = h.provider.calls.lock().await;
    assert_eq!(captures.len(), 1);
    assert_eq!(
        captures[0].headers["idempotency-key"],
        id(&payment).to_string()
    );
    let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&captures[0].body)
        .into_owned()
        .collect();
    assert_eq!(fields["amount"], "2500");
    assert_eq!(fields["currency"], "eur");
    drop(captures);
    let receipt = h
        .op(
            &h.f.actor,
            "B153",
            "adapter.result",
            json!({"id":call}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt["result"]["payment_confirmed"], false);
    assert_eq!(
        h.op(
            &h.f.actor,
            "B125",
            "get_payment",
            json!({"id":id(&payment)}),
            None
        )
        .await
        .unwrap()["data"]["status"],
        "pending"
    );
    assert!(h.op(&h.f.actor,"B153","adapter.call",json!({"adapter_id":h.profiles[0].id,"specification":{"kind":"payment_intent","payment_id":id(&payment)}}),None).await.is_err());
    assert_eq!(
        h.op(
            &h.worker,
            "B054",
            "outbox.claim",
            json!({"effects_only":true}),
            None
        )
        .await
        .unwrap()["claimed"],
        false
    );
    let oid = order(&h, "payment-invalid-amount").await;
    let payment = h
        .op(
            &h.f.actor,
            "B125",
            "create_intent",
            json!({"order_id":oid,"connector_id":h.profiles[0].id}),
            None,
        )
        .await
        .unwrap();
    h.provider
        .wrong_amount
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let claim = h.claim().await;
    assert!(
        kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), claim, &h.service)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM app_connector_calls WHERE id=$1")
            .bind(Uuid::parse_str(payment["connector_call_id"].as_str().unwrap()).unwrap())
            .fetch_one(&h.f.admin)
            .await
            .unwrap(),
        "unknown"
    );
    assert_eq!(h.provider.calls.lock().await.len(), 2);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn subscriptions_refuse_unpublished_products_without_queuing_effects() {
    let f = Fixture::new().await;
    let product = f.op("B121", "create", json!({"sku":"subscription-publication","name":"Synthetic recurring product","inventory_tracked":false}), None).await.unwrap();
    let price = f.op("B122", "set_price", json!({"product_id":id(&product),"currency":"EUR","amount_minor":500,"interval_unit":"month","interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}), None).await.unwrap();
    let customer = f.actor.principal_id();
    let h = Harness::with_fixture(
        f,
        vec![Provider::Stripe {
            api_version: "2025-09-30.clover".into(),
            customers: BTreeMap::from([(customer, "cus_synthetic".into())]),
            prices: BTreeMap::from([(id(&price), "price_synthetic".into())]),
        }],
    )
    .await;
    let counts = || async {
        sqlx::query_as::<_, (i64,i64,i64,i64,i64,i64)>("SELECT (SELECT count(*) FROM app_records WHERE tenant_id=$1 AND kind='commerce.subscription'), (SELECT count(*) FROM app_connector_calls WHERE tenant_id=$1), (SELECT count(*) FROM app_record_history WHERE tenant_id=$1), (SELECT count(*) FROM app_outbox WHERE tenant_id=$1), (SELECT count(*) FROM app_idempotency WHERE tenant_id=$1), (SELECT COALESCE(sum(reserved_value),0)::bigint FROM app_quotas WHERE tenant_id=$1)").bind(h.f.actor.tenant_id()).fetch_one(&h.f.admin).await.unwrap()
    };
    for status in ["draft", "archived"] {
        if status == "archived" {
            h.f.op("B121", "publish", json!({"id":id(&product)}), Some(1))
                .await
                .unwrap();
            let subscription = h
                .op(
                    &h.f.actor,
                    "B126",
                    "subscribe",
                    json!({"price_id":id(&price),"connector_id":h.profiles[0].id}),
                    None,
                )
                .await
                .unwrap();
            assert_eq!(subscription["data"]["status"], "pending");
            h.f.op("B121", "archive", json!({"id":id(&product)}), Some(2))
                .await
                .unwrap();
        }
        let before = counts().await;
        assert_eq!(
            h.op(
                &h.f.actor,
                "B126",
                "subscribe",
                json!({"price_id":id(&price),"connector_id":h.profiles[0].id}),
                None
            )
            .await,
            Err(AppError::NotFound),
            "{status}"
        );
        assert_eq!(
            counts().await,
            before,
            "refusal leaves no durable command or financial effect"
        );
        assert!(h.provider.calls.lock().await.is_empty());
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn subscription_and_refund_protocols_bind_price_customer_amount_and_revalidate_receipts() {
    let f = Fixture::new().await;
    let product = f.op("B121", "create", json!({"sku":"subscription-protocol","name":"Synthetic recurring product","inventory_tracked":false}), None).await.unwrap();
    f.op("B121", "publish", json!({"id":id(&product)}), Some(1))
        .await
        .unwrap();
    let price = f.op("B122", "set_price", json!({"product_id":id(&product),"currency":"EUR","amount_minor":500,"interval_unit":"month","interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}), None).await.unwrap();
    let customer = f.actor.principal_id();
    let h = Harness::with_fixture(
        f,
        vec![Provider::Stripe {
            api_version: "2025-09-30.clover".into(),
            customers: BTreeMap::from([(customer, "cus_synthetic".into())]),
            prices: BTreeMap::from([(id(&price), "price_synthetic".into())]),
        }],
    )
    .await;
    let request = OperationRequest {
        component_id: "B126".into(),
        action: "subscribe".into(),
        payload: json!({"price_id":id(&price),"connector_id":h.profiles[0].id}),
        expected_version: None,
        idempotency_key: "subscription-protocol-once".into(),
    };
    let (a, b) = tokio::join!(
        h.dispatcher
            .dispatch(&h.f.core, h.f.actor.clone(), request.clone()),
        h.dispatcher.dispatch(&h.f.core, h.f.actor.clone(), request)
    );
    let subscription = a.unwrap();
    assert_eq!(subscription, b.unwrap());
    assert_eq!(h.send().await["state"], "delivered");
    let receipt = h
        .op(
            &h.f.actor,
            "B153",
            "adapter.result",
            json!({"id":subscription["connector_call_id"]}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt["result"]["subscription_confirmed"], false);
    assert_eq!(receipt["result"]["customer"], "cus_synthetic");
    assert_eq!(receipt["result"]["price"], "price_synthetic");
    let state = h
        .op(
            &h.f.actor,
            "B126",
            "get_subscription",
            json!({"id":id(&subscription)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(state["data"]["status"], "pending");
    assert_eq!(state["data"]["provider_reference"], "sub_synthetic");
    assert_eq!(state["active_in_period"], false);
    {
        let captures = h.provider.calls.lock().await;
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].method, "POST");
        assert_eq!(captures[0].path, "/v1/subscriptions");
        assert_eq!(captures[0].headers["stripe-version"], "2025-09-30.clover");
        assert_eq!(
            captures[0].headers["idempotency-key"],
            id(&subscription).to_string()
        );
        let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&captures[0].body)
            .into_owned()
            .collect();
        assert_eq!(fields["customer"], "cus_synthetic");
        assert_eq!(fields["items[0][price]"], "price_synthetic");
        assert_eq!(
            fields["metadata[kyro_subscription]"],
            id(&subscription).to_string()
        );
    }
    let oid = order(&h, "refund-protocol").await;
    let payment = h
        .op(
            &h.f.actor,
            "B125",
            "create_intent",
            json!({"order_id":oid,"connector_id":h.profiles[0].id}),
            None,
        )
        .await
        .unwrap();
    h.send().await;
    // Only settlement is doubled here. The native raw-body HMAC suite proves
    // callback settlement independently; this suite checks outbound protocols.
    sqlx::query("UPDATE app_records SET data=jsonb_set(data,'{status}','\"succeeded\"'),version=version+1 WHERE tenant_id=$1 AND application_id=$2 AND id=$3 AND kind='commerce.payment'")
        .bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).bind(id(&payment)).execute(&h.f.admin).await.unwrap();
    assert!(matches!(
        h.op(
            &h.f.actor,
            "B128",
            "request",
            json!({"payment_id":id(&payment),"amount_minor":2501,"connector_id":h.profiles[0].id}),
            None
        )
        .await,
        Err(AppError::Conflict("refund_exceeds_payment"))
    ));
    let refund = h
        .op(
            &h.f.actor,
            "B128",
            "request",
            json!({"payment_id":id(&payment),"amount_minor":1000,"connector_id":h.profiles[0].id}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(h.send().await["state"], "delivered");
    let refund_call = Uuid::parse_str(refund["connector_call_id"].as_str().unwrap()).unwrap();
    let receipt = h
        .op(
            &h.f.actor,
            "B153",
            "adapter.result",
            json!({"id":refund_call}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt["result"]["refund_confirmed"], false);
    assert_eq!(receipt["result"]["payment_intent"], "pi_synthetic");
    assert_eq!(receipt["result"]["amount_minor"], 1000);
    let payment_state = h
        .op(
            &h.f.actor,
            "B125",
            "get_payment",
            json!({"id":id(&payment)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(payment_state["data"]["refunded_minor"], 0);
    assert_eq!(payment_state["data"]["refund_reserved_minor"], 1000);
    {
        let captures = h.provider.calls.lock().await;
        assert_eq!(captures.len(), 3);
        assert_eq!(captures[2].path, "/v1/refunds");
        assert_eq!(
            captures[2].headers["idempotency-key"],
            id(&refund).to_string()
        );
        let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&captures[2].body)
            .into_owned()
            .collect();
        assert_eq!(fields["amount"], "1000");
        assert_eq!(fields["payment_intent"], "pi_synthetic");
        assert_eq!(fields["metadata[kyro_refund]"], id(&refund).to_string());
    }
    sqlx::query("UPDATE app_connector_calls SET result=jsonb_set(result,'{amount_minor}','1001') WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).bind(refund_call).execute(&h.f.admin).await.unwrap();
    assert_eq!(
        h.op(
            &h.f.actor,
            "B153",
            "adapter.result",
            json!({"id":refund_call}),
            None
        )
        .await,
        Err(AppError::Invalid("financial_receipt_amount_mismatch"))
    );
    h.provider
        .wrong_amount
        .store(true, std::sync::atomic::Ordering::SeqCst);
    h.op(
        &h.f.actor,
        "B126",
        "subscribe",
        json!({"price_id":id(&price),"connector_id":h.profiles[0].id}),
        None,
    )
    .await
    .unwrap();
    let bad_claim = h.claim().await;
    assert!(
        kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), bad_claim, &h.service)
            .await
            .is_err()
    );
    assert_eq!(h.provider.calls.lock().await.len(), 4);
    assert_eq!(
        h.op(
            &h.worker,
            "B054",
            "outbox.claim",
            json!({"effects_only":true}),
            None
        )
        .await
        .unwrap()["claimed"],
        false
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn object_storage_calendar_and_geocoding_use_scoped_bounded_protocols() {
    use sha2::{Digest, Sha256};
    let h = Harness::new(vec![
        Provider::S3 {
            region: "us-east-1".into(),
            bucket: "synthetic-bucket".into(),
            prefix: "apps".into(),
        },
        Provider::GoogleCalendar {
            calendar_id: "private@example.test".into(),
        },
        Provider::Nominatim {
            user_agent: "Kyro synthetic test (test@example.test)".into(),
        },
    ])
    .await;
    let doc = Uuid::new_v4();
    let bytes = b"synthetic-clean-file";
    sqlx::query("INSERT INTO app_documents(tenant_id,application_id,id,owner_id,kind,state,metadata) VALUES($1,$2,$3,$4,'file','clean','{}')").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).bind(doc).bind(h.f.actor.principal_id()).execute(&h.f.admin).await.unwrap();
    sqlx::query("INSERT INTO app_document_versions(tenant_id,application_id,document_id,version,display_name,media_type,size_bytes,sha256,content,created_by) VALUES($1,$2,$3,1,'synthetic.txt','text/plain',$4,$5,$6,$7)").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).bind(doc).bind(bytes.len() as i64).bind(Sha256::digest(bytes).to_vec()).bind(bytes.as_slice()).bind(h.f.actor.principal_id()).execute(&h.f.admin).await.unwrap();
    let object_id = Uuid::new_v4();
    h.invoke(
        0,
        json!({"kind":"object_put","object_id":object_id,"document_id":doc,"version":1}),
    )
    .await;
    assert_eq!(h.send().await["state"], "delivered");
    h.invoke(0, json!({"kind":"object_get","object_id":object_id}))
        .await;
    let get = h.send().await;
    assert_eq!(
        get["result"]["content_base64"],
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    let captures = h.provider.calls.lock().await;
    assert!(captures[0].path.contains(&format!(
        "/{}/{}/{}/",
        h.f.actor.tenant_id(),
        h.f.actor.application_id(),
        h.f.actor.principal_id()
    )));
    assert!(
        captures[0].headers["authorization"]
            .to_str()
            .unwrap()
            .contains(
                "SignedHeaders=content-type;host;if-none-match;x-amz-content-sha256;x-amz-date"
            )
    );
    drop(captures);
    h.invoke(0, json!({"kind":"object_delete","object_id":object_id}))
        .await;
    assert_eq!(h.send().await["state"], "delivered");
    assert!(h.provider.objects.lock().await.is_empty());
    let cid = Uuid::new_v4();
    let mut tx = h.f.core.begin(h.f.actor.clone()).await.unwrap();
    tx.insert("calendar.note",cid,json!({"summary":"Synthetic appointment","starts_at":"2026-10-06T08:00:00Z","ends_at":"2026-10-06T09:00:00Z"})).await.unwrap();
    tx.commit().await.unwrap();
    h.op(
        &h.f.actor,
        "B021",
        "policy.set",
        json!({"kind":"calendar.note","action":"read","owner":true,"roles":["admin"],"fields":{"summary":["admin"],"starts_at":["admin"],"ends_at":["admin"]}}),
        None,
    )
    .await
    .unwrap();
    h.invoke(
        1,
        json!({"kind":"calendar_create","source":{"kind":"calendar.note","id":cid,"version":1}}),
    )
    .await;
    assert_eq!(h.send().await["state"], "delivered");
    h.invoke(1,json!({"kind":"calendar_read","from":"2026-10-06T00:00:00Z","until":"2026-10-07T00:00:00Z"})).await;
    assert_eq!(h.send().await["result"]["synchronization_complete"], true);
    assert!(h.op(&h.f.actor,"B154","adapter.call",json!({"adapter_id":h.profiles[1].id,"specification":{"kind":"calendar_create","source":{"kind":"calendar.note","id":cid,"version":2}}}),None).await.is_err());
    h.invoke(2, json!({"kind":"geocode","address":"synthetic address"}))
        .await;
    let geo = h.send().await;
    assert_eq!(geo["result"]["items"][0]["latitude"], 48.85);
    assert_eq!(geo["result"]["items"][0]["longitude"], 2.35);
    assert_eq!(geo["result"]["attribution"]["licence"], "ODbL-1.0");
    assert_eq!(geo["result"]["attribution"]["display_required"], true);
    let mut public = h.profiles[2].clone();
    public.endpoint = "https://nominatim.openstreetmap.org".into();
    public.allowed_hosts = BTreeSet::from(["nominatim.openstreetmap.org".into()]);
    assert!(
        ConnectorService::new(vec![public], Arc::new(SecretVault::default()), [43; 32]).is_err()
    );
    h.invoke(2, json!({"kind":"geocode","address":"synthetic second"}))
        .await;
    assert_eq!(h.send().await["state"], "queued");
    assert_eq!(h.provider.calls.lock().await.len(), 6);
}

fn oauth() -> Provider {
    Provider::OAuth {
        settings: OAuthProvider {
            issuer: "https://issuer.example.test".into(),
            authorization_endpoint: "http://provider.test:0/authorize".into(),
            revocation_endpoint: "http://provider.test:0/revoke".into(),
            client_id: "synthetic-client".into(),
            redirect_uri: "http://127.0.0.1:9000/connector-callback".into(),
            scopes: BTreeSet::from([
                "https://www.googleapis.com/auth/calendar.events".into(),
                "synthetic.optional".into(),
            ]),
            client_secret_post: false,
        },
    }
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn oauth_refresh_cannot_expand_a_partially_granted_scope_or_be_acknowledged_manually() {
    let h = Harness::new(vec![oauth()]).await;
    let (_, complete) = authorization(&h).await;
    h.op(&h.f.actor, "B156", "oauth.complete", complete, None)
        .await
        .unwrap();
    h.send().await;
    h.provider
        .expanded_on_refresh
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let queued = h
        .op(
            &h.f.actor,
            "B156",
            "oauth.refresh",
            json!({"id":h.profiles[0].id}),
            Some(1),
        )
        .await
        .unwrap();
    let claim = h.claim().await;
    assert!(h.op(&h.worker,"B054","outbox.ack",json!({"id":claim.id,"lease_id":claim.lease_id,"generation":claim.generation,"outcome":"delivered","receipt":{"invented":true}}),None).await.is_err());
    assert!(
        kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), claim, &h.service)
            .await
            .is_err()
    );
    assert_eq!(
        h.op(
            &h.f.actor,
            "B156",
            "oauth.status",
            json!({"id":h.profiles[0].id}),
            None
        )
        .await
        .unwrap()["state"],
        "unknown"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM app_connector_calls WHERE id=$1")
            .bind(id(&queued))
            .fetch_one(&h.f.admin)
            .await
            .unwrap(),
        "unknown"
    );
    assert_eq!(
        h.op(
            &h.worker,
            "B054",
            "outbox.claim",
            json!({"effects_only":true}),
            None
        )
        .await
        .unwrap()["claimed"],
        false
    );
    assert_eq!(
        h.provider
            .token_counter
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
}
async fn authorization(h: &Harness) -> (Uuid, Value) {
    let begin = h
        .op(
            &h.f.actor,
            "B156",
            "oauth.begin",
            json!({"id":h.profiles[0].id}),
            None,
        )
        .await
        .unwrap();
    let url = url::Url::parse(begin["secret_once"]["authorization_url"].as_str().unwrap()).unwrap();
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], url.port().unwrap()));
    let response = reqwest::Client::builder()
        .no_proxy()
        .resolve("provider.test", address)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(url)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 302);
    let callback = url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    let q: BTreeMap<_, _> = callback.query_pairs().into_owned().collect();
    (
        id(&begin),
        json!({"adapter_id":h.profiles[0].id,"id":id(&begin),"state":q["state"],"code":q["code"],"issuer":q["iss"]}),
    )
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn oauth_pkce_state_session_scope_rotation_revocation_and_no_credential_response() {
    let h = Harness::new(vec![
        oauth(),
        Provider::GoogleCalendar {
            calendar_id: "private@example.test".into(),
        },
    ])
    .await;
    assert!(h.op(&h.f.actor,"B154","adapter.call",json!({"adapter_id":h.profiles[1].id,"specification":{"kind":"calendar_read","from":"2026-10-06T00:00:00Z","until":"2026-10-07T00:00:00Z"}}),None).await.is_err());
    let (_, complete) = authorization(&h).await;
    let (rotated_session, _) = Fixture::session(
        &h.f.admin,
        &h.f.core,
        h.f.actor.tenant_id(),
        h.f.actor.application_id(),
        h.f.actor.principal_id(),
        &[],
    )
    .await;
    assert_eq!(
        h.op(
            &rotated_session,
            "B156",
            "oauth.complete",
            complete.clone(),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    let (other, _) = Fixture::session(
        &h.f.admin,
        &h.f.core,
        h.f.actor.tenant_id(),
        h.f.actor.application_id(),
        Uuid::new_v4(),
        &["admin"],
    )
    .await;
    assert_eq!(
        h.op(&other, "B156", "oauth.complete", complete.clone(), None)
            .await,
        Err(AppError::NotFound)
    );
    let mut wrong = complete.clone();
    wrong["state"] = json!("wrong-state");
    assert_eq!(
        h.op(&h.f.actor, "B156", "oauth.complete", wrong, None)
            .await
            .unwrap()["attempts_remaining"],
        4
    );
    let mut wrong = complete.clone();
    wrong["issuer"] = json!("https://other-issuer.example.test");
    assert!(
        h.op(&h.f.actor, "B156", "oauth.complete", wrong, None)
            .await
            .is_err()
    );
    let queued = h
        .op(&h.f.actor, "B156", "oauth.complete", complete.clone(), None)
        .await
        .unwrap();
    assert!(
        h.op(&h.f.actor, "B156", "oauth.complete", complete, None)
            .await
            .is_err()
    );
    let delivered = h.send().await;
    assert_eq!(delivered["state"], "delivered");
    assert_eq!(delivered["result"]["state"], "active");
    assert!(!delivered.to_string().contains("synthetic-access"));
    assert!(!delivered.to_string().contains("synthetic-refresh"));
    let receipt = h
        .op(
            &h.f.actor,
            "B156",
            "adapter.result",
            json!({"id":id(&queued)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt["result"]["version"], 1);
    let cipher:Vec<u8>=sqlx::query_scalar("SELECT credential_cipher FROM app_connector_oauth_connections WHERE tenant_id=$1 AND principal_id=$2").bind(h.f.actor.tenant_id()).bind(h.f.actor.principal_id()).fetch_one(&h.f.admin).await.unwrap();
    assert!(
        !cipher
            .windows(b"synthetic-access-1".len())
            .any(|w| w == b"synthetic-access-1")
    );
    let spec = json!({"kind":"calendar_read","from":"2026-10-06T00:00:00Z","until":"2026-10-07T00:00:00Z"});
    h.invoke(1, spec.clone()).await;
    assert_eq!(h.send().await["state"], "delivered");
    assert_eq!(
        h.provider.calls.lock().await.last().unwrap().headers["authorization"],
        "Bearer synthetic-access-1"
    );
    let stale = h.invoke(1, spec.clone()).await;
    sqlx::query(
        "UPDATE app_outbox SET available_at=clock_timestamp()+interval '1 hour' WHERE id=$1",
    )
    .bind(Uuid::parse_str(stale["outbox_id"].as_str().unwrap()).unwrap())
    .execute(&h.f.admin)
    .await
    .unwrap();
    h.op(
        &h.f.actor,
        "B156",
        "oauth.refresh",
        json!({"id":h.profiles[0].id}),
        Some(1),
    )
    .await
    .unwrap();
    let refresh = h.send().await;
    assert_eq!(refresh["result"]["version"], 3);
    assert!(!refresh.to_string().contains("synthetic-refresh"));
    sqlx::query("UPDATE app_outbox SET available_at=clock_timestamp() WHERE id=$1")
        .bind(Uuid::parse_str(stale["outbox_id"].as_str().unwrap()).unwrap())
        .execute(&h.f.admin)
        .await
        .unwrap();
    let count = h.provider.calls.lock().await.len();
    let claim = h.claim().await;
    assert!(
        kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), claim, &h.service)
            .await
            .is_err()
    );
    assert_eq!(h.provider.calls.lock().await.len(), count);
    h.invoke(1, spec.clone()).await;
    h.send().await;
    assert_eq!(
        h.provider.calls.lock().await.last().unwrap().headers["authorization"],
        "Bearer synthetic-access-2"
    );
    h.op(
        &h.f.actor,
        "B156",
        "oauth.revoke",
        json!({"id":h.profiles[0].id}),
        Some(3),
    )
    .await
    .unwrap();
    assert!(
        h.op(
            &h.f.actor,
            "B154",
            "adapter.call",
            json!({"adapter_id":h.profiles[1].id,"specification":spec}),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(h.send().await["result"]["state"], "revoked");
    assert_eq!(
        h.op(
            &h.f.actor,
            "B156",
            "oauth.status",
            json!({"id":h.profiles[0].id}),
            None
        )
        .await
        .unwrap()["state"],
        "revoked"
    );
    assert!(sqlx::query_scalar::<_,bool>("SELECT credential_cipher IS NULL FROM app_connector_oauth_connections WHERE tenant_id=$1 AND principal_id=$2").bind(h.f.actor.tenant_id()).bind(h.f.actor.principal_id()).fetch_one(&h.f.admin).await.unwrap());
    let (_, complete) = authorization(&h).await;
    h.provider
        .wrong_scope
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let queued = h
        .op(&h.f.actor, "B156", "oauth.complete", complete, None)
        .await
        .unwrap();
    let claim = h.claim().await;
    assert!(
        kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), claim, &h.service)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM app_connector_calls WHERE id=$1")
            .bind(id(&queued))
            .fetch_one(&h.f.admin)
            .await
            .unwrap(),
        "unknown"
    );
    assert_eq!(
        h.op(
            &h.worker,
            "B054",
            "outbox.claim",
            json!({"effects_only":true}),
            None
        )
        .await
        .unwrap()["claimed"],
        false
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn http_blocks_deliver_closed_contracts_sign_webhooks_and_hold_uncertain_effects() {
    let h = Harness::new(vec![rest("/goods"), rest("/slow")]).await;
    let request = OperationRequest {
        component_id: "B058".into(),
        action: "webhook.prepare".into(),
        payload: json!({"connector_id":h.profiles[0].id,"path":"/goods","method":"POST","body":{"name":"synthetic"}}),
        expected_version: None,
        idempotency_key: "same-webhook-call".into(),
    };
    let (one, two) = tokio::join!(
        h.dispatcher
            .dispatch(&h.f.core, h.f.actor.clone(), request.clone()),
        h.dispatcher
            .dispatch(&h.f.core, h.f.actor.clone(), request.clone()),
    );
    let queued = one.unwrap();
    assert_eq!(two.unwrap(), queued);
    assert_eq!(h.send().await["state"], "delivered");
    let capture = h.provider.calls.lock().await;
    assert_eq!(capture.len(), 1);
    assert_eq!(capture[0].path, "/goods");
    assert_eq!(capture[0].method, "POST");
    assert!(!capture[0].headers.contains_key("authorization"));
    assert_eq!(
        capture[0].headers["idempotency-key"].to_str().unwrap(),
        id(&queued).to_string()
    );
    kyro_app::exchange::verify_webhook_signature(
        b"public-synthetic-only-connector-secret-32",
        capture[0].headers["x-kyro-timestamp"].to_str().unwrap(),
        &capture[0].body,
        capture[0].headers["x-kyro-signature"].to_str().unwrap(),
        Utc::now(),
    )
    .unwrap();
    assert!(
        kyro_app::exchange::verify_webhook_signature(
            b"public-synthetic-only-connector-secret-32",
            capture[0].headers["x-kyro-timestamp"].to_str().unwrap(),
            b"changed",
            capture[0].headers["x-kyro-signature"].to_str().unwrap(),
            Utc::now(),
        )
        .is_err()
    );
    drop(capture);
    let result = h
        .op(
            &h.f.actor,
            "B058",
            "webhook.result",
            json!({"id":id(&queued)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result["result"]["answer"], "synthetic response");
    assert!(!result.to_string().contains("connector-secret"));
    let mut normal = request.clone();
    normal.component_id = "B059".into();
    normal.action = "http.prepare".into();
    normal.idempotency_key = "same-http-call".into();
    let http = h
        .dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), normal.clone())
        .await
        .unwrap();
    assert_eq!(h.send().await["state"], "delivered");
    let capture = h.provider.calls.lock().await;
    assert_eq!(capture.len(), 2);
    assert!(capture[1].headers.contains_key("authorization"));
    assert!(!capture[1].headers.contains_key("x-kyro-signature"));
    drop(capture);
    assert_eq!(
        h.op(
            &h.f.actor,
            "B059",
            "http.result",
            json!({"id":id(&http)}),
            None
        )
        .await
        .unwrap()["state"],
        "delivered"
    );
    for (field, value) in [
        ("path", json!("http://127.0.0.1/secret")),
        ("method", json!("DELETE")),
        (
            "body",
            json!({"name":"synthetic","url":"https://foreign.test"}),
        ),
    ] {
        let mut invalid = normal.clone();
        invalid.idempotency_key = Uuid::new_v4().to_string();
        invalid.payload[field] = value;
        assert!(
            h.dispatcher
                .dispatch(&h.f.core, h.f.actor.clone(), invalid)
                .await
                .is_err()
        );
    }
    let (foreign, _) = Fixture::session(
        &h.f.admin,
        &h.f.core,
        h.f.actor.tenant_id(),
        h.f.actor.application_id(),
        Uuid::new_v4(),
        &["admin"],
    )
    .await;
    assert_eq!(
        h.op(
            &foreign,
            "B058",
            "webhook.result",
            json!({"id":id(&queued)}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    let slow=h.op(&h.f.actor,"B058","webhook.prepare",json!({"connector_id":h.profiles[1].id,"path":"/slow","method":"POST","body":{"name":"slow"}}),None).await.unwrap();
    assert_eq!(h.send().await["state"], "unknown");
    assert_eq!(
        h.op(
            &h.f.actor,
            "B058",
            "webhook.result",
            json!({"id":id(&slow)}),
            None
        )
        .await
        .unwrap()["state"],
        "unknown"
    );
    assert_eq!(
        h.op(
            &h.worker,
            "B054",
            "outbox.claim",
            json!({"effects_only":true}),
            None
        )
        .await
        .unwrap()["claimed"],
        false
    );
    assert_eq!(h.provider.calls.lock().await.len(), 3);
    // A source changed after admission must prevent an otherwise valid send.
    let source = Uuid::new_v4();
    let mut tx = h.f.core.begin(h.f.actor.clone()).await.unwrap();
    tx.insert("synthetic.http-source", source, json!({"public":"first"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    h.op(&h.f.actor,"B021","policy.set",json!({"kind":"synthetic.http-source","action":"read","owner":true,"roles":[],"fields":{"public":["admin"]}}),None).await.unwrap();
    h.op(&h.f.actor,"B059","http.prepare",json!({"connector_id":h.profiles[0].id,"path":"/goods","method":"POST","body":{"name":"source"},"source_record":{"kind":"synthetic.http-source","id":source,"version":1}}),None).await.unwrap();
    let mut tx = h.f.core.begin(h.f.actor.clone()).await.unwrap();
    tx.update(
        "synthetic.http-source",
        source,
        1,
        json!({"public":"second"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let claim = h.claim().await;
    assert!(
        kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), claim, &h.service)
            .await
            .is_err()
    );
    assert_eq!(h.provider.calls.lock().await.len(), 3);
    // Without an admitted connector service, a preparer cannot promise delivery.
    assert!(matches!(
        h.f.op("B058", "webhook.prepare", request.payload.clone(), None)
            .await,
        Err(AppError::Unavailable)
    ));
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn rest_delivery_is_private_idempotent_and_unknown_is_held_without_retry() {
    let h = Harness::new(vec![rest("/goods"), rest("/slow"), rest("/redirect")]).await;
    let request = OperationRequest {
        component_id: "B157".into(),
        action: "adapter.call".into(),
        payload: json!({"adapter_id":h.profiles[0].id,"specification":{"kind":"rest","operation":"lookup","input":{"name":"synthetic"}}}),
        expected_version: None,
        idempotency_key: "same-closed-call".into(),
    };
    let queued = h
        .dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        h.dispatcher
            .dispatch(&h.f.core, h.f.actor.clone(), request)
            .await
            .unwrap(),
        queued
    );
    assert_eq!(h.send().await["state"], "delivered");
    let capture = h.provider.calls.lock().await;
    assert_eq!(capture.len(), 1);
    assert_eq!(capture[0].path, "/goods");
    assert_eq!(capture[0].method, "POST");
    assert!(capture[0].headers.contains_key("idempotency-key"));
    drop(capture);
    let receipt = h
        .op(
            &h.f.actor,
            "B157",
            "adapter.result",
            json!({"id":id(&queued)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt["result"]["answer"], "synthetic response");
    assert_eq!(receipt["estimated_units"], 7);
    assert_eq!(receipt["invoice_verified"], false);
    let (other, _) = Fixture::session(
        &h.f.admin,
        &h.f.core,
        h.f.actor.tenant_id(),
        h.f.actor.application_id(),
        Uuid::new_v4(),
        &["admin"],
    )
    .await;
    assert_eq!(
        h.op(
            &other,
            "B157",
            "adapter.result",
            json!({"id":id(&queued)}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    assert!(h.op(&h.f.actor,"B157","adapter.call",json!({"adapter_id":h.profiles[0].id,"specification":{"kind":"rest","operation":"lookup","input":{"name":"ok","url":"http://127.0.0.1/"}}}),None).await.is_err());
    // Operator timeout configuration, not a browser-selected deadline.
    let slow = h
        .invoke(
            1,
            json!({"kind":"rest","operation":"lookup","input":{"name":"slow"}}),
        )
        .await;
    assert_eq!(h.send().await["state"], "unknown");
    let rejected = h
        .invoke(
            2,
            json!({"kind":"rest","operation":"lookup","input":{"name":"redirect"}}),
        )
        .await;
    assert_eq!(h.send().await["state"], "unknown");
    let v = h
        .op(
            &h.f.actor,
            "B157",
            "adapter.result",
            json!({"id":id(&rejected)}),
            None,
        )
        .await
        .unwrap();
    assert!(v["estimated_units"].is_null());
    let empty = h
        .op(
            &h.worker,
            "B054",
            "outbox.claim",
            json!({"effects_only":true}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(empty["claimed"], false);
    assert_eq!(h.provider.calls.lock().await.len(), 3);
    let held:i64=sqlx::query_scalar("SELECT reserved_value FROM app_quotas WHERE tenant_id=$1 AND application_id=$2 AND quota_key='connector_budget_units'").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).fetch_one(&h.f.admin).await.unwrap();
    assert_eq!(held, 14);
    assert_ne!(id(&slow), id(&rejected));
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and test-support loopback transport"]
async fn mcp_locks_catalog_encodes_headers_and_rejects_drift_before_tool_execution() {
    let tool = McpTool {
        input: object(&[("name", string()), ("optional", string())], &["name"]),
        output: object(&[("answer", string())], &["answer"]),
        effect: false,
        mirrored_headers: BTreeMap::from([
            ("name".into(), "name".into()),
            ("optional".into(), "optional".into()),
        ]),
    };
    let h = Harness::new(vec![Provider::Mcp {
        protocol_version: "2026-07-28".into(),
        tools: BTreeMap::from([("lookup".into(), tool.clone())]),
    }])
    .await;
    let mut input = serde_json::to_value(&tool.input).unwrap();
    for (key, header) in &tool.mirrored_headers {
        input["properties"][key]["x-mcp-header"] = json!(header);
    }
    *h.provider.tool.lock().await =
        Some(json!({"name":"lookup","inputSchema":input,"outputSchema":tool.output}));
    h.invoke(
        0,
        json!({"kind":"mcp","tool":"lookup","arguments":{"name":" Cèdre "}}),
    )
    .await;
    assert_eq!(h.send().await["state"], "delivered");
    let capture = h.provider.calls.lock().await;
    assert_eq!(capture.len(), 2);
    let headers = &capture[1].headers;
    assert_eq!(headers["mcp-protocol-version"], "2026-07-28");
    assert_eq!(
        headers["mcp-param-name"],
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(" Cèdre ")
        )
    );
    assert!(!headers.contains_key("mcp-param-optional"));
    assert_eq!(headers.get_all("authorization").iter().count(), 1);
    drop(capture);
    *h.provider.tool.lock().await =
        Some(json!({"name":"lookup","inputSchema":{},"outputSchema":tool.output}));
    h.invoke(
        0,
        json!({"kind":"mcp","tool":"lookup","arguments":{"name":"drift"}}),
    )
    .await;
    let claim = h.claim().await;
    assert!(
        kyro_app::connectors::send_claimed(&h.f.core, h.worker.clone(), claim, &h.service)
            .await
            .is_err()
    );
    assert_eq!(h.provider.calls.lock().await.len(), 3);
    assert_eq!(
        h.op(
            &h.worker,
            "B054",
            "outbox.claim",
            json!({"effects_only":true}),
            None
        )
        .await
        .unwrap()["claimed"],
        false
    );
}
