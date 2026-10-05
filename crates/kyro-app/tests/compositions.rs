mod support;
use chrono::{Datelike, Days, TimeZone, Utc};
use kyro_app::{Actor, AppError, OperationRequest};
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn opposite_multi_product_baskets_reserve_without_deadlock_and_refuse_duplicates() {
    let f = Fixture::new().await;
    let mut products = Vec::new();
    for name in ["first", "second"] {
        let product = f
            .op(
                "B121",
                "create",
                json!({"sku":Uuid::new_v4().to_string(),"name":name,"inventory_tracked":true}),
                None,
            )
            .await
            .unwrap();
        f.op("B130", "adjust", json!({"product_id":id(&product),"delta_on_hand":10,"reason":"synthetic initial stock"}), Some(1)).await.unwrap();
        f.op("B122", "set_price", json!({"product_id":id(&product),"currency":"EUR","amount_minor":100,"interval_unit":"one_time","interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}), None).await.unwrap();
        f.op("B121", "publish", json!({"id":id(&product)}), Some(1))
            .await
            .unwrap();
        products.push(id(&product));
    }
    products.sort();
    let mut quotes = Vec::new();
    for basket in [products.clone(), products.iter().rev().copied().collect()] {
        quotes.push(f.op("B123", "quote", json!({"currency":"EUR","items":basket.iter().map(|product| json!({"product_id":product,"quantity":2})).collect::<Vec<_>>()}), None).await.unwrap());
    }
    // Hold both balances before dispatch, so both orders acquire their first
    // product lock before any reservation proceeds. Opposite lock orders then
    // deterministically expose the original cycle rather than relying on luck.
    let mut gate = f.admin.begin().await.unwrap();
    sqlx::query("SELECT product_id FROM app_commerce_inventory WHERE tenant_id=$1 AND product_id=ANY($2) ORDER BY product_id FOR UPDATE").bind(f.actor.tenant_id()).bind(&products).fetch_all(&mut *gate).await.unwrap();
    let release = async {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%app_commerce_%FOR UPDATE%'").fetch_one(&f.admin).await.unwrap();
                if waiting >= 2 { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("both orders must reach a controlled inventory/product wait");
        gate.commit().await.unwrap();
    };
    let (a, b, ()) = tokio::join!(
        f.op("B124", "create", json!({"quote_id":id(&quotes[0])}), None),
        f.op("B124", "create", json!({"quote_id":id(&quotes[1])}), None),
        release
    );
    assert!(
        a.is_ok() && b.is_ok(),
        "both baskets have sufficient stock: {a:?} / {b:?}"
    );
    for product in &products {
        let balance = f
            .op("B130", "get", json!({"id":product}), None)
            .await
            .unwrap();
        assert_eq!(balance["on_hand"], 10);
        assert_eq!(balance["reserved"], 4);
    }
    let rows: (i64,i64) = sqlx::query_as("SELECT count(*),sum(quantity)::bigint FROM app_commerce_inventory_reservations WHERE tenant_id=$1 AND status='reserved'").bind(f.actor.tenant_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(rows, (4, 8));
    assert_eq!(f.op("B123", "quote", json!({"currency":"EUR","items":[{"product_id":products[0],"quantity":1},{"product_id":products[0],"quantity":1}]}), None).await, Err(AppError::invalid("duplicate_quote_product")));
}

async fn as_actor(
    f: &Fixture,
    actor: Actor,
    component: &str,
    action: &str,
    payload: Value,
    version: Option<i64>,
) -> Result<Value, AppError> {
    f.dispatcher
        .dispatch(
            &f.core,
            actor,
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
async fn stock_five_units_accept_exactly_five_of_eight_concurrent_orders() {
    let f = Fixture::new().await;
    let product = f.op("B121","create",json!({"sku":Uuid::new_v4().to_string(),"name":"synthetic item","inventory_tracked":true}),None).await.unwrap();
    f.op(
        "B130",
        "adjust",
        json!({"product_id":id(&product),"delta_on_hand":5,"reason":"initial stock"}),
        Some(1),
    )
    .await
    .unwrap();
    f.op("B122","set_price",json!({"product_id":id(&product),"currency":"EUR","amount_minor":2500,"interval_unit":"one_time","interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}),None).await.unwrap();
    f.op("B121", "publish", json!({"id":id(&product)}), Some(1))
        .await
        .unwrap();
    let mut people = Vec::new();
    for _ in 0..8 {
        people.push(
            Fixture::session(
                &f.admin,
                &f.core,
                f.actor.tenant_id(),
                f.actor.application_id(),
                Uuid::new_v4(),
                &["member"],
            )
            .await
            .0,
        );
    }
    let mut quotes = Vec::new();
    for actor in &people {
        quotes.push(
            as_actor(
                &f,
                actor.clone(),
                "B123",
                "quote",
                json!({"currency":"EUR","items":[{"product_id":id(&product),"quantity":1}]}),
                None,
            )
            .await
            .unwrap(),
        );
    }
    let replies =
        futures_util::future::join_all(people.into_iter().zip(quotes).map(|(actor, quote)| {
            let fixture = &f;
            async move {
                as_actor(
                    fixture,
                    actor,
                    "B124",
                    "create",
                    json!({"quote_id":id(&quote)}),
                    None,
                )
                .await
            }
        }))
        .await;
    let errors: Vec<_> = replies
        .iter()
        .filter_map(|r| r.as_ref().err().map(AppError::code))
        .collect();
    assert_eq!(
        replies.iter().filter(|r| r.is_ok()).count(),
        5,
        "errors: {errors:?}"
    );
    assert_eq!(errors.len(), 3);
    assert!(
        replies
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(e, AppError::Quota)),
        "errors: {errors:?}"
    );
    let inventory = f
        .op("B130", "get", json!({"id":id(&product)}), None)
        .await
        .unwrap();
    assert_eq!(inventory["on_hand"], 5);
    assert_eq!(inventory["reserved"], 5);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn promotion_discount_and_concurrent_redemption_cancel_and_expiry_are_server_bound() {
    let f = Fixture::new().await;
    let product=f.op("B121","create",json!({"sku":"promotion-tests","name":"Synthetic promotion product","inventory_tracked":false}),None).await.unwrap();
    f.op("B122","set_price",json!({"product_id":id(&product),"currency":"EUR","amount_minor":1999,"interval_unit":"one_time","interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}),None).await.unwrap();
    f.op("B121", "publish", json!({"id":id(&product)}), Some(1))
        .await
        .unwrap();
    let promo=f.op("B129","create",json!({"code":" welcome ","discount_basis_points":2500,"minimum_subtotal_minor":2000,"maximum_redemptions":1,"valid_from":"2020-01-01T00:00:00Z","valid_until":"2099-01-01T00:00:00Z"}),None).await.unwrap();
    let payload = json!({"currency":"EUR","items":[{"product_id":id(&product),"quantity":2}],"promotion_code":"welcome"});
    let first = f.op("B123", "quote", payload.clone(), None).await.unwrap();
    let second = f.op("B123", "quote", payload.clone(), None).await.unwrap();
    assert_eq!(first["subtotal_minor"], 3998);
    assert_eq!(first["discount_minor"], 999);
    assert_eq!(first["total_minor"], 2999);
    assert_eq!(f.op("B123","quote",json!({"currency":"EUR","items":[{"product_id":id(&product),"quantity":1}],"promotion_code":"WELCOME"}),None).await,Err(AppError::Quota));
    let (a, b) = tokio::join!(
        f.op("B124", "create", json!({"quote_id":id(&first)}), None),
        f.op("B124", "create", json!({"quote_id":id(&second)}), None)
    );
    let (winner, waiting_quote) = match (a, b) {
        (Ok(a), Err(AppError::Quota)) => (a, second),
        (Err(AppError::Quota), Ok(b)) => (b, first),
        result => panic!("one promotion slot must have one winner: {result:?}"),
    };
    assert_eq!(
        f.op("B129", "get", json!({"id":id(&promo)}), None)
            .await
            .unwrap()["redemption_count"],
        1
    );
    assert_eq!(
        f.op("B123", "quote", payload.clone(), None).await,
        Err(AppError::Quota)
    );
    let cancellation = OperationRequest {
        component_id: "B124".into(),
        action: "cancel".into(),
        payload: json!({"order_id":id(&winner)}),
        expected_version: Some(1),
        idempotency_key: "promotion-release-once".into(),
    };
    let cancelled = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), cancellation.clone())
        .await
        .unwrap();
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), cancellation)
            .await
            .unwrap(),
        cancelled
    );
    assert_eq!(
        f.op("B129", "get", json!({"id":id(&promo)}), None)
            .await
            .unwrap()["redemption_count"],
        0
    );
    let retried = f
        .op(
            "B124",
            "create",
            json!({"quote_id":id(&waiting_quote)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(retried["total_minor"], 2999);
    // A provider cannot take a second slot, nor can a caller supply the price.
    assert!(f.op("B123","quote",json!({"currency":"EUR","items":[{"product_id":id(&product),"quantity":2,"unit_amount_minor":1}]}),None).await.is_err());
    assert!(f.op("B129","create",json!({"code":"invalid-both","discount_basis_points":2500,"fixed_discount_minor":100,"minimum_subtotal_minor":0,"valid_from":"2020-01-01T00:00:00Z"}),None).await.is_err());
    f.op("B129","create",json!({"code":"expired","fixed_discount_minor":99999,"minimum_subtotal_minor":0,"valid_from":"2020-01-01T00:00:00Z","valid_until":"2021-01-01T00:00:00Z"}),None).await.unwrap();
    assert_eq!(f.op("B123","quote",json!({"currency":"EUR","items":[{"product_id":id(&product),"quantity":1}],"promotion_code":"expired"}),None).await,Err(AppError::NotFound));
    let fixed=f.op("B129","create",json!({"code":"free","fixed_discount_minor":99999,"minimum_subtotal_minor":0,"valid_from":"2020-01-01T00:00:00Z"}),None).await.unwrap();
    let free=f.op("B123","quote",json!({"currency":"EUR","items":[{"product_id":id(&product),"quantity":1}],"promotion_code":"free"}),None).await.unwrap();
    assert_eq!(free["discount_minor"], 1999);
    assert_eq!(free["total_minor"], 0);
    let order = f
        .op("B124", "create", json!({"quote_id":id(&free)}), None)
        .await
        .unwrap();
    assert_eq!(order["status"], "paid");
    assert_eq!(
        f.op("B129", "get", json!({"id":id(&fixed)}), None)
            .await
            .unwrap()["redemption_count"],
        1
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn support_teams_filter_notes_reads_assignments_and_live_group_revocation() {
    let f = Fixture::new().await;
    let team = f
        .op("B014", "create", json!({"name":"support A"}), None)
        .await
        .unwrap();
    let other_team = f
        .op("B014", "create", json!({"name":"support B"}), None)
        .await
        .unwrap();
    let (requester, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let (agent, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member", "support.manage"],
    )
    .await;
    let (other, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member", "support.manage"],
    )
    .await;
    for member in [&agent, &other] {
        let invitation=f.op("B013","invite",json!({"target_principal_id":member.principal_id(),"role":"member","expires_in_hours":1}),None).await.unwrap();
        as_actor(
            &f,
            member.clone(),
            "B013",
            "accept",
            json!({"token":invitation["token"]}),
            None,
        )
        .await
        .unwrap();
    }
    for (group, principal) in [
        (id(&team), agent.principal_id()),
        (id(&other_team), other.principal_id()),
    ] {
        f.op(
            "B014",
            "member_add",
            json!({"group_id":group,"principal_id":principal}),
            None,
        )
        .await
        .unwrap();
    }
    let ticket = as_actor(&f,requester.clone(),"B133","ticket.create",json!({"title":"synthetic support","description":"public question","priority":"normal","team_id":id(&team)}),None).await.unwrap();
    let note = as_actor(
        &f,
        agent.clone(),
        "B133",
        "ticket.add_internal_note",
        json!({"id":id(&ticket),"body":"private-team-A-note"}),
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(note["data"]["messages"].as_array().unwrap().len(), 1);
    let customer = as_actor(
        &f,
        requester.clone(),
        "B133",
        "ticket.get",
        json!({"id":id(&ticket)}),
        None,
    )
    .await
    .unwrap();
    assert!(customer["data"]["messages"].as_array().unwrap().is_empty());
    assert!(customer["data"].get("assignee_id").is_none());
    assert!(!customer.to_string().contains("private-team-A-note"));
    assert!(
        as_actor(
            &f,
            other.clone(),
            "B133",
            "ticket.get",
            json!({"id":id(&ticket)}),
            None
        )
        .await
        .is_err()
    );
    assert!(
        as_actor(
            &f,
            other.clone(),
            "B133",
            "ticket.assign",
            json!({"id":id(&ticket),"assignee_id":other.principal_id()}),
            Some(2)
        )
        .await
        .is_err()
    );
    assert!(
        as_actor(
            &f,
            agent.clone(),
            "B133",
            "ticket.assign",
            json!({"id":id(&ticket),"assignee_id":other.principal_id()}),
            Some(2)
        )
        .await
        .is_err()
    );
    let assigned = as_actor(
        &f,
        agent.clone(),
        "B133",
        "ticket.assign",
        json!({"id":id(&ticket),"assignee_id":agent.principal_id()}),
        Some(2),
    )
    .await
    .unwrap();
    assert_eq!(assigned["version"], 3);
    let list = as_actor(&f, other, "B133", "ticket.list", json!({"limit":1}), None)
        .await
        .unwrap();
    assert!(list["items"].as_array().unwrap().is_empty());
    f.op(
        "B014",
        "member_remove",
        json!({"group_id":id(&team),"principal_id":agent.principal_id()}),
        None,
    )
    .await
    .unwrap();
    assert!(
        as_actor(
            &f,
            agent,
            "B133",
            "ticket.get",
            json!({"id":id(&ticket)}),
            None
        )
        .await
        .is_err()
    );
    assert!(
        as_actor(
            &f,
            requester,
            "B133",
            "ticket.get",
            json!({"id":id(&ticket)}),
            None
        )
        .await
        .is_ok()
    );
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn booking_capacity_waitlist_and_establishment_acl() {
    let f = Fixture::new().await;
    let day = Utc::now()
        .date_naive()
        .checked_add_days(Days::new(2))
        .unwrap();
    let establishment = f
        .op(
            "B111",
            "create_establishment",
            json!({"name":"site","timezone":"UTC"}),
            None,
        )
        .await
        .unwrap();
    let site = Uuid::parse_str(establishment["id"].as_str().unwrap()).unwrap();
    let (reader, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let resource =
        json!({"establishment_id":site,"category":"service","name":"appointment","capacity":1});
    assert!(
        as_actor(
            &f,
            reader.clone(),
            "B111",
            "create_resource",
            resource.clone(),
            None
        )
        .await
        .is_err()
    );
    let resource = f
        .op("B111", "create_resource", resource, None)
        .await
        .unwrap();
    let rid = id(&resource);
    let availability=f.op("B112","set_availability",json!({"resource_id":rid,"weekday":day.weekday().num_days_from_monday(),"timezone":"UTC","local_start":"09:00:00","local_end":"18:00:00","valid_from":day,"valid_until":day}),None).await.unwrap();
    let slot=f.op("B113","create_slot",json!({"resource_id":rid,"availability_id":id(&availability),"starts_at":Utc.from_utc_datetime(&day.and_hms_opt(10,0,0).unwrap()),"ends_at":Utc.from_utc_datetime(&day.and_hms_opt(11,0,0).unwrap()),"capacity":1}),None).await.unwrap();
    let slot_id = id(&slot);
    let request = json!({"slot_id":slot_id,"units":1});
    let (one, two) = tokio::join!(
        as_actor(
            &f,
            f.actor.clone(),
            "B114",
            "reserve",
            request.clone(),
            None
        ),
        as_actor(&f, reader.clone(), "B114", "reserve", request.clone(), None)
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    let (booking, winner, waiting) = if let Ok(one) = one {
        (one, f.actor.clone(), reader.clone())
    } else {
        (two.unwrap(), reader.clone(), f.actor.clone())
    };
    let wait = as_actor(&f, waiting.clone(), "B116", "join_waitlist", request, None)
        .await
        .unwrap();
    assert_eq!(wait["status"], "waiting");
    as_actor(
        &f,
        winner,
        "B115",
        "cancel_booking",
        json!({"booking_id":booking["booking_id"]}),
        None,
    )
    .await
    .unwrap();
    let list = f
        .op("B116", "list_waitlist", json!({"slot_id":slot_id}), None)
        .await
        .unwrap();
    assert_eq!(list["waitlist"][0]["status"], "promoted");
    let reserved:i32=sqlx::query_scalar("SELECT reserved_units FROM app_sched_slots WHERE tenant_id=$1 AND application_id=$2 AND id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(slot_id).fetch_one(&f.admin).await.unwrap();
    assert_eq!(reserved, 1);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn expense_exact_money_separate_approver_and_private_reads() {
    let f = Fixture::new().await;
    let expense=f.op("B137","expense.create",json!({"amount":"12.34","currency":"EUR","merchant":"synthetic merchant","incurred_on":"2026-10-05","attachment_ids":[]}),None).await.unwrap();
    let eid = id(&expense);
    assert_eq!(expense["data"]["amount_minor"], 1234);
    assert!(f.op("B137","expense.create",json!({"amount":"12.345","currency":"EUR","merchant":"synthetic","incurred_on":"2026-10-05","attachment_ids":[]}),None).await.is_err());
    f.op("B137", "expense.submit", json!({"id":eid}), Some(1))
        .await
        .unwrap();
    assert_eq!(
        f.op("B137", "expense.approve", json!({"id":eid}), Some(2))
            .await,
        Err(AppError::Forbidden)
    );
    let (reader, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    assert!(
        as_actor(&f, reader, "B137", "expense.get", json!({"id":eid}), None)
            .await
            .is_err()
    );
    let (approver, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["expenses.approve"],
    )
    .await;
    let approved = as_actor(
        &f,
        approver,
        "B137",
        "expense.approve",
        json!({"id":eid}),
        Some(2),
    )
    .await
    .unwrap();
    assert_eq!(approved["data"]["status"], "approved");
    assert!(
        f.op(
            "B137",
            "expense.attach",
            json!({"id":eid,"attachment_ids":[]}),
            Some(3)
        )
        .await
        .is_err()
    );
}
