use kyro_domain::{Environment, Error};
use kyro_gateway::{DisabledReason, GatewayConfig};

const SYNTHETIC_REGISTRY: &str = include_str!("../../../config/models.synthetic.json");

#[test]
fn qualified_keyless_destination_is_admissible_and_enabled_without_a_secret() {
    let mut registry: serde_json::Value = serde_json::from_str(SYNTHETIC_REGISTRY).unwrap();
    registry["destinations"][0]["secret_ref"] = serde_json::Value::Null;
    let bytes = serde_json::to_vec(&registry).unwrap();
    let admission =
        GatewayConfig::for_admission_from_registry_json(&bytes, Environment::Development, true)
            .unwrap();
    assert!(admission.registry()[0].admissible);
    assert!(!admission.registry()[0].enabled);
    let execution =
        GatewayConfig::from_registry_json(&bytes, Environment::Development, true, None).unwrap();
    assert!(execution.registry()[0].enabled);
    assert_eq!(execution.registry()[0].disabled_reason, None);
    registry["destinations"][0]["qualified"] = serde_json::json!(false);
    let unqualified = GatewayConfig::from_registry_json(
        &serde_json::to_vec(&registry).unwrap(),
        Environment::Development,
        true,
        None,
    )
    .unwrap();
    assert!(!unqualified.registry()[0].admissible);
    assert!(!unqualified.registry()[0].enabled);
}

#[test]
fn synthetic_registry_requires_development_opt_in_and_server_secret() {
    let without_secret = GatewayConfig::from_registry_json(
        SYNTHETIC_REGISTRY.as_bytes(),
        Environment::Development,
        true,
        None,
    )
    .expect("trusted synthetic registry parses");
    let row = without_secret.registry().remove(0);
    assert!(!row.enabled);
    assert_eq!(row.disabled_reason, Some(DisabledReason::MissingSecret));

    let with_secret = GatewayConfig::from_registry_json(
        SYNTHETIC_REGISTRY.as_bytes(),
        Environment::Development,
        true,
        Some("invented-test-token"),
    )
    .expect("synthetic registry with a server secret parses");
    assert!(with_secret.registry()[0].enabled);
    assert!(!format!("{with_secret:?}").contains("invented-test-token"));

    assert!(matches!(
        GatewayConfig::from_registry_json(
            SYNTHETIC_REGISTRY.as_bytes(),
            Environment::Production,
            false,
            Some("invented-test-token"),
        ),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn admission_registry_is_eligible_without_enabling_execution() {
    let config = GatewayConfig::for_admission_from_registry_json(
        SYNTHETIC_REGISTRY.as_bytes(),
        Environment::Development,
        true,
    )
    .expect("admission registry parses without any secret");
    let model = config.registry().remove(0);
    assert!(model.admissible);
    assert!(!model.enabled);
    assert_eq!(model.disabled_reason, None);
}

#[test]
fn registry_rejects_non_loopback_pins_and_host_confusion() {
    let private_pin = SYNTHETIC_REGISTRY.replace("127.0.0.1:4318", "10.0.0.8:4318");
    assert!(matches!(
        GatewayConfig::from_registry_json(
            private_pin.as_bytes(),
            Environment::Development,
            true,
            Some("invented-test-token"),
        ),
        Err(Error::Invalid(_))
    ));

    let mismatched_host = SYNTHETIC_REGISTRY.replace(
        "\"allowed_host\": \"127.0.0.1\"",
        "\"allowed_host\": \"attacker.invalid\"",
    );
    assert!(matches!(
        GatewayConfig::from_registry_json(
            mismatched_host.as_bytes(),
            Environment::Development,
            true,
            Some("invented-test-token"),
        ),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn cloud_registry_accepts_public_neighbors_but_rejects_special_purpose_pins() {
    let mut registry: serde_json::Value = serde_json::from_str(SYNTHETIC_REGISTRY).unwrap();
    let destination = &mut registry["destinations"][0];
    destination["kind"] = serde_json::json!("cloud");
    destination["base_url"] = serde_json::json!("https://provider.example.test/v1/");
    destination["allowed_host"] = serde_json::json!("provider.example.test");
    for (pin, allowed) in [
        ("192.0.1.10:443", true),
        ("198.51.1.10:443", true),
        ("[::ffff:192.0.1.10]:443", true),
        ("[::ffff:198.51.1.10]:443", true),
        ("192.0.0.1:443", false),
        ("192.0.2.1:443", false),
        ("192.168.1.1:443", false),
        ("198.18.1.1:443", false),
        ("198.19.1.1:443", false),
        ("198.51.100.1:443", false),
        ("[::ffff:192.0.0.1]:443", false),
        ("[::ffff:192.0.2.1]:443", false),
        ("[::ffff:198.51.100.1]:443", false),
    ] {
        registry["destinations"][0]["pinned_addresses"] = serde_json::json!([pin]);
        assert_eq!(
            GatewayConfig::for_admission_from_registry_json(
                &serde_json::to_vec(&registry).unwrap(),
                Environment::Production,
                false,
            )
            .is_ok(),
            allowed,
            "pin {pin}"
        );
    }
}
