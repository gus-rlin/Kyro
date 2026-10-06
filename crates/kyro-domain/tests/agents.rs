use kyro_domain::{
    Error,
    agents::*,
    spec::{AppNode, AppSpec, ChangeOperation, ChangeSet},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn task(id: &str, node: &str) -> TaskContract {
    TaskContract {
        id: id.into(),
        objective: format!("Configure {node}"),
        components: vec![ComponentRef {
            id: "B031".into(),
            version: "0.1.2".into(),
        }],
        reads: BTreeSet::new(),
        writes: BTreeSet::from([Resource::Node { id: node.into() }]),
        dependencies: BTreeSet::new(),
        invariants: BTreeSet::new(),
        capabilities: BTreeSet::new(),
        protected_criteria: BTreeSet::new(),
        max_attempts: 1,
        deterministic: None,
    }
}
fn plan(tasks: Vec<TaskContract>) -> Plan {
    Plan {
        objective: "Synthetic application".into(),
        tasks,
        missing_capabilities: vec![],
    }
}
#[test]
fn resource_node_ids_follow_appspec_rules_while_task_ids_stay_restricted() {
    let ids = [
        "customer:profile".to_string(),
        "client:profil-équipe".to_string(),
        "x".repeat(65),
        "é".repeat(64),
        " ".to_string(),
    ];
    for id in ids {
        let spec = AppSpec {
            nodes: vec![AppNode {
                id: id.clone(),
                kind: "B031".into(),
                properties: json!({"version":"0.1.2"}).as_object().unwrap().clone(),
            }],
            ..Default::default()
        };
        spec.validate().unwrap();
        for resource in [
            Resource::Node { id: id.clone() },
            Resource::Property {
                id: id.clone(),
                key: "configuration".into(),
            },
        ] {
            let mut t = task("configure", &id);
            t.reads.insert(resource.clone());
            t.writes = BTreeSet::from([resource]);
            let p = plan(vec![t.clone()]);
            p.validate(&Limits::default()).unwrap();
            p.validate_result(
                &t,
                &ChangeSet {
                    operations: vec![ChangeOperation::SetProperty {
                        node_id: id.clone(),
                        key: "configuration".into(),
                        value: json!({}),
                    }],
                },
                &spec,
            )
            .unwrap();
        }
        if id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            assert!(
                plan(vec![task(&id, "valid-node")])
                    .validate(&Limits::default())
                    .is_err()
            );
        }
    }
    for id in [
        "".to_string(),
        "x".repeat(129),
        "é".repeat(65),
        "node\n".to_string(),
        "node\u{0085}".to_string(),
    ] {
        for resource in [
            Resource::Node { id: id.clone() },
            Resource::Property {
                id: id.clone(),
                key: "configuration".into(),
            },
        ] {
            let mut t = task("configure", "valid-node");
            t.reads.insert(resource.clone());
            assert!(plan(vec![t.clone()]).validate(&Limits::default()).is_err());
            t.reads.clear();
            t.writes = BTreeSet::from([resource]);
            assert!(plan(vec![t]).validate(&Limits::default()).is_err());
        }
    }
    plan(vec![task(&"x".repeat(64), "valid-node")])
        .validate(&Limits::default())
        .unwrap();
}
#[test]
fn graph_refuses_cycles_and_missing_contracts() {
    let mut a = task("a", "a");
    let mut b = task("b", "b");
    a.dependencies.insert("b".into());
    b.dependencies.insert("a".into());
    assert!(plan(vec![a, b]).validate(&Limits::default()).is_err());
    let mut a = task("a", "a");
    a.dependencies.insert("absent".into());
    assert!(plan(vec![a]).validate(&Limits::default()).is_err());
    let mut a = task("a", "a");
    a.writes.clear();
    assert!(plan(vec![a]).validate(&Limits::default()).is_err());
}
#[test]
fn existing_nodes_cannot_change_to_an_undeclared_version_or_component() {
    let t = task("records", "records");
    let p = plan(vec![t.clone()]);
    let spec:AppSpec=serde_json::from_value(json!({"schema_version":1,"nodes":[{"id":"records","kind":"B031","properties":{"version":"0.1.2","configuration":{}}}],"preferences":{}})).unwrap();
    let edit = |version: &str| ChangeSet {
        operations: vec![ChangeOperation::SetProperty {
            node_id: "records".into(),
            key: "version".into(),
            value: json!(version),
        }],
    };
    assert!(p.validate_result(&t, &edit("0.1.2"), &spec).is_ok());
    assert!(matches!(
        p.validate_result(&t, &edit("0.9.0"), &spec),
        Err(Error::Forbidden)
    ));
    let mut foreign = spec.clone();
    foreign.nodes[0].kind = "B081".into();
    assert!(matches!(
        p.validate_result(&t, &edit("0.1.2"), &foreign),
        Err(Error::Forbidden)
    ));
    let property = Resource::Property {
        id: "records".into(),
        key: "unknown".into(),
    };
    assert_ne!(property.value(&AppSpec::default()), property.value(&spec));
    assert_ne!(property.value(&foreign), property.value(&spec));
}
#[test]
fn shared_invariant_orders_different_nodes_and_read_write_conflicts() {
    let mut a = task("a", "first-file");
    let mut b = task("b", "other-file");
    a.invariants.insert("stock_capacity".into());
    b.invariants = a.invariants.clone();
    assert!(matches!(
        plan(vec![a.clone(), b.clone()]).validate(&Limits::default()),
        Err(Error::Conflict(_))
    ));
    b.dependencies.insert("a".into());
    assert!(
        plan(vec![a.clone(), b.clone()])
            .validate(&Limits::default())
            .is_ok()
    );
    b.dependencies.clear();
    b.invariants.clear();
    a.invariants.clear();
    b.reads = a.writes.clone();
    assert!(plan(vec![a, b]).validate(&Limits::default()).is_err());
    assert!(
        plan(
            (0..4)
                .map(|i| task(&format!("task{i}"), &format!("node{i}")))
                .collect()
        )
        .validate(&Limits::default())
        .is_ok()
    );
}
#[test]
fn scope_and_component_versions_are_enforced_on_model_changes() {
    let t = task("records", "records");
    let p = plan(vec![t.clone()]);
    let node = AppNode {
        id: "records".into(),
        kind: "B031".into(),
        properties: json!({"version":"0.1.2","configuration":{}})
            .as_object()
            .unwrap()
            .clone(),
    };
    assert!(
        p.validate_changes(
            &t,
            &ChangeSet {
                operations: vec![ChangeOperation::AddNode { node: node.clone() }]
            }
        )
        .is_ok()
    );
    let mut wrong = node.clone();
    wrong.kind = "B999".into();
    assert!(
        p.validate_changes(
            &t,
            &ChangeSet {
                operations: vec![ChangeOperation::AddNode { node: wrong }]
            }
        )
        .is_err()
    );
    assert!(
        p.validate_changes(
            &t,
            &ChangeSet {
                operations: vec![ChangeOperation::SetPreference {
                    key: "authority".into(),
                    value: json!("admin")
                }]
            }
        )
        .is_err()
    );
    assert!(serde_json::from_value::<TaskResult>(json!({"task_id":"records","changes":{"operations":[]},"limitations":[],"approved":true})).is_err());
    assert!(serde_json::from_value::<StartRequest>(json!({"request":"build","limits":Limits::default(),"plan_only":true,"permissions":["manage"]})).is_err());
}
#[test]
fn property_snapshots_distinguish_absence_null_and_unrelated_edits() {
    let mut a = AppSpec::default();
    a.nodes.push(AppNode {
        id: "record".into(),
        kind: "B031".into(),
        properties: Default::default(),
    });
    let r = Resource::Property {
        id: "record".into(),
        key: "configuration".into(),
    };
    let absent = r.value(&a);
    a.nodes[0]
        .properties
        .insert("configuration".into(), Value::Null);
    assert_ne!(absent, r.value(&a));
    let before = r.value(&a);
    a.preferences.insert("locale".into(), json!("en-US"));
    assert_eq!(before, r.value(&a));
    assert!(
        Resource::Node {
            id: "record".into()
        }
        .overlaps(&r)
    );
}
#[test]
fn plan_call_bound_includes_planning_reviews_and_retries() {
    let mut limits = Limits::default();
    limits.max_calls = 4;
    assert!(
        plan(vec![task("a", "a"), task("b", "b")])
            .validate(&limits)
            .is_err()
    );
    let mut p = plan(vec![task("a", "a")]);
    p.missing_capabilities.push("unavailable capability".into());
    assert!(matches!(
        p.validate(&Limits::default()),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn replacement_requires_the_removed_and_added_components() {
    let mut t = task("replace", "records");
    t.components = vec![ComponentRef {
        id: "B081".into(),
        version: "0.1.2".into(),
    }];
    let old = AppNode {
        id: "records".into(),
        kind: "B031".into(),
        properties: json!({"version":"0.1.2","configuration":{}})
            .as_object()
            .unwrap()
            .clone(),
    };
    let mut new = old.clone();
    new.kind = "B081".into();
    let mut spec = AppSpec::default();
    spec.nodes.push(old);
    let changes = ChangeSet {
        operations: vec![
            ChangeOperation::RemoveNode {
                node_id: "records".into(),
            },
            ChangeOperation::AddNode { node: new },
        ],
    };
    assert!(
        plan(vec![t.clone()])
            .validate_result(&t, &changes, &spec)
            .is_err()
    );
    t.components.push(ComponentRef {
        id: "B031".into(),
        version: "0.1.2".into(),
    });
    assert!(
        plan(vec![t.clone()])
            .validate_result(&t, &changes, &spec)
            .is_ok()
    );
}
