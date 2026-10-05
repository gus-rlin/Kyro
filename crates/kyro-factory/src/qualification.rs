//! Operator-only qualification of a configured profile. Jobs cannot choose a
//! probe, a resource ceiling, a command, or a mount through this interface.
use crate::{
    Result, digest, digest_bytes, fail,
    sandbox::{Cleanup, SandboxConfig, builder_spec, docker, input_tar},
};
use serde::Serialize;
use std::{collections::BTreeMap, time::Duration};
const PROBE: &[u8] = include_bytes!("../../../scripts/p2/sandbox/resource-probe.mjs");
#[derive(Debug, Serialize)]
pub struct IsolationObservation {
    pub schema_version: u32,
    pub profile_kind: String,
    pub profile_digest: String,
    pub probe_digest: String,
    pub qualification_renderer_digest: String,
    pub checks: BTreeMap<String, serde_json::Value>,
}
pub async fn qualify_builder(config: &SandboxConfig) -> Result<IsolationObservation> {
    qualify(config, true).await
}
pub async fn qualify_verifier(config: &SandboxConfig) -> Result<IsolationObservation> {
    qualify(config, false).await
}
async fn qualify(config: &SandboxConfig, builder: bool) -> Result<IsolationObservation> {
    config.validate()?;
    crate::cleanup::reap_expired(config).await?;
    let profile_digest = if builder {
        digest(&config.builder_profile()?)?
    } else {
        digest(&config.verifier_profile()?)?
    };
    let workspace_bytes = if builder { 8589934592 } else { 4294967296 };
    let memory_bytes = if builder {
        4294967296u64
    } else {
        2147483648u64
    };
    let pids = if builder { 256u64 } else { 128u64 };
    let modes: &[&str] = if builder {
        &[
            "filesystem",
            "network",
            "cpu",
            "pids",
            "disk",
            "memory",
            "deadline",
        ]
    } else {
        &["filesystem", "network", "cpu", "pids", "memory", "deadline"]
    };
    let mut checks = BTreeMap::new();
    for &mode in modes {
        let name = format!("kyro-p2-probe-{}", uuid::Uuid::new_v4());
        let guard = Cleanup {
            name: name.clone(),
            active: true,
        };
        let result=async {
            docker(&crate::cleanup::disposable_args(config,240,vec!["run".into(),"-d".into(),"--name".into(),name.clone(),"--network".into(),"none".into(),"--read-only".into(),
                "--user".into(),"1000:1000".into(),"--cpus".into(),"2".into(),"--memory".into(),memory_bytes.to_string(),"--memory-swap".into(),memory_bytes.to_string(),"--pids-limit".into(),pids.to_string(),
                "--cap-drop".into(),"ALL".into(),"--security-opt".into(),"seccomp=unconfined".into(),
                "--mount".into(),format!("type=volume,src={},dst=/tools-root,readonly",config.tools_root_volume),
                "--tmpfs".into(),"/sandbox:rw,nosuid,nodev,size=512m,uid=1000,gid=1000,mode=0700".into(),
                "--tmpfs".into(),"/tmp:rw,nosuid,nodev,size=128m,uid=1000,gid=1000,mode=1777".into(),"--entrypoint".into(),"/bin/sh".into(),config.tools_image.clone(),"-c".into(),"sleep 180".into()])?,None,Duration::from_secs(30)).await?.require("qualification_launch_failed")?;
            docker(&["exec".into(),name.clone(),"mkdir".into(),"-p".into(),"/sandbox/input".into(),"/sandbox/output".into()],None,Duration::from_secs(10)).await?.require("qualification_prepare_failed")?;
            docker(&["exec".into(),name.clone(),"chmod".into(),"0777".into(),"/sandbox/output".into()],None,Duration::from_secs(10)).await?.require("qualification_prepare_failed")?;
            let mut spec=if builder {builder_spec(workspace_bytes)}else{
                let mut spec=crate::verifier::common_spec("/tools-root",1000,vec![],vec!["PATH=/usr/local/bin:/usr/bin:/bin".into(),"HOME=/work".into()],workspace_bytes);
                spec["mounts"].as_array_mut().unwrap().push(serde_json::json!({"destination":"/input","type":"bind","source":"/sandbox/input","options":["rbind","ro","nosuid","nodev","noexec"]}));
                spec
            };
            spec["process"]["args"]=serde_json::json!(["/usr/local/bin/node","/input/resource-probe.mjs",mode]);
            let files=BTreeMap::from([("input/resource-probe.mjs".into(),PROBE.to_vec()),("config.json".into(),serde_json::to_vec(&spec).map_err(|_|fail("qualification_spec_failed","/spec"))?)]);
            let archive=input_tar(&files)?;
            docker(&["exec".into(),"-i".into(),name.clone(),"tar".into(),"--no-same-owner".into(),"-xf".into(),"-".into(),"-C".into(),"/sandbox".into()],Some(&archive),Duration::from_secs(10)).await?.require("qualification_input_failed")?;
            let limits=docker(&["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),"cat /sys/fs/cgroup/cpu.max /sys/fs/cgroup/memory.max /sys/fs/cgroup/pids.max".into()],None,Duration::from_secs(10)).await?;
            limits.require("qualification_cgroup_failed")?;
            if String::from_utf8_lossy(&limits.stdout)!=format!("200000 100000\n{memory_bytes}\n{pids}\n") {return Err(fail("qualification_limits_differ","/limits"));}
            let deadline=if mode=="deadline" {Duration::from_secs(3)}else{Duration::from_secs(90)};
            let network=if builder{"none"}else{"host"};
            let outcome=docker(&["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),format!("cd /sandbox && /usr/local/bin/runsc --root=/sandbox/state --ignore-cgroups=true --network={network} --platform=systrap run probe")],None,deadline).await;
            let observed=if mode=="deadline" {
                match outcome {Err(e) if e.code=="sandbox_deadline_exceeded"=>serde_json::json!({"supervisor_timeout_seconds":3,"stopped":true}),_=>return Err(fail("qualification_deadline_not_enforced","/deadline"))}
            } else if mode=="memory" {
                let outcome=outcome?;
                if outcome.success {return Err(fail("qualification_memory_not_enforced","/memory"));}
                let memory=docker(&["exec".into(),name.clone(),"cat".into(),"/sys/fs/cgroup/memory.events".into()],None,Duration::from_secs(10)).await?;
                memory.require("qualification_cgroup_failed")?;
                let text=String::from_utf8_lossy(&memory.stdout);
                let killed=text.lines().find_map(|line|line.strip_prefix("oom_kill ")).and_then(|s|s.parse::<u64>().ok()).unwrap_or(0);
                if killed==0 {return Err(fail("qualification_memory_not_observed","/memory"));}
                serde_json::json!({"cgroup_oom_kill":killed,"stopped":true,"memory_max":memory_bytes})
            } else if mode=="pids" {
                let outcome=outcome?;
                let events=docker(&["exec".into(),name.clone(),"cat".into(),"/sys/fs/cgroup/pids.events".into()],None,Duration::from_secs(10)).await?;
                events.require("qualification_cgroup_failed")?;
                let reached=String::from_utf8_lossy(&events.stdout).lines().find_map(|line|line.strip_prefix("max ")).and_then(|s|s.parse::<u64>().ok()).unwrap_or(0);
                let guest:Option<serde_json::Value>=serde_json::from_slice(&outcome.stdout).ok();
                // Reaching the outer ceiling can kill the Sentry before the
                // guest writes a report. The kernel's cgroup denial, not an
                // untrusted stdout assertion, is the authoritative observation.
                if outcome.success && guest.as_ref().is_none_or(|v|v["denied"]!=true) {return Err(fail("qualification_pids_not_stopped","/pids"));}
                // An unprivileged verifier can hit its guest RLIMIT_NPROC
                // before the outer cgroup. Preserve which limit was observed.
                let guest_limit=!builder && guest.as_ref().is_some_and(|v|v["denied"]==true && v["spawned"].as_u64().is_some_and(|n|n>0 && n<pids));
                if reached==0 && !guest_limit {return Err(fail("qualification_pid_ceiling_not_observed","/pids"));}
                serde_json::json!({"guest_observation":guest,"guest_completed":outcome.success,"cgroup_denials":reached,"guest_limit_observed":guest_limit,"pids_max":pids})
            } else {
                let outcome=outcome?;
                let value:serde_json::Value=serde_json::from_slice(&outcome.stdout).map_err(|_|{
                    let mut error=fail("qualification_report_invalid",mode);
                    error.execution=Some(crate::ExecutionDiagnostic {stdout:outcome.stdout.clone(),stderr:outcome.stderr.clone(),truncated:outcome.truncated});error
                })?;
                if !outcome.success {let mut error=fail("qualification_probe_failed",mode);error.execution=Some(crate::ExecutionDiagnostic {stdout:outcome.stdout,stderr:outcome.stderr,truncated:outcome.truncated});return Err(error);}
                match mode {
                    "network"=>{if value["targets"]!=3 || value["connected"]!=0 {return Err(fail("qualification_network_failed","/network"));}value},
                    "cpu"=>{
                        let metrics=docker(&["exec".into(),name.clone(),"cat".into(),"/sys/fs/cgroup/cpu.stat".into()],None,Duration::from_secs(10)).await?;metrics.require("qualification_cgroup_failed")?;
                        let throttled=String::from_utf8_lossy(&metrics.stdout).lines().find_map(|line|line.strip_prefix("nr_throttled ")).and_then(|s|s.parse::<u64>().ok()).unwrap_or(0);
                        if throttled==0 {return Err(fail("qualification_cpu_not_observed","/cpu"));}
                        serde_json::json!({"workers":value["workers"],"throttled_periods":throttled,"cpu_quota":200000,"cpu_period":100000})
                    },
                    "disk"=>{
                        if value["denied"]!=true || value["written"].as_u64().is_none_or(|n|n==0 || n>536870912) {return Err(fail("qualification_disk_not_enforced","/disk"));}
                        value
                    },
                    "filesystem"=>{
                        if value["forbidden_absent"]!=4 || value["workspace_capacity_bytes"]!=workspace_bytes {return Err(fail("qualification_filesystem_failed","/filesystem"));}value
                    },
                    _=>return Err(fail("qualification_probe_invalid","/probe"))
                }
            };
            Ok(observed)
        }.await;
        let cleanup = guard.close().await;
        let observed = match result {
            Ok(o) => {
                cleanup?;
                o
            }
            Err(e) => {
                let _ = cleanup;
                return Err(e);
            }
        };
        // A shortened timeout exercises the same supervisor kill path. The
        // production 900-second ceiling remains in the bound builder profile.
        checks.insert(mode.into(), observed);
    }
    Ok(IsolationObservation {
        schema_version: 1,
        profile_kind: if builder { "builder" } else { "verifier" }.into(),
        profile_digest,
        probe_digest: digest_bytes(PROBE),
        qualification_renderer_digest: digest_bytes(include_bytes!("qualification.rs")),
        checks,
    })
}
