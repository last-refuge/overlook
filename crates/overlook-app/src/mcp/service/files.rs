use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::mcp::write::WritePermission;

const STATE: &str = ".mcp";
const TOKEN_BYTES: usize = 32;
const KEY_BYTES: usize = 32;
const MAX_GRANT_BYTES: usize = 64 * 1024;

pub(super) struct Profile {
    path: PathBuf,
    state: PathBuf,
}

pub(super) struct Endpoint {
    path: PathBuf,
    socket: Metadata,
    // Keep the lock until Drop has unlinked the socket. Never unlink the lock
    // file itself: doing so can give different processes different lock inodes.
    _lock: File,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Ok(current) = fs::symlink_metadata(&self.path) {
            if current.file_type().is_socket()
                && current.dev() == self.socket.dev()
                && current.ino() == self.socket.ino()
            {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct Grant {
    token_digest: [u8; 32],
    pub account_ids: BTreeSet<String>,
    #[serde(default)]
    pub permissions: BTreeSet<WritePermission>,
}

impl Profile {
    pub(super) fn open(profile: &Path) -> anyhow::Result<Self> {
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(profile)?;
        let path = canonical_profile(profile)?;
        let state = path.join(STATE);
        create_private_dir(&state)?;
        Ok(Self { path, state })
    }

    pub(super) fn listen(&self) -> anyhow::Result<(Endpoint, UnixListener)> {
        let lock = private_file(&self.state.join("owner.lock"), true)?;
        // SAFETY: lock owns this valid file descriptor across Endpoint's lifetime.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error())
                .context("Could not acquire the profile service lock");
        }

        let dir = runtime_dir();
        create_private_dir(&dir)?;
        let path = socket_path(&self.path, &dir);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.file_type().is_socket() && metadata.uid() == uid(),
                    "Unexpected file at the profile service socket path"
                );
                // The service lock is held, so this socket is left by a dead owner.
                fs::remove_file(&path)?;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        let listener =
            UnixListener::bind(&path).context("Could not bind the profile service socket")?;
        let socket = fs::symlink_metadata(&path)?;
        let endpoint = Endpoint {
            path,
            socket,
            _lock: lock,
        };
        fs::set_permissions(&endpoint.path, Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok((endpoint, listener))
    }

    pub(super) fn account_key(&self) -> anyhow::Result<[u8; KEY_BYTES]> {
        // Called only by the owner while holding the profile's service lock.
        let path = self.state.join("account-key");
        match private_file(&path, false) {
            Ok(mut file) => {
                let mut key = [0; KEY_BYTES];
                ensure!(
                    file.metadata()?.len() == KEY_BYTES as u64,
                    "Invalid profile account key"
                );
                file.read_exact(&mut key)?;
                Ok(key)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let key = random_bytes()?;
                self.write_private(&path, &key)?;
                Ok(key)
            }
            Err(err) => Err(err.into()),
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn sample_database(&self) -> anyhow::Result<PathBuf> {
        let path = self.state.join("sample.sqlite");
        private_file(&path, true)?;
        Ok(path)
    }

    pub(super) fn grant(
        &self,
        client: &str,
        account_ids: &[String],
        permissions: BTreeSet<WritePermission>,
    ) -> anyhow::Result<String> {
        let token = URL_SAFE_NO_PAD.encode(random_bytes::<TOKEN_BYTES>()?);
        let grant = Grant {
            token_digest: self.token_digest(client, &token),
            account_ids: account_ids.iter().cloned().collect(),
            permissions,
        };
        self.write_private(&self.grant_path(client), &serde_json::to_vec(&grant)?)?;
        Ok(token)
    }

    pub(super) fn revoke(&self, client: &str) -> anyhow::Result<()> {
        match fs::remove_file(self.grant_path(client)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub(super) fn authorize(&self, client: &str, token: &str) -> Option<Grant> {
        if !valid_client(client) || !valid_token(token) {
            return None;
        }
        let file = private_file(&self.grant_path(client), false).ok()?;
        let mut bytes = Vec::new();
        file.take((MAX_GRANT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_GRANT_BYTES {
            return None;
        }
        let grant: Grant = serde_json::from_slice(&bytes).ok()?;
        let presented = self.token_digest(client, token);
        let difference = presented
            .iter()
            .zip(grant.token_digest)
            .fold(0, |diff, (left, right)| diff | (left ^ right));
        (std::hint::black_box(difference) == 0).then_some(grant)
    }

    fn token_digest(&self, client: &str, token: &str) -> [u8; 32] {
        Sha256::new()
            .chain_update(b"overlook-mcp-grant\0")
            .chain_update(self.path.as_os_str().as_bytes())
            .chain_update([0])
            .chain_update(client.as_bytes())
            .chain_update([0])
            .chain_update(token.as_bytes())
            .finalize()
            .into()
    }

    fn grant_path(&self, client: &str) -> PathBuf {
        self.state
            .join(format!("grant-{:x}", Sha256::digest(client.as_bytes())))
    }

    // Each client has its own file. Atomic rename and unlink linearize updates
    // for a single client, and updates to different clients cannot clobber one another.
    fn write_private(&self, path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.state)?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}

pub(super) fn existing_socket(profile: &Path) -> anyhow::Result<PathBuf> {
    let profile = canonical_profile(profile)?;
    let dir = runtime_dir();
    check_private_dir(&dir)?;
    let socket = socket_path(&profile, &dir);
    let metadata = fs::symlink_metadata(&socket)?;
    ensure!(
        metadata.file_type().is_socket() && metadata.uid() == uid() && metadata.mode() & 0o077 == 0,
        "Insecure profile service socket"
    );
    Ok(socket)
}

pub(super) fn public_id(key: &[u8; KEY_BYTES], account_id: &str) -> String {
    // The only input IDs are the fixed sample IDs. The secret profile key keeps
    // public IDs unlinkable across independently initialized profiles.
    let digest = Sha256::new()
        .chain_update(b"overlook-mcp-account\0")
        .chain_update(account_id.as_bytes())
        .chain_update([0])
        .chain_update(key)
        .finalize();
    URL_SAFE_NO_PAD.encode(digest)
}

pub(super) fn valid_client(client: &str) -> bool {
    !client.trim().is_empty() && client.len() <= 128 && !client.chars().any(char::is_control)
}

pub(super) fn valid_token(token: &str) -> bool {
    let mut decoded = [0; TOKEN_BYTES];
    URL_SAFE_NO_PAD.decode_slice(token.as_bytes(), &mut decoded) == Ok(TOKEN_BYTES)
}

fn canonical_profile(profile: &Path) -> anyhow::Result<PathBuf> {
    let path = profile
        .canonicalize()
        .context("Could not resolve the profile path")?;
    let metadata = path.metadata()?;
    ensure!(
        metadata.is_dir() && metadata.uid() == uid() && metadata.mode() & 0o022 == 0,
        "The profile directory must be owned by the local user with no group or other write access"
    );
    Ok(path)
}

fn runtime_dir() -> PathBuf {
    // A fixed, short path fits macOS's sockaddr_un even for long profile names
    // and lets independently launched app and CLI processes find one another.
    PathBuf::from(format!("/tmp/overlook-mcp-{}", uid()))
}

fn socket_path(profile: &Path, dir: &Path) -> PathBuf {
    dir.join(format!(
        "{:x}.sock",
        Sha256::digest(profile.as_os_str().as_bytes())
    ))
}

fn create_private_dir(path: &Path) -> anyhow::Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }
    check_private_dir(path)
}

fn check_private_dir(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && metadata.uid() == uid() && metadata.mode() & 0o077 == 0,
        "The service directory must be private and owned by the local user"
    );
    Ok(())
}

fn private_file(path: &Path, create: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata: Metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Insecure profile service file",
        ));
    }
    Ok(file)
}

fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn uid() -> u32 {
    // SAFETY: geteuid has no arguments or preconditions.
    unsafe { libc::geteuid() }
}
