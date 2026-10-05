//! Operator-configured P1 bridge. The build worker has public trust only; the
//! independent attestor signs and commits a release after observing its image.
use crate::{
    Result,
    artifacts::*,
    assembler::{self, SourceBundle},
    catalogue::SignedCatalogue,
    crypto::{PublicIdentity, Purpose, Signer, Trust},
    digest, digest_bytes, fail,
    resolver::{self, Permit},
    sandbox::{DockerSandbox, SandboxConfig},
    verifier::{ProtectedCriteria, ProtectedVerifier},
};
use kyro_domain::{
    Environment, Error,
    factory::SignedCompositionLock,
    task::{JobLease, JobPayload},
};
use kyro_store::{Store, factory::FactoryArtifactInput};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedKey {
    pub id: String,
    pub purpose: Purpose,
    pub public_pem: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub source_root: PathBuf,
    pub archive_root: PathBuf,
    pub tools_root: PathBuf,
    pub attestor_socket: PathBuf,
    pub runtime_base_root: PathBuf,
    pub runtime_base_digest: String,
    pub sandbox: SandboxConfig,
    pub capabilities: BTreeSet<String>,
    pub trust: Vec<TrustedKey>,
}
pub struct FactoryControl {
    config: OperatorConfig,
    trust: Trust,
    base: BTreeMap<String, Vec<u8>>,
    pub(crate) exports: std::sync::Mutex<crate::delivery::ExportCache>,
}
pub struct Prepared {
    pub bundle: SourceBundle,
    pub binding: ArtifactBinding,
    pub directory: PathBuf,
}
impl FactoryControl {
    pub fn new(config: OperatorConfig) -> Result<Self> {
        config.sandbox.validate()?;
        for root in [
            &config.source_root,
            &config.archive_root,
            &config.tools_root,
            &config.runtime_base_root,
        ] {
            safe_directory(root)?;
        }
        if !config.attestor_socket.is_absolute()
            || config.attestor_socket.as_os_str().len() > 100
            || config.capabilities.len() > 147
            || config.capabilities.iter().any(|v| !crate::label(v))
        {
            return Err(fail(
                "factory_operator_configuration_invalid",
                "/configuration",
            ));
        }
        safe_directory(
            config
                .attestor_socket
                .parent()
                .ok_or(fail("factory_socket_invalid", "/configuration"))?,
        )?;
        let trust = Trust::new(
            config
                .trust
                .iter()
                .map(|key| PublicIdentity {
                    id: key.id.clone(),
                    purposes: BTreeSet::from([key.purpose.clone()]),
                    pem: key.public_pem.as_bytes().to_vec(),
                })
                .collect(),
        )?;
        let mut base: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for file in BASE_FILES {
            let bytes = read_regular(&config.runtime_base_root.join(file), 4194304)?;
            if bytes.is_empty() {
                return Err(fail("factory_runtime_base_invalid", "/base"));
            }
            base.insert(file.into(), bytes);
        }
        let base_hashes: BTreeMap<_, _> = base
            .iter()
            .map(|(p, b)| (p.clone(), digest_bytes(b)))
            .collect();
        if !kyro_domain::factory::valid_digest(&config.runtime_base_digest)
            || digest(&base_hashes)? != config.runtime_base_digest
        {
            return Err(fail("factory_runtime_base_changed", "/base"));
        }
        Ok(Self {
            config,
            trust,
            base,
            exports: std::sync::Mutex::new(crate::delivery::ExportCache::default()),
        })
    }
    pub fn from_env() -> Result<Option<Arc<Self>>> {
        let Some(path) = std::env::var_os("KYRO_FACTORY_CONFIG_FILE") else {
            return Ok(None);
        };
        let config = serde_json::from_slice(&read_regular(Path::new(&path), 65536)?)
            .map_err(|_| fail("factory_configuration_invalid", "/configuration"))?;
        Ok(Some(Arc::new(Self::new(config)?)))
    }
    pub fn trust(&self) -> &Trust {
        &self.trust
    }
    pub fn config(&self) -> &OperatorConfig {
        &self.config
    }
    pub async fn current_catalogue(&self, store: &Store) -> kyro_domain::Result<SignedCatalogue> {
        let (revision, hash, body) = store.factory_catalogue().await?;
        self.catalogue(body, revision, &hash).map_err(domain_error)
    }
    fn catalogue(
        &self,
        body: serde_json::Value,
        revision: u64,
        hash: &str,
    ) -> Result<SignedCatalogue> {
        let catalogue: SignedCatalogue =
            serde_json::from_value(body).map_err(|_| fail("catalogue_invalid", "/catalogue"))?;
        catalogue.verify(&self.trust, revision)?;
        if catalogue.catalogue.revision != revision || digest(&catalogue.catalogue)? != hash {
            return Err(fail("catalogue_binding_invalid", "/catalogue"));
        }
        Ok(catalogue)
    }
    fn permit(&self, project_id: Uuid, revision: i64, environment: Environment) -> Permit {
        // P1 projects are application roots. Their immutable UUID is the stable
        // application identity; a request cannot bind a lock to another root.
        Permit {
            project_id,
            application_id: project_id,
            source_revision: revision,
            environment,
            capabilities: self.config.capabilities.clone(),
        }
    }
    pub async fn composition(
        &self,
        store: &Store,
        actor_id: Uuid,
        project_id: Uuid,
        revision: i64,
        signer: &Signer,
    ) -> kyro_domain::Result<SignedCompositionLock> {
        store.authorize(actor_id, project_id, "execute").await?;
        let snapshot = store.get_project(actor_id, project_id).await?;
        if snapshot.revision.revision != revision {
            return Err(Error::StaleRevision {
                expected: revision,
                current: snapshot.revision.revision,
            });
        }
        let catalogue = self.current_catalogue(store).await?;
        resolver::seal(
            resolver::resolve(
                &snapshot.revision.spec,
                &catalogue,
                &self.trust,
                &self.permit(project_id, revision, store.environment),
            )
            .map_err(domain_error)?,
            signer,
        )
        .map_err(domain_error)
    }
    pub async fn admit(
        &self,
        store: &Store,
        actor_id: Uuid,
        project_id: Uuid,
        revision: i64,
        lock: &SignedCompositionLock,
    ) -> kyro_domain::Result<()> {
        let snapshot = store.get_project(actor_id, project_id).await?;
        if snapshot.revision.revision != revision {
            return Err(Error::StaleRevision {
                expected: revision,
                current: snapshot.revision.revision,
            });
        }
        let catalogue = self.current_catalogue(store).await?;
        resolver::verify(
            lock,
            &snapshot.revision.spec,
            &catalogue,
            &self.trust,
            &self.permit(project_id, revision, store.environment),
        )
        .map_err(domain_error)
    }
    pub async fn prepare(&self, store: &Store, lease: &JobLease) -> kyro_domain::Result<Prepared> {
        let snapshot = store.factory_snapshot(lease).await?;
        let catalogue = self
            .catalogue(
                snapshot.signed_catalogue,
                snapshot.catalogue_revision,
                &snapshot.catalogue_digest,
            )
            .map_err(domain_error)?;
        let JobPayload::BuildApplication { lock } = &lease.payload else {
            return Err(Error::Invalid("factory payload required".into()));
        };
        let bundle = assembler::assemble(
            &self.config.source_root,
            &snapshot.spec,
            lock,
            &catalogue,
            &self.trust,
            &self.permit(lease.project_id, lease.source_revision, lease.environment),
        )
        .map_err(domain_error)?;
        let base_hashes: BTreeMap<_, _> = self
            .base
            .iter()
            .map(|(p, b)| (p.clone(), digest_bytes(b)))
            .collect();
        let binding = ArtifactBinding {
            schema_version: 1,
            project_id: lease.project_id,
            application_id: lease.project_id,
            revision: lease.source_revision,
            environment: lease.environment,
            lock_digest: digest(lock).map_err(domain_error)?,
            source_digest: digest(&bundle.manifest).map_err(domain_error)?,
            migration_digest: digest(&bundle.manifest.migrations).map_err(domain_error)?,
            configuration_digest: digest_bytes(&bundle.files["application.json"]),
            runtime_base_digest: digest(&base_hashes).map_err(domain_error)?,
            tools_image_digest: self.config.sandbox.tools_image[7..].into(),
            sandbox_profile_digest: digest(
                &self
                    .config
                    .sandbox
                    .builder_profile()
                    .map_err(domain_error)?,
            )
            .map_err(domain_error)?,
        };
        let job = self.config.archive_root.join(lease.job_id.to_string());
        managed_directory(&job).map_err(domain_error)?;
        let directory = job.join(format!("attempt-{}", lease.generation));
        managed_directory(&directory).map_err(domain_error)?;
        bundle
            .write(&directory.join("sources"))
            .map_err(domain_error)?;
        // Sources and Git are deterministically reconstructed on each retry.
        crate::export::export(&bundle, &directory.join("git")).map_err(domain_error)?;
        Ok(Prepared {
            bundle,
            binding,
            directory,
        })
    }
    pub async fn execute(
        &self,
        store: &Store,
        lease: &JobLease,
    ) -> kyro_domain::Result<kyro_domain::task::Job> {
        let prepared = self.prepare(store, lease).await?;
        let remaining = (lease.deadline - chrono::Utc::now())
            .to_std()
            .map_err(|_| Error::Conflict("factory deadline expired".into()))?;
        // The supervisor also has its fixed 900s ceiling. The durable P1 TTL is
        // stricter when it expires first; dropping the future triggers cleanup.
        let output = tokio::time::timeout(
            remaining,
            DockerSandbox::new(self.config.sandbox.clone())
                .map_err(domain_error)?
                .build(&prepared.bundle),
        )
        .await
        .map_err(|_| Error::Conflict("factory deadline expired".into()))?
        .map_err(domain_error)?;
        store.factory_snapshot(lease).await?;
        if output.profile_digest != prepared.binding.sandbox_profile_digest {
            return Err(Error::Invalid("factory profile changed".into()));
        }
        let image = package(
            prepared.binding.clone(),
            &self.base,
            &output.binaries,
            &prepared.bundle.files["application.json"],
        )
        .map_err(domain_error)?;
        image
            .write(&prepared.directory.join("candidate"))
            .map_err(domain_error)?;
        store.factory_snapshot(lease).await?;
        let request = AttestationRequest {
            actor_id: lease.actor_id,
            project_id: lease.project_id,
            job_id: lease.job_id,
            generation: lease.generation,
            lease_owner: lease.lease_owner,
            image_digest: image.image_digest.clone(),
        };
        let artifact =
            match attestor_client(&self.config.attestor_socket, &request, remaining).await {
                Ok(artifact) => artifact,
                Err(error) => {
                    // A transport failure can follow a committed release. Reload
                    // that exact protected reference instead of rebuilding/re-signing.
                    match self.committed(store, &request).await? {
                        Some(artifact) => artifact,
                        None => return Err(domain_error(error)),
                    }
                }
            };
        self.validate_artifact(&image, &prepared, &artifact)
            .map_err(domain_error)?;
        // The attestor committed artifact+job+event itself. The worker cannot
        // manufacture that reference through a builder report or a local key.
        let job = store
            .get_job(lease.actor_id, lease.project_id, lease.job_id)
            .await?;
        if job.status != kyro_domain::task::JobStatus::Succeeded
            || job.result
                != Some(kyro_domain::task::JobResult::BuildApplication {
                    artifact_id: artifact.id,
                    image_digest: artifact.image_digest,
                    release_digest: artifact.release_digest,
                })
        {
            return Err(Error::Conflict("attestation was not committed".into()));
        }
        Ok(job)
    }
    pub fn validate_artifact(
        &self,
        image: &OciArtifact,
        prepared: &Prepared,
        input: &FactoryArtifactInput,
    ) -> Result<()> {
        if image.binding != prepared.binding
            || input.application_id != image.binding.application_id
            || input.image_digest != image.image_digest
            || input.lock_digest != image.binding.lock_digest
            || input.source_digest != image.binding.source_digest
            || input.source_manifest
                != serde_json::to_value(&prepared.bundle.manifest)
                    .map_err(|_| fail("artifact_invalid", "/artifact"))?
        {
            return Err(fail("artifact_context_invalid", "/artifact"));
        }
        let evidence: SignedEvidence = serde_json::from_value(input.signed_evidence.clone())
            .map_err(|_| fail("evidence_invalid", "/evidence"))?;
        let release: SignedRelease = serde_json::from_value(input.signed_release.clone())
            .map_err(|_| fail("release_invalid", "/release"))?;
        let lock = serde_json::from_slice(&prepared.bundle.files["composition-lock.json"])
            .map_err(|_| fail("verifier_plan_invalid", "/verifier"))?;
        let criteria = ProtectedCriteria::composition(&self.config.sandbox, &lock)?;
        release.verify(
            image,
            &evidence,
            &criteria.digest()?,
            criteria.required_checks(),
            &self.trust,
        )?;
        if release.release.artifact_id != input.id
            || input.release_digest != digest(&release)?
            || input.evidence_digest != digest(&evidence)?
        {
            return Err(fail("artifact_reference_invalid", "/artifact"));
        }
        Ok(())
    }
    /// Historical evidence remains readable after a catalogue update. Running
    /// or rebuilding uses the fresh admission path instead of this read path.
    pub fn verify_stored_artifact(
        &self,
        stored: &kyro_store::factory::FactoryArtifact,
    ) -> Result<()> {
        let input = &stored.artifact;
        input
            .validate()
            .map_err(|_| fail("artifact_invalid", "/artifact"))?;
        let release: SignedRelease = serde_json::from_value(input.signed_release.clone())
            .map_err(|_| fail("release_invalid", "/release"))?;
        let evidence: SignedEvidence = serde_json::from_value(input.signed_evidence.clone())
            .map_err(|_| fail("evidence_invalid", "/evidence"))?;
        self.trust
            .verify(Purpose::Release, &release.release, &release.signature)?;
        self.trust
            .verify(Purpose::Evidence, &evidence.evidence, &evidence.signature)?;
        let r = &release.release;
        let e = &evidence.evidence;
        let b = &r.binding;
        let source: crate::assembler::SourceManifest =
            serde_json::from_value(input.source_manifest.clone())
                .map_err(|_| fail("source_manifest_invalid", "/artifact"))?;
        b.validate()?;
        if b.project_id != stored.project_id
            || b.application_id != stored.project_id
            || b.application_id != input.application_id
            || b.revision != stored.source_revision
            || b.environment != stored.environment
            || r.artifact_id != input.id
            || r.schema_version != 1
            || r.verification != "verified_synthetic"
            || r.image_digest != input.image_digest
            || r.evidence_digest != input.evidence_digest
            || digest(&evidence)? != input.evidence_digest
            || digest(&release)? != input.release_digest
            || b.lock_digest != input.lock_digest
            || b.source_digest != input.source_digest
            || digest(&source)? != input.source_digest
            || source.schema_version != 1
            || source.lock_digest != b.lock_digest
            || digest(&source.migrations)? != b.migration_digest
            || e.schema_version != 1
            || e.verifier_version != "kyro-verifier-1"
            || e.sandbox_run_id.is_nil()
            || !kyro_domain::factory::valid_digest(&e.criteria_digest)
            || !kyro_domain::factory::valid_digest(&e.observed_report_digest)
            || e.started_at > e.finished_at
            || (e.finished_at - e.started_at).num_seconds() > 600
            || e.finished_at > chrono::Utc::now() + chrono::Duration::seconds(5)
            || e.binding != *b
            || e.image_digest != r.image_digest
            || e.required_checks.is_empty()
            || e.required_checks != e.passed_checks
        {
            return Err(fail("artifact_reference_invalid", "/artifact"));
        }
        Ok(())
    }
    async fn committed(
        &self,
        store: &Store,
        request: &AttestationRequest,
    ) -> kyro_domain::Result<Option<FactoryArtifactInput>> {
        let job = store
            .get_job(request.actor_id, request.project_id, request.job_id)
            .await?;
        let Some(kyro_domain::task::JobResult::BuildApplication {
            artifact_id,
            image_digest,
            release_digest,
        }) = job.result
        else {
            return Ok(None);
        };
        if job.status != kyro_domain::task::JobStatus::Succeeded
            || image_digest != request.image_digest
        {
            return Ok(None);
        }
        let stored = store
            .get_factory_artifact(request.actor_id, request.project_id, artifact_id)
            .await?;
        if stored.job_id != request.job_id
            || stored.job_generation != request.generation
            || stored.lease_owner != request.lease_owner
            || stored.artifact.release_digest != release_digest
        {
            return Err(Error::Conflict("committed artifact fence changed".into()));
        }
        self.verify_stored_artifact(&stored).map_err(domain_error)?;
        Ok(Some(stored.artifact))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestationRequest {
    pub actor_id: Uuid,
    pub project_id: Uuid,
    pub job_id: Uuid,
    pub generation: i64,
    pub lease_owner: Uuid,
    pub image_digest: String,
}
pub struct Attestor {
    control: Arc<FactoryControl>,
    evidence: Signer,
    release: Signer,
    store: Store,
}
impl Attestor {
    pub fn new(
        control: Arc<FactoryControl>,
        store: Store,
        evidence: Signer,
        release: Signer,
    ) -> Result<Self> {
        if evidence.purpose() != &Purpose::Evidence || release.purpose() != &Purpose::Release {
            return Err(fail("attestor_signing_roles_invalid", "/attestor"));
        }
        Ok(Self {
            control,
            store,
            evidence,
            release,
        })
    }
    pub async fn attest(
        &self,
        request: &AttestationRequest,
    ) -> kyro_domain::Result<FactoryArtifactInput> {
        if request.generation < 1
            || !request
                .image_digest
                .strip_prefix("sha256:")
                .is_some_and(kyro_domain::factory::valid_digest)
        {
            return Err(Error::Invalid("attestation reference invalid".into()));
        }
        self.store
            .authorize(request.actor_id, request.project_id, "execute")
            .await?;
        if let Some(committed) = self.control.committed(&self.store, request).await? {
            return Ok(committed);
        }
        let lease = self
            .store
            .running_factory_lease(
                request.actor_id,
                request.project_id,
                request.job_id,
                request.generation,
                request.lease_owner,
            )
            .await?;
        let prepared = self.control.prepare(&self.store, &lease).await?;
        let image = OciArtifact::read(
            &prepared.directory.join("candidate"),
            &request.image_digest,
            prepared.binding.clone(),
        )
        .map_err(domain_error)?;
        let migrations = prepared
            .bundle
            .manifest
            .migrations
            .keys()
            .map(|path| (path.clone(), prepared.bundle.files[path].clone()))
            .collect();
        let verifier =
            ProtectedVerifier::new(self.control.config.sandbox.clone()).map_err(domain_error)?;
        let remaining = (lease.deadline - chrono::Utc::now())
            .to_std()
            .map_err(|_| Error::Conflict("factory deadline expired".into()))?;
        let lock = serde_json::from_slice(&prepared.bundle.files["composition-lock.json"])
            .map_err(|_| Error::Internal)?;
        let verification =
            tokio::time::timeout(remaining, verifier.composition(&image, &migrations, &lock));
        tokio::pin!(verification);
        let mut fences = tokio::time::interval(Duration::from_secs(1));
        let observed = loop {
            tokio::select! {
                result=&mut verification=>break result.map_err(|_|Error::Conflict("factory deadline expired".into()))?.map_err(domain_error)?,
                _=fences.tick()=>{self.store.factory_snapshot(&lease).await?;}
            }
        };
        self.store.factory_snapshot(&lease).await?;
        let evidence = observed.attest(&self.evidence).map_err(domain_error)?;
        let criteria = ProtectedCriteria::composition(&self.control.config.sandbox, &lock)
            .map_err(domain_error)?;
        let release = SignedRelease::create(
            Uuid::new_v4(),
            &image,
            &evidence,
            &criteria.digest().map_err(domain_error)?,
            criteria.required_checks(),
            &self.control.trust,
            &self.release,
        )
        .map_err(domain_error)?;
        let input = FactoryArtifactInput {
            id: release.release.artifact_id,
            application_id: image.binding.application_id,
            image_digest: image.image_digest.clone(),
            lock_digest: image.binding.lock_digest.clone(),
            source_digest: image.binding.source_digest.clone(),
            evidence_digest: digest(&evidence).map_err(domain_error)?,
            release_digest: digest(&release).map_err(domain_error)?,
            signed_release: serde_json::to_value(release).map_err(|_| Error::Internal)?,
            signed_evidence: serde_json::to_value(evidence).map_err(|_| Error::Internal)?,
            source_manifest: serde_json::to_value(&prepared.bundle.manifest)
                .map_err(|_| Error::Internal)?,
        };
        self.store
            .finish_build_application(&lease, &input, |value| {
                self.control
                    .validate_artifact(&image, &prepared, value)
                    .map_err(domain_error)
            })
            .await?;
        Ok(input)
    }
    #[cfg(unix)]
    pub async fn serve(
        self: Arc<Self>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        let socket = &self.control.config.attestor_socket;
        let (listener, _guard) = bind_attestor_socket(socket).await?;
        crate::cleanup::reap_expired(&self.control.config.sandbox).await?;
        let mut connections = tokio::task::JoinSet::new();
        let permit = Arc::new(tokio::sync::Semaphore::new(1));
        loop {
            tokio::select! {
                change=shutdown.changed()=>{if change.is_err()||*shutdown.borrow(){break;}}
                accepted=listener.accept()=> {
                    let (mut connection,_)=accepted.map_err(|_|fail("attestor_socket_unavailable","/attestor"))?;
                    let task=self.clone(); let permits=permit.clone();
                    // Bound accepted connections as well as concurrent verifiers.
                    if connections.len()>=8 {continue;}
                    connections.spawn(async move {
                        let Some(_permit)=permits.try_acquire_owned().ok() else{return;};
                        let result=tokio::time::timeout(Duration::from_secs(2),read_frame(&mut connection,4096)).await;
                        let Ok(Ok(bytes))=result else{return;};
                        let Ok(request)=serde_json::from_slice::<AttestationRequest>(&bytes) else{return;};
                        let response=match task.attest(&request).await {Ok(value)=>AttestationResponse::Verified {artifact:Box::new(value)},Err(_)=>AttestationResponse::Refused};
                        if let Ok(bytes)=serde_json::to_vec(&response) {let _=tokio::time::timeout(Duration::from_secs(2),write_frame(&mut connection,&bytes)).await;}
                    });
                }
                _=connections.join_next(),if !connections.is_empty()=>{}
            }
        }
        // Active work is stopped on service shutdown; sandbox Drop guards reap
        // disposable workloads. No response or DB release is fabricated.
        connections.shutdown().await;
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum AttestationResponse {
    Verified { artifact: Box<FactoryArtifactInput> },
    Refused,
}
#[cfg(unix)]
struct SocketCleanup {
    path: PathBuf,
    device: u64,
    inode: u64,
    _lock: std::fs::File,
}
#[cfg(unix)]
async fn bind_attestor_socket(path: &Path) -> Result<(tokio::net::UnixListener, SocketCleanup)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    safe_directory(
        path.parent()
            .ok_or(fail("attestor_socket_invalid", "/attestor"))?,
    )?;
    let lock_path = path.with_extension("lock");
    if let Ok(meta) = std::fs::symlink_metadata(&lock_path)
        && (!meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() != 0
            || meta.permissions().mode() & 0o077 != 0)
    {
        return Err(fail("attestor_lock_refused", "/attestor"));
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|_| fail("attestor_lock_unavailable", "/attestor"))?;
    let opened = lock
        .metadata()
        .map_err(|_| fail("attestor_lock_unavailable", "/attestor"))?;
    let named = std::fs::symlink_metadata(&lock_path)
        .map_err(|_| fail("attestor_lock_unavailable", "/attestor"))?;
    if !named.is_file()
        || named.file_type().is_symlink()
        || opened.dev() != named.dev()
        || opened.ino() != named.ino()
        || opened.len() != 0
        || opened.permissions().mode() & 0o077 != 0
    {
        return Err(fail("attestor_lock_refused", "/attestor"));
    }
    lock.try_lock()
        .map_err(|_| fail("attestor_already_running", "/attestor"))?;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err(fail("attestor_socket_refused", "/attestor"));
        }
        // The process lock is released by the kernel after a crash. A peer that
        // does not implement our lock protocol must also be preserved.
        match tokio::time::timeout(
            Duration::from_millis(200),
            tokio::net::UnixStream::connect(path),
        )
        .await
        {
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(path)
                    .map_err(|_| fail("attestor_socket_unavailable", "/attestor"))?;
            }
            _ => return Err(fail("attestor_already_running", "/attestor")),
        }
    }
    let listener = tokio::net::UnixListener::bind(path)
        .map_err(|_| fail("attestor_socket_unavailable", "/attestor"))?;
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| fail("attestor_socket_unavailable", "/attestor"))?;
    let guard = SocketCleanup {
        path: path.to_owned(),
        device: meta.dev(),
        inode: meta.ino(),
        _lock: lock,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
        .map_err(|_| fail("attestor_socket_permissions", "/attestor"))?;
    Ok((listener, guard))
}
#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|meta| meta.dev() == self.device && meta.ino() == self.inode)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(all(test, unix))]
mod socket_tests {
    use super::*;
    #[tokio::test]
    async fn socket_restart_recovers_stale_path_and_preserves_live_instance_and_foreign_file() {
        let root = std::env::temp_dir().join(format!("kyro-attestor-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("attestor.sock");
        let stale = tokio::net::UnixListener::bind(&path).unwrap();
        drop(stale);
        let (listener, guard) = bind_attestor_socket(&path).await.unwrap();
        assert_eq!(
            bind_attestor_socket(&path).await.err().unwrap().code,
            "attestor_already_running"
        );
        assert!(tokio::net::UnixStream::connect(&path).await.is_ok());
        drop(listener);
        drop(guard);
        assert!(!path.exists());
        let foreign = tokio::net::UnixListener::bind(&path).unwrap();
        assert_eq!(
            bind_attestor_socket(&path).await.err().unwrap().code,
            "attestor_already_running"
        );
        drop(foreign);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"foreign").unwrap();
        assert_eq!(
            bind_attestor_socket(&path).await.err().unwrap().code,
            "attestor_socket_refused"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"foreign");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(root.join("attestor.lock")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
#[cfg(unix)]
async fn attestor_client(
    socket: &Path,
    request: &AttestationRequest,
    timeout: Duration,
) -> Result<FactoryArtifactInput> {
    let mut stream = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::UnixStream::connect(socket),
    )
    .await
    .map_err(|_| fail("attestor_unavailable", "/attestor"))?
    .map_err(|_| fail("attestor_unavailable", "/attestor"))?;
    let bytes = serde_json::to_vec(request)
        .map_err(|_| fail("attestation_request_invalid", "/attestor"))?;
    if bytes.len() > 4096 {
        return Err(fail("attestation_request_invalid", "/attestor"));
    }
    tokio::time::timeout(Duration::from_secs(2), write_frame(&mut stream, &bytes))
        .await
        .map_err(|_| fail("attestor_timeout", "/attestor"))??;
    let bytes = tokio::time::timeout(timeout, read_frame(&mut stream, 65536))
        .await
        .map_err(|_| fail("attestor_timeout", "/attestor"))??;
    match serde_json::from_slice(&bytes)
        .map_err(|_| fail("attestation_response_invalid", "/attestor"))?
    {
        AttestationResponse::Verified { artifact } => Ok(*artifact),
        AttestationResponse::Refused => Err(fail("attestation_refused", "/attestor")),
    }
}
#[cfg(not(unix))]
async fn attestor_client(
    _: &Path,
    _: &AttestationRequest,
    _: Duration,
) -> Result<FactoryArtifactInput> {
    Err(fail("attestor_platform_unsupported", "/attestor"))
}
async fn read_frame(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    max: usize,
) -> Result<Vec<u8>> {
    let size = stream
        .read_u32()
        .await
        .map_err(|_| fail("attestor_transport_failed", "/attestor"))? as usize;
    if size == 0 || size > max {
        return Err(fail("attestor_response_limit", "/attestor"));
    }
    let mut bytes = vec![0; size];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| fail("attestor_transport_failed", "/attestor"))?;
    Ok(bytes)
}
async fn write_frame(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    bytes: &[u8],
) -> Result<()> {
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|_| fail("attestor_transport_failed", "/attestor"))?;
    stream
        .write_all(bytes)
        .await
        .map_err(|_| fail("attestor_transport_failed", "/attestor"))
}
pub fn load_signer(path: &Path, id: String, purpose: Purpose) -> Result<Signer> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|_| fail("signing_key_unavailable", "/signer"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(fail("signing_key_permissions", "/signer"));
        }
    }
    Signer::from_pem(id, purpose, Zeroizing::new(read_regular(path, 16384)?))
}
pub fn domain_error(error: crate::Diagnostic) -> Error {
    match error.code {
        "attestor_unavailable" | "attestor_timeout" => Error::Unavailable,
        _ => Error::Invalid(format!("factory: {}", error.code)),
    }
}
pub(crate) fn safe_directory(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(fail("factory_root_invalid", "/configuration"));
    }
    for ancestor in path.ancestors() {
        let meta = std::fs::symlink_metadata(ancestor)
            .map_err(|_| fail("factory_root_unavailable", "/configuration"))?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(fail("factory_root_refused", "/configuration"));
        }
    }
    Ok(())
}
fn managed_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir(path).map_err(|_| fail("factory_archive_unavailable", "/archive"))?;
    }
    safe_directory(path)
}
pub fn read_regular(path: &Path, max: u64) -> Result<Vec<u8>> {
    safe_directory(
        path.parent()
            .ok_or(fail("factory_file_invalid", "/configuration"))?,
    )?;
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| fail("factory_file_unavailable", "/configuration"))?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max {
        return Err(fail("factory_file_refused", "/configuration"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| fail("factory_file_unavailable", "/configuration"))?
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| fail("factory_file_unavailable", "/configuration"))?;
    if bytes.len() as u64 > max {
        return Err(fail("factory_file_limit", "/configuration"));
    }
    Ok(bytes)
}
