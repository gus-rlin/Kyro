use kyro_factory::{qualification::qualify_builder, sandbox::SandboxConfig};
#[tokio::test]
#[ignore = "requires prepared gVisor tools; deliberately reaches disposable cgroup ceilings"]
async fn hostile_probes_reach_the_actual_builder_ceiling_and_cleanup() {
    let config: SandboxConfig =
        serde_json::from_str(&std::env::var("KYRO_P2_SANDBOX_CONFIG").expect("prepared profile"))
            .unwrap();
    let observations = qualify_builder(&config).await.unwrap_or_else(|error| {
        if let Some(log) = &error.execution {
            eprintln!(
                "{}\n{}",
                String::from_utf8_lossy(&log.stdout),
                String::from_utf8_lossy(&log.stderr)
            );
        }
        panic!("{error}");
    });
    println!("{}", serde_json::to_string(&observations).unwrap());
    assert_eq!(observations.checks.len(), 7);
}

#[tokio::test]
#[ignore = "requires prepared gVisor tools; reaches the smaller verifier ceilings"]
async fn hostile_probes_reach_the_verifier_ceiling_and_cleanup() {
    let config: SandboxConfig =
        serde_json::from_str(&std::env::var("KYRO_P2_SANDBOX_CONFIG").expect("prepared profile"))
            .unwrap();
    let observations = kyro_factory::qualification::qualify_verifier(&config)
        .await
        .unwrap_or_else(|error| {
            if let Some(log) = &error.execution {
                eprintln!(
                    "{}\n{}",
                    String::from_utf8_lossy(&log.stdout),
                    String::from_utf8_lossy(&log.stderr)
                );
            }
            panic!("{error}");
        });
    println!("{}", serde_json::to_string(&observations).unwrap());
    assert_eq!(observations.checks.len(), 6);
}
