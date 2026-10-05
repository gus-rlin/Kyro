use kyro_factory::{cleanup::reap_expired, digest, sandbox::SandboxConfig};
use std::{process::Command, time::Duration};
use uuid::Uuid;
fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
struct Objects {
    containers: Vec<String>,
    volumes: Vec<String>,
}
impl Drop for Objects {
    fn drop(&mut self) {
        for name in &self.containers {
            let _ = Command::new("docker").args(["rm", "-f", name]).output();
        }
        for name in &self.volumes {
            let _ = Command::new("docker").args(["volume", "rm", name]).output();
        }
    }
}
#[tokio::test]
#[ignore = "requires operator Docker controller and pinned disposable tools image"]
async fn expired_owned_objects_are_reaped_but_live_foreign_and_mounted_volumes_survive() {
    let config: SandboxConfig =
        serde_json::from_str(&std::env::var("KYRO_P2_SANDBOX_CONFIG").unwrap()).unwrap();
    let owner = format!("io.kyro.p2.disposable={}", digest(&config).unwrap());
    let expired = format!(
        "io.kyro.p2.expires-at={}",
        chrono::Utc::now().timestamp() - 1
    );
    let future = format!(
        "io.kyro.p2.expires-at={}",
        chrono::Utc::now().timestamp() + 60
    );
    let names: Vec<_> = (0..4)
        .map(|_| format!("kyro-p2-probe-{}", Uuid::new_v4()))
        .collect();
    let volumes: Vec<_> = (0..3)
        .map(|_| format!("kyro-p2-candidate-root-{}", Uuid::new_v4()))
        .collect();
    let _guard = Objects {
        containers: names.clone(),
        volumes: volumes.clone(),
    };
    for (name, expiry, label) in [
        (&volumes[0], &expired, owner.as_str()),
        (&volumes[1], &future, owner.as_str()),
        (&volumes[2], &expired, "io.kyro.p2.disposable=foreign"),
    ] {
        docker(&[
            "volume", "create", "--label", label, "--label", expiry, name,
        ]);
    }
    for (name, expiry, label, sleep, mount) in [
        (&names[0], &expired, owner.as_str(), "30", None),
        (&names[1], &future, owner.as_str(), "30", Some(&volumes[0])),
        (
            &names[2],
            &expired,
            "io.kyro.p2.disposable=foreign",
            "30",
            None,
        ),
        (&names[3], &future, owner.as_str(), "1", None),
    ] {
        let mut args = vec![
            "run",
            "-d",
            "--rm",
            "--label",
            label,
            "--label",
            expiry,
            "--name",
            name,
            "--network",
            "none",
            "--read-only",
            "--user",
            "1000:1000",
            "--cap-drop",
            "ALL",
            "--cpus",
            "1",
            "--memory",
            "64m",
            "--pids-limit",
            "8",
        ];
        let mount_value =
            mount.map(|volume| format!("type=volume,src={volume},dst=/candidate,readonly"));
        if let Some(value) = mount_value.as_deref() {
            args.extend(["--mount", value]);
        }
        args.extend(["--entrypoint", "/bin/sleep", &config.tools_image, sleep]);
        docker(&args);
    }
    assert_eq!(reap_expired(&config).await.unwrap(), 1);
    for name in [&names[1], &names[2]] {
        assert!(
            docker(&[
                "ps",
                "-a",
                "--filter",
                &format!("name=^/{name}$"),
                "--format",
                "{{.Names}}"
            ])
            .contains(name)
        );
    }
    for volume in &volumes {
        assert_eq!(
            docker(&["volume", "inspect", "--format", "{{.Name}}", volume]).trim(),
            volume
        );
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while !docker(&[
            "ps",
            "-a",
            "--filter",
            &format!("name=^/{}$", names[3]),
            "--format",
            "{{.ID}}",
        ])
        .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    docker(&["rm", "-f", &names[1]]);
    assert_eq!(reap_expired(&config).await.unwrap(), 1);
    assert_eq!(
        docker(&["volume", "inspect", "--format", "{{.Name}}", &volumes[1]]).trim(),
        volumes[1]
    );
    assert_eq!(
        docker(&["volume", "inspect", "--format", "{{.Name}}", &volumes[2]]).trim(),
        volumes[2]
    );
    println!(
        "expired owned container removed; mounted expired volume held then removed; live and foreign objects preserved; PID1 auto-removal observed"
    );
}
