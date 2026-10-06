//! Owner-only local state files on Unix. Other platforms retain default permissions.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state path is a symlink",
        )),
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    let meta = fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private directory is not owned by the current user",
            ));
        }
    }
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state path is not a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn protect_options(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
}

fn protect_handle(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    validate_private_file(&meta)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn validate_private_file(meta: &fs::Metadata) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private file must be owned by the current user with one link",
            ));
        }
    }
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state path is not a regular file",
        ));
    }
    Ok(())
}

// Never open/close an extra ordinary descriptor for an existing SQLite database
// or sidecar. On Unix, close releases this process's POSIX locks on that inode,
// including locks owned by SQLite; SHM can then be truncated under its mapping.
fn harden_regular_path(path: &Path) -> io::Result<bool> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state path is a symlink",
        ));
    }
    validate_private_file(&meta)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o7777 != 0o600 {
            // Path-based, no-follow chmod preserves locks without weakening the
            // symlink boundary. SQLite can remove sidecars during this check.
            if let Err(error) = chmod_private_path(path) {
                if error.kind() == io::ErrorKind::NotFound {
                    return Ok(false);
                }
                return Err(error);
            }
        }
    }
    Ok(true)
}

#[cfg(unix)]
fn chmod_private_path(path: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "private file path contains NUL",
        )
    })?;
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::fs::PermissionsExt;
        // Older glibc rejects fchmodat(AT_SYMLINK_NOFOLLOW). O_PATH pins the
        // inode without participating in POSIX locks; closing it preserves
        // SQLite's locks. Validate the pinned inode before chmod via procfs.
        let fd = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        validate_private_file(&file.metadata()?)?;
        let pinned_path = format!("/proc/self/fd/{}", file.as_raw_fd());
        fs::set_permissions(pinned_path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "private file hardening requires procfs",
                )
            } else {
                error
            }
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        if unsafe {
            libc::fchmodat(
                libc::AT_FDCWD,
                name.as_ptr(),
                0o600,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Restrict an existing file without disturbing its POSIX locks. Missing files
/// are allowed; symlinks, hardlinks and files owned by another user are refused.
pub fn harden_existing_file(path: &Path) -> io::Result<()> {
    harden_regular_path(path).map(|_| ())
}

/// Create a private database before SQLite writes any content. Existing files
/// are hardened without opening a competing descriptor. Serialize the empty
/// file's creation/close before another Store in this process can open SQLite.
pub fn prepare_sqlite_file(path: &Path) -> io::Result<()> {
    static CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CREATION
        .lock()
        .map_err(|_| io::Error::other("SQLite file creation lock is poisoned"))?;
    loop {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        protect_options(&mut options);
        match options.open(path) {
            Ok(file) => return protect_handle(&file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if harden_regular_path(path)? {
                    return Ok(());
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// Create a new owner-only file, refusing every existing path or alias.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    protect_options(&mut options);
    let file = options.open(path)?;
    protect_handle(&file)?;
    Ok(file)
}

/// Open or create a private file without truncating existing data.
pub fn open_private_file(path: &Path, append: bool) -> io::Result<File> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .append(append)
        .truncate(false);
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
    use std::os::fd::AsRawFd;

    // An independent process must still be excluded after the permission check.
    // A second descriptor's close must not release the parent's POSIX lock.
    #[test]
    #[ignore = "subprocess helper for the POSIX lock tests"]
    fn lock_probe_child() {
        let Some(path) = std::env::var_os("GENTLY_TEST_LOCK_PATH") else {
            return;
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_len = 1;
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) };
        assert_eq!(
            result, -1,
            "permission checks released an existing POSIX lock"
        );
        let code = io::Error::last_os_error().raw_os_error().unwrap();
        assert!(code == libc::EACCES || code == libc::EAGAIN);
    }

    fn assert_preserves_lock(operation: impl FnOnce(&Path) -> io::Result<()>) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock-fixture");
        let file = open_private_file(&path, false).unwrap();
        // Exercise an actual permission change, not just an already-private
        // metadata check. fchmod on this retained descriptor preserves its lock.
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .unwrap();
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_len = 1;
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) },
            0
        );
        operation(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_fs::tests::lock_probe_child",
                "--nocapture",
                "--ignored",
            ])
            .env_clear()
            .env("GENTLY_TEST_LOCK_PATH", &path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
    }

    #[test]
    fn hardening_preserves_existing_posix_locks() {
        assert_preserves_lock(harden_existing_file);
    }

    #[test]
    fn preparing_existing_sqlite_file_preserves_posix_locks() {
        assert_preserves_lock(prepare_sqlite_file);
    }

    #[test]
    fn sqlite_file_preparation_is_private_and_refuses_aliases() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        prepare_sqlite_file(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = dir.path().join("hardlink");
        fs::hard_link(&path, &link).unwrap();
        assert!(prepare_sqlite_file(&link).is_err());
        assert!(harden_existing_file(&link).is_err());
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(prepare_sqlite_file(&link).is_err());
        assert!(harden_existing_file(&link).is_err());
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn hardlinked_private_write_is_refused_before_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("original");
        let alias = dir.path().join("alias");
        fs::write(&target, "original fixture content").unwrap();
        fs::hard_link(&target, &alias).unwrap();
        let result = write_private_file(&alias, "replacement fixture");
        assert!(result.is_err(), "private writes followed a hardlink");
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "original fixture content"
        );
    }
}
