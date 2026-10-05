//! Operator-owned offline media Sentry. Jobs supply bytes and a closed request,
//! never an executable, image, mount, path or resource ceiling.
use crate::{
    Result, digest, digest_bytes, fail,
    sandbox::{Cleanup, SandboxConfig, docker, docker_with_limit, input_tar},
};
use kyro_app::documents::{
    processing::{MediaInput, MediaReceipt, input_digest},
    processor::{ProcessorOutput, ProcessorRequest},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    pub sandbox: SandboxConfig,
    pub processor_volume: String,
    pub processor_digest: String,
}
impl MediaConfig {
    pub fn validate(&self) -> Result<()> {
        self.sandbox.validate()?;
        if !self
            .processor_volume
            .starts_with("kyro-p2-media-processor-")
            || self.processor_volume.len() > 100
            || !self
                .processor_volume
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || !kyro_domain::factory::valid_digest(&self.processor_digest)
        {
            return Err(fail("media_configuration_invalid", "/media"));
        }
        Ok(())
    }
    pub fn profile(&self) -> Result<serde_json::Value> {
        self.validate()?;
        Ok(
            json!({"schema_version":1,"engine":"runsc-20260928.0","platform":"systrap","outer_uid":1000,"guest_uid":0,
            "network":"none","cpu":1,"memory_bytes":536870912u64,"pids":128,"guest_processes":32,"workspace_bytes":134217728u64,"output_bytes":16777216u64,
            "timeout_seconds":30,"tools_image":self.sandbox.tools_image,"tools_root_digest":self.sandbox.tools_root_digest,
            "processor_digest":self.processor_digest,"renderer_digest":digest_bytes(include_bytes!("media.rs"))}),
        )
    }
}
pub struct MediaSandbox {
    config: MediaConfig,
}
impl MediaSandbox {
    pub fn new(config: MediaConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }
    pub async fn process(&self, input: &MediaInput) -> Result<(ProcessorOutput, MediaReceipt)> {
        self.process_bytes(input.request(), input.bytes()).await
    }
    /// The fixed-byte interface also permits a test fixture; it does not commit
    /// an application result and cannot sign a catalogue qualification.
    pub async fn process_bytes(
        &self,
        request: &ProcessorRequest,
        bytes: &[u8],
    ) -> Result<(ProcessorOutput, MediaReceipt)> {
        self.process_bounded(request, bytes, Duration::from_secs(30))
            .await
    }
    #[cfg(feature = "test-support")]
    pub async fn process_with_deadline(
        &self,
        request: &ProcessorRequest,
        bytes: &[u8],
        deadline: Duration,
    ) -> Result<(ProcessorOutput, MediaReceipt)> {
        if deadline.is_zero() || deadline > Duration::from_secs(30) {
            return Err(fail("media_deadline_invalid", "/media"));
        }
        self.process_bounded(request, bytes, deadline).await
    }
    async fn process_bounded(
        &self,
        request: &ProcessorRequest,
        bytes: &[u8],
        deadline: Duration,
    ) -> Result<(ProcessorOutput, MediaReceipt)> {
        if bytes.is_empty()
            || bytes.len() > 5242880
            || digest_bytes(bytes) != request.source_sha256()
        {
            return Err(fail("media_input_refused", "/input"));
        }
        let run = uuid::Uuid::new_v4();
        crate::cleanup::reap_expired(&self.config.sandbox).await?;
        let name = format!("kyro-p2-media-{run}");
        let guard = Cleanup {
            name: name.clone(),
            active: true,
        };
        let result=async {
            docker(&crate::cleanup::disposable_args(&self.config.sandbox,240,vec!["run".into(),"-d".into(),"--name".into(),name.clone(),"--network".into(),"none".into(),"--read-only".into(),
                "--user".into(),"1000:1000".into(),"--cpus".into(),"1".into(),"--memory".into(),"512m".into(),"--memory-swap".into(),"512m".into(),"--pids-limit".into(),"128".into(),
                "--cap-drop".into(),"ALL".into(),"--security-opt".into(),"no-new-privileges".into(),"--security-opt".into(),"seccomp=unconfined".into(),
                "--mount".into(),format!("type=volume,src={},dst=/tools-root,readonly",self.config.sandbox.tools_root_volume),
                "--mount".into(),format!("type=volume,src={},dst=/processor,readonly",self.config.processor_volume),
                "--tmpfs".into(),"/sandbox:rw,nosuid,nodev,size=64m,uid=1000,gid=1000,mode=0700".into(),
                "--tmpfs".into(),"/tmp:rw,nosuid,nodev,size=16m,uid=1000,gid=1000,mode=1777".into(),
                "--entrypoint".into(),"/bin/sh".into(),self.config.sandbox.tools_image.clone(),"-c".into(),"sleep 180".into()])?,None,Duration::from_secs(30)).await?.require("media_sandbox_launch_failed")?;
            let root=docker(&["exec".into(),name.clone(),"sha256sum".into(),"/tools-root/.kyro-root-files.sha256".into()],None,Duration::from_secs(10)).await?;
            root.require("media_tools_unavailable")?;
            if !String::from_utf8_lossy(&root.stdout).starts_with(&self.config.sandbox.tools_root_digest){return Err(fail("media_tools_changed","/media"));}
            docker(&["exec".into(),"--user".into(),"0:0".into(),name.clone(),"/bin/sh".into(),"-c".into(),"cd /tools-root && sha256sum --quiet -c .kyro-root-files.sha256 && find . ! -name '.kyro-root-*' -printf '%y %m %U %G %p %l\\n' | LC_ALL=C sort | cmp - .kyro-root-metadata.txt".into()],None,Duration::from_secs(60)).await?.require("media_tools_changed")?;
            let processor=docker(&["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),"test -f /processor/kyro-app-worker && test ! -L /processor/kyro-app-worker && test -x /processor/kyro-app-worker && test $(find /processor -mindepth 1 | wc -l) -eq 1 && sha256sum /processor/kyro-app-worker".into()],None,Duration::from_secs(10)).await?;
            processor.require("media_processor_unavailable")?;
            if !String::from_utf8_lossy(&processor.stdout).starts_with(&self.config.processor_digest){return Err(fail("media_processor_changed","/media"));}
            docker(&["exec".into(),name.clone(),"mkdir".into(),"-p".into(),"/sandbox/input".into(),"/sandbox/output".into()],None,Duration::from_secs(10)).await?.require("media_prepare_failed")?;
            docker(&["exec".into(),name.clone(),"chmod".into(),"0777".into(),"/sandbox/output".into()],None,Duration::from_secs(10)).await?.require("media_prepare_failed")?;
            let spec=media_spec();
            let files=BTreeMap::from([("config.json".into(),serde_json::to_vec(&spec).map_err(|_|fail("media_spec_invalid","/media"))?),
                ("input/request.json".into(),serde_json::to_vec(request).map_err(|_|fail("media_input_refused","/input"))?),("input/content".into(),bytes.to_vec())]);
            let archive=input_tar(&files)?;
            docker(&["exec".into(),"-i".into(),name.clone(),"tar".into(),"--no-same-owner".into(),"-xf".into(),"-".into(),"-C".into(),"/sandbox".into()],Some(&archive),Duration::from_secs(15)).await?.require("media_input_failed")?;
            docker(&["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),"cd /sandbox && GOMAXPROCS=2 /usr/local/bin/runsc --root=/sandbox/state --ignore-cgroups=true --network=none --platform=systrap run media".into()],None,deadline).await?.require("media_processing_refused")?;
            let status=docker(&["exec".into(),name.clone(),"/usr/local/bin/runsc".into(),"--root=/sandbox/state".into(),"list".into(),"--format=json".into()],None,Duration::from_secs(5)).await?;
            status.require("media_reap_failed")?;
            let states:Option<Vec<serde_json::Value>>=serde_json::from_slice(&status.stdout).map_err(|_|fail("media_reap_failed","/media"))?;
            if states.is_some_and(|s|!s.is_empty()) {docker(&["exec".into(),name.clone(),"/usr/local/bin/runsc".into(),"--root=/sandbox/state".into(),"delete".into(),"--force".into(),"media".into()],None,Duration::from_secs(5)).await?.require("media_reap_failed")?;}
            let metadata=docker(&["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),"test -f /sandbox/output/result.json && test ! -L /sandbox/output/result.json && stat -c %s /sandbox/output/result.json".into()],None,Duration::from_secs(5)).await?;
            metadata.require("media_output_missing")?;
            let size=std::str::from_utf8(&metadata.stdout).ok().and_then(|s|s.trim().parse::<usize>().ok()).filter(|s|(1..=12582912).contains(s)).ok_or(fail("media_output_limit","/output"))?;
            let output=docker_with_limit(&["exec".into(),name.clone(),"cat".into(),"/sandbox/output/result.json".into()],None,Duration::from_secs(10),12582912).await?;
            output.require("media_output_unavailable")?;
            if output.truncated || output.stdout.len()!=size{return Err(fail("media_output_limit","/output"));}
            let value:ProcessorOutput=serde_json::from_slice(&output.stdout).map_err(|_|fail("media_output_invalid","/output"))?;
            // Hash canonical output, matching the protected application parser.
            let canonical=serde_json::to_vec(&value).map_err(|_|fail("media_output_invalid","/output"))?;
            let receipt=MediaReceipt {schema_version:1,run_id:run,input_digest:input_digest(request,bytes).map_err(|_|fail("media_input_refused","/input"))?,output_digest:digest_bytes(&canonical),
                processor_digest:self.config.processor_digest.clone(),tools_image_digest:self.config.sandbox.tools_image[7..].into(),tools_root_digest:self.config.sandbox.tools_root_digest.clone(),
                profile_digest:digest(&self.config.profile()?)?,renderer_digest:digest_bytes(include_bytes!("media.rs"))};
            Ok((value,receipt))
        }.await;
        let closed = guard.close().await;
        match (result, closed) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (_, Err(error)) => Err(error),
        }
    }
}
fn media_spec() -> serde_json::Value {
    let mut spec = crate::verifier::common_spec(
        "/tools-root",
        0,
        vec!["/processor/kyro-app-worker", "--media-process"],
        vec![
            "PATH=/usr/bin:/bin".into(),
            "HOME=/work".into(),
            "LANG=C.UTF-8".into(),
            "OMP_THREAD_LIMIT=1".into(),
            "TOKIO_WORKER_THREADS=2".into(),
        ],
        134217728,
    );
    spec["hostname"] = json!("kyro-media");
    spec["linux"]["resources"] = json!({"memory":{"limit":536870912u64},"cpu":{"quota":100000,"period":100000},"pids":{"limit":128}});
    spec["process"]["rlimits"] = json!([{"type":"RLIMIT_NOFILE","hard":128,"soft":128},{"type":"RLIMIT_NPROC","hard":32,"soft":32},{"type":"RLIMIT_FSIZE","hard":12582912,"soft":12582912}]);
    spec["mounts"].as_array_mut().unwrap().extend([
        json!({"destination":"/input","type":"bind","source":"/sandbox/input","options":["rbind","ro","nosuid","nodev","noexec"]}),
        json!({"destination":"/output","type":"bind","source":"/sandbox/output","options":["rbind","rw","nosuid","nodev","noexec"]}),
        json!({"destination":"/processor","type":"bind","source":"/processor","options":["rbind","ro","nosuid","nodev"]})]);
    spec
}
