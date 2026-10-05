//! Resumable bytes from an observed release. Callers supply a logical name,
//! never an archive directory, executable, URL, ref or host path.
use crate::{
    Result,
    artifacts::{OciArtifact, SignedRelease},
    assembler::{SourceBundle, SourceManifest},
    digest, digest_bytes, fail,
    service::FactoryControl,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};
const CHUNK: usize = 65536;
const MAX_FILE: u64 = 536870912;
type DeliveryFiles = (BTreeMap<String, Vec<u8>>, Option<crate::export::GitExport>);

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind {
    Sources,
    Oci,
    Git,
}
#[derive(Clone, Debug, Serialize)]
pub struct ExportFile {
    pub size_bytes: u64,
    pub sha256: String,
}
#[derive(Serialize)]
pub struct ExportIndex {
    pub artifact_id: uuid::Uuid,
    pub source_digest: String,
    pub image_digest: String,
    pub format: ExportKind,
    pub chunk_bytes: usize,
    pub files: BTreeMap<String, ExportFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<crate::export::GitExport>,
}
pub struct ExportChunk {
    pub artifact_id: uuid::Uuid,
    pub path: String,
    pub offset: usize,
    pub next_offset: usize,
    pub complete: bool,
    pub file: ExportFile,
    pub chunk_sha256: String,
    pub bytes: Vec<u8>,
}
struct VerifiedFile {
    descriptor: ExportFile,
    chunks: Vec<String>,
    portable: Option<Vec<u8>>,
}
struct VerifiedExport {
    root: PathBuf,
    files: BTreeMap<String, VerifiedFile>,
    git: Option<crate::export::GitExport>,
}
/// Two verified exports only. OCI/source contents are dropped; Git retains at
/// most two 16 MiB bundles. Neither authorization nor sessions are cached.
#[derive(Default)]
pub(crate) struct ExportCache {
    entries: BTreeMap<String, Arc<VerifiedExport>>,
    order: VecDeque<String>,
}
impl VerifiedExport {
    fn new(
        root: PathBuf,
        files: BTreeMap<String, Vec<u8>>,
        git: Option<crate::export::GitExport>,
    ) -> Result<Self> {
        if files.len() > 1101 || files.values().map(Vec::len).sum::<usize>() > MAX_FILE as usize {
            return Err(fail("artifact_export_limit", "/export"));
        }
        let portable = git.is_some();
        if portable && files.values().map(Vec::len).sum::<usize>() > 16777216 {
            return Err(fail("artifact_export_limit", "/export"));
        }
        let files = files
            .into_iter()
            .map(|(path, bytes)| {
                let descriptor = ExportFile {
                    size_bytes: bytes.len() as u64,
                    sha256: digest_bytes(&bytes),
                };
                let chunks = bytes.chunks(CHUNK).map(digest_bytes).collect();
                (
                    path,
                    VerifiedFile {
                        descriptor,
                        chunks,
                        portable: portable.then_some(bytes),
                    },
                )
            })
            .collect();
        Ok(Self { root, files, git })
    }
    fn chunk(&self, path: &str, offset: usize) -> Result<(ExportFile, Vec<u8>)> {
        if !crate::catalogue::source_path(path) || offset > MAX_FILE as usize {
            return Err(fail("artifact_export_path_refused", "/export"));
        }
        let file = self
            .files
            .get(path)
            .ok_or(fail("artifact_export_path_refused", "/export"))?;
        let size = file.descriptor.size_bytes as usize;
        if offset >= size {
            return Err(fail("artifact_export_offset_refused", "/export"));
        }
        let end = (offset + CHUNK).min(size);
        if let Some(bytes) = &file.portable {
            return Ok((file.descriptor.clone(), bytes[offset..end].to_vec()));
        }
        crate::service::safe_directory(&self.root)?;
        let target = self.root.join(path);
        crate::service::safe_directory(
            target
                .parent()
                .ok_or(fail("artifact_export_path_refused", "/export"))?,
        )?;
        let meta = std::fs::symlink_metadata(&target)
            .map_err(|_| fail("artifact_export_unavailable", "/export"))?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() != file.descriptor.size_bytes
        {
            return Err(fail("artifact_export_file_refused", "/export"));
        }
        let mut input = std::fs::File::open(&target)
            .map_err(|_| fail("artifact_export_unavailable", "/export"))?;
        let opened = input
            .metadata()
            .map_err(|_| fail("artifact_export_unavailable", "/export"))?;
        if !opened.is_file() || opened.len() != meta.len() {
            return Err(fail("artifact_export_file_refused", "/export"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (opened.dev(), opened.ino()) != (meta.dev(), meta.ino()) {
                return Err(fail("artifact_export_file_refused", "/export"));
            }
        }
        let start = offset / CHUNK * CHUNK;
        let limit = end.div_ceil(CHUNK) * CHUNK;
        let limit = limit.min(size);
        input
            .seek(SeekFrom::Start(start as u64))
            .map_err(|_| fail("artifact_export_unavailable", "/export"))?;
        let mut blocks = vec![0; limit - start];
        input
            .read_exact(&mut blocks)
            .map_err(|_| fail("artifact_export_unavailable", "/export"))?;
        // Verify the enclosing blocks before returning even an unaligned range.
        // These hashes were derived only after the entire signed export passed
        // source/OCI validation; a changed file cannot turn into accepted bytes.
        for (i, bytes) in blocks.chunks(CHUNK).enumerate() {
            if digest_bytes(bytes) != file.chunks[start / CHUNK + i] {
                return Err(fail("artifact_export_source_changed", "/export"));
            }
        }
        Ok((
            file.descriptor.clone(),
            blocks[offset - start..end - start].to_vec(),
        ))
    }
}
impl FactoryControl {
    fn verified_export(
        &self,
        stored: &kyro_store::factory::FactoryArtifact,
        kind: ExportKind,
    ) -> Result<Arc<VerifiedExport>> {
        self.verify_stored_artifact(stored)?;
        let key = format!(
            "{}:{}:{kind:?}:{}",
            stored.artifact.id,
            stored.job_generation,
            digest(&(
                &stored.artifact.signed_release,
                &stored.artifact.signed_evidence,
                &stored.artifact.source_manifest
            ))?
        );
        let mut cache = self
            .exports
            .lock()
            .map_err(|_| fail("artifact_export_unavailable", "/export"))?;
        if let Some(entry) = cache.entries.get(&key) {
            return Ok(entry.clone());
        }
        let (files, git) = self.delivery_files(stored, kind)?;
        let attempt = self.attempt(stored);
        let root = attempt.join(if matches!(kind, ExportKind::Oci) {
            "candidate"
        } else {
            "sources"
        });
        let entry = Arc::new(VerifiedExport::new(root, files, git)?);
        while cache.entries.len() >= 2 {
            if let Some(old) = cache.order.pop_front() {
                cache.entries.remove(&old);
            }
        }
        cache.order.push_back(key.clone());
        cache.entries.insert(key, entry.clone());
        Ok(entry)
    }
    fn attempt(&self, stored: &kyro_store::factory::FactoryArtifact) -> PathBuf {
        self.config()
            .archive_root
            .join(stored.job_id.to_string())
            .join(format!("attempt-{}", stored.job_generation))
    }
    pub fn export_index(
        &self,
        stored: &kyro_store::factory::FactoryArtifact,
        kind: ExportKind,
    ) -> Result<ExportIndex> {
        let export = self.verified_export(stored, kind)?;
        Ok(ExportIndex {
            artifact_id: stored.artifact.id,
            source_digest: stored.artifact.source_digest.clone(),
            image_digest: stored.artifact.image_digest.clone(),
            format: kind,
            chunk_bytes: CHUNK,
            files: export
                .files
                .iter()
                .map(|(path, file)| (path.clone(), file.descriptor.clone()))
                .collect(),
            git: export.git.clone(),
        })
    }
    pub fn export_chunk(
        &self,
        stored: &kyro_store::factory::FactoryArtifact,
        kind: ExportKind,
        path: &str,
        offset: usize,
    ) -> Result<ExportChunk> {
        if !crate::catalogue::source_path(path) || offset > MAX_FILE as usize {
            return Err(fail("artifact_export_path_refused", "/export"));
        }
        let export = self.verified_export(stored, kind)?;
        let (file, bytes) = export.chunk(path, offset)?;
        let next_offset = offset + bytes.len();
        Ok(ExportChunk {
            artifact_id: stored.artifact.id,
            path: path.into(),
            offset,
            next_offset,
            complete: next_offset as u64 == file.size_bytes,
            file,
            chunk_sha256: digest_bytes(&bytes),
            bytes,
        })
    }
    fn delivery_files(
        &self,
        stored: &kyro_store::factory::FactoryArtifact,
        kind: ExportKind,
    ) -> Result<DeliveryFiles> {
        let attempt = self.attempt(stored);
        crate::service::safe_directory(&attempt)?;
        if matches!(kind, ExportKind::Oci) {
            let release: SignedRelease =
                serde_json::from_value(stored.artifact.signed_release.clone())
                    .map_err(|_| fail("artifact_reference_invalid", "/export"))?;
            let image = OciArtifact::read(
                &attempt.join("candidate"),
                &stored.artifact.image_digest,
                release.release.binding,
            )?;
            return Ok((image.files, None));
        }
        let manifest: SourceManifest =
            serde_json::from_value(stored.artifact.source_manifest.clone())
                .map_err(|_| fail("source_manifest_invalid", "/export"))?;
        if manifest.files.len() > 1100
            || manifest
                .files
                .values()
                .any(|file| file.size_bytes > 2097152)
            || manifest
                .files
                .values()
                .map(|file| file.size_bytes)
                .sum::<u64>()
                > 16777216
        {
            return Err(fail("artifact_export_limit", "/export"));
        }
        let directory = attempt.join("sources");
        let mut files = BTreeMap::new();
        for (path, proof) in &manifest.files {
            if !crate::catalogue::source_path(path) {
                return Err(fail("artifact_export_path_refused", "/export"));
            }
            let bytes = read_export_file(&directory, path, proof.size_bytes)?;
            if digest_bytes(&bytes) != proof.sha256 {
                return Err(fail("artifact_export_source_changed", "/export"));
            }
            files.insert(path.clone(), bytes);
        }
        let bundle = SourceBundle { manifest, files };
        bundle.verify()?;
        let manifest_bytes = read_export_file(
            &directory,
            "source-manifest.json",
            serde_json::to_vec(&bundle.manifest)
                .map_err(|_| fail("source_manifest_invalid", "/export"))?
                .len() as u64,
        )?;
        if manifest_bytes
            != serde_json::to_vec(&bundle.manifest)
                .map_err(|_| fail("source_manifest_invalid", "/export"))?
            || digest(&bundle.manifest)? != stored.artifact.source_digest
        {
            return Err(fail("artifact_export_source_changed", "/export"));
        }
        if matches!(kind, ExportKind::Git) {
            let (receipt, bytes) = crate::export::portable_bundle(&bundle, &attempt.join("git"))?;
            return Ok((
                BTreeMap::from([("application.bundle".into(), bytes)]),
                Some(receipt),
            ));
        }
        let mut files = bundle.files;
        files.insert("source-manifest.json".into(), manifest_bytes);
        Ok((files, None))
    }
}
fn read_export_file(root: &Path, path: &str, size: u64) -> Result<Vec<u8>> {
    crate::service::safe_directory(root)?;
    let target = root.join(path);
    crate::service::safe_directory(
        target
            .parent()
            .ok_or(fail("artifact_export_path_refused", "/export"))?,
    )?;
    let meta = std::fs::symlink_metadata(&target)
        .map_err(|_| fail("artifact_export_unavailable", "/export"))?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != size || size > 2097152 {
        return Err(fail("artifact_export_file_refused", "/export"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(target)
        .map_err(|_| fail("artifact_export_unavailable", "/export"))?
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| fail("artifact_export_unavailable", "/export"))?;
    if bytes.len() as u64 != size {
        return Err(fail("artifact_export_file_refused", "/export"));
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resumed_ranges_verify_enclosing_blocks_and_reject_late_tampering_and_paths() {
        let root = std::env::temp_dir().join(format!("kyro-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let bytes: Vec<u8> = (0..CHUNK * 3 + 11).map(|i| (i % 251) as u8).collect();
        std::fs::write(root.join("data.bin"), &bytes).unwrap();
        let export = VerifiedExport::new(
            root.clone(),
            BTreeMap::from([("data.bin".into(), bytes.clone())]),
            None,
        )
        .unwrap();
        for offset in [0, 1, CHUNK - 1, CHUNK, CHUNK * 3, bytes.len() - 1] {
            let (file, chunk) = export.chunk("data.bin", offset).unwrap();
            assert_eq!(file.sha256, digest_bytes(&bytes));
            assert_eq!(chunk, bytes[offset..(offset + CHUNK).min(bytes.len())]);
        }
        for path in ["../data.bin", "data.bin/other", "missing.bin"] {
            assert!(export.chunk(path, 0).is_err());
        }
        assert!(export.chunk("data.bin", bytes.len()).is_err());
        let mut changed = bytes.clone();
        changed[CHUNK - 1] ^= 1;
        std::fs::write(root.join("data.bin"), &changed).unwrap();
        assert!(export.chunk("data.bin", CHUNK - 1).is_err());
        assert_eq!(
            export.chunk("data.bin", CHUNK * 2).unwrap().1,
            bytes[CHUNK * 2..CHUNK * 3]
        );
        std::fs::remove_file(root.join("data.bin")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", root.join("data.bin")).unwrap();
            assert!(export.chunk("data.bin", 0).is_err());
            std::fs::remove_file(root.join("data.bin")).unwrap();
        }
        std::fs::remove_dir(root).unwrap();
    }
}
