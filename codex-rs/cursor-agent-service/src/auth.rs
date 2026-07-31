use serde::Deserialize;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;
use zeroize::Zeroize;
use zeroize::Zeroizing;

const MAX_CREDENTIAL_STORE_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct CursorCredentialStore {
    path: PathBuf,
}

impl CursorCredentialStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn from_environment() -> Result<Self, CursorCredentialStoreError> {
        let path = resolve_store_path(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )?;
        Ok(Self::new(path))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(target_os = "linux")]
    pub fn load(&self) -> Result<CursorCredentials, CursorCredentialStoreError> {
        use std::fs::OpenOptions;
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;

        if let Ok(metadata) = std::fs::symlink_metadata(&self.path)
            && metadata.file_type().is_symlink()
        {
            return Err(CursorCredentialStoreError::Symlink {
                path: self.path.clone(),
            });
        }

        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&self.path)
            .map_err(|source| {
                if source.raw_os_error() == Some(libc::ELOOP) {
                    CursorCredentialStoreError::Symlink {
                        path: self.path.clone(),
                    }
                } else {
                    CursorCredentialStoreError::Open {
                        path: self.path.clone(),
                        source,
                    }
                }
            })?;
        let metadata = file
            .metadata()
            .map_err(|source| CursorCredentialStoreError::Metadata {
                path: self.path.clone(),
                source,
            })?;
        let expected_uid = unsafe { libc::geteuid() };
        validate_metadata(&self.path, &metadata, expected_uid)?;

        let initial_capacity = usize::try_from(metadata.len())
            .unwrap_or(MAX_CREDENTIAL_STORE_BYTES)
            .min(MAX_CREDENTIAL_STORE_BYTES);
        let mut bytes = Zeroizing::new(Vec::with_capacity(initial_capacity));
        file.by_ref()
            .take((MAX_CREDENTIAL_STORE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| CursorCredentialStoreError::Read {
                path: self.path.clone(),
                source,
            })?;
        if bytes.len() > MAX_CREDENTIAL_STORE_BYTES {
            return Err(CursorCredentialStoreError::TooLarge {
                path: self.path.clone(),
                actual_bytes: bytes.len() as u64,
                max_bytes: MAX_CREDENTIAL_STORE_BYTES,
            });
        }

        let parsed = serde_json::from_slice::<CursorCredentials>(&bytes);
        bytes.zeroize();
        let credentials = parsed.map_err(|source| CursorCredentialStoreError::InvalidJson {
            path: self.path.clone(),
            source,
        })?;

        if credentials.access_token().trim().is_empty() {
            return Err(CursorCredentialStoreError::EmptyToken {
                field: "accessToken",
            });
        }
        if credentials.refresh_token().trim().is_empty() {
            return Err(CursorCredentialStoreError::EmptyToken {
                field: "refreshToken",
            });
        }

        Ok(credentials)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn load(&self) -> Result<CursorCredentials, CursorCredentialStoreError> {
        Err(CursorCredentialStoreError::UnsupportedPlatform)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CursorCredentials {
    access_token: Zeroizing<String>,
    refresh_token: Zeroizing<String>,
}

impl CursorCredentials {
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    pub(crate) fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
}

impl fmt::Debug for CursorCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("CursorCredentials { access_token: <redacted>, refresh_token: <redacted> }")
    }
}

#[derive(Debug, Error)]
pub enum CursorCredentialStoreError {
    #[error("Cursor credential reuse is supported only on Linux")]
    UnsupportedPlatform,
    #[error("neither XDG_CONFIG_HOME nor HOME resolves the Cursor credential store")]
    MissingConfigHome,
    #[error("Cursor credential store {path:?} is a symbolic link")]
    Symlink { path: PathBuf },
    #[error("failed to open Cursor credential store {path:?}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to inspect open Cursor credential store {path:?}: {source}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Cursor credential store {path:?} is not a regular file")]
    NotRegular { path: PathBuf },
    #[error(
        "Cursor credential store {path:?} is owned by uid {actual_uid}, expected uid {expected_uid}"
    )]
    WrongOwner {
        path: PathBuf,
        actual_uid: u32,
        expected_uid: u32,
    },
    #[error(
        "Cursor credential store {path:?} has insecure permissions {mode:#o}; group and world permission bits must be clear"
    )]
    InsecurePermissions { path: PathBuf, mode: u32 },
    #[error(
        "Cursor credential store {path:?} is {actual_bytes} bytes, exceeding the {max_bytes}-byte limit"
    )]
    TooLarge {
        path: PathBuf,
        actual_bytes: u64,
        max_bytes: usize,
    },
    #[error("failed to read Cursor credential store {path:?}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Cursor credential store {path:?} does not match the pinned schema: {source}")]
    InvalidJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("Cursor credential store field {field} must not be empty")]
    EmptyToken { field: &'static str },
}

fn resolve_store_path(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, CursorCredentialStoreError> {
    let config_home = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|value| PathBuf::from(value).join(".config"))
        })
        .ok_or(CursorCredentialStoreError::MissingConfigHome)?;
    Ok(config_home.join("cursor/auth.json"))
}

#[cfg(target_os = "linux")]
fn validate_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
    expected_uid: u32,
) -> Result<(), CursorCredentialStoreError> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    if !metadata.file_type().is_file() {
        return Err(CursorCredentialStoreError::NotRegular {
            path: path.to_path_buf(),
        });
    }

    let actual_uid = metadata.uid();
    if actual_uid != expected_uid {
        return Err(CursorCredentialStoreError::WrongOwner {
            path: path.to_path_buf(),
            actual_uid,
            expected_uid,
        });
    }

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(CursorCredentialStoreError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }

    if metadata.len() > MAX_CREDENTIAL_STORE_BYTES as u64 {
        return Err(CursorCredentialStoreError::TooLarge {
            path: path.to_path_buf(),
            actual_bytes: metadata.len(),
            max_bytes: MAX_CREDENTIAL_STORE_BYTES,
        });
    }

    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
#[path = "auth_tests.rs"]
mod tests;
