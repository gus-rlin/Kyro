mod support;

use kyro_app::{Actor, AppError, AppResult, OperationRequest};
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

async fn as_actor(
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

async fn workflow_operator(f: &mut Fixture) {
    let (actor, token) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        f.actor.principal_id(),
        &["app_admin", "compensation_manager", "feature_manager"],
    )
    .await;
    for permission in [
        "records:read",
        "fields:read",
        "workflow:start",
        "workflow:resume",
        "workflow:read",
        "approval:request",
        "approval:read",
        "config:write",
        "config:read",
        "feature:read",
        "compensation:read",
    ] {
        sqlx::query("INSERT INTO app_role_permissions(tenant_id,application_id,role,permission) VALUES($1,$2,'app_admin',$3)")
            .bind(actor.tenant_id()).bind(actor.application_id()).bind(permission).execute(&f.admin).await.unwrap();
    }
    f.actor = f.core.authenticate(&token).await.unwrap();
    f.token = token;
}

async fn data_schema(f: &Fixture, entity: &str, relationships: Value) {
    f.op(
        "B036",
        "migrate",
        json!({"entity":entity,"version":1,"definition":{
        "fields":{"name":{"type":"string","required":true,"unique":true,"max_length":64},
            "count":{"type":"integer","required":true},"secret":{"type":"string","readable":false}},
        "relationships":relationships}}),
        None,
    )
    .await
    .unwrap();
}

async fn counters(f: &Fixture) -> (i64, i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2),
        (SELECT count(*) FROM app_record_history WHERE tenant_id=$1 AND application_id=$2),
        (SELECT count(*) FROM app_events WHERE tenant_id=$1 AND application_id=$2),
        (SELECT count(*) FROM app_outbox WHERE tenant_id=$1 AND application_id=$2),
        (SELECT used_value FROM app_quotas WHERE tenant_id=$1 AND application_id=$2 AND quota_key='records')")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn typed_relations_and_batches_roll_back_all_observable_state() {
    let f = Fixture::new().await;
    data_schema(&f, "child", json!({})).await;
    data_schema(&f, "parent", json!({"child":{"target_entity":"child","cardinality":"one_to_one","on_target_delete":"restrict"}})).await;
    let parent = f
        .op(
            "B031",
            "create",
            json!({"entity":"parent","values":{"name":"parent","count":1,"secret":"private"}}),
            None,
        )
        .await
        .unwrap();
    let child = f
        .op(
            "B031",
            "create",
            json!({"entity":"child","values":{"name":"child","count":2}}),
            None,
        )
        .await
        .unwrap();
    assert!(parent["values"].get("secret").is_none());
    let relation =
        json!({"entity":"parent","id":parent["id"],"relationship":"child","target_id":child["id"]});
    assert_eq!(
        f.op("B032", "link", relation.clone(), None).await.unwrap()["linked"],
        true
    );
    let linked = f
        .op(
            "B032",
            "related",
            json!({"entity":"parent","id":parent["id"],"relationship":"child"}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(linked["items"], json!([child["id"]]));
    let before = counters(&f).await;
    assert!(f.op("B032", "link", relation.clone(), None).await.is_err());
    assert!(
        f.op(
            "B031",
            "delete",
            json!({"entity":"child","id":child["id"]}),
            Some(1)
        )
        .await
        .is_err()
    );
    let provisional = Uuid::new_v4();
    assert!(f.op("B033","batch",json!({"operations":[
        {"operation":"create","entity":"child","id":provisional,"values":{"name":"provisional","count":4}},
        {"operation":"update","entity":"parent","id":parent["id"],"expected_version":99,"values":{"count":9}}
    ]}),None).await.is_err());
    assert_eq!(counters(&f).await, before);
    assert_eq!(
        f.op(
            "B031",
            "get",
            json!({"entity":"child","id":provisional}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    let batch = f.op("B033","batch",json!({"operations":[
        {"operation":"unlink","entity":"parent","id":parent["id"],"relationship":"child","target_id":child["id"]},
        {"operation":"update","entity":"parent","id":parent["id"],"expected_version":1,"values":{"count":3}},
        {"operation":"delete","entity":"child","id":child["id"],"expected_version":1}
    ]}),None).await.unwrap();
    assert_eq!(batch["results"].as_array().unwrap().len(), 3);
    assert_eq!(
        f.op(
            "B031",
            "get",
            json!({"entity":"parent","id":parent["id"]}),
            None
        )
        .await
        .unwrap()["values"]["count"],
        3
    );
    assert_eq!(
        f.op(
            "B032",
            "related",
            json!({"entity":"parent","id":parent["id"],"relationship":"child"}),
            None
        )
        .await
        .unwrap()["items"],
        json!([])
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn published_cas_draft_conflicts_history_masking_and_cache_epoch() {
    let f = Fixture::new().await;
    data_schema(&f, "item", json!({})).await;
    let item = f
        .op(
            "B031",
            "create",
            json!({"entity":"item","values":{"name":"one","count":1,"secret":"hidden"}}),
            None,
        )
        .await
        .unwrap();
    let query = json!({"entity":"item"});
    assert_eq!(
        f.op("B039", "cache.query", query.clone(), None)
            .await
            .unwrap()["items"][0]["values"]["count"],
        1
    );
    let draft = json!({"entity":"item","id":item["id"],"base_version":1,"values":{"name":"one","count":8,"secret":"hidden"}});
    assert_eq!(
        f.op("B038", "draft.save", draft, None).await.unwrap()["draft_revision"],
        1
    );
    let change = |count| json!({"entity":"item","id":item["id"],"values":{"count":count}});
    let (a, b) = tokio::join!(
        f.op("B034", "compare_and_swap", change(2), Some(1)),
        f.op("B034", "compare_and_swap", change(3), Some(1))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let winner = a.or(b).unwrap();
    assert_eq!(winner["version"], 2);
    assert_eq!(
        f.op("B039", "cache.query", query.clone(), None)
            .await
            .unwrap()["items"][0]["values"]["count"],
        winner["values"]["count"]
    );
    assert!(
        f.op(
            "B038",
            "draft.promote",
            json!({"entity":"item","id":item["id"],"draft_revision":1}),
            Some(2)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op(
            "B031",
            "get",
            json!({"entity":"item","id":item["id"]}),
            None
        )
        .await
        .unwrap(),
        winner
    );
    let draft = json!({"entity":"item","id":item["id"],"base_version":2,"values":{"name":"one","count":8,"secret":"hidden"}});
    assert!(
        f.op(
            "B038",
            "draft.discard",
            json!({"entity":"item","id":item["id"]}),
            Some(9)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op(
            "B038",
            "draft.discard",
            json!({"entity":"item","id":item["id"]}),
            Some(1)
        )
        .await
        .unwrap()["discarded"],
        true
    );
    assert_eq!(
        f.op("B038", "draft.save", draft, None).await.unwrap()["draft_revision"],
        1
    );
    let published = f
        .op(
            "B038",
            "draft.promote",
            json!({"entity":"item","id":item["id"],"draft_revision":1}),
            Some(2),
        )
        .await
        .unwrap();
    assert_eq!(published["version"], 3);
    assert_eq!(published["values"]["count"], 8);
    assert_eq!(
        f.op("B039", "cache.query", query, None).await.unwrap()["items"][0],
        published
    );
    let history = f
        .op(
            "B037",
            "history",
            json!({"entity":"item","id":item["id"]}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(history["items"].as_array().unwrap().len(), 3);
    assert!(!history.to_string().contains("hidden"));
    assert!(
        f.op(
            "B035",
            "query",
            json!({"entity":"item","field":"secret","value":"hidden"}),
            None
        )
        .await
        .is_err()
    );
    assert!(f.op("B036","migrate",json!({"entity":"item","version":2,"definition":{"fields":{"mandatory":{"type":"string","required":true}}}}),Some(1)).await.is_err());
    assert_eq!(
        f.op("B036", "inspect", json!({"entity":"item"}), None)
            .await
            .unwrap()["version"],
        1
    );
    assert_eq!(
        f.op("B039", "cache.invalidate", json!({"entity":"item"}), None)
            .await
            .unwrap()["invalidated"],
        true
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn exact_values_dst_and_declarative_rules_obey_field_permissions() {
    let mut f = Fixture::new().await;
    workflow_operator(&mut f).await;
    assert_eq!(f.op("B041","add_amount",json!({"left":{"minor_units":9007199254740993_i64,"currency":"EUR"},"right":{"minor_units":2,"currency":"EUR"}}),None).await.unwrap()["minor_units"],9007199254740995_i64);
    for pair in [
        json!({"left":{"minor_units":i64::MAX,"currency":"EUR"},"right":{"minor_units":1,"currency":"EUR"}}),
        json!({"left":{"minor_units":1,"currency":"EUR"},"right":{"minor_units":1,"currency":"USD"}}),
    ] {
        assert!(f.op("B041", "add_amount", pair, None).await.is_err());
    }
    assert!(
        f.op(
            "B041",
            "validate_amount",
            json!({"minor_units":1.5,"currency":"EUR"}),
            None
        )
        .await
        .is_err()
    );
    for time in ["2026-03-29T02:30:00", "2026-10-25T02:30:00"] {
        assert!(
            f.op(
                "B042",
                "resolve_local_time",
                json!({"local_datetime":time,"time_zone":"Europe/Paris"}),
                None
            )
            .await
            .is_err()
        );
    }
    let early = f.op("B042","resolve_local_time",json!({"local_datetime":"2026-10-25T02:30:00","time_zone":"Europe/Paris","disambiguation":"earlier"}),None).await.unwrap();
    let late = f.op("B042","resolve_local_time",json!({"local_datetime":"2026-10-25T02:30:00","time_zone":"Europe/Paris","disambiguation":"later"}),None).await.unwrap();
    assert_eq!(early["instant"], "2026-10-25T00:30:00Z");
    assert_eq!(late["instant"], "2026-10-25T01:30:00Z");
    let record = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert(
        "rule_item",
        record,
        json!({"amount":7,"private":"confidential"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let result = f.op("B043","evaluate",json!({"record_kind":"rule_item","record_id":record,"predicate":{"op":"compare","field":"amount","comparison":"gte","value":5}}),None).await.unwrap();
    assert_eq!(result["matches"], true);
    assert!(
        f.op(
            "B043",
            "evaluate",
            json!({"record_kind":"rule_item","record_id":record,"predicate":"return true;"}),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(f.op("B044","compute",json!({"record_kind":"rule_item","record_id":record,"fields":{
        "double":{"op":"add","left":{"op":"source","field":"amount"},"right":{"op":"source","field":"amount"}},
        "next":{"op":"add","left":{"op":"reference","field":"double"},"right":{"op":"literal","value":1}}}}),None).await.unwrap()["fields"]["next"],15);
    assert!(f.op("B044","compute",json!({"record_kind":"rule_item","record_id":record,"fields":{"a":{"op":"reference","field":"b"},"b":{"op":"reference","field":"a"}}}),None).await.is_err());
    let (limited, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["rule_reader"],
    )
    .await;
    for permission in ["records:read", "field:read:amount"] {
        sqlx::query("INSERT INTO app_role_permissions(tenant_id,application_id,role,permission) VALUES($1,$2,'rule_reader',$3)")
            .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(permission).execute(&f.admin).await.unwrap();
    }
    assert_eq!(as_actor(&f,&limited,"B043","evaluate",json!({"record_kind":"rule_item","record_id":record,"predicate":{"op":"exists","field":"private"}}),None).await,Err(AppError::Forbidden));
    assert_eq!(as_actor(&f,&limited,"B044","compute",json!({"record_kind":"rule_item","record_id":record,"fields":{"p":{"op":"source","field":"private"}}}),None).await,Err(AppError::Forbidden));
    let mut tx = f.core.begin_read(f.actor.clone()).await.unwrap();
    assert_eq!(tx.get("rule_item", record).await.unwrap().version, 1);
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn state_workflow_and_approval_persist_cas_and_separate_people() {
    let mut f = Fixture::new().await;
    workflow_operator(&mut f).await;
    let machine = Uuid::new_v4();
    let subject = Uuid::new_v4();
    f.op("B045","register_machine",json!({"machine_id":machine,"version":1,"states":["new","done"],"transitions":[{"from":"new","to":"done"}]}),None).await.unwrap();
    f.op(
        "B045",
        "create_subject",
        json!({"subject_id":subject,"machine_id":machine,"initial_state":"new"}),
        None,
    )
    .await
    .unwrap();
    assert!(
        f.op(
            "B045",
            "transition",
            json!({"subject_id":subject,"machine_id":machine,"from":"new","to":"missing"}),
            Some(1)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op(
            "B045",
            "transition",
            json!({"subject_id":subject,"machine_id":machine,"from":"new","to":"done"}),
            Some(1)
        )
        .await
        .unwrap()["data"]["state"],
        "done"
    );
    assert!(
        f.op(
            "B045",
            "transition",
            json!({"subject_id":subject,"machine_id":machine,"from":"new","to":"done"}),
            Some(1)
        )
        .await
        .is_err()
    );
    let definition = Uuid::new_v4();
    let instance = Uuid::new_v4();
    f.op("B046","register_definition",json!({"definition_record_id":definition,"definition_id":Uuid::new_v4(),"version":1,"steps":[{"kind":"wait","key":"approved"},{"kind":"complete"}]}),None).await.unwrap();
    assert_eq!(f.op("B046","start",json!({"instance_id":instance,"definition_record_id":definition,"definition_version":1}),None).await.unwrap()["data"]["status"],"waiting");
    assert!(
        f.op(
            "B046",
            "resume",
            json!({"instance_id":instance,"signal_key":"wrong","signal":true}),
            Some(1)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op(
            "B046",
            "get_instance",
            json!({"instance_id":instance}),
            None
        )
        .await
        .unwrap()["version"],
        1
    );
    assert_eq!(
        f.op(
            "B046",
            "resume",
            json!({"instance_id":instance,"signal_key":"approved","signal":true}),
            Some(1)
        )
        .await
        .unwrap()["data"]["status"],
        "completed"
    );
    let replay = f
        .op(
            "B046",
            "resume",
            json!({"instance_id":instance,"signal_key":"approved","signal":true}),
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(replay["version"], 2);
    assert_eq!(replay["data"]["signals"].as_array().unwrap().len(), 1);
    let policy = Uuid::new_v4();
    let approval = Uuid::new_v4();
    f.op(
        "B047",
        "register_policy",
        json!({"policy_id":policy,"version":1,"approver_roles":["app_admin","approver"]}),
        None,
    )
    .await
    .unwrap();
    f.op("B047","request",json!({"approval_id":approval,"policy_id":policy,"resource_id":subject,"reason":"synthetic review"}),None).await.unwrap();
    assert_eq!(
        f.op("B047", "approve", json!({"approval_id":approval}), Some(1))
            .await,
        Err(AppError::Forbidden)
    );
    let (approver, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["approver"],
    )
    .await;
    assert_eq!(
        as_actor(
            &f,
            &approver,
            "B047",
            "approve",
            json!({"approval_id":approval,"comment":"checked"}),
            Some(1)
        )
        .await
        .unwrap()["data"]["state"],
        "approved"
    );
    assert!(
        as_actor(
            &f,
            &approver,
            "B047",
            "reject",
            json!({"approval_id":approval}),
            Some(2)
        )
        .await
        .is_err()
    );
    let saved = f
        .op("B047", "get_request", json!({"approval_id":approval}), None)
        .await
        .unwrap();
    assert_eq!(
        saved["data"]["decided_by"],
        approver.principal_id().to_string()
    );
    assert_eq!(saved["version"], 2);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn compensation_configuration_and_flags_fail_closed_and_apply_once() {
    let mut f = Fixture::new().await;
    workflow_operator(&mut f).await;
    let target = Uuid::new_v4();
    let plan = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert("sample_item", target, json!({"count":1}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    f.op("B048","prepare",json!({"compensation_id":plan,"target_kind":"sample_item","target_id":target,"expected_target_version":1,"restore_data":{"count":0},"reason":"explicit rollback"}),None).await.unwrap();
    assert!(
        f.op("B048", "execute", json!({"compensation_id":plan}), Some(99))
            .await
            .is_err()
    );
    let completed = f
        .op("B048", "execute", json!({"compensation_id":plan}), Some(1))
        .await
        .unwrap();
    assert_eq!(completed["data"]["completed_target_version"], 2);
    assert_eq!(
        f.op("B048", "execute", json!({"compensation_id":plan}), Some(1))
            .await
            .unwrap(),
        completed
    );
    let stale = Uuid::new_v4();
    f.op("B048","prepare",json!({"compensation_id":stale,"target_kind":"sample_item","target_id":target,"expected_target_version":1,"restore_data":{"count":9},"reason":"stale plan"}),None).await.unwrap();
    assert!(
        f.op("B048", "execute", json!({"compensation_id":stale}), Some(1))
            .await
            .is_err()
    );
    let mut tx = f.core.begin_read(f.actor.clone()).await.unwrap();
    assert_eq!(
        tx.get("sample_item", target).await.unwrap().data,
        json!({"count":0})
    );
    tx.commit().await.unwrap();
    let flag = Uuid::new_v4();
    assert_eq!(
        f.op(
            "B049",
            "get_flag",
            json!({"flag_id":flag,"key":"can_export"}),
            None
        )
        .await
        .unwrap()["enabled"],
        false
    );
    f.op(
        "B049",
        "set_flag",
        json!({"flag_id":flag,"key":"can_export","enabled":true}),
        None,
    )
    .await
    .unwrap();
    assert!(
        f.op(
            "B049",
            "set_flag",
            json!({"flag_id":flag,"key":"can_export","enabled":false}),
            Some(9)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op(
            "B049",
            "get_flag",
            json!({"flag_id":flag,"key":"can_export"}),
            None
        )
        .await
        .unwrap()["enabled"],
        true
    );
    let schema = Uuid::new_v4();
    let config = Uuid::new_v4();
    f.op("B050","register_schema",json!({"schema_id":schema,"version":1,"fields":{"capacity":{"kind":"integer","required":true,"minimum":1,"maximum":5},"enabled":{"kind":"boolean","required":true}}}),None).await.unwrap();
    let good = json!({"capacity":3,"enabled":true});
    assert_eq!(
        f.op(
            "B050",
            "validate",
            json!({"schema_id":schema,"config":good}),
            None
        )
        .await
        .unwrap()["valid"],
        true
    );
    for invalid in [
        json!({"capacity":6,"enabled":true}),
        json!({"capacity":3.5,"enabled":true}),
        json!({"capacity":3,"enabled":true,"sql":"DROP TABLE"}),
    ] {
        assert!(
            f.op(
                "B050",
                "set_config",
                json!({"config_id":config,"schema_id":schema,"config":invalid}),
                None
            )
            .await
            .is_err()
        );
    }
    assert_eq!(
        f.op("B050", "get_config", json!({"config_id":config}), None)
            .await,
        Err(AppError::NotFound)
    );
    let saved = f
        .op(
            "B050",
            "set_config",
            json!({"config_id":config,"schema_id":schema,"config":good}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(saved["data"]["schema_version"], 1);
    assert!(
        f.op(
            "B050",
            "set_config",
            json!({"config_id":config,"schema_id":schema,"config":{"capacity":4,"enabled":false}}),
            Some(99)
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op("B050", "get_config", json!({"config_id":config}), None)
            .await
            .unwrap(),
        saved
    );
}
