//! The native HA app owns this one private pairing credential, not HA options.
use crate::config::ConfigError;
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};
use zeroize::Zeroizing;

const FILE_NAME: &str = "integration-token";
const READ_LIMIT: usize = 4096;

/// Publish a complete credential once. A competing publisher must reuse the
/// winner; malformed or insecure existing state is never replaced or repaired.
pub(crate) fn load_or_create(directory: &Path) -> Result<Zeroizing<String>, ConfigError> {
    match DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(ConfigError::ManagedCredential),
    }
    let metadata = fs::symlink_metadata(directory).map_err(|_| ConfigError::ManagedCredential)?;
    // SAFETY: geteuid has no arguments and cannot fail.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o7777 != 0o700 {
        return Err(ConfigError::ManagedCredential);
    }
    let path = directory.join(FILE_NAME);
    if let Some(token) = read_existing(&path, owner)? {
        return Ok(token);
    }

    let token = random_token()?;
    // A separate nonce is not the credential and may safely identify a temp file.
    let temporary = directory.join(format!(".integration-{}.tmp", random_token()?.as_str()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)
        .map_err(|_| ConfigError::ManagedCredential)?;
    let published = (|| {
        file.write_all(token.as_bytes())
            .map_err(|_| ConfigError::ManagedCredential)?;
        file.sync_all()
            .map_err(|_| ConfigError::ManagedCredential)?;
        // A hard link atomically publishes complete bytes without replacing a
        // credential concurrently created by another starting process.
        match fs::hard_link(&temporary, &path) {
            Ok(()) => File::open(directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| ConfigError::ManagedCredential),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(_) => Err(ConfigError::ManagedCredential),
        }
    })();
    drop(file);
    let cleanup = fs::remove_file(&temporary).map_err(|_| ConfigError::ManagedCredential);
    published?;
    cleanup?;
    read_existing(&path, owner)?.ok_or(ConfigError::ManagedCredential)
}

fn read_existing(path: &Path, owner: u32) -> Result<Option<Zeroizing<String>>, ConfigError> {
    let file = match OpenOptions::new()
        .read(true)
        // Reject symlinks, and inspect special files without blocking on a FIFO.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConfigError::ManagedCredential),
    };
    let metadata = file
        .metadata()
        .map_err(|_| ConfigError::ManagedCredential)?;
    if !metadata.is_file() || metadata.uid() != owner || metadata.mode() & 0o7777 != 0o600 {
        return Err(ConfigError::ManagedCredential);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(READ_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigError::ManagedCredential)?;
    if bytes.len() > READ_LIMIT {
        return Err(ConfigError::ManagedCredential);
    }
    let token = std::str::from_utf8(&bytes).map_err(|_| ConfigError::ManagedCredential)?;
    if token.len() != 64
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ConfigError::ManagedCredential);
    }
    Ok(Some(Zeroizing::new(token.to_owned())))
}

fn random_token() -> Result<Zeroizing<String>, ConfigError> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut *bytes))
        .map_err(|_| ConfigError::ManagedCredential)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = Zeroizing::new(String::with_capacity(64));
    for byte in bytes.iter() {
        token.push(char::from(HEX[usize::from(byte >> 4)]));
        token.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use crate::media::credentials::{FILE_NAME, load_or_create};
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink},
        sync::{Arc, Barrier},
        thread,
    };

    #[test]
    fn generated_credentials_are_private_random_and_stable_on_reopen() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("media");
        let first = load_or_create(&directory).unwrap();
        let reopened = load_or_create(&directory).unwrap();
        let independent = load_or_create(&root.path().join("other")).unwrap();
        assert_eq!(first.len(), 64);
        assert!(first == reopened, "reopen changed the credential");
        assert!(first != independent, "independent app credentials collided");
        assert_eq!(fs::metadata(&directory).unwrap().mode() & 0o7777, 0o700);
        let file = fs::metadata(directory.join(FILE_NAME)).unwrap();
        assert!(file.is_file());
        assert_eq!(file.mode() & 0o7777, 0o600);
        assert_eq!(file.uid(), fs::metadata(root.path()).unwrap().uid());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }

    #[test]
    fn simultaneous_starts_observe_the_same_complete_credential() {
        let root = tempfile::tempdir().unwrap();
        let directory = Arc::new(root.path().join("media"));
        let barrier = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let directory = Arc::clone(&directory);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    load_or_create(&directory).unwrap()
                })
            })
            .collect();
        let tokens: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert!(
            tokens.iter().all(|token| token == &tokens[0]),
            "concurrent credential mismatch"
        );
        assert_eq!(fs::read_dir(&*directory).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_oversized_or_insecure_files_fail_closed_without_reset() {
        for content in [vec![], vec![b'z'; 64], vec![0xff; 64], vec![b'a'; 4097]] {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join("media");
            let _ = load_or_create(&directory).unwrap();
            let path = directory.join(FILE_NAME);
            fs::write(&path, &content).unwrap();
            assert!(load_or_create(&directory).is_err());
            assert!(
                fs::read(&path).unwrap() == content,
                "corrupt credentials were overwritten"
            );
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        }
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("media");
        let _ = load_or_create(&directory).unwrap();
        let path = directory.join(FILE_NAME);
        let before = fs::read(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_or_create(&directory).is_err());
        assert!(fs::read(&path).unwrap() == before);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o644);
    }

    #[test]
    fn symlinks_special_files_and_insecure_directories_are_not_followed_or_repaired() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::DirBuilder::new().mode(0o700).create(&target).unwrap();
        let link = root.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(load_or_create(&link).is_err());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);

        let _ = load_or_create(&target).unwrap();
        let path = target.join(FILE_NAME);
        let saved = root.path().join("saved");
        fs::rename(&path, &saved).unwrap();
        symlink(&saved, &path).unwrap();
        assert!(load_or_create(&target).is_err());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(load_or_create(&target).is_err());

        let insecure = root.path().join("insecure");
        fs::create_dir(&insecure).unwrap();
        fs::set_permissions(&insecure, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load_or_create(&insecure).is_err());
        assert_eq!(fs::metadata(&insecure).unwrap().mode() & 0o7777, 0o755);
        assert_eq!(fs::read_dir(&insecure).unwrap().count(), 0);
    }

    #[test]
    fn errors_are_fixed_and_do_not_disclose_paths_or_private_state() {
        let root = tempfile::tempdir().unwrap();
        let error = load_or_create(&root.path().join("private-value").join("media"))
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "Cannot load private app pairing credentials. Existing credentials must remain private and valid."
        );
        assert!(!format!("{error:?}").contains("private-value"));
    }

    #[test]
    fn a_fifo_at_the_credential_path_is_rejected_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt as _;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("media");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let path = directory.join(FILE_NAME);
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live, NUL-terminated path; mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(load_or_create(&directory).is_err());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }
}
