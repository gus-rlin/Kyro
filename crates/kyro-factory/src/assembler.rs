//! Files are copied by a verified manifest. Application values are JSON only.
use crate::{
    Result,
    catalogue::{SignedCatalogue, source_path},
    crypto::Trust,
    digest, digest_bytes, fail,
    resolver::Permit,
};
use kyro_domain::{factory::SignedCompositionLock, spec::AppSpec};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileProvenance {
    pub sha256: String,
    pub size_bytes: u64,
    pub components: BTreeSet<String>,
    pub template: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    pub schema_version: u32,
    pub lock_digest: String,
    pub catalogue_digest: String,
    pub toolchain: String,
    pub files: BTreeMap<String, FileProvenance>,
    pub migrations: BTreeMap<String, String>,
}
pub struct SourceBundle {
    pub manifest: SourceManifest,
    pub files: BTreeMap<String, Vec<u8>>,
}
pub fn runtime_source(path: &str) -> bool {
    if matches!(path, "Cargo.toml" | "Cargo.lock") {
        return true;
    }
    if matches!(
        path,
        "scripts/p2/sandbox/build.sh"
            | "scripts/p2/sandbox/verify-records.mjs"
            | "scripts/p2/sandbox/resource-probe.mjs"
            | "crates/kyro-agents/src/contract-v2.schema.json"
    ) {
        return true;
    }
    let p: Vec<_> = path.split('/').collect();
    p.len() >= 3
        && p[0] == "crates"
        && [
            "kyro-domain",
            "kyro-gateway",
            "kyro-app",
            "kyro-store",
            "kyro-api",
            "kyro-worker",
            "kyro-factory",
            "kyro-agents",
        ]
        .contains(&p[1])
        && (p.len() == 3 && matches!(p[2], "Cargo.toml" | "build.rs")
            || p.len() >= 4
                && (p[2] == "src" && path.ends_with(".rs")
                    || p[2] == "migrations" && path.ends_with(".sql")))
}
pub fn read_source(root: &Path, relative: &str) -> Result<Vec<u8>> {
    if !source_path(relative) || !runtime_source(relative) {
        return Err(fail("source_path_refused", relative));
    }
    let root = fs::canonicalize(root).map_err(|_| fail("source_root_unavailable", "/source"))?;
    let mut path = root.clone();
    for component in relative.split('/') {
        path.push(component);
        let meta =
            fs::symlink_metadata(&path).map_err(|_| fail("source_file_unavailable", relative))?;
        if meta.file_type().is_symlink() {
            return Err(fail("source_symlink_refused", relative));
        }
    }
    let meta = fs::metadata(&path).map_err(|_| fail("source_file_unavailable", relative))?;
    if !meta.is_file() || meta.len() > 2097152 {
        return Err(fail("source_file_limit", relative));
    }
    fs::read(&path).map_err(|_| fail("source_file_unavailable", relative))
}
pub fn assemble(
    root: &Path,
    spec: &AppSpec,
    lock: &SignedCompositionLock,
    catalogue: &SignedCatalogue,
    trust: &Trust,
    permit: &Permit,
) -> Result<SourceBundle> {
    crate::resolver::verify(lock, spec, catalogue, trust, permit)?;
    let mut expected: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    let mut migrations = BTreeMap::new();
    for (id, component) in &lock.lock.components {
        let manifest = catalogue.admitted(id, &component.version)?;
        for (path, sha) in &manifest.source_files {
            let (entry, owners) = expected
                .entry(path.clone())
                .or_insert_with(|| (sha.clone(), BTreeSet::new()));
            if entry != sha {
                return Err(fail("source_versions_incompatible", path));
            }
            owners.insert(id.clone());
        }
        for (path, sha) in &manifest.migration_digests {
            if migrations
                .insert(path.clone(), sha.clone())
                .is_some_and(|old| old != *sha)
            {
                return Err(fail("migration_versions_incompatible", path));
            }
            if manifest.source_files.get(path) != Some(sha) {
                return Err(fail("migration_not_in_source_manifest", path));
            }
        }
    }
    for mandatory in [
        "Cargo.toml",
        "Cargo.lock",
        "crates/kyro-app/Cargo.toml",
        "crates/kyro-app/src/main.rs",
    ] {
        if !expected.contains_key(mandatory) {
            return Err(fail("runtime_template_missing", mandatory));
        }
    }
    let mut files = BTreeMap::new();
    let mut provenance = BTreeMap::new();
    let mut bytes = 0usize;
    for (path, (sha, owners)) in expected {
        let value = read_source(root, &path)?;
        if digest_bytes(&value) != sha {
            return Err(fail("source_digest_mismatch", &path));
        }
        bytes = bytes
            .checked_add(value.len())
            .ok_or(fail("source_bundle_limit", "/sources"))?;
        if bytes > 16777216 {
            return Err(fail("source_bundle_limit", "/sources"));
        }
        provenance.insert(
            path.clone(),
            FileProvenance {
                sha256: sha,
                size_bytes: value.len() as u64,
                components: owners,
                template: "admitted-runtime-1".into(),
            },
        );
        files.insert(path, value);
    }
    let enabled: BTreeSet<_> = lock
        .lock
        .components
        .keys()
        .filter(|id| id[1..].parse::<u16>().is_ok_and(|n| n <= 160))
        .cloned()
        .collect();
    let generated = BTreeMap::from([
        (
            "crates/kyro-app/factory-components.json".to_owned(),
            serde_json::to_vec(&enabled)
                .map_err(|_| fail("source_serialization", "/configuration"))?,
        ),
        (
            "crates/kyro-app/factory-plan.json".to_owned(),
            serde_json::to_vec(lock).map_err(|_| fail("source_serialization", "/configuration"))?,
        ),
        (
            "application.json".to_owned(),
            serde_json::to_vec(spec).map_err(|_| fail("source_serialization", "/configuration"))?,
        ),
        (
            "composition-lock.json".to_owned(),
            serde_json::to_vec(lock).map_err(|_| fail("source_serialization", "/lock"))?,
        ),
        (
            "rust-toolchain.toml".to_owned(),
            b"[toolchain]\nchannel = \"1.96.1\"\nprofile = \"minimal\"\n".to_vec(),
        ),
        (
            ".gitignore".to_owned(),
            b"/target/\n/.env\n/.env.*\n*.pem\n*.key\n*.dump\n".to_vec(),
        ),
    ]);
    for (path, value) in generated {
        if files.contains_key(&path) {
            return Err(fail("generated_file_collision", path));
        }
        provenance.insert(
            path.clone(),
            FileProvenance {
                sha256: digest_bytes(&value),
                size_bytes: value.len() as u64,
                components: lock.lock.components.keys().cloned().collect(),
                template: "factory-json-1".into(),
            },
        );
        files.insert(path, value);
    }
    let manifest = SourceManifest {
        schema_version: 1,
        lock_digest: digest(lock)?,
        catalogue_digest: lock.lock.catalogue_digest.clone(),
        toolchain: lock.lock.toolchain.clone(),
        files: provenance,
        migrations,
    };
    Ok(SourceBundle { manifest, files })
}
impl SourceBundle {
    pub fn verify(&self) -> Result<()> {
        if self.files.len() != self.manifest.files.len()
            || self.manifest.schema_version != 1
            || self.manifest.toolchain != "rust-1.96.1-linux-x86_64"
            || self.files.len() > 1100
            || self.files.values().map(Vec::len).sum::<usize>() > 16777216
        {
            return Err(fail("source_bundle_corrupt", "/sources"));
        }
        let lock: SignedCompositionLock = serde_json::from_slice(
            self.files
                .get("composition-lock.json")
                .ok_or(fail("source_bundle_corrupt", "/lock"))?,
        )
        .map_err(|_| fail("source_bundle_corrupt", "/lock"))?;
        lock.validate_shape()
            .map_err(|_| fail("source_bundle_corrupt", "/lock"))?;
        if self.manifest.lock_digest != digest(&lock)?
            || self.manifest.catalogue_digest != lock.lock.catalogue_digest
            || self.manifest.toolchain != lock.lock.toolchain
        {
            return Err(fail("source_bundle_corrupt", "/manifest"));
        }
        let mut migrations = BTreeMap::new();
        for component in lock.lock.components.values() {
            for (path, hash) in &component.migration_digests {
                if migrations
                    .insert(path.clone(), hash.clone())
                    .is_some_and(|old| old != *hash)
                {
                    return Err(fail("source_bundle_corrupt", "/migrations"));
                }
            }
        }
        if migrations != self.manifest.migrations {
            return Err(fail("source_bundle_corrupt", "/migrations"));
        }
        for (path, value) in &self.files {
            let p = self
                .manifest
                .files
                .get(path)
                .ok_or(fail("source_bundle_corrupt", path))?;
            if !source_path(path)
                || p.sha256 != digest_bytes(value)
                || p.size_bytes != value.len() as u64
                || p.components.is_empty()
                || p.components
                    .iter()
                    .any(|id| !lock.lock.components.contains_key(id))
                || !matches!(p.template.as_str(), "admitted-runtime-1" | "factory-json-1")
            {
                return Err(fail("source_bundle_corrupt", path));
            }
        }
        for (path, hash) in &self.manifest.migrations {
            if self
                .manifest
                .files
                .get(path)
                .is_none_or(|p| p.sha256 != *hash)
            {
                return Err(fail("source_bundle_corrupt", "/migrations"));
            }
        }
        Ok(())
    }
    /// A caller supplies a new, managed job directory. Existing files are accepted
    /// only byte-for-byte, which makes a partially written source bundle resumable.
    pub fn write(&self, directory: &Path) -> Result<()> {
        self.verify()?;
        if directory.exists() {
            let m = fs::symlink_metadata(directory)
                .map_err(|_| fail("source_output_unavailable", "/output"))?;
            if m.file_type().is_symlink() || !m.is_dir() {
                return Err(fail("source_output_refused", "/output"));
            }
        } else {
            fs::create_dir(directory).map_err(|_| fail("source_output_unavailable", "/output"))?;
        }
        self.check_output_inventory(directory)?;
        for (path, value) in &self.files {
            let mut target = directory.to_owned();
            let mut parts = path.split('/').peekable();
            while let Some(part) = parts.next() {
                target.push(part);
                if parts.peek().is_some() {
                    match fs::symlink_metadata(&target) {
                        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
                        Ok(_) => return Err(fail("source_output_refused", path)),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            fs::create_dir(&target)
                                .map_err(|_| fail("source_output_unavailable", path))?
                        }
                        Err(_) => return Err(fail("source_output_unavailable", path)),
                    }
                }
            }
            match fs::symlink_metadata(&target) {
                Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
                    if m.len() != value.len() as u64 {
                        return Err(fail("source_resume_diverged", path));
                    }
                    let existing =
                        fs::read(&target).map_err(|_| fail("source_output_unavailable", path))?;
                    if existing != *value {
                        return Err(fail("source_resume_diverged", path));
                    }
                }
                Ok(_) => return Err(fail("source_output_refused", path)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => atomic_write(&target, value)?,
                Err(_) => return Err(fail("source_output_unavailable", path)),
            }
        }
        let manifest = serde_json::to_vec(&self.manifest)
            .map_err(|_| fail("source_serialization", "/manifest"))?;
        let path = directory.join("source-manifest.json");
        if path.exists() {
            let meta = fs::symlink_metadata(&path)
                .map_err(|_| fail("source_output_unavailable", "/manifest"))?;
            if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 2097152 {
                return Err(fail("source_output_refused", "/manifest"));
            }
            if fs::read(&path).map_err(|_| fail("source_output_unavailable", "/manifest"))?
                != manifest
            {
                return Err(fail("source_resume_diverged", "/manifest"));
            }
        } else {
            atomic_write(&path, &manifest)?;
        }
        Ok(())
    }

    fn check_output_inventory(&self, directory: &Path) -> Result<()> {
        let mut expected: BTreeSet<String> = self.files.keys().cloned().collect();
        expected.extend([
            "source-manifest.json".into(),
            ".kyro-export-start.json".into(),
            ".kyro-export.json".into(),
        ]);
        let mut contents = self.files.clone();
        contents.insert(
            "source-manifest.json".into(),
            serde_json::to_vec(&self.manifest)
                .map_err(|_| fail("source_serialization", "/manifest"))?,
        );
        let mut directories = BTreeSet::new();
        for path in &expected {
            let mut parent = Path::new(path).parent();
            while let Some(p) = parent {
                if p.as_os_str().is_empty() {
                    break;
                }
                directories.insert(p.to_string_lossy().replace('\\', "/"));
                parent = p.parent();
            }
        }
        fn scan(
            root: &Path,
            path: &Path,
            expected: &BTreeSet<String>,
            contents: &BTreeMap<String, Vec<u8>>,
            directories: &BTreeSet<String>,
            seen: &mut usize,
        ) -> Result<()> {
            for entry in
                fs::read_dir(path).map_err(|_| fail("source_output_unavailable", "/output"))?
            {
                *seen += 1;
                if *seen > 4096 {
                    return Err(fail("source_output_limit", "/output"));
                }
                let entry = entry.map_err(|_| fail("source_output_unavailable", "/output"))?;
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| fail("source_output_refused", "/output"))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let meta = fs::symlink_metadata(entry.path())
                    .map_err(|_| fail("source_output_unavailable", &relative))?;
                if meta.file_type().is_symlink() {
                    return Err(fail("source_output_refused", relative));
                }
                if relative == ".git" {
                    if !meta.is_dir() || !root.join(".kyro-export-start.json").is_file() {
                        return Err(fail("source_output_unexpected", relative));
                    }
                    continue;
                }
                if meta.is_dir() {
                    if !directories.contains(&relative) {
                        return Err(fail("source_output_unexpected", relative));
                    }
                    scan(root, &entry.path(), expected, contents, directories, seen)?;
                } else if !meta.is_file() {
                    return Err(fail("source_output_unexpected", relative));
                } else if !expected.contains(&relative) {
                    let file = entry.file_name().to_string_lossy().into_owned();
                    let pair = file
                        .strip_prefix('.')
                        .and_then(|v| v.strip_suffix(".tmp"))
                        .and_then(|v| v.rsplit_once('.'));
                    let Some((name, id)) = pair else {
                        return Err(fail("source_output_unexpected", relative));
                    };
                    if uuid::Uuid::parse_str(id)
                        .ok()
                        .is_none_or(|value| value.to_string() != id)
                    {
                        return Err(fail("source_output_unexpected", relative));
                    }
                    let source = entry
                        .path()
                        .parent()
                        .unwrap()
                        .join(name)
                        .strip_prefix(root)
                        .map_err(|_| fail("source_output_refused", "/output"))?
                        .to_string_lossy()
                        .replace('\\', "/");
                    let value = contents
                        .get(&source)
                        .ok_or(fail("source_output_unexpected", &relative))?;
                    if meta.len() > value.len() as u64 {
                        return Err(fail("source_output_unexpected", relative));
                    }
                    let partial = fs::read(entry.path())
                        .map_err(|_| fail("source_output_unavailable", &relative))?;
                    if !value.starts_with(&partial) {
                        return Err(fail("source_resume_diverged", relative));
                    }
                    // A killed atomic writer never published this file. Only a
                    // regular, bounded prefix of an exact admitted source can
                    // be discarded; its bytes never become build input.
                    fs::remove_file(entry.path())
                        .map_err(|_| fail("source_output_unavailable", relative))?;
                }
            }
            Ok(())
        }
        scan(
            directory,
            directory,
            &expected,
            &contents,
            &directories,
            &mut 0,
        )
    }
}
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .ok_or(fail("output_path_invalid", "/output"))?
        .to_string_lossy();
    let temp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| fail("output_write_failed", "/output"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| fail("output_write_failed", "/output"))?;
    fs::rename(&temp, path).map_err(|_| fail("output_publish_failed", "/output"))?;
    #[cfg(unix)]
    fs::File::open(
        path.parent()
            .ok_or(fail("output_path_invalid", "/output"))?,
    )
    .and_then(|f| f.sync_all())
    .map_err(|_| fail("output_publish_failed", "/output"))?;
    Ok(())
}
pub fn collect_runtime_sources(root: &Path) -> Result<BTreeMap<String, String>> {
    fn collect(root: &Path, directory: &Path, result: &mut BTreeMap<String, String>) -> Result<()> {
        let mut children = fs::read_dir(directory)
            .map_err(|_| fail("source_root_unavailable", "/source"))?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|_| fail("source_root_unavailable", "/source"))?;
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let path = child.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| fail("source_root_escape", "/source"))?
                .to_string_lossy()
                .replace('\\', "/");
            let meta = child
                .file_type()
                .map_err(|_| fail("source_file_unavailable", &relative))?;
            if meta.is_symlink() {
                return Err(fail("source_symlink_refused", relative));
            }
            if meta.is_dir() {
                if matches!(child.file_name().to_str(), Some("src" | "migrations"))
                    || directory.ends_with("src")
                    || directory.starts_with(root.join("crates"))
                        && directory.components().count() > root.components().count() + 2
                {
                    collect(root, &path, result)?;
                }
            } else if runtime_source(&relative) {
                let bytes = read_source(root, &relative)?;
                result.insert(relative, digest_bytes(&bytes));
            }
        }
        Ok(())
    }
    let root = fs::canonicalize(root).map_err(|_| fail("source_root_unavailable", "/source"))?;
    let mut files = BTreeMap::new();
    for path in ["Cargo.toml", "Cargo.lock", "scripts/p2/sandbox/build.sh"] {
        files.insert(path.into(), digest_bytes(&read_source(&root, path)?));
    }
    for name in [
        "kyro-domain",
        "kyro-gateway",
        "kyro-app",
        "kyro-store",
        "kyro-api",
        "kyro-worker",
        "kyro-factory",
        "kyro-agents",
    ] {
        collect(&root, &root.join("crates").join(name), &mut files)?;
    }
    if files.len() > 1024 {
        return Err(fail("source_file_count_limit", "/sources"));
    }
    Ok(files)
}
