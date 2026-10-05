//! The protected driver and the candidate have distinct gVisor Sentries.
//! Their only shared surface is the supervisor's isolated loopback namespace.
use crate::{
    Result,
    artifacts::{EvidenceBundle, OciArtifact, SignedEvidence},
    crypto::{Purpose, Signer},
    digest, digest_bytes, fail,
    sandbox::{Cleanup, SandboxConfig, docker},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    time::Duration,
};

const DRIVER: &[u8] = include_bytes!("../../../scripts/p2/sandbox/verify-records.mjs");
const CHECKS: [&str; 11] = [
    "fresh_database",
    "runtime_privileges",
    "http_health",
    "record_nominal",
    "record_validation",
    "foreign_tenant",
    "cross_application",
    "idempotency",
    "stale_version",
    "session_revocation",
    "network_denied",
];

#[derive(Clone, Debug, Serialize)]
pub struct VerifierProfile {
    engine: &'static str,
    platform: &'static str,
    supervisor_uid: u32,
    candidate_uid: u32,
    driver_uid: u32,
    cpus: u32,
    memory_bytes: u64,
    pids: u32,
    workspace_bytes: u64,
    timeout_seconds: u64,
    network: &'static str,
    tools_root_digest: String,
    driver_digest: String,
    renderer_digest: String,
}
impl SandboxConfig {
    pub fn verifier_profile(&self) -> Result<VerifierProfile> {
        self.validate()?;
        Ok(VerifierProfile {
            engine: "runsc-20260928.0",
            platform: "systrap",
            supervisor_uid: 1000,
            candidate_uid: 1000,
            driver_uid: 1000,
            cpus: 2,
            memory_bytes: 2147483648,
            pids: 128,
            workspace_bytes: 4294967296,
            timeout_seconds: 600,
            network: "isolated_outer_loopback",
            tools_root_digest: self.tools_root_digest.clone(),
            driver_digest: digest_bytes(DRIVER),
            renderer_digest: digest_bytes(include_bytes!("verifier.rs")),
        })
    }
}
/// Criteria are selected by the trusted factory, not deserialized from a job.
#[derive(Clone, Debug, Serialize)]
pub struct ProtectedCriteria {
    schema_version: u32,
    suite: &'static str,
    required_checks: BTreeSet<String>,
    driver_digest: String,
    profile_digest: String,
}
impl ProtectedCriteria {
    pub fn records(config: &SandboxConfig) -> Result<Self> {
        Ok(Self {
            schema_version: 1,
            suite: "records",
            required_checks: CHECKS.into_iter().map(str::to_owned).collect(),
            driver_digest: digest_bytes(DRIVER),
            profile_digest: digest(&config.verifier_profile()?)?,
        })
    }
    pub fn composition(
        config: &SandboxConfig,
        lock: &kyro_domain::factory::SignedCompositionLock,
    ) -> Result<Self> {
        lock.validate_shape()
            .map_err(|_| fail("verifier_plan_invalid", "/verifier"))?;
        let mut required_checks: BTreeSet<String> = [
            "fresh_database",
            "runtime_privileges",
            "http_health",
            "foreign_tenant",
            "cross_application",
            "session_revocation",
            "network_denied",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let supports = |id: &str, actions: &[&str]| {
            lock.lock.nodes.values().any(|node| {
                if node.component_id != id {
                    return false;
                }
                let allowed = node
                    .configuration
                    .get("allowed_actions")
                    .and_then(serde_json::Value::as_array);
                allowed.is_none_or(|values| {
                    values.is_empty()
                        || actions
                            .iter()
                            .all(|action| values.iter().any(|v| v.as_str() == Some(action)))
                })
            })
        };
        if supports("B031", &["create", "get", "update"]) {
            required_checks.extend(
                [
                    "record_nominal",
                    "record_validation",
                    "idempotency",
                    "stale_version",
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        for (components, checks) in [
            (
                &["B111", "B112", "B113", "B114", "B115", "B119"][..],
                &["booking_capacity", "booking_cancel_scan", "booking_access"][..],
            ),
            (
                &["B013", "B014", "B133"][..],
                &["support_notes", "support_team_scope", "support_revocation"][..],
            ),
            (
                &["B121", "B122", "B123", "B124", "B130"][..],
                &["stock_capacity", "stock_receipt_replay", "stock_access"][..],
            ),
        ] {
            if components
                .iter()
                .all(|id| lock.lock.components.contains_key(*id))
            {
                required_checks.extend(checks.iter().map(|s| s.to_string()));
            }
        }
        Ok(Self {
            schema_version: 1,
            suite: "composition",
            required_checks,
            driver_digest: digest_bytes(DRIVER),
            profile_digest: digest(&config.verifier_profile()?)?,
        })
    }
    pub fn required_checks(&self) -> &BTreeSet<String> {
        &self.required_checks
    }
    pub fn digest(&self) -> Result<String> {
        digest(self)
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckReceipt {
    pub input_digest: String,
    pub observed_digest: String,
    pub passed: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedReport {
    pub schema_version: u32,
    pub kind: String,
    pub run_id: uuid::Uuid,
    pub image_digest: String,
    pub binding_digest: String,
    pub criteria_digest: String,
    pub checks: BTreeMap<String, CheckReceipt>,
}
/// Only a successful protected execution can construct this value. A builder's
/// serialized report cannot be converted into an attestation by a public API.
pub struct VerifiedRun {
    evidence: EvidenceBundle,
    report: ObservedReport,
}
impl VerifiedRun {
    pub fn report(&self) -> &ObservedReport {
        &self.report
    }
    pub fn attest(self, signer: &Signer) -> Result<SignedEvidence> {
        if signer.purpose() != &Purpose::Evidence {
            return Err(fail("evidence_signer_invalid", "/evidence"));
        }
        let signature = signer.sign(&self.evidence)?;
        Ok(SignedEvidence {
            evidence: self.evidence,
            signature,
        })
    }
}
pub struct ProtectedVerifier {
    config: SandboxConfig,
}
impl ProtectedVerifier {
    pub fn new(config: SandboxConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }
    /// `migrations` must be read from the protected admitted source snapshot.
    /// They are independently hashed against the job binding before any launch.
    /// The candidate's migration executable is never granted an admin credential.
    pub async fn records(
        &self,
        image: &OciArtifact,
        migrations: &BTreeMap<String, Vec<u8>>,
    ) -> Result<VerifiedRun> {
        self.verify(
            image,
            migrations,
            ProtectedCriteria::records(&self.config)?,
            None,
        )
        .await
    }
    pub async fn composition(
        &self,
        image: &OciArtifact,
        migrations: &BTreeMap<String, Vec<u8>>,
        lock: &kyro_domain::factory::SignedCompositionLock,
    ) -> Result<VerifiedRun> {
        if digest(lock)? != image.binding.lock_digest
            || lock.lock.application_id != image.binding.application_id
        {
            return Err(fail("verifier_plan_changed", "/verifier"));
        }
        self.verify(
            image,
            migrations,
            ProtectedCriteria::composition(&self.config, lock)?,
            Some(lock),
        )
        .await
    }
    async fn verify(
        &self,
        image: &OciArtifact,
        migrations: &BTreeMap<String, Vec<u8>>,
        criteria: ProtectedCriteria,
        lock: Option<&kyro_domain::factory::SignedCompositionLock>,
    ) -> Result<VerifiedRun> {
        image.verify()?;
        let expected: BTreeMap<_, _> = migrations
            .iter()
            .map(|(p, b)| (p.clone(), digest_bytes(b)))
            .collect();
        if migrations.is_empty()
            || migrations.len() > 64
            || migrations.values().map(Vec::len).sum::<usize>() > 4194304
            || migrations.keys().any(|p| {
                !crate::catalogue::source_path(p)
                    || !p.starts_with("crates/kyro-app/migrations/")
                    || !p.ends_with(".sql")
            })
            || digest(&expected)? != image.binding.migration_digest
            || self.config.tools_image.strip_prefix("sha256:")
                != Some(image.binding.tools_image_digest.as_str())
        {
            return Err(fail("verifier_inputs_changed", "/verifier"));
        }
        let profile = self.config.verifier_profile()?;
        let run_id = uuid::Uuid::new_v4();
        let started_at = chrono::Utc::now();
        let name = format!("kyro-p2-verify-{run_id}");
        let guard = Cleanup {
            name: name.clone(),
            active: true,
        };
        let candidate_volume = format!("kyro-p2-candidate-root-{run_id}");
        let volume_guard = VolumeCleanup {
            name: candidate_volume.clone(),
            supervisor: name.clone(),
            preparer: format!("kyro-p2-candidate-prepare-{run_id}"),
            active: true,
        };
        // Random, disposable workload credentials. The administrator password is
        // generated inside the protected Sentry and never enters this context.
        let runtime_password = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let session_key = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let result=async {
            // Populate only validated OCI files in a private volume, then stop
            // the preparer before mounting it read-only. A root beneath the
            // supervisor's mutable tmpfs cannot be remounted RO by rootless
            // runsc on this kernel (verified failure real-03).
            let candidate_files: BTreeMap<_,_>=image.runtime_files()?.into_iter().map(|(p,b)|{
                let mode=if p.starts_with("opt/app/bin/") || p=="lib64/ld-linux-x86-64.so.2" {0o555}else {0o444};
                (p,(b,mode))
            }).collect();
            prepare_candidate(&self.config,&candidate_volume,run_id,&candidate_files).await?;
            docker(&crate::cleanup::disposable_args(&self.config,1200,vec!["run".into(),"-d".into(),"--name".into(),name.clone(),"--network".into(),"none".into(),"--read-only".into(),
                "--user".into(),"1000:1000".into(),"--cpus".into(),"2".into(),"--memory".into(),"2g".into(),"--memory-swap".into(),"2g".into(),"--pids-limit".into(),"128".into(),
                "--cap-drop".into(),"ALL".into(),"--security-opt".into(),"seccomp=unconfined".into(),
                "--mount".into(),format!("type=volume,src={},dst=/tools-root,readonly",self.config.tools_root_volume),
                "--mount".into(),format!("type=volume,src={candidate_volume},dst=/candidate-root,readonly"),
                "--tmpfs".into(),"/tmp:rw,nosuid,nodev,size=128m,uid=1000,gid=1000,mode=1777".into(),
                "--tmpfs".into(),"/sandbox:rw,nosuid,nodev,size=512m,uid=1000,gid=1000,mode=0700".into(),"--entrypoint".into(),"/bin/sh".into(),self.config.tools_image.clone(),
                "-c".into(),"sleep 660".into()])?,None,Duration::from_secs(30)).await?.require("verifier_launch_failed")?;
            let index=docker(&["exec".into(),name.clone(),"sha256sum".into(),"/tools-root/.kyro-root-files.sha256".into()],None,Duration::from_secs(10)).await?;
            index.require("verifier_tools_unavailable")?;
            if !String::from_utf8_lossy(&index.stdout).starts_with(&self.config.tools_root_digest) {return Err(fail("verifier_tools_changed", "/verifier"));}
            docker(&["exec".into(),"--user".into(),"0:0".into(),name.clone(),"/bin/sh".into(),"-c".into(),
                "cd /tools-root && sha256sum --quiet -c .kyro-root-files.sha256 && find . ! -name '.kyro-root-*' -printf '%y %m %U %G %p %l\\n' | LC_ALL=C sort | cmp - .kyro-root-metadata.txt".into()],None,Duration::from_secs(90)).await?.require("verifier_tools_changed")?;
            docker(&["exec".into(),name.clone(),"mkdir".into(),"-p".into(),"/sandbox/driver-data".into(),"/sandbox/driver".into(),"/sandbox/application".into(),"/sandbox/debug-driver".into(),"/sandbox/debug-application".into()],None,Duration::from_secs(10)).await?.require("verifier_prepare_failed")?;
            let migration_paths:Vec<_>=migrations.keys().map(|p|format!("/input/{p}")).collect();
            let context=json!({"run_id":run_id,"application_id":image.binding.application_id,"image_digest":image.image_digest,
                "binding_digest":digest(&image.binding)?,"runtime_password":runtime_password,"session_key":session_key,"migrations":migration_paths,
                "nodes":lock.map(|l|&l.lock.nodes)});
            let mut files: BTreeMap<String,(Vec<u8>,u32)>=migrations.iter().map(|(p,b)|(format!("driver-data/{p}"),(b.clone(),0o444))).collect();
            files.insert("driver-data/verify-records.mjs".into(),(DRIVER.to_vec(),0o444));
            files.insert("driver-data/context.json".into(),(serde_json::to_vec(&context).map_err(|_| fail("verifier_serialization", "/context"))?,0o444));
            files.insert("driver-data/criteria.json".into(),(serde_json::to_vec(&criteria).map_err(|_| fail("verifier_serialization", "/criteria"))?,0o444));
            let environment=vec!["RUST_LOG=error".into(),"HOME=/work".into(),"PATH=/opt/app/bin".into(),
                format!("KYRO_APP_DATABASE_URL=postgres://kyro_app_runtime:{runtime_password}@127.0.0.1:15432/kyro_verify"),
                format!("KYRO_APP_SESSION_HMAC_KEY={session_key}"),"KYRO_APP_TOKEN_ISSUER=kyro-verifier".into(),
                "KYRO_APP_TOKEN_AUDIENCE=kyro-verifier".into(),"KYRO_APP_LISTEN_ADDRESS=127.0.0.1:18081".into()];
            let driver_spec=driver_spec(profile.workspace_bytes);
            let app_spec=candidate_spec(environment);
            files.insert("driver/config.json".into(),(serde_json::to_vec(&driver_spec).map_err(|_| fail("verifier_serialization", "/driver"))?,0o444));
            files.insert("application/config.json".into(),(serde_json::to_vec(&app_spec).map_err(|_| fail("verifier_serialization", "/application"))?,0o444));
            let bytes=verifier_tar(&files)?;
            docker(&["exec".into(),"-i".into(),name.clone(),"tar".into(),"--no-same-owner".into(),"-xf".into(),"-".into(),"-C".into(),"/sandbox".into()],Some(&bytes),Duration::from_secs(30)).await?.require("verifier_input_failed")?;
            // The driver output pipe belongs to its Sentry only. The candidate's
            // stdout goes to a different, private supervisor file and is ignored.
            let driver_args=["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),
                "cd /sandbox/driver && /usr/local/bin/runsc --root=/sandbox/driver-state --debug --debug-log=/sandbox/debug-driver/ --ignore-cgroups=true --network=host --platform=systrap run driver".into()];
            let launch_args=["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),
                "cd /sandbox/application && /usr/local/bin/node -e 'let n=0;function probe(){const s=require(\"net\").connect(18082,\"127.0.0.1\");s.once(\"connect\",()=>{s.destroy();process.exit(0)});s.once(\"error\",()=>{s.destroy();if(++n>=300)process.exit(1);setTimeout(probe,100)})}probe()' && /usr/local/bin/runsc --root=/sandbox/application-state --debug --debug-log=/sandbox/debug-application/ --ignore-cgroups=true --network=host --platform=systrap run --detach candidate >/sandbox/application.log 2>&1".into()];
            let driver=docker(&driver_args,None,Duration::from_secs(profile.timeout_seconds));
            let launch=docker(&launch_args,None,Duration::from_secs(60));
            let (driver,launch)=tokio::join!(driver,launch);
            let driver=driver?;
            let launch=launch?;
            if !driver.success || !launch.success {
                let business=["booking_capacity","booking_cancel_scan","booking_access","support_notes","support_team_scope","support_revocation","stock_capacity","stock_receipt_replay","stock_access"];
                let check=CHECKS.iter().chain(business.iter()).copied().find(|name|
                    String::from_utf8_lossy(&driver.stderr).lines().any(|line|line==format!("verification_failed:{name}")))
                    .unwrap_or("setup");
                eprintln!("protected verification failed: {check}");
                let detail=docker(&["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),
                    "tail -n 50 /sandbox/debug-driver/* /sandbox/debug-application/* /sandbox/application.log".into()],None,Duration::from_secs(10)).await?;
                let mut error=fail("verifier_execution_failed", "/verifier");
                let mut stderr=driver.stderr;let keep=detail.stdout.len().min(65536-stderr.len());stderr.extend_from_slice(&detail.stdout[..keep]);
                error.execution=Some(crate::ExecutionDiagnostic {stdout:redact(&driver.stdout,&[&runtime_password,&session_key]),
                    stderr:redact(&stderr,&[&runtime_password,&session_key]),truncated:driver.truncated || detail.truncated || keep<detail.stdout.len()});
                return Err(error);
            }
            if driver.truncated {return Err(fail("verifier_report_limit", "/report"));}
            let report:ObservedReport=serde_json::from_slice(&driver.stdout).map_err(|_| fail("verifier_report_invalid", "/report"))?;
            validate_report(&report,run_id,image,&criteria)?;
            Ok(report)
        }.await;
        let cleanup = guard.close().await;
        let volume_cleanup = volume_guard.close().await;
        let report = match result {
            Ok(r) => {
                cleanup?;
                volume_cleanup?;
                r
            }
            Err(e) => {
                let _ = cleanup;
                let _ = volume_cleanup;
                return Err(e);
            }
        };
        let finished_at = chrono::Utc::now();
        if (finished_at - started_at).num_seconds() > 600 {
            return Err(fail("verifier_deadline_exceeded", "/verifier"));
        }
        let evidence = EvidenceBundle {
            schema_version: 1,
            image_digest: image.image_digest.clone(),
            binding: image.binding.clone(),
            criteria_digest: criteria.digest()?,
            verifier_version: "kyro-verifier-1".into(),
            sandbox_run_id: run_id,
            required_checks: criteria.required_checks.clone(),
            passed_checks: criteria.required_checks,
            observed_report_digest: digest(&report)?,
            started_at,
            finished_at,
        };
        Ok(VerifiedRun { evidence, report })
    }
}
fn validate_report(
    report: &ObservedReport,
    run_id: uuid::Uuid,
    image: &OciArtifact,
    criteria: &ProtectedCriteria,
) -> Result<()> {
    if report.schema_version != 1
        || report.kind != "protected_application_verification"
        || report.run_id != run_id
        || report.image_digest != image.image_digest
        || report.binding_digest != digest(&image.binding)?
        || report.criteria_digest != criteria.digest()?
        || report.checks.keys().cloned().collect::<BTreeSet<_>>() != criteria.required_checks
        || report.checks.values().any(|r| {
            !r.passed
                || !kyro_domain::factory::valid_digest(&r.input_digest)
                || !kyro_domain::factory::valid_digest(&r.observed_digest)
        })
    {
        return Err(fail("verifier_report_refused", "/report"));
    }
    Ok(())
}
fn redact(bytes: &[u8], values: &[&str]) -> Vec<u8> {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    for value in values {
        if !value.is_empty() {
            text = text.replace(value, "[REDACTED]");
        }
    }
    text.into_bytes()
}
pub(crate) fn common_spec(
    root: &str,
    uid: u32,
    args: Vec<&str>,
    environment: Vec<String>,
    workspace_bytes: u64,
) -> serde_json::Value {
    json!({"ociVersion":"1.2.1","root":{"path":root,"readonly":true},
        "process":{"terminal":false,"user":{"uid":uid,"gid":uid},"args":args,"env":environment,"cwd":"/",
            "noNewPrivileges":true,"capabilities":{"bounding":[],"effective":[],"inheritable":[],"permitted":[],"ambient":[]},
            "rlimits":[{"type":"RLIMIT_NOFILE","hard":256,"soft":256},{"type":"RLIMIT_NPROC","hard":128,"soft":128},{"type":"RLIMIT_FSIZE","hard":268435456,"soft":268435456}]},
        "hostname":"kyro-verify","mounts":[{"destination":"/proc","type":"proc","source":"proc"},
            {"destination":"/dev","type":"tmpfs","source":"tmpfs","options":["nosuid","strictatime","mode=755","size=65536k"]},
            {"destination":"/dev/pts","type":"devpts","source":"devpts","options":["nosuid","noexec","newinstance","ptmxmode=0666","mode=0620"]},
            {"destination":"/dev/shm","type":"tmpfs","source":"shm","options":["nosuid","nodev","noexec","mode=1777","size=65536k"]},
            {"destination":"/tmp","type":"tmpfs","source":"tmpfs","options":["nosuid","nodev","noexec","mode=1777","size=134217728"]},
            {"destination":"/work","type":"tmpfs","source":"tmpfs","options":["nosuid","nodev","mode=1777",format!("size={workspace_bytes}")]}],
        "linux":{"namespaces":[{"type":"user"},{"type":"pid"},{"type":"ipc"},{"type":"uts"},{"type":"mount"}],
            "resources":{"memory":{"limit":2147483648u64},"cpu":{"quota":200000,"period":100000},"pids":{"limit":128}},
            "uidMappings":[{"containerID":0,"hostID":1000,"size":1}],"gidMappings":[{"containerID":0,"hostID":1000,"size":1}]}})
}
fn driver_spec(workspace_bytes: u64) -> serde_json::Value {
    let mut spec = common_spec(
        "/tools-root",
        1000,
        vec!["/usr/local/bin/node", "/input/verify-records.mjs"],
        vec![
            "PATH=/usr/local/bin:/usr/bin:/bin".into(),
            "HOME=/work".into(),
        ],
        workspace_bytes,
    );
    spec["mounts"].as_array_mut().unwrap().push(json!({"destination":"/input","type":"bind","source":"/sandbox/driver-data","options":["rbind","ro","nosuid","nodev","noexec"]}));
    spec
}
fn candidate_spec(environment: Vec<String>) -> serde_json::Value {
    common_spec(
        "/candidate-root",
        1000,
        vec!["/opt/app/bin/kyro-app"],
        environment,
        67108864,
    )
}
async fn prepare_candidate(
    config: &SandboxConfig,
    volume: &str,
    run_id: uuid::Uuid,
    files: &BTreeMap<String, (Vec<u8>, u32)>,
) -> Result<()> {
    crate::cleanup::reap_expired(config).await?;
    docker(
        &crate::cleanup::disposable_args(
            config,
            1200,
            vec![
                "volume".into(),
                "create".into(),
                "--name".into(),
                volume.into(),
            ],
        )?,
        None,
        Duration::from_secs(10),
    )
    .await?
    .require("verifier_volume_failed")?;
    let name = format!("kyro-p2-candidate-prepare-{run_id}");
    let guard = Cleanup {
        name: name.clone(),
        active: true,
    };
    let result = async {
        docker(
            &crate::cleanup::disposable_args(
                config,
                180,
                vec![
                    "run".into(),
                    "-d".into(),
                    "--name".into(),
                    name.clone(),
                    "--network".into(),
                    "none".into(),
                    "--read-only".into(),
                    "--user".into(),
                    "0:0".into(),
                    "--cpus".into(),
                    "1".into(),
                    "--memory".into(),
                    "256m".into(),
                    "--memory-swap".into(),
                    "256m".into(),
                    "--pids-limit".into(),
                    "16".into(),
                    "--cap-drop".into(),
                    "ALL".into(),
                    "--mount".into(),
                    format!("type=volume,src={volume},dst=/candidate"),
                    "--entrypoint".into(),
                    "/bin/sh".into(),
                    config.tools_image.clone(),
                    "-c".into(),
                    "sleep 120".into(),
                ],
            )?,
            None,
            Duration::from_secs(30),
        )
        .await?
        .require("verifier_prepare_failed")?;
        docker(
            &[
                "exec".into(),
                name.clone(),
                "mkdir".into(),
                "-p".into(),
                "/candidate/proc".into(),
                "/candidate/dev/pts".into(),
                "/candidate/dev/shm".into(),
                "/candidate/tmp".into(),
                "/candidate/work".into(),
            ],
            None,
            Duration::from_secs(10),
        )
        .await?
        .require("verifier_prepare_failed")?;
        let archive = verifier_tar(files)?;
        docker(
            &[
                "exec".into(),
                "-i".into(),
                name.clone(),
                "tar".into(),
                "--no-same-owner".into(),
                "-xf".into(),
                "-".into(),
                "-C".into(),
                "/candidate".into(),
            ],
            Some(&archive),
            Duration::from_secs(30),
        )
        .await?
        .require("verifier_input_failed")?;
        Ok(())
    }
    .await;
    let cleanup = guard.close().await;
    match result {
        Ok(()) => cleanup,
        Err(e) => {
            let _ = cleanup;
            Err(e)
        }
    }
}
struct VolumeCleanup {
    name: String,
    supervisor: String,
    preparer: String,
    active: bool,
}
impl VolumeCleanup {
    async fn close(mut self) -> Result<()> {
        docker(
            &["volume".into(), "rm".into(), self.name.clone()],
            None,
            Duration::from_secs(10),
        )
        .await?
        .require("verifier_volume_cleanup_failed")?;
        self.active = false;
        Ok(())
    }
}
impl Drop for VolumeCleanup {
    fn drop(&mut self) {
        if self.active
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let name = self.name.clone();
            let supervisor = self.supervisor.clone();
            let preparer = self.preparer.clone();
            runtime.spawn(async move {
                // Cancellation may occur while either holder is alive.
                // Stop both before removing the volume, irrespective of
                // the destruction order of the independent container guard.
                for holder in [supervisor, preparer] {
                    let _ = docker(
                        &["rm".into(), "-f".into(), holder],
                        None,
                        Duration::from_secs(10),
                    )
                    .await;
                }
                let _ = docker(
                    &["volume".into(), "rm".into(), name],
                    None,
                    Duration::from_secs(10),
                )
                .await;
            });
        }
    }
}
fn verifier_tar(files: &BTreeMap<String, (Vec<u8>, u32)>) -> Result<Vec<u8>> {
    if files.len() > 128 || files.values().map(|(b, _)| b.len()).sum::<usize>() > 402653184 {
        return Err(fail("verifier_input_limit", "/input"));
    }
    let mut archive = tar::Builder::new(Vec::new());
    archive.follow_symlinks(false);
    for (p, (b, mode)) in files {
        if !crate::catalogue::source_path(p) || ![0o444, 0o555].contains(mode) {
            return Err(fail("verifier_input_path", "/input"));
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(b.len() as u64);
        header.set_mode(*mode);
        header.set_uid(1000);
        header.set_gid(1000);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        archive
            .append_data(&mut header, p, Cursor::new(b))
            .map_err(|_| fail("verifier_input_failed", "/input"))?;
    }
    archive
        .into_inner()
        .map_err(|_| fail("verifier_input_failed", "/input"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactBinding;
    fn config() -> SandboxConfig {
        SandboxConfig {
            tools_image: format!("sha256:{}", "1".repeat(64)),
            tools_root_volume: "kyro-p2-tools-root-unit".into(),
            tools_root_digest: "2".repeat(64),
        }
    }
    #[test]
    fn protected_report_requires_every_bound_observation() {
        let criteria = ProtectedCriteria::records(&config()).unwrap();
        let binding = ArtifactBinding {
            schema_version: 1,
            project_id: uuid::Uuid::new_v4(),
            application_id: uuid::Uuid::new_v4(),
            revision: 0,
            environment: kyro_domain::Environment::Development,
            lock_digest: "1".repeat(64),
            source_digest: "2".repeat(64),
            migration_digest: "3".repeat(64),
            configuration_digest: "4".repeat(64),
            runtime_base_digest: "5".repeat(64),
            tools_image_digest: "6".repeat(64),
            sandbox_profile_digest: "7".repeat(64),
        };
        // validate_report checks the context protocol only. The public execution
        // path separately verifies OCI bytes before this function can run.
        let image = OciArtifact {
            files: BTreeMap::new(),
            image_digest: format!("sha256:{}", "8".repeat(64)),
            binding,
        };
        let run_id = uuid::Uuid::new_v4();
        let mut report = ObservedReport {
            schema_version: 1,
            kind: "protected_application_verification".into(),
            run_id,
            image_digest: image.image_digest.clone(),
            binding_digest: digest(&image.binding).unwrap(),
            criteria_digest: criteria.digest().unwrap(),
            checks: criteria
                .required_checks
                .iter()
                .map(|name| {
                    (
                        name.clone(),
                        CheckReceipt {
                            input_digest: "a".repeat(64),
                            observed_digest: "b".repeat(64),
                            passed: true,
                        },
                    )
                })
                .collect(),
        };
        validate_report(&report, run_id, &image, &criteria).unwrap();
        report.checks.get_mut("fresh_database").unwrap().passed = false;
        assert!(validate_report(&report, run_id, &image, &criteria).is_err());
        report.checks.get_mut("fresh_database").unwrap().passed = true;
        report.run_id = uuid::Uuid::new_v4();
        assert!(validate_report(&report, run_id, &image, &criteria).is_err());
        report.run_id = run_id;
        report.criteria_digest = "9".repeat(64);
        assert!(validate_report(&report, run_id, &image, &criteria).is_err());
        report.criteria_digest = criteria.digest().unwrap();
        report.checks.remove("foreign_tenant");
        assert!(validate_report(&report, run_id, &image, &criteria).is_err());
    }
    #[test]
    fn candidate_has_no_driver_mount_and_diagnostics_redact_credentials() {
        let candidate = candidate_spec(vec![]);
        assert_eq!(candidate["root"]["readonly"], true);
        assert_eq!(candidate["process"]["user"]["uid"], 1000);
        assert!(
            candidate["mounts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["type"] != "bind")
        );
        assert_eq!(
            redact(
                b"password=private-runtime-value",
                &["private-runtime-value"]
            ),
            b"password=[REDACTED]"
        );
        let driver = driver_spec(4294967296);
        assert_eq!(
            driver["mounts"].as_array().unwrap().last().unwrap()["source"],
            "/sandbox/driver-data"
        );
    }
}
