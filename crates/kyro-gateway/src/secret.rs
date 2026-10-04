use kyro_domain::{Error, Result};
use std::path::Path;
#[cfg(unix)]
use std::{fs::OpenOptions, io::Read};
use zeroize::Zeroizing;

/// Reads one regular, owner-only file without following a symlink; errors carry no path or bytes.
pub(crate) fn read_secret_file(path: &Path) -> Result<Zeroizing<String>> {
    if !path.is_absolute() {
        return Err(Error::Invalid("chemin secret absolu requis".into()));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(Error::Invalid(
            "fichier runtime secret réservé à Unix".into(),
        ))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        // Runtime parents are immutable mounts, except the owner-only secret
        // tmpfs. Refuse redirected ancestors as well as the final component.
        for parent in path.ancestors().skip(1) {
            let metadata = std::fs::symlink_metadata(parent).map_err(|_| Error::Unavailable)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(Error::Invalid("répertoire secret non conforme".into()));
            }
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| Error::Unavailable)?;
        let metadata = file.metadata().map_err(|_| Error::Unavailable)?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.uid()
                != std::fs::metadata("/proc/self")
                    .map_err(|_| Error::Unavailable)?
                    .uid()
            || metadata.nlink() != 1
            || metadata.len() == 0
            || metadata.len() > 8192
        {
            return Err(Error::Invalid("fichier secret non conforme".into()));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.by_ref()
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Unavailable)?;
        if bytes.len() > 8192 {
            return Err(Error::ResourceLimit);
        }
        let value =
            std::str::from_utf8(&bytes).map_err(|_| Error::Invalid("secret invalide".into()))?;
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid("secret invalide".into()));
        }
        Ok(Zeroizing::new(value.to_owned()))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn secret_file_rejects_permissions_symlinks_and_unbounded_content() {
        let directory = std::env::temp_dir().join(format!("kyro-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("key");
        std::fs::write(&file, "test-canary-file-secret").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            read_secret_file(&file).unwrap().as_str(),
            "test-canary-file-secret"
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_secret_file(&file).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.join("link");
        symlink(&file, &link).unwrap();
        assert!(read_secret_file(&link).is_err());
        let parent_link = directory.join("redirected");
        symlink(&directory, &parent_link).unwrap();
        assert!(read_secret_file(&parent_link.join("key")).is_err());
        let hard_link = directory.join("hard-link");
        std::fs::hard_link(&file, &hard_link).unwrap();
        assert!(read_secret_file(&file).is_err());
        std::fs::remove_file(&hard_link).unwrap();
        assert!(read_secret_file(&directory).is_err());
        std::fs::write(&file, vec![b'a'; 8193]).unwrap();
        assert!(read_secret_file(&file).is_err());
        std::fs::write(&file, b"Bearer bad\n").unwrap();
        assert!(read_secret_file(&file).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
