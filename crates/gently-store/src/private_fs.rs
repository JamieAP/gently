//! Owner-only local state files on Unix. Other platforms retain default permissions.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() =>
            Err(io::Error::new(io::ErrorKind::InvalidInput, "private state path is a symlink")),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Create a private directory or restrict an existing directory.
/// Only call this on directories owned by this application or harness.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    reject_symlink(path)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)] {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    let meta = fs::metadata(path)?;
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "private directory is not owned by the current user"));
        }
    }
    if !meta.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "private state path is not a directory"));
    }
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn protect_options(options: &mut OpenOptions) {
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
}

fn protect_handle(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "private file must be owned by the current user with one link"));
        }
    }
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "private state path is not a regular file"));
    }
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Restrict an existing file before reading it. Missing files are allowed.
pub fn harden_existing_file(path: &Path) -> io::Result<()> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    protect_options(&mut options);
    match options.open(path) {
        Ok(file) => protect_handle(&file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Open or create a private file without truncating existing data.
pub fn open_private_file(path: &Path, append: bool) -> io::Result<File> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.create(true).write(true).append(append).truncate(false);
    protect_options(&mut options);
    let file = options.open(path)?;
    protect_handle(&file)?;
    Ok(file)
}

/// Write only after the file has owner-only permissions, including old files.
pub fn write_private_file(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    let mut file = open_private_file(path, false)?;
    file.set_len(0)?;
    file.write_all(bytes.as_ref())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn hardlinked_private_write_is_refused_before_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("original");
        let alias = dir.path().join("alias");
        fs::write(&target, "original fixture content").unwrap();
        fs::hard_link(&target, &alias).unwrap();
        let result = write_private_file(&alias, "replacement fixture");
        assert!(result.is_err(), "private writes followed a hardlink");
        assert_eq!(fs::read_to_string(&target).unwrap(), "original fixture content");
    }
}
