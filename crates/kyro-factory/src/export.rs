//! Local Git only. Fixed commands, no hooks, transport or ambient Git config.
use crate::{
    Result,
    assembler::{SourceBundle, atomic_write},
    digest, fail,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitExport {
    pub source_manifest_digest: String,
    pub commit: String,
    pub reference: String,
}
fn git(directory: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.fileMode=false",
            "-c",
            "protocol.file.allow=never",
            "-c",
            "safe.directory=*",
        ])
        .arg("-C")
        .arg(directory)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Kyro Factory")
        .env("GIT_AUTHOR_EMAIL", "factory@example.invalid")
        .env("GIT_COMMITTER_NAME", "Kyro Factory")
        .env("GIT_COMMITTER_EMAIL", "factory@example.invalid")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .env("LANG", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .output()
        .map_err(|_| fail("git_unavailable", "/export"))?;
    if !output.status.success() || output.stdout.len() > 2097152 || output.stderr.len() > 65536 {
        return Err(fail("git_export_failed", "/export"));
    }
    String::from_utf8(output.stdout).map_err(|_| fail("git_output_invalid", "/export"))
}
fn check_git_directory(directory: &Path) -> Result<()> {
    let path = directory.join(".git");
    if path.exists() {
        let meta =
            fs::symlink_metadata(&path).map_err(|_| fail("git_export_unavailable", "/export"))?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(fail("git_directory_refused", "/export"));
        }
        // A managed export never has worktrees, object alternates or links into a
        // user's repository. Inspect before executing any Git command there.
        for name in [
            "config",
            "HEAD",
            "objects",
            "objects/info",
            "objects/info/alternates",
            "refs",
            "refs/heads",
            "index",
        ] {
            let p = path.join(name);
            if let Ok(m) = fs::symlink_metadata(&p)
                && (m.file_type().is_symlink() || name == "objects/info/alternates")
            {
                return Err(fail("git_directory_refused", "/export"));
            }
        }
    }
    Ok(())
}
/// Repeating after source writes, an index write, or commit creation yields the
/// same tree/commit. No source revision is changed by an export failure.
pub fn export(bundle: &SourceBundle, directory: &Path) -> Result<GitExport> {
    export_inner(bundle, directory, false)
}
#[cfg(feature = "test-support")]
pub fn interrupt_after_commit(bundle: &SourceBundle, directory: &Path) -> Result<GitExport> {
    export_inner(bundle, directory, true)
}
fn export_inner(bundle: &SourceBundle, directory: &Path, interrupt: bool) -> Result<GitExport> {
    let manifest_digest = digest(&bundle.manifest)?;
    let start = directory.join(".kyro-export-start.json");
    if directory.join(".git").exists() && !start.exists() {
        return Err(fail("existing_git_repository_refused", "/export"));
    }
    if start.exists() {
        let meta = fs::symlink_metadata(&start)
            .map_err(|_| fail("export_checkpoint_invalid", "/export"))?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() > 4096
            || fs::read(&start).map_err(|_| fail("export_checkpoint_invalid", "/export"))?
                != serde_json::to_vec(&manifest_digest)
                    .map_err(|_| fail("export_checkpoint_invalid", "/export"))?
        {
            return Err(fail("export_resume_diverged", "/export"));
        }
    }
    bundle.write(directory)?;
    check_git_directory(directory)?;
    if !start.exists() {
        atomic_write(
            &start,
            &serde_json::to_vec(&manifest_digest)
                .map_err(|_| fail("export_checkpoint_invalid", "/export"))?,
        )?;
    }
    let marker = directory.join(".kyro-export.json");
    if marker.exists() {
        let meta = fs::symlink_metadata(&marker)
            .map_err(|_| fail("export_checkpoint_invalid", "/export"))?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 4096 {
            return Err(fail("export_checkpoint_invalid", "/export"));
        }
        let old: GitExport = serde_json::from_slice(
            &fs::read(&marker).map_err(|_| fail("export_checkpoint_invalid", "/export"))?,
        )
        .map_err(|_| fail("export_checkpoint_invalid", "/export"))?;
        if old.source_manifest_digest != manifest_digest {
            return Err(fail("export_resume_diverged", "/export"));
        }
        if git(directory, &["rev-parse", "refs/heads/kyro/application"])?.trim() != old.commit {
            return Err(fail("export_reference_diverged", "/export"));
        }
        verify_tree(bundle, directory, &old.commit)?;
        return Ok(old);
    }
    if !directory.join(".git").exists() {
        git(
            directory,
            &["init", "--quiet", "--initial-branch=kyro/application"],
        )?;
    }
    let files: Vec<_> = bundle
        .files
        .keys()
        .map(String::as_str)
        .chain(std::iter::once("source-manifest.json"))
        .collect();
    let mut add = vec!["add", "--force", "--"];
    add.extend(files);
    git(directory, &add)?;
    let tree = git(directory, &["write-tree"])?;
    let tree = tree.trim();
    if tree.len() != 40 || !tree.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(fail("git_tree_invalid", "/export"));
    }
    let commit = git(
        directory,
        &[
            "commit-tree",
            tree,
            "-m",
            &format!("Kyro source {manifest_digest}"),
        ],
    )?;
    let commit = commit.trim().to_owned();
    verify_tree(bundle, directory, &commit)?;
    if interrupt {
        return Err(fail("export_interrupted_after_commit", "/export"));
    }
    let old = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", "--verify", "refs/heads/kyro/application"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .map_err(|_| fail("git_unavailable", "/export"))?;
    if old.status.success() && String::from_utf8_lossy(&old.stdout).trim() != commit {
        return Err(fail("export_reference_diverged", "/export"));
    }
    // The managed ref is published only after the whole tree has been verified.
    git(
        directory,
        &["update-ref", "refs/heads/kyro/application", &commit],
    )?;
    let result = GitExport {
        source_manifest_digest: manifest_digest,
        commit,
        reference: "refs/heads/kyro/application".into(),
    };
    atomic_write(
        &marker,
        &serde_json::to_vec(&result).map_err(|_| fail("export_checkpoint_invalid", "/export"))?,
    )?;
    Ok(result)
}
fn verify_tree(bundle: &SourceBundle, directory: &Path, commit: &str) -> Result<()> {
    let listing = git(directory, &["ls-tree", "-r", "--name-only", commit])?;
    let names: std::collections::BTreeSet<_> = listing.lines().collect();
    let expected: std::collections::BTreeSet<_> = bundle
        .files
        .keys()
        .map(String::as_str)
        .chain(std::iter::once("source-manifest.json"))
        .collect();
    if names != expected {
        return Err(fail("git_tree_diverged", "/export"));
    }
    for (name, value) in &bundle.files {
        let bytes = git(directory, &["show", &format!("{commit}:{name}")])?;
        if bytes.as_bytes() != value {
            return Err(fail("git_blob_diverged", name));
        }
    }
    let manifest = git(
        directory,
        &["show", &format!("{commit}:source-manifest.json")],
    )?;
    if manifest.as_bytes()
        != serde_json::to_vec(&bundle.manifest)
            .map_err(|_| fail("source_serialization", "/manifest"))?
    {
        return Err(fail("git_blob_diverged", "/manifest"));
    }
    Ok(())
}

/// A portable bundle contains only the verified managed reference. Local paths,
/// operator Git configuration, hooks and untracked files are never exported.
pub fn portable_bundle(bundle: &SourceBundle, directory: &Path) -> Result<(GitExport, Vec<u8>)> {
    use std::io::Read;
    let receipt = export(bundle, directory)?;
    let mut child = Command::new("git")
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.file.allow=never",
            "-c",
            "safe.directory=*",
        ])
        .arg("-C")
        .arg(directory)
        .args(["bundle", "create", "-", "refs/heads/kyro/application"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LANG", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| fail("git_unavailable", "/export"))?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or(fail("git_bundle_unavailable", "/export"))?
        .take(16777217)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > 16777216 {
        let _ = child.kill();
        let _ = child.wait();
        return Err(fail("git_bundle_limit", "/export"));
    }
    if !child
        .wait()
        .map_err(|_| fail("git_bundle_unavailable", "/export"))?
        .success()
        || !(bytes.starts_with(b"# v2 git bundle\n") || bytes.starts_with(b"# v3 git bundle\n"))
    {
        return Err(fail("git_bundle_invalid", "/export"));
    }
    Ok((receipt, bytes))
}
