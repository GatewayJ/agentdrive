//! Prepare an empty host directory for a driver that creates its mount point.
use agentdrive_model::{Error, ErrorCode, Result};
use std::path::Path;

pub(crate) fn start<T>(path: &Path, mount: impl FnOnce() -> Result<T>) -> Result<T> {
    let removed = match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            let reparse = {
                use std::os::windows::fs::MetadataExt;
                metadata.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let reparse = metadata.file_type().is_symlink();
            if !metadata.is_dir() || reparse {
                return Err(Error::new(
                    ErrorCode::MountBusy,
                    "mount path must be an empty regular directory",
                ));
            }
            if std::fs::read_dir(path)?.next().transpose()?.is_some() {
                return Err(Error::new(
                    ErrorCode::MountBusy,
                    "mount directory must be empty",
                ));
            }
            // remove_dir also rejects files created after the emptiness check.
            std::fs::remove_dir(path)?;
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let result = mount();
    if removed && let Err(mut error) = result {
        if let Err(restore) = std::fs::create_dir(path)
            && restore.kind() != std::io::ErrorKind::AlreadyExists
        {
            error.message = format!(
                "{}; restoring mount directory failed: {restore}",
                error.message
            );
        }
        return Err(error);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_empty_directory_is_available_for_driver_creation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mount");
        std::fs::create_dir(&path).unwrap();
        start(&path, || {
            assert!(!path.exists());
            std::fs::create_dir(&path)?;
            Ok(())
        })
        .unwrap();
        assert!(path.is_dir());
    }

    #[test]
    fn failed_mount_restores_existing_directory_and_preserves_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mount");
        std::fs::create_dir(&path).unwrap();
        let failure = Error::new(ErrorCode::PermissionDenied, "driver rejected mount");
        let result = start::<()>(&path, || Err(failure.clone()));
        assert_eq!(result.unwrap_err(), failure);
        assert!(path.is_dir());
    }

    #[test]
    fn nonempty_directories_and_files_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mount");
        std::fs::create_dir(&path).unwrap();
        let file = path.join("data");
        std::fs::write(&file, b"preserve").unwrap();
        for target in [&path, &file] {
            assert_eq!(
                start::<()>(target, || panic!("driver must not start"))
                    .unwrap_err()
                    .code,
                ErrorCode::MountBusy
            );
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"preserve");
    }

    #[test]
    fn failed_mount_preserves_a_replacement_directory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mount");
        std::fs::create_dir(&path).unwrap();
        let error = start::<()>(&path, || {
            std::fs::create_dir(&path)?;
            std::fs::write(path.join("new"), b"new contents")?;
            Err(Error::new(ErrorCode::Io, "mount failed"))
        })
        .unwrap_err();
        assert_eq!(error.message, "mount failed");
        assert_eq!(std::fs::read(path.join("new")).unwrap(), b"new contents");
    }
}
