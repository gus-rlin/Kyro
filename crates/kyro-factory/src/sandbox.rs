//! The Docker supervisor has only an offline gVisor workload as its child.
//! Configurations here come from the operator; no job can select an image,
//! mount, command, resource limit, or host path.
use crate::{Result, assembler::SourceBundle, digest, digest_bytes, fail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, io::Cursor, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

const LOG_LIMIT: usize = 65536;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    pub tools_image: String,
    pub tools_root_volume: String,
    pub tools_root_digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxProfile {
    pub engine: String,
    pub platform: String,
    pub host_uid: u32,
    pub guest_uid: u32,
    pub cpus: u32,
    pub memory_bytes: u64,
    pub pids: u32,
    pub workspace_bytes: u64,
    pub output_bytes: u64,
    pub timeout_seconds: u64,
    pub network: String,
    pub tools_root_digest: String,
    pub driver_digest: String,
    pub renderer_digest: String,
}
impl SandboxConfig {
    pub fn validate(&self) -> Result<()> {
        if !self
            .tools_image
            .strip_prefix("sha256:")
            .is_some_and(kyro_domain::factory::valid_digest)
            || !self.tools_root_volume.starts_with("kyro-p2-tools-root-")
            || self.tools_root_volume.len() > 100
            || !self
                .tools_root_volume
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || !kyro_domain::factory::valid_digest(&self.tools_root_digest)
        {
            return Err(fail("sandbox_configuration_invalid", "/sandbox"));
        }
        Ok(())
    }
    pub fn builder_profile(&self) -> Result<SandboxProfile> {
        self.validate()?;
        Ok(SandboxProfile {
            engine: "runsc-20260928.0".into(),
            platform: "systrap".into(),
            host_uid: 1000,
            guest_uid: 0,
            cpus: 2,
            memory_bytes: 4294967296,
            pids: 256,
            workspace_bytes: 8589934592,
            output_bytes: 536870912,
            timeout_seconds: 900,
            network: "none".into(),
            tools_root_digest: self.tools_root_digest.clone(),
            driver_digest: digest_bytes(include_bytes!("../../../scripts/p2/sandbox/build.sh")),
            renderer_digest: digest_bytes(include_bytes!("sandbox.rs")),
        })
    }
}
pub struct BuildOutput {
    pub run_id: uuid::Uuid,
    pub profile_digest: String,
    pub binaries: BTreeMap<String, Vec<u8>>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub logs_truncated: bool,
}
pub struct DockerSandbox {
    config: SandboxConfig,
}
impl DockerSandbox {
    pub fn new(config: SandboxConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }
    pub async fn build(&self, bundle: &SourceBundle) -> Result<BuildOutput> {
        bundle.verify()?;
        crate::cleanup::reap_expired(&self.config).await?;
        let profile = self.config.builder_profile()?;
        let run_id = uuid::Uuid::new_v4();
        let name = format!("kyro-p2-build-{run_id}");
        let guard = Cleanup {
            name: name.clone(),
            active: true,
        };
        let outcome = async {
        docker(&crate::cleanup::disposable_args(&self.config,1200,vec!["run".into(),"-d".into(),"--name".into(),name.clone(),"--network".into(),"none".into(),"--read-only".into(),
            "--user".into(),"1000:1000".into(),"--cpus".into(),"2".into(),"--memory".into(),"4g".into(),"--memory-swap".into(),"4g".into(),"--pids-limit".into(),"256".into(),
            "--cap-drop".into(),"ALL".into(),"--security-opt".into(),"seccomp=unconfined".into(),
            "--mount".into(),format!("type=volume,src={},dst=/tools-root,readonly",self.config.tools_root_volume),
            "--tmpfs".into(),"/tmp:rw,nosuid,nodev,size=128m,uid=1000,gid=1000,mode=1777".into(),
            "--tmpfs".into(),"/sandbox:rw,nosuid,nodev,size=512m,uid=1000,gid=1000,mode=0700".into(),"--entrypoint".into(),"/bin/sh".into(),self.config.tools_image.clone(),
            "-c".into(),"sleep 960".into()])?,None,Duration::from_secs(30)).await?.require("sandbox_launch_failed")?;
        docker(&["exec".into(),name.clone(),"mkdir".into(),"-p".into(),"/sandbox/input".into(),"/sandbox/output".into(),"/sandbox/runsc-debug".into()],None,Duration::from_secs(10)).await?.require("sandbox_prepare_failed")?;
        // The single outer UID mapping need not equal the guest UID. This is
        // the only writable gofer directory, beneath a private 0700 parent.
        docker(&["exec".into(),name.clone(),"chmod".into(),"0777".into(),"/sandbox/output".into()],None,Duration::from_secs(10)).await?.require("sandbox_prepare_failed")?;
        let root_index = docker(&["exec".into(),name.clone(),"sha256sum".into(),"/tools-root/.kyro-root-files.sha256".into()],None,Duration::from_secs(10)).await?;
        root_index.require("sandbox_tools_unavailable")?;
        if !String::from_utf8_lossy(&root_index.stdout).starts_with(&self.config.tools_root_digest) { return Err(fail("sandbox_tools_changed", "/sandbox")); }
        docker(&["exec".into(),"--user".into(),"0:0".into(),name.clone(),"/bin/sh".into(),"-c".into(),"cd /tools-root && sha256sum --quiet -c .kyro-root-files.sha256 && find . ! -name '.kyro-root-*' -printf '%y %m %U %G %p %l\\n' | LC_ALL=C sort | cmp - .kyro-root-metadata.txt".into()],None,Duration::from_secs(90)).await?.require("sandbox_tools_changed")?;
        let driver = docker(&["exec".into(),name.clone(),"sha256sum".into(),"/tools-root/opt/kyro/build.sh".into()],None,Duration::from_secs(10)).await?;
        driver.require("sandbox_tools_unavailable")?;
        if !String::from_utf8_lossy(&driver.stdout).starts_with(&profile.driver_digest) { return Err(fail("sandbox_driver_changed", "/sandbox")); }
        let config = builder_spec(profile.workspace_bytes);
        let files = bundle.files.iter().map(|(p,b)| (format!("input/{p}"),b.clone())).chain([
            ("config.json".into(),serde_json::to_vec(&config).map_err(|_| fail("sandbox_spec_invalid", "/sandbox"))?),
        ]).collect();
        let tar = input_tar(&files)?;
        docker(&["exec".into(),"-i".into(),name.clone(),"tar".into(),"--no-same-owner".into(),"-xf".into(),"-".into(),"-C".into(),"/sandbox".into()],Some(&tar),Duration::from_secs(30)).await?.require("sandbox_input_failed")?;
        let mut execution = docker(&["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),
            "cd /sandbox && /usr/local/bin/runsc --root=/sandbox/state --debug --debug-log=/sandbox/runsc-debug/ --ignore-cgroups=true --network=none --platform=systrap run build".into()],None,Duration::from_secs(profile.timeout_seconds)).await?;
        if !execution.success {
            let debug = docker(&["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),"tail -n 80 /sandbox/runsc-debug/*".into()],None,Duration::from_secs(10)).await?;
            let kept = debug.stdout.len().min(LOG_LIMIT - execution.stderr.len());
            execution.stderr.extend_from_slice(&debug.stdout[..kept]);execution.truncated |= kept < debug.stdout.len() || debug.truncated;
        }
        execution.require("sandbox_build_failed")?;
        // Reap any descendant before inspecting a build output. A live guest
        // cannot race Docker's copy or controller filesystem validation.
        let states = docker(&["exec".into(),name.clone(),"/usr/local/bin/runsc".into(),"--root=/sandbox/state".into(),"list".into(),"--format=json".into()],None,Duration::from_secs(10)).await?;
        states.require("sandbox_reap_failed")?;
        let states: Option<Vec<serde_json::Value>> = serde_json::from_slice(&states.stdout).map_err(|_| fail("sandbox_reap_failed", "/sandbox"))?;
        if states.is_some_and(|s| !s.is_empty()) {
            docker(&["exec".into(),name.clone(),"/usr/local/bin/runsc".into(),"--root=/sandbox/state".into(),"delete".into(),"--force".into(),"build".into()],None,Duration::from_secs(10)).await?.require("sandbox_reap_failed")?;
        }
        let mut binaries = BTreeMap::new();
        for binary in crate::artifacts::BINARIES {
            let path = format!("/sandbox/output/{binary}");
            let metadata = docker(&["exec".into(),name.clone(),"/bin/sh".into(),"-c".into(),format!("test -f {path} && test ! -L {path} && stat -c %s {path}")],None,Duration::from_secs(10)).await?;
            metadata.require("sandbox_artifact_missing")?;
            let size = std::str::from_utf8(&metadata.stdout).ok().and_then(|s|s.trim().parse::<usize>().ok()).filter(|s| (4..=134217728).contains(s)).ok_or(fail("sandbox_artifact_refused", "/output"))?;
            // docker cp archives the container rootfs and can miss a live tmpfs.
            // A bounded exec stream reads the exact, now quiescent mount instead.
            let mut content = docker_with_limit(&["exec".into(),name.clone(),"cat".into(),path],None,Duration::from_secs(30),134217728).await?;
            if !content.success {content.stdout.clear();content.require("sandbox_artifact_unavailable")?;}
            if content.truncated || content.stdout.len()!=size { return Err(fail("sandbox_artifact_refused", "/output")); }
            let bytes=content.stdout;
            if &bytes[..4] != b"\x7fELF" { return Err(fail("sandbox_artifact_refused", "/output")); }
            binaries.insert(binary.into(),bytes);
        }
        Ok(BuildOutput {run_id,profile_digest:digest(&profile)?,binaries,logs_truncated:execution.truncated,stdout:execution.stdout,stderr:execution.stderr})
        }.await;
        let cleanup = guard.close().await;
        match outcome {
            Ok(output) => {
                cleanup?;
                Ok(output)
            }
            Err(error) => {
                let _ = cleanup;
                Err(error)
            }
        }
    }
}
pub(crate) fn builder_spec(workspace_bytes: u64) -> serde_json::Value {
    json!({"ociVersion":"1.2.1","root":{"path":"/tools-root","readonly":true},
        // Only guest UID 0 maps to the unprivileged outer UID 1000. A different
        // guest UID cannot create files in a gofer bind with this rootless map.
        // All guest capabilities are empty; root/input stay read-only. PID
        // enforcement comes from the outer cgroup, not root's RLIMIT_NPROC.
        "process":{"terminal":false,"user":{"uid":0,"gid":0},"args":["/bin/sh","/opt/kyro/build.sh"],"env":["PATH=/opt/rust/bin:/usr/bin:/bin","HOME=/work"],"cwd":"/",
            "noNewPrivileges":true,"capabilities":{"bounding":[],"effective":[],"inheritable":[],"permitted":[],"ambient":[]},
            "rlimits":[{"type":"RLIMIT_NOFILE","hard":512,"soft":512},{"type":"RLIMIT_NPROC","hard":256,"soft":256},{"type":"RLIMIT_FSIZE","hard":134217728,"soft":134217728}]},
        "hostname":"kyro-build","mounts":[{"destination":"/proc","type":"proc","source":"proc"},
            {"destination":"/dev","type":"tmpfs","source":"tmpfs","options":["nosuid","strictatime","mode=755","size=65536k"]},
            {"destination":"/dev/pts","type":"devpts","source":"devpts","options":["nosuid","noexec","newinstance","ptmxmode=0666","mode=0620"]},
            {"destination":"/dev/shm","type":"tmpfs","source":"shm","options":["nosuid","nodev","noexec","mode=1777","size=65536k"]},
            {"destination":"/tmp","type":"tmpfs","source":"tmpfs","options":["nosuid","nodev","mode=1777","size=134217728"]},
            {"destination":"/work","type":"tmpfs","source":"tmpfs","options":["nosuid","nodev","mode=1777",format!("size={workspace_bytes}")]},
            {"destination":"/input","type":"bind","source":"/sandbox/input","options":["rbind","ro","nosuid","nodev"]},
            {"destination":"/output","type":"bind","source":"/sandbox/output","options":["rbind","rw","nosuid","nodev","noexec"]}],
        "linux":{"namespaces":[{"type":"user"},{"type":"pid"},{"type":"ipc"},{"type":"uts"},{"type":"mount"}],
            "resources":{"memory":{"limit":4294967296u64},"cpu":{"quota":200000,"period":100000},"pids":{"limit":256}},
            "uidMappings":[{"containerID":0,"hostID":1000,"size":1}],"gidMappings":[{"containerID":0,"hostID":1000,"size":1}]}})
}
pub(crate) fn input_tar(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    if files.len() > 1100 || files.values().map(Vec::len).sum::<usize>() > 33554432 {
        return Err(fail("sandbox_input_limit", "/input"));
    }
    let mut tar = tar::Builder::new(Vec::new());
    tar.follow_symlinks(false);
    for (p, b) in files {
        if !crate::catalogue::source_path(p) {
            return Err(fail("sandbox_input_path_invalid", "/input"));
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(b.len() as u64);
        header.set_mode(0o444);
        header.set_uid(1000);
        header.set_gid(1000);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append_data(&mut header, p, Cursor::new(b))
            .map_err(|_| fail("sandbox_input_failed", "/input"))?;
    }
    tar.into_inner()
        .map_err(|_| fail("sandbox_input_failed", "/input"))
}
pub(crate) struct CommandOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}
impl CommandOutput {
    pub fn require(&self, code: &'static str) -> Result<()> {
        if self.success {
            Ok(())
        } else {
            let mut error = fail(code, "/sandbox");
            error.execution = Some(crate::ExecutionDiagnostic {
                stdout: self.stdout.clone(),
                stderr: self.stderr.clone(),
                truncated: self.truncated,
            });
            Err(error)
        }
    }
}
async fn bounded(
    mut pipe: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut result = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = pipe.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let kept = count.min(limit - result.len());
        result.extend_from_slice(&buffer[..kept]);
        truncated |= kept < count;
    }
    Ok((result, truncated))
}
pub(crate) async fn docker(
    args: &[String],
    input: Option<&[u8]>,
    deadline: Duration,
) -> Result<CommandOutput> {
    docker_with_limit(args, input, deadline, LOG_LIMIT).await
}
pub(crate) async fn docker_with_limit(
    args: &[String],
    input: Option<&[u8]>,
    deadline: Duration,
    stdout_limit: usize,
) -> Result<CommandOutput> {
    let mut command = Command::new("docker");
    command
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/tmp")
        .env("DOCKER_HOST", "unix:///var/run/docker.sock")
        .args(args)
        .kill_on_drop(true)
        .stdin(if input.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| fail("sandbox_supervisor_unavailable", "/sandbox"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or(fail("sandbox_supervisor_failed", "/sandbox"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(fail("sandbox_supervisor_failed", "/sandbox"))?;
    let execute = async {
        let send = async {
            if let Some(bytes) = input {
                let mut stdin = child
                    .stdin
                    .take()
                    .ok_or(std::io::Error::other("missing stdin"))?;
                stdin.write_all(bytes).await?;
                stdin.shutdown().await?;
            }
            Ok::<_, std::io::Error>(())
        };
        let (sent, out, err) = tokio::join!(
            send,
            bounded(stdout, stdout_limit),
            bounded(stderr, LOG_LIMIT)
        );
        sent?;
        let (stdout, a) = out?;
        let (stderr, b) = err?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>(CommandOutput {
            success: status.success(),
            stdout,
            stderr,
            truncated: a || b,
        })
    };
    match tokio::time::timeout(deadline, execute).await {
        Ok(value) => value.map_err(|_| fail("sandbox_supervisor_failed", "/sandbox")),
        Err(_) => {
            let _ = child.kill().await;
            Err(fail("sandbox_deadline_exceeded", "/sandbox"))
        }
    }
}
pub(crate) struct Cleanup {
    pub(crate) name: String,
    pub(crate) active: bool,
}
impl Cleanup {
    pub(crate) async fn close(mut self) -> Result<()> {
        let result = docker(
            &["rm".into(), "-f".into(), self.name.clone()],
            None,
            Duration::from_secs(10),
        )
        .await?;
        if !result.success {
            let remaining = docker(
                &[
                    "ps".into(),
                    "-a".into(),
                    "--filter".into(),
                    format!("name=^/{}$", self.name),
                    "--format".into(),
                    "{{.ID}}".into(),
                ],
                None,
                Duration::from_secs(10),
            )
            .await?;
            remaining.require("sandbox_cleanup_failed")?;
            if !remaining.stdout.is_empty() {
                return Err(fail("sandbox_cleanup_failed", "/cleanup"));
            }
        }
        self.active = false;
        Ok(())
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.active {
            // Never block an executor thread during cancellation. Explicit
            // completion/error cleanup is awaited; PID 1 is the crash fallback.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let name = self.name.clone();
                runtime.spawn(async move {
                    let _ = docker(
                        &["rm".into(), "-f".into(), name],
                        None,
                        Duration::from_secs(10),
                    )
                    .await;
                });
            }
        }
    }
}
