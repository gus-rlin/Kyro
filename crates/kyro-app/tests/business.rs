mod support;

use chrono::{Duration, Utc};
use kyro_app::{Actor, AppError, AppResult, OperationRequest};
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

async fn durable_counts(f: &Fixture) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM app_record_history WHERE tenant_id=$1 AND application_id=$2), (SELECT count(*) FROM app_idempotency WHERE tenant_id=$1 AND application_id=$2), (SELECT count(*) FROM app_events WHERE tenant_id=$1 AND application_id=$2)")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn idempotent_crm_receipt_refuses_removed_private_visibility_without_mutating() {
    let f = Fixture::new().await;
    let (writer, token) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &[
            "crm.write",
            "crm.read",
            "crm.private.write",
            "crm.private.read",
        ],
    )
    .await;
    let contact = op(&f, &writer, "B131", "contact.create", json!({"full_name":"Synthetic private contact","email":"private@example.test","phone":"0123456789","private_notes":"private fixture"}), None).await.unwrap();
    let request = OperationRequest {
        component_id: "B131".into(),
        action: "contact.update".into(),
        payload: json!({"id":id(&contact),"full_name":"Changed synthetic contact"}),
        expected_version: Some(1),
        idempotency_key: "private-receipt".into(),
    };
    let receipt = f
        .dispatcher
        .dispatch(&f.core, writer.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(receipt["data"]["private_notes"], "private fixture");
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, writer.clone(), request.clone())
            .await
            .unwrap(),
        receipt
    );
    sqlx::query("DELETE FROM app_memberships WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3 AND role='crm.private.read'").bind(writer.tenant_id()).bind(writer.application_id()).bind(writer.principal_id()).execute(&f.admin).await.unwrap();
    let current = f.core.authenticate(&token).await.unwrap();
    assert!(current.permissions().contains("*"));
    let public = op(
        &f,
        &current,
        "B131",
        "contact.get",
        json!({"id":id(&contact)}),
        None,
    )
    .await
    .unwrap();
    assert!(public["data"].get("private_notes").is_none());
    let before = durable_counts(&f).await;
    assert_eq!(
        f.dispatcher.dispatch(&f.core, current, request).await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    assert_eq!(durable_counts(&f).await, before);
    assert_eq!(
        op(
            &f,
            &writer,
            "B131",
            "contact.get",
            json!({"id":id(&contact)}),
            None
        )
        .await
        .unwrap()["version"],
        2
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn idempotent_support_receipt_refuses_removed_team_without_mutating() {
    let f = Fixture::new().await;
    let (agent, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member", "support.agent"],
    )
    .await;
    let team = f
        .op(
            "B014",
            "create",
            json!({"name":"Synthetic support team"}),
            None,
        )
        .await
        .unwrap();
    f.op(
        "B014",
        "member_add",
        json!({"group_id":id(&team),"principal_id":agent.principal_id()}),
        None,
    )
    .await
    .unwrap();
    let ticket = f.op("B133", "ticket.create", json!({"title":"Synthetic issue","description":"Synthetic body","team_id":id(&team),"priority":"normal"}), None).await.unwrap();
    let request = OperationRequest {
        component_id: "B133".into(),
        action: "ticket.add_internal_note".into(),
        payload: json!({"id":id(&ticket),"body":"Private support note"}),
        expected_version: Some(1),
        idempotency_key: "support-receipt".into(),
    };
    let receipt = f
        .dispatcher
        .dispatch(&f.core, agent.clone(), request.clone())
        .await
        .unwrap();
    assert!(receipt.to_string().contains("Private support note"));
    f.op(
        "B014",
        "member_remove",
        json!({"group_id":id(&team),"principal_id":agent.principal_id()}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        op(
            &f,
            &agent,
            "B133",
            "ticket.get",
            json!({"id":id(&ticket)}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    let before = durable_counts(&f).await;
    assert_eq!(
        f.dispatcher.dispatch(&f.core, agent, request).await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    assert_eq!(durable_counts(&f).await, before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT version FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND id=$3"
        )
        .bind(f.actor.tenant_id())
        .bind(f.actor.application_id())
        .bind(id(&ticket))
        .fetch_one(&f.admin)
        .await
        .unwrap(),
        2
    );
}

async fn op(
    f: &Fixture,
    actor: &Actor,
    component: &str,
    action: &str,
    payload: Value,
    version: Option<i64>,
) -> AppResult<Value> {
    f.dispatcher
        .dispatch(
            &f.core,
            actor.clone(),
            OperationRequest {
                component_id: component.into(),
                action: action.into(),
                payload,
                idempotency_key: Uuid::new_v4().to_string(),
                expected_version: version,
            },
        )
        .await
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn crm_private_fields_pipeline_edges_and_exact_opportunity_amounts() {
    let f = Fixture::new().await;
    let organization = f
        .op(
            "B131",
            "organization.create",
            json!({"name":"Synthetic company","billing_email":"billing@example.test"}),
            None,
        )
        .await
        .unwrap();
    let contact = f.op("B131","contact.create",json!({"full_name":"Synthetic contact","organization_id":id(&organization),"email":"contact@example.test","private_notes":"private note"}),None).await.unwrap();
    let (reader, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["crm.read"],
    )
    .await;
    let public = op(
        &f,
        &reader,
        "B131",
        "contact.get",
        json!({"id":id(&contact)}),
        None,
    )
    .await
    .unwrap();
    for field in ["email", "phone", "private_notes"] {
        assert!(public["data"].get(field).is_none());
    }
    assert_eq!(public["data"]["full_name"], "Synthetic contact");
    assert!(
        op(
            &f,
            &reader,
            "B131",
            "contact.update",
            json!({"id":id(&contact),"private_notes":"forbidden"}),
            Some(1)
        )
        .await
        .is_err()
    );
    assert!(
        f.op(
            "B131",
            "contact.create",
            json!({"full_name":"bad","email":"invalid"}),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op(
            "B131",
            "organization.contacts",
            json!({"id":id(&organization)}),
            None
        )
        .await
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let pipeline = f.op("B132","pipeline.define",json!({"name":"Synthetic pipeline","stages":["new","qualified","won"],"transitions":[{"from":"new","to":"qualified","roles":["sales.write"]},{"from":"qualified","to":"won","roles":["sales.write"]}]}),None).await.unwrap();
    let lead = f
        .op(
            "B132",
            "lead.create",
            json!({"contact_id":id(&contact),"source":"synthetic","pipeline_id":id(&pipeline)}),
            None,
        )
        .await
        .unwrap();
    assert!(
        f.op(
            "B132",
            "lead.transition",
            json!({"id":id(&lead),"to":"won"}),
            Some(1)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op(
            "B132",
            "lead.transition",
            json!({"id":id(&lead),"to":"qualified"}),
            Some(1)
        )
        .await
        .unwrap()["data"]["stage"],
        "qualified"
    );
    assert!(
        f.op(
            "B132",
            "lead.transition",
            json!({"id":id(&lead),"to":"won"}),
            Some(1)
        )
        .await
        .is_err()
    );
    let opportunity = f.op("B132","opportunity.create",json!({"contact_id":id(&contact),"pipeline_id":id(&pipeline),"amount":"1234.56","currency":"EUR"}),None).await.unwrap();
    assert_eq!(opportunity["data"]["amount_minor"], 123456);
    assert!(f.op("B132","opportunity.create",json!({"contact_id":id(&contact),"pipeline_id":id(&pipeline),"amount":"1.001","currency":"EUR"}),None).await.is_err());
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn task_dag_and_intervention_qualification() {
    let f = Fixture::new().await;
    let project = f
        .op(
            "B134",
            "project.create",
            json!({"name":"Synthetic project"}),
            None,
        )
        .await
        .unwrap();
    let one = f
        .op(
            "B134",
            "task.create",
            json!({"project_id":id(&project),"title":"first","depends_on":[]}),
            None,
        )
        .await
        .unwrap();
    let two = f
        .op(
            "B134",
            "task.create",
            json!({"project_id":id(&project),"title":"second","depends_on":[id(&one)]}),
            None,
        )
        .await
        .unwrap();
    assert!(
        f.op(
            "B134",
            "task.set_dependencies",
            json!({"id":id(&one),"depends_on":[id(&two)]}),
            Some(1)
        )
        .await
        .is_err()
    );
    f.op(
        "B134",
        "task.transition",
        json!({"id":id(&two),"to":"in_progress"}),
        Some(1),
    )
    .await
    .unwrap();
    assert!(
        f.op(
            "B134",
            "task.transition",
            json!({"id":id(&two),"to":"done"}),
            Some(2)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op("B134", "task.get", json!({"id":id(&one)}), None)
            .await
            .unwrap()["version"],
        1
    );
    f.op(
        "B134",
        "task.transition",
        json!({"id":id(&one),"to":"in_progress"}),
        Some(1),
    )
    .await
    .unwrap();
    f.op(
        "B134",
        "task.transition",
        json!({"id":id(&one),"to":"done"}),
        Some(2),
    )
    .await
    .unwrap();
    assert_eq!(
        f.op(
            "B134",
            "task.transition",
            json!({"id":id(&two),"to":"done"}),
            Some(2)
        )
        .await
        .unwrap()["data"]["status"],
        "done"
    );
    let site = f
        .op(
            "B012",
            "create",
            json!({"name":"Synthetic work site"}),
            None,
        )
        .await
        .unwrap();
    let (technician, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let work = f.op("B135","work_order.create",json!({"title":"Synthetic intervention","site_id":id(&site),"scheduled_start":"2026-10-06T10:00:00Z","scheduled_end":"2026-10-06T11:00:00Z","required_skills":["electrical"]}),None).await.unwrap();
    f.op("B135","work_order.qualify",json!({"principal_id":technician.principal_id(),"skills":["plumbing"],"site_ids":[id(&site)]}),None).await.unwrap();
    assert_eq!(
        f.op(
            "B135",
            "work_order.assign",
            json!({"id":id(&work),"assignee_id":technician.principal_id()}),
            Some(1)
        )
        .await,
        Err(AppError::Forbidden)
    );
    f.op("B135","work_order.qualify",json!({"principal_id":technician.principal_id(),"skills":["electrical"],"site_ids":[id(&site)]}),Some(1)).await.unwrap();
    f.op(
        "B135",
        "work_order.assign",
        json!({"id":id(&work),"assignee_id":technician.principal_id()}),
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(
        op(
            &f,
            &technician,
            "B135",
            "work_order.get",
            json!({"id":id(&work)}),
            None
        )
        .await
        .unwrap()["data"]["title"],
        "Synthetic intervention"
    );
    let (replacement, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    f.op("B135", "work_order.qualify", json!({"principal_id":replacement.principal_id(),"skills":["electrical"],"site_ids":[id(&site)]}), None).await.unwrap();
    f.op(
        "B135",
        "work_order.assign",
        json!({"id":id(&work),"assignee_id":replacement.principal_id()}),
        Some(2),
    )
    .await
    .unwrap();
    assert_eq!(
        op(
            &f,
            &technician,
            "B135",
            "work_order.get",
            json!({"id":id(&work)}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    assert!(
        op(
            &f,
            &technician,
            "B135",
            "work_order.list",
            json!({"limit":1}),
            None
        )
        .await
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        op(
            &f,
            &replacement,
            "B135",
            "work_order.list",
            json!({"limit":1}),
            None
        )
        .await
        .unwrap()["items"][0]["id"],
        work["id"]
    );
    f.op(
        "B135",
        "work_order.assign",
        json!({"id":id(&work),"assignee_id":technician.principal_id()}),
        Some(3),
    )
    .await
    .unwrap();
    op(
        &f,
        &technician,
        "B135",
        "work_order.transition",
        json!({"id":id(&work),"to":"in_progress"}),
        Some(4),
    )
    .await
    .unwrap();
    assert_eq!(
        op(
            &f,
            &technician,
            "B135",
            "work_order.complete",
            json!({"id":id(&work),"completion_notes":"synthetic completion"}),
            Some(5)
        )
        .await
        .unwrap()["data"]["status"],
        "completed"
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn local_month_lock_and_time_entry_separate_approver() {
    let f = Fixture::new().await;
    let project = f
        .op(
            "B134",
            "project.create",
            json!({"name":"Time test project","time_zone":"Europe/Paris"}),
            None,
        )
        .await
        .unwrap();
    let entry = json!({"project_id":id(&project),"start_at":"2026-10-01T00:30:00+02:00","end_at":"2026-10-01T01:30:00+02:00","time_zone":"Europe/Paris","description":"synthetic work"});
    f.op(
        "B136",
        "time_period.lock",
        json!({"project_id":id(&project),"month":"2026-10"}),
        None,
    )
    .await
    .unwrap();
    assert!(
        f.op("B136", "time_entry.create", entry.clone(), None)
            .await
            .is_err(),
        "the local October period must cover an instant still in September UTC"
    );
    let mut other_display = entry.clone();
    other_display["time_zone"] = json!("UTC");
    assert!(
        f.op("B136", "time_entry.create", other_display, None)
            .await
            .is_err()
    );
    f.op(
        "B136",
        "time_period.unlock",
        json!({"project_id":id(&project),"month":"2026-10"}),
        Some(1),
    )
    .await
    .unwrap();
    let time = f
        .op("B136", "time_entry.create", entry.clone(), None)
        .await
        .unwrap();
    assert_eq!(time["data"]["seconds"], 3600);
    f.op(
        "B136",
        "time_period.lock",
        json!({"project_id":id(&project),"month":"2026-10"}),
        Some(2),
    )
    .await
    .unwrap();
    let mut correction = entry.clone();
    correction["start_at"] = json!("2026-11-01T00:30:00+01:00");
    correction["end_at"] = json!("2026-11-01T02:30:00+01:00");
    assert!(
        f.op(
            "B136",
            "time_entry.correct",
            json!({"id":id(&time),"entry":correction,"reason":"synthetic correction"}),
            Some(1)
        )
        .await
        .is_err(),
        "the original locked period cannot be escaped by moving the corrected entry"
    );
    f.op(
        "B136",
        "time_period.unlock",
        json!({"project_id":id(&project),"month":"2026-10"}),
        Some(3),
    )
    .await
    .unwrap();
    let corrected = f
        .op(
            "B136",
            "time_entry.correct",
            json!({"id":id(&time),"entry":correction,"reason":"synthetic correction"}),
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(corrected["data"]["seconds"], 7200);
    let history: i64 = sqlx::query_scalar("SELECT count(*) FROM app_record_history WHERE tenant_id=$1 AND application_id=$2 AND record_id=$3")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(id(&time)).fetch_one(&f.admin).await.unwrap();
    assert_eq!(history, 2);
    f.op(
        "B136",
        "time_entry.submit",
        json!({"id":id(&time)}),
        Some(2),
    )
    .await
    .unwrap();
    assert_eq!(
        f.op(
            "B136",
            "time_entry.approve",
            json!({"id":id(&time)}),
            Some(3)
        )
        .await,
        Err(AppError::Forbidden)
    );
    let (approver, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["time.approve"],
    )
    .await;
    assert_eq!(
        op(
            &f,
            &approver,
            "B136",
            "time_entry.approve",
            json!({"id":id(&time)}),
            Some(3)
        )
        .await
        .unwrap()["data"]["status"],
        "approved"
    );
    assert!(
        f.op(
            "B136",
            "time_entry.correct",
            json!({"id":id(&time),"entry":entry,"reason":"after approval"}),
            Some(4)
        )
        .await
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn purchases_separate_approver_partial_receipt_and_unique_asset_loan() {
    let f = Fixture::new().await;
    let supplier = f
        .op(
            "B138",
            "supplier.create",
            json!({"name":"Synthetic supplier","email":"supplier@example.test"}),
            None,
        )
        .await
        .unwrap();
    let purchase = f.op("B138","purchase.create",json!({"supplier_id":id(&supplier),"currency":"EUR","lines":[{"description":"synthetic part","quantity":2,"unit_price_minor":125}]}),None).await.unwrap();
    assert_eq!(purchase["data"]["total_minor"], 250);
    f.op(
        "B138",
        "purchase.submit",
        json!({"id":id(&purchase)}),
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(
        f.op(
            "B138",
            "purchase.approve",
            json!({"id":id(&purchase)}),
            Some(2)
        )
        .await,
        Err(AppError::Forbidden)
    );
    let (approver, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["purchases.approve"],
    )
    .await;
    op(
        &f,
        &approver,
        "B138",
        "purchase.approve",
        json!({"id":id(&purchase)}),
        Some(2),
    )
    .await
    .unwrap();
    f.op(
        "B138",
        "purchase.order",
        json!({"id":id(&purchase)}),
        Some(3),
    )
    .await
    .unwrap();
    assert!(
        f.op(
            "B138",
            "purchase.receive",
            json!({"id":id(&purchase),"quantities":[3]}),
            Some(4)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op(
            "B138",
            "purchase.receive",
            json!({"id":id(&purchase),"quantities":[1]}),
            Some(4)
        )
        .await
        .unwrap()["data"]["status"],
        "partially_received"
    );
    assert_eq!(
        f.op(
            "B138",
            "purchase.receive",
            json!({"id":id(&purchase),"quantities":[1]}),
            Some(5)
        )
        .await
        .unwrap()["data"]["status"],
        "received"
    );
    assert!(
        f.op(
            "B138",
            "purchase.receive",
            json!({"id":id(&purchase),"quantities":[1]}),
            Some(6)
        )
        .await
        .is_err()
    );
    let asset = f
        .op(
            "B139",
            "asset.create",
            json!({"name":"Synthetic asset","serial_number":Uuid::new_v4().to_string()}),
            None,
        )
        .await
        .unwrap();
    let request = json!({"asset_id":id(&asset),"due_at":Utc::now()+Duration::days(1)});
    let (a, b) = tokio::join!(
        f.op("B139", "loan.checkout", request.clone(), None),
        f.op("B139", "loan.checkout", request, None)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let loan = a.or(b).unwrap();
    let receipt = OperationRequest {
        component_id: "B139".into(),
        action: "loan.return".into(),
        payload: json!({"id":id(&loan)}),
        idempotency_key: "asset-return-once".into(),
        expected_version: Some(1),
    };
    let returned = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), receipt.clone())
        .await
        .unwrap();
    assert_eq!(returned["data"]["status"], "returned");
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), receipt)
            .await
            .unwrap(),
        returned
    );
    assert_eq!(
        f.op("B139", "asset.get", json!({"id":id(&asset)}), None)
            .await
            .unwrap()["data"]["status"],
        "available"
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn forms_pin_definition_mask_fields_and_refuse_unauthorized_edits() {
    let f = Fixture::new().await;
    let definition = json!({"name":"Synthetic process","fields":[
        {"name":"public","field_type":"string","required":true,"visible_to":["cases.read","member"],"editable_by":["member"]},
        {"name":"private","field_type":"string","required":true,"visible_to":["admin"],"editable_by":["admin"]}],
        "transitions":[{"from":"draft","to":"submitted","roles":["member"]}]});
    let form = f
        .op("B140", "form.publish", definition.clone(), None)
        .await
        .unwrap();
    assert!(
        f.op(
            "B140",
            "case.create",
            json!({"form_id":id(&form),"fields":{"public":"one"}}),
            None
        )
        .await
        .is_err()
    );
    let case = f
        .op(
            "B140",
            "case.create",
            json!({"form_id":id(&form),"fields":{"public":"one","private":"secret"}}),
            None,
        )
        .await
        .unwrap();
    let (reader, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["cases.read"],
    )
    .await;
    let visible = op(
        &f,
        &reader,
        "B140",
        "case.get",
        json!({"id":id(&case)}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(visible["data"]["fields"], json!({"public":"one"}));
    assert!(visible["data"].get("definition").is_none());
    assert_eq!(
        op(
            &f,
            &reader,
            "B140",
            "case.update_fields",
            json!({"id":id(&case),"fields":{"private":"forbidden"}}),
            Some(1)
        )
        .await,
        Err(AppError::Forbidden)
    );
    let mut newer = definition;
    newer["form_id"] = json!(id(&form));
    newer["fields"][1]["field_type"] = json!("integer");
    f.op("B140", "form.publish", newer, Some(1)).await.unwrap();
    let changed = f
        .op(
            "B140",
            "case.update_fields",
            json!({"id":id(&case),"fields":{"private":"old-schema-text"}}),
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(changed["data"]["form_version"], 1);
    assert_eq!(changed["data"]["fields"]["private"], "old-schema-text");
    assert!(
        f.op(
            "B140",
            "case.update_fields",
            json!({"id":id(&case),"fields":{"unknown":true}}),
            Some(2)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op(
            "B140",
            "case.transition",
            json!({"id":id(&case),"to":"submitted"}),
            Some(2)
        )
        .await
        .unwrap()["data"]["status"],
        "submitted"
    );
}
