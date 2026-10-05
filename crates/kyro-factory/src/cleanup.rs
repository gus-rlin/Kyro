//! Crash recovery touches only bounded, expired objects of this tools profile.
//! Workloads never receive the daemon, so ownership labels are operator data.
use crate::{
    Result, digest, fail,
    sandbox::{SandboxConfig, docker},
};
use std::{collections::BTreeMap, time::Duration};

const OWNER: &str = "io.kyro.p2.disposable";
const EXPIRES: &str = "io.kyro.p2.expires-at";

pub(crate) fn disposable_args(
    config: &SandboxConfig,
    seconds: i64,
    mut args: Vec<String>,
) -> Result<Vec<String>> {
    config.validate()?;
    if !(1..=1800).contains(&seconds) || args.len() < 2 {
        return Err(fail("sandbox_cleanup_context_invalid", "/cleanup"));
    }
    let expires = chrono::Utc::now()
        .timestamp()
        .checked_add(seconds)
        .ok_or(fail("sandbox_cleanup_context_invalid", "/cleanup"))?;
    let mut labels = vec![
        "--label".into(),
        format!("{OWNER}={}", digest(config)?),
        "--label".into(),
        format!("{EXPIRES}={expires}"),
    ];
    if args[0] == "run" {
        labels.push("--rm".into());
    }
    args.splice(2..2, labels);
    Ok(args)
}

fn owned_name(name: &str, volume: bool) -> bool {
    let prefixes: &[&str] = if volume {
        &["kyro-p2-candidate-root-"]
    } else {
        &[
            "kyro-p2-build-",
            "kyro-p2-verify-",
            "kyro-p2-probe-",
            "kyro-p2-media-",
            "kyro-p2-candidate-prepare-",
        ]
    };
    prefixes.iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|id| {
            uuid::Uuid::parse_str(id).is_ok_and(|value| !value.is_nil() && value.to_string() == id)
        })
    })
}
fn expired(labels: &BTreeMap<String, String>, owner: &str, now: i64) -> Result<bool> {
    if labels.get(OWNER).map(String::as_str) != Some(owner) {
        return Err(fail("sandbox_cleanup_owner_changed", "/cleanup"));
    }
    let expires = labels
        .get(EXPIRES)
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .ok_or(fail("sandbox_cleanup_expiry_invalid", "/cleanup"))?;
    Ok(expires <= now)
}

/// Called on operator startup and before launches. Unexpired objects and tools
/// volumes are preserved, including work owned by another controller/profile.
pub async fn reap_expired(config: &SandboxConfig) -> Result<usize> {
    config.validate()?;
    let owner = digest(config)?;
    let filter = format!("label={OWNER}={owner}");
    let now = chrono::Utc::now().timestamp();
    let mut removed = 0;
    for volume in [false, true] {
        let args = if volume {
            vec!["volume", "ls", "--filter", &filter, "--format", "{{.Name}}"]
        } else {
            vec!["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"]
        };
        let list = docker(
            &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            None,
            Duration::from_secs(10),
        )
        .await?;
        list.require("sandbox_cleanup_listing_failed")?;
        if list.truncated || list.stdout.len() > 16384 {
            return Err(fail("sandbox_cleanup_listing_limit", "/cleanup"));
        }
        let text = std::str::from_utf8(&list.stdout)
            .map_err(|_| fail("sandbox_cleanup_listing_invalid", "/cleanup"))?;
        if text.lines().count() > 128 {
            return Err(fail("sandbox_cleanup_listing_limit", "/cleanup"));
        }
        for name in text.lines() {
            if !owned_name(name, volume) {
                return Err(fail("sandbox_cleanup_name_refused", "/cleanup"));
            }
            let args = if volume {
                vec!["volume", "inspect", "--format", "{{json .Labels}}", name]
            } else {
                vec![
                    "inspect",
                    "--type",
                    "container",
                    "--format",
                    "{{json .Config.Labels}}",
                    name,
                ]
            };
            let inspected = docker(
                &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                None,
                Duration::from_secs(10),
            )
            .await?;
            // A --rm container can disappear between the listing and inspection.
            if !inspected.success && !volume {
                continue;
            }
            inspected.require("sandbox_cleanup_inspection_failed")?;
            let labels: BTreeMap<String, String> = serde_json::from_slice(&inspected.stdout)
                .map_err(|_| fail("sandbox_cleanup_labels_invalid", "/cleanup"))?;
            if !expired(&labels, &owner, now)? {
                continue;
            }
            if volume {
                let attached = docker(
                    &[
                        "ps".into(),
                        "-a".into(),
                        "--filter".into(),
                        format!("volume={name}"),
                        "--format".into(),
                        "{{.ID}}".into(),
                    ],
                    None,
                    Duration::from_secs(10),
                )
                .await?;
                attached.require("sandbox_cleanup_listing_failed")?;
                if !attached.stdout.is_empty() {
                    continue;
                }
            }
            let args = if volume {
                vec!["volume", "rm", name]
            } else {
                vec!["rm", "-f", name]
            };
            let outcome = docker(
                &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                None,
                Duration::from_secs(10),
            )
            .await?;
            if outcome.success {
                removed += 1;
            } else if volume {
                return Err(fail("sandbox_cleanup_volume_failed", "/cleanup"));
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_scope_never_includes_tools_foreign_names_or_unexpired_work() {
        let id = uuid::Uuid::new_v4();
        assert!(owned_name(&format!("kyro-p2-candidate-root-{id}"), true));
        for name in [
            "kyro-p2-tools-root-real",
            "other-container",
            "kyro-p2-build-../other",
        ] {
            assert!(!owned_name(name, false));
            assert!(!owned_name(name, true));
        }
        let mut labels = BTreeMap::from([
            (OWNER.into(), "profile".into()),
            (EXPIRES.into(), "100".into()),
        ]);
        assert!(!expired(&labels, "profile", 99).unwrap());
        assert!(expired(&labels, "profile", 100).unwrap());
        assert!(expired(&labels, "other", 101).is_err());
        labels.insert(EXPIRES.into(), "invalid".into());
        assert!(expired(&labels, "profile", 101).is_err());
    }
}
