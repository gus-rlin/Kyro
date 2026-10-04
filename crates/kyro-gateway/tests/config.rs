use kyro_domain::{Environment, Error};
use kyro_gateway::{DisabledReason, GatewayConfig};

const SYNTHETIC_REGISTRY: &str = include_str!("../../../config/models.synthetic.json");
const NEBIUS_REGISTRY: &str = include_str!("../../../config/models.nebius.example.json");

#[test]
fn standard_retention_is_explicit_and_limited_to_nano_development_chat() {
    let mut registry: serde_json::Value = serde_json::from_str(include_str!(
        "../../../config/models.nebius.chat.example.json"
    ))
    .unwrap();
    registry["destinations"][0]["qualified"] = serde_json::json!(true);
    registry["destinations"][0]["pinned_addresses"] = serde_json::json!(["1.1.1.1:443"]);
    registry["destinations"][0]["nebius"]["retention_evidence"] =
        serde_json::json!("https://docs.nebius.com/legal/token-factory");
    let parse = |value: &serde_json::Value, env| {
        GatewayConfig::for_admission_from_registry_json(
            &serde_json::to_vec(value).unwrap(),
            env,
            false,
        )
    };
    assert!(parse(&registry, Environment::Development).is_err());
    registry["destinations"][0]["nebius"]["provider_standard_retention_accepted"] =
        serde_json::json!(true);
    let admitted = parse(&registry, Environment::Development).unwrap();
    assert!(admitted.registry()[0].admissible);
    assert!(parse(&registry, Environment::Production).is_err());
    registry["destinations"][0]["retention_seconds"] = serde_json::json!(0);
    assert!(parse(&registry, Environment::Development).is_err());
    registry["destinations"][0]["retention_seconds"] = serde_json::Value::Null;
    registry["destinations"][0]["models"][0]["output_mode"] = serde_json::json!("structured_json");
    assert!(parse(&registry, Environment::Development).is_err());
}

#[test]
fn nebius_stays_disabled_without_verified_retention_and_capabilities() {
    let mut registry: serde_json::Value = serde_json::from_str(NEBIUS_REGISTRY).unwrap();
    let disabled = GatewayConfig::from_registry_json(
        NEBIUS_REGISTRY.as_bytes(),
        Environment::Development,
        false,
        Some("fake-canary-nebius-key"),
    )
    .unwrap();
    assert!(!disabled.registry()[0].admissible);
    assert_eq!(
        disabled.registry()[0].disabled_reason,
        Some(DisabledReason::NotQualified)
    );
    registry["destinations"][0]["qualified"] = serde_json::json!(true);
    assert!(
        GatewayConfig::for_admission_from_registry_json(
            &serde_json::to_vec(&registry).unwrap(),
            Environment::Development,
            false
        )
        .is_err()
    );
    registry["destinations"][0]["retention_seconds"] = serde_json::json!(0);
    registry["destinations"][0]["nebius"]["json_schema"] = serde_json::json!(true);
    registry["destinations"][0]["nebius"]["retention_evidence"] =
        serde_json::json!("https://docs.nebius.com/legal/token-factory");
    let bytes = serde_json::to_vec(&registry).unwrap();
    let no_key =
        GatewayConfig::from_registry_json(&bytes, Environment::Development, false, None).unwrap();
    assert_eq!(
        no_key.registry()[0].disabled_reason,
        Some(DisabledReason::MissingSecret)
    );
    for bad in [
        "http://docs.nebius.com/legal/token-factory",
        "https://nebius.com.attacker.invalid/",
        "https://user:secret@nebius.com/",
    ] {
        registry["destinations"][0]["nebius"]["retention_evidence"] = serde_json::json!(bad);
        assert!(
            GatewayConfig::for_admission_from_registry_json(
                &serde_json::to_vec(&registry).unwrap(),
                Environment::Development,
                false
            )
            .is_err()
        );
    }
}

#[test]
fn cloud_registry_accepts_provider_model_ids() {
    let mut registry: serde_json::Value = serde_json::from_str(SYNTHETIC_REGISTRY).unwrap();
    let destination = &mut registry["destinations"][0];
    destination["id"] = serde_json::json!("cloud-test");
    destination["provider"] = serde_json::json!("cloud-test");
    destination["kind"] = serde_json::json!("cloud");
    destination["base_url"] = serde_json::json!("https://api.tokenfactory.nebius.com/v1/");
    destination["allowed_host"] = serde_json::json!("api.tokenfactory.nebius.com");
    destination["pinned_addresses"] = serde_json::json!(["213.239.161.19:443"]);
    destination["models"][0]["id"] = serde_json::json!("nvidia/Nemotron-3_5-Lightning");
    GatewayConfig::for_admission_from_registry_json(
        &serde_json::to_vec(&registry).unwrap(),
        Environment::Development,
        false,
    )
    .expect("provider identifiers are opaque IDs, not URLs");
}

#[cfg(unix)]
#[test]
fn secret_sources_are_unambiguous_and_admission_never_reads_them() {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let directory =
        std::env::temp_dir().join(format!("kyro-secret-source-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let file = directory.join("key");
    fs::write(&file, "fake-source-canary").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let registry = directory.join("registry.json");
    fs::write(&registry, NEBIUS_REGISTRY).unwrap();
    for (admission, both, missing, ok) in [
        (false, false, false, true),
        (false, true, false, false),
        (false, false, true, false),
        (true, true, true, true),
    ] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "secret_source_probe", "--ignored"])
            .env("KYRO_MODEL_REGISTRY_PATH", &registry)
            .env_remove("KYRO_MODEL_API_KEY")
            .env(
                "KYRO_MODEL_API_KEY_FILE",
                if missing {
                    directory.join("missing")
                } else {
                    file.clone()
                },
            )
            .env("KYRO_PROBE_ADMISSION", if admission { "1" } else { "0" })
            .env("KYRO_PROBE_EXPECT_OK", if ok { "1" } else { "0" });
        if both {
            command.env("KYRO_MODEL_API_KEY", "fake-source-canary");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "closed source probe failed");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("fake-source-canary"));
    }
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess fixture invoked by secret_sources_are_unambiguous_and_admission_never_reads_them"]
fn secret_source_probe() {
    // Full --include-ignored suites also discover this fixture: no probe context means no-op.
    let Ok(expected) = std::env::var("KYRO_PROBE_EXPECT_OK") else {
        return;
    };
    let actual = if std::env::var("KYRO_PROBE_ADMISSION").unwrap() == "1" {
        GatewayConfig::for_admission_from_env(Environment::Development)
    } else {
        GatewayConfig::from_env(Environment::Development)
    };
    assert_eq!(actual.is_ok(), expected == "1");
}

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
