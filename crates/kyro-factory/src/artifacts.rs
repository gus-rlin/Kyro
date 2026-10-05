//! OCI content and evidence are bound to bytes, never to a builder's assertion.
use crate::{
    Result,
    crypto::{Purpose, Signer, Trust},
    digest, digest_bytes, fail,
};
use kyro_domain::factory::valid_digest;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read},
    path::Path,
};

const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const LAYER: &str = "application/vnd.oci.image.layer.v1.tar";
const MAX_IMAGE: usize = 536870912;
pub const BINARIES: [&str; 3] = ["kyro-app", "kyro-app-migrate", "kyro-app-worker"];
pub const BASE_FILES: [&str; 5] = [
    "lib64/ld-linux-x86-64.so.2",
    "lib/x86_64-linux-gnu/libc.so.6",
    "lib/x86_64-linux-gnu/libm.so.6",
    "lib/x86_64-linux-gnu/libgcc_s.so.1",
    "etc/ssl/certs/ca-certificates.crt",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBinding {
    pub schema_version: u32,
    pub project_id: uuid::Uuid,
    pub application_id: uuid::Uuid,
    pub revision: i64,
    pub environment: kyro_domain::Environment,
    pub lock_digest: String,
    pub source_digest: String,
    pub migration_digest: String,
    pub configuration_digest: String,
    pub runtime_base_digest: String,
    pub tools_image_digest: String,
    pub sandbox_profile_digest: String,
}
impl ArtifactBinding {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.project_id.is_nil()
            || self.application_id.is_nil()
            || self.revision < 0
            || [
                &self.lock_digest,
                &self.source_digest,
                &self.migration_digest,
                &self.configuration_digest,
                &self.runtime_base_digest,
                &self.tools_image_digest,
                &self.sandbox_profile_digest,
            ]
            .iter()
            .any(|s| !valid_digest(s))
        {
            return Err(fail("artifact_binding_invalid", "/artifact"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Descriptor {
    media_type: String,
    digest: String,
    size: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Index {
    schema_version: u32,
    manifests: Vec<Descriptor>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ImageManifest {
    schema_version: u32,
    media_type: String,
    config: Descriptor,
    layers: Vec<Descriptor>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageConfig {
    architecture: String,
    os: String,
    config: RuntimeConfig,
    rootfs: RootFs,
    kyro: ArtifactBinding,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "PascalCase")]
struct RuntimeConfig {
    user: String,
    working_dir: String,
    entrypoint: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootFs {
    r#type: String,
    diff_ids: Vec<String>,
}
pub struct OciArtifact {
    pub files: BTreeMap<String, Vec<u8>>,
    pub image_digest: String,
    pub binding: ArtifactBinding,
}
fn descriptor(media: &str, value: &[u8]) -> Descriptor {
    Descriptor {
        media_type: media.into(),
        digest: format!("sha256:{}", digest_bytes(value)),
        size: value.len() as u64,
    }
}
fn blob_path(d: &Descriptor) -> Result<String> {
    let hash = d
        .digest
        .strip_prefix("sha256:")
        .filter(|h| valid_digest(h))
        .ok_or(fail("oci_descriptor_invalid", "/descriptor"))?;
    if d.size == 0 || d.size > MAX_IMAGE as u64 {
        return Err(fail("oci_descriptor_limit", "/descriptor"));
    }
    Ok(format!("blobs/sha256/{hash}"))
}
fn append(builder: &mut tar::Builder<Vec<u8>>, path: &str, bytes: &[u8], mode: u32) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(bytes))
        .map_err(|_| fail("oci_layer_failed", "/layer"))
}
/// `base` is pinned public runtime content supplied by the operator, not by a build.
pub fn package(
    binding: ArtifactBinding,
    base: &BTreeMap<String, Vec<u8>>,
    binaries: &BTreeMap<String, Vec<u8>>,
    configuration: &[u8],
) -> Result<OciArtifact> {
    binding.validate()?;
    if base.keys().map(String::as_str).collect::<BTreeSet<_>>() != BTreeSet::from(BASE_FILES)
        || binaries.keys().map(String::as_str).collect::<BTreeSet<_>>() != BTreeSet::from(BINARIES)
        || base.values().any(|b| b.is_empty() || b.len() > 4194304)
        || binaries
            .values()
            .any(|b| b.len() < 4 || &b[..4] != b"\x7fELF" || b.len() > 134217728)
        || configuration.len() > 524288
        || digest_bytes(configuration) != binding.configuration_digest
    {
        return Err(fail("oci_inputs_invalid", "/artifact"));
    }
    let base_hashes: BTreeMap<_, _> = base
        .iter()
        .map(|(p, b)| (p.clone(), digest_bytes(b)))
        .collect();
    if digest(&base_hashes)? != binding.runtime_base_digest {
        return Err(fail("oci_runtime_base_changed", "/base"));
    }
    let mut tar = tar::Builder::new(Vec::new());
    tar.follow_symlinks(false);
    for (p, b) in base {
        append(
            &mut tar,
            p,
            b,
            if p == "lib64/ld-linux-x86-64.so.2" {
                0o555
            } else {
                0o444
            },
        )?;
    }
    for (p, b) in binaries {
        append(&mut tar, &format!("opt/app/bin/{p}"), b, 0o555)?;
    }
    append(&mut tar, "opt/app/application.json", configuration, 0o444)?;
    let layer = tar
        .into_inner()
        .map_err(|_| fail("oci_layer_failed", "/layer"))?;
    let layer_descriptor = descriptor(LAYER, &layer);
    let config = serde_json::to_vec(&ImageConfig {
        architecture: "amd64".into(),
        os: "linux".into(),
        config: RuntimeConfig {
            user: "1000:1000".into(),
            working_dir: "/opt/app".into(),
            entrypoint: vec!["/opt/app/bin/kyro-app".into()],
        },
        rootfs: RootFs {
            r#type: "layers".into(),
            diff_ids: vec![layer_descriptor.digest.clone()],
        },
        kyro: binding.clone(),
    })
    .map_err(|_| fail("oci_serialization", "/config"))?;
    let config_descriptor = descriptor(CONFIG, &config);
    let manifest = serde_json::to_vec(&ImageManifest {
        schema_version: 2,
        media_type: MANIFEST.into(),
        config: config_descriptor.clone(),
        layers: vec![layer_descriptor.clone()],
    })
    .map_err(|_| fail("oci_serialization", "/manifest"))?;
    let image_descriptor = descriptor(MANIFEST, &manifest);
    let index = serde_json::to_vec(&Index {
        schema_version: 2,
        manifests: vec![image_descriptor.clone()],
    })
    .map_err(|_| fail("oci_serialization", "/index"))?;
    let files = BTreeMap::from([
        (
            "oci-layout".into(),
            b"{\"imageLayoutVersion\":\"1.0.0\"}".to_vec(),
        ),
        ("index.json".into(), index),
        (blob_path(&image_descriptor)?, manifest),
        (blob_path(&config_descriptor)?, config),
        (blob_path(&layer_descriptor)?, layer),
    ]);
    let artifact = OciArtifact {
        files,
        image_digest: image_descriptor.digest,
        binding,
    };
    artifact.verify()?;
    Ok(artifact)
}
impl OciArtifact {
    /// Read only OCI descriptors. The expected binding/reference must come from
    /// the protected job record, never from an unsigned file beside the image.
    pub fn read(
        directory: &Path,
        expected_image_digest: &str,
        expected_binding: ArtifactBinding,
    ) -> Result<Self> {
        expected_binding.validate()?;
        if !expected_image_digest
            .strip_prefix("sha256:")
            .is_some_and(valid_digest)
        {
            return Err(fail("oci_reference_invalid", "/image"));
        }
        for p in [
            directory.to_path_buf(),
            directory.join("blobs"),
            directory.join("blobs/sha256"),
        ] {
            let m =
                fs::symlink_metadata(p).map_err(|_| fail("oci_directory_unavailable", "/image"))?;
            if !m.is_dir() || m.file_type().is_symlink() {
                return Err(fail("oci_directory_refused", "/image"));
            }
        }
        let read = |path: &str, limit: u64| -> Result<Vec<u8>> {
            if !crate::catalogue::source_path(path) {
                return Err(fail("oci_path_invalid", "/image"));
            }
            let p = directory.join(path);
            let m = fs::symlink_metadata(&p).map_err(|_| fail("oci_blob_missing", "/image"))?;
            if !m.is_file() || m.file_type().is_symlink() || m.len() == 0 || m.len() > limit {
                return Err(fail("oci_blob_refused", "/image"));
            }
            let file = fs::File::open(p).map_err(|_| fail("oci_blob_unavailable", "/image"))?;
            let mut bytes = Vec::new();
            file.take(limit + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| fail("oci_blob_unavailable", "/image"))?;
            if bytes.len() as u64 > limit {
                return Err(fail("oci_blob_refused", "/image"));
            }
            Ok(bytes)
        };
        let layout = read("oci-layout", 1024)?;
        let index_bytes = read("index.json", 4096)?;
        let index: Index = serde_json::from_slice(&index_bytes)
            .map_err(|_| fail("oci_index_invalid", "/image"))?;
        if index.schema_version != 2
            || index.manifests.len() != 1
            || index.manifests[0].digest != expected_image_digest
        {
            return Err(fail("oci_index_invalid", "/image"));
        }
        let manifest_path = blob_path(&index.manifests[0])?;
        let manifest_bytes = read(&manifest_path, 8192)?;
        let manifest: ImageManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|_| fail("oci_manifest_invalid", "/image"))?;
        if manifest.layers.len() != 1 {
            return Err(fail("oci_manifest_invalid", "/image"));
        }
        let config_path = blob_path(&manifest.config)?;
        let layer_path = blob_path(&manifest.layers[0])?;
        let config = read(&config_path, 8192)?;
        let layer = read(&layer_path, MAX_IMAGE as u64)?;
        let image = Self {
            files: BTreeMap::from([
                ("oci-layout".into(), layout),
                ("index.json".into(), index_bytes),
                (manifest_path, manifest_bytes),
                (config_path, config),
                (layer_path, layer),
            ]),
            image_digest: expected_image_digest.into(),
            binding: expected_binding,
        };
        image.verify()?;
        Ok(image)
    }
    pub fn verify(&self) -> Result<()> {
        if self.files.len() != 5
            || self.files.values().map(Vec::len).sum::<usize>() > MAX_IMAGE
            || self.files.get("oci-layout").map(Vec::as_slice)
                != Some(b"{\"imageLayoutVersion\":\"1.0.0\"}")
        {
            return Err(fail("oci_layout_invalid", "/image"));
        }
        let index: Index = serde_json::from_slice(
            self.files
                .get("index.json")
                .ok_or(fail("oci_index_missing", "/image"))?,
        )
        .map_err(|_| fail("oci_index_invalid", "/image"))?;
        if index.schema_version != 2
            || index.manifests.len() != 1
            || index.manifests[0].media_type != MANIFEST
            || index.manifests[0].digest != self.image_digest
        {
            return Err(fail("oci_index_invalid", "/image"));
        }
        let get = |d: &Descriptor| -> Result<&[u8]> {
            let bytes = self
                .files
                .get(&blob_path(d)?)
                .ok_or(fail("oci_blob_missing", "/image"))?;
            if bytes.len() as u64 != d.size || format!("sha256:{}", digest_bytes(bytes)) != d.digest
            {
                return Err(fail("oci_blob_changed", "/image"));
            }
            Ok(bytes.as_slice())
        };
        let m: ImageManifest = serde_json::from_slice(get(&index.manifests[0])?)
            .map_err(|_| fail("oci_manifest_invalid", "/image"))?;
        if m.schema_version != 2
            || m.media_type != MANIFEST
            || m.config.media_type != CONFIG
            || m.layers.len() != 1
            || m.layers[0].media_type != LAYER
        {
            return Err(fail("oci_manifest_invalid", "/image"));
        }
        let c: ImageConfig = serde_json::from_slice(get(&m.config)?)
            .map_err(|_| fail("oci_config_invalid", "/image"))?;
        if c.architecture != "amd64"
            || c.os != "linux"
            || c.config.user != "1000:1000"
            || c.config.working_dir != "/opt/app"
            || c.config.entrypoint != ["/opt/app/bin/kyro-app"]
            || c.rootfs.r#type != "layers"
            || c.rootfs.diff_ids != [m.layers[0].digest.clone()]
            || c.kyro != self.binding
        {
            return Err(fail("oci_config_invalid", "/image"));
        }
        self.binding.validate()?;
        let files = read_layer(get(&m.layers[0])?)?;
        let hashes: BTreeMap<_, _> = BASE_FILES
            .iter()
            .map(|p| ((*p).to_owned(), digest_bytes(&files[*p])))
            .collect();
        if digest(&hashes)? != self.binding.runtime_base_digest
            || digest_bytes(&files["opt/app/application.json"]) != self.binding.configuration_digest
        {
            return Err(fail("oci_content_binding_changed", "/image"));
        }
        let expected = BTreeSet::from([
            "oci-layout".to_string(),
            "index.json".into(),
            blob_path(&index.manifests[0])?,
            blob_path(&m.config)?,
            blob_path(&m.layers[0])?,
        ]);
        if self.files.keys().cloned().collect::<BTreeSet<_>>() != expected {
            return Err(fail("oci_extra_content", "/image"));
        }
        Ok(())
    }
    pub fn runtime_files(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        self.verify()?;
        let idx: Index = serde_json::from_slice(&self.files["index.json"])
            .map_err(|_| fail("oci_index_invalid", "/image"))?;
        let m: ImageManifest = serde_json::from_slice(&self.files[&blob_path(&idx.manifests[0])?])
            .map_err(|_| fail("oci_manifest_invalid", "/image"))?;
        read_layer(&self.files[&blob_path(&m.layers[0])?])
    }
    pub fn write(&self, root: &Path) -> Result<()> {
        self.verify()?;
        if root.exists() {
            return Err(fail("oci_output_exists", "/image"));
        }
        fs::create_dir(root).map_err(|_| fail("oci_output_unavailable", "/image"))?;
        fs::create_dir_all(root.join("blobs/sha256"))
            .map_err(|_| fail("oci_output_unavailable", "/image"))?;
        for (p, b) in &self.files {
            crate::assembler::atomic_write(&root.join(p), b)?;
        }
        Ok(())
    }
}
fn read_layer(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let expected: BTreeSet<String> = BASE_FILES
        .iter()
        .map(|p| (*p).to_owned())
        .chain(BINARIES.iter().map(|p| format!("opt/app/bin/{p}")))
        .chain(["opt/app/application.json".into()])
        .collect();
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut files = BTreeMap::new();
    for entry in archive
        .entries()
        .map_err(|_| fail("oci_layer_invalid", "/image"))?
    {
        let e = entry.map_err(|_| fail("oci_layer_invalid", "/image"))?;
        let p = std::str::from_utf8(&e.path_bytes())
            .map_err(|_| fail("oci_path_invalid", "/image"))?
            .to_owned();
        let size = e.size();
        let binary = p.starts_with("opt/app/bin/");
        if !expected.contains(&p)
            || files.contains_key(&p)
            || !e.header().entry_type().is_file()
            || size == 0
            || size > if binary { 134217728 } else { 4194304 }
            || e.header().mode().ok()
                != Some(if binary || p == "lib64/ld-linux-x86-64.so.2" {
                    0o555
                } else {
                    0o444
                })
            || e.header().uid().ok() != Some(0)
            || e.header().gid().ok() != Some(0)
            || e.header().mtime().ok() != Some(0)
        {
            return Err(fail("oci_layer_content_refused", "/image"));
        }
        let mut content = Vec::new();
        e.take(size + 1)
            .read_to_end(&mut content)
            .map_err(|_| fail("oci_layer_invalid", "/image"))?;
        if content.len() as u64 != size
            || binary && (content.len() < 4 || &content[..4] != b"\x7fELF")
        {
            return Err(fail("oci_layer_content_refused", "/image"));
        }
        files.insert(p, content);
    }
    if files.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err(fail("oci_layer_incomplete", "/image"));
    }
    Ok(files)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBundle {
    pub schema_version: u32,
    pub image_digest: String,
    pub binding: ArtifactBinding,
    pub criteria_digest: String,
    pub verifier_version: String,
    pub sandbox_run_id: uuid::Uuid,
    pub required_checks: BTreeSet<String>,
    pub passed_checks: BTreeSet<String>,
    pub observed_report_digest: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: chrono::DateTime<chrono::Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedEvidence {
    pub evidence: EvidenceBundle,
    pub signature: String,
}
impl SignedEvidence {
    pub fn verify(
        &self,
        image: &OciArtifact,
        criteria_digest: &str,
        required: &BTreeSet<String>,
        trust: &Trust,
    ) -> Result<()> {
        image.verify()?;
        let e = &self.evidence;
        trust.verify(Purpose::Evidence, e, &self.signature)?;
        if e.schema_version != 1
            || e.image_digest != image.image_digest
            || e.binding != image.binding
            || !valid_digest(criteria_digest)
            || e.criteria_digest != criteria_digest
            || e.required_checks != *required
            || required.is_empty()
            || e.passed_checks != *required
            || e.verifier_version != "kyro-verifier-1"
            || e.sandbox_run_id.is_nil()
            || !valid_digest(&e.observed_report_digest)
            || e.finished_at < e.started_at
            || (e.finished_at - e.started_at).num_seconds() > 600
            || e.finished_at > chrono::Utc::now() + chrono::Duration::seconds(5)
        {
            return Err(fail("evidence_context_invalid", "/evidence"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub artifact_id: uuid::Uuid,
    pub image_digest: String,
    pub evidence_digest: String,
    pub binding: ArtifactBinding,
    pub verification: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRelease {
    pub release: ReleaseManifest,
    pub signature: String,
}
impl SignedRelease {
    pub fn create(
        artifact_id: uuid::Uuid,
        image: &OciArtifact,
        evidence: &SignedEvidence,
        criteria: &str,
        required: &BTreeSet<String>,
        trust: &Trust,
        signer: &Signer,
    ) -> Result<Self> {
        evidence.verify(image, criteria, required, trust)?;
        if artifact_id.is_nil() || signer.purpose() != &Purpose::Release {
            return Err(fail("release_context_invalid", "/release"));
        }
        let release = ReleaseManifest {
            schema_version: 1,
            artifact_id,
            image_digest: image.image_digest.clone(),
            evidence_digest: digest(evidence)?,
            binding: image.binding.clone(),
            verification: "verified_synthetic".into(),
        };
        let signature = signer.sign(&release)?;
        Ok(Self { release, signature })
    }
    pub fn verify(
        &self,
        image: &OciArtifact,
        evidence: &SignedEvidence,
        criteria: &str,
        required: &BTreeSet<String>,
        trust: &Trust,
    ) -> Result<()> {
        evidence.verify(image, criteria, required, trust)?;
        trust.verify(Purpose::Release, &self.release, &self.signature)?;
        let r = &self.release;
        if r.schema_version != 1
            || r.artifact_id.is_nil()
            || r.image_digest != image.image_digest
            || r.binding != image.binding
            || r.evidence_digest != digest(evidence)?
            || r.verification != "verified_synthetic"
        {
            return Err(fail("release_context_invalid", "/release"));
        }
        Ok(())
    }
}
