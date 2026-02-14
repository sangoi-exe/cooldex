use chrono::DateTime;
use chrono::Utc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tempfile::NamedTempFile;
use tracing::warn;

use crate::protocol::RateLimitSnapshot;
use crate::token_data::TokenData;
use codex_app_server_protocol::AuthMode;
use codex_keyring_store::DefaultKeyringStore;
use codex_keyring_store::KeyringStore;
use once_cell::sync::Lazy;
use uuid::Uuid;

/// Determine where Codex should store CLI auth credentials.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuthCredentialsStoreMode {
    #[default]
    /// Persist credentials in CODEX_HOME/auth.json.
    File,
    /// Persist credentials in the keyring. Fail if unavailable.
    Keyring,
    /// Use keyring when available; otherwise, fall back to a file in CODEX_HOME.
    Auto,
    /// Store credentials in memory only for the current process.
    Ephemeral,
}

/// Legacy structure for `$CODEX_HOME/auth.json`.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct AuthDotJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<AuthMode>,

    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenData>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<DateTime<Utc>>,
}

pub const AUTH_STORE_VERSION: u32 = 1;
const AUTH_STORE_LOCKFILE_NAME: &str = "auth.json.lock";
const AUTH_STORE_LOCK_MAX_RETRIES: usize = 20;
const AUTH_STORE_LOCK_RETRY_SLEEP: Duration = Duration::from_millis(100);

/// Versioned auth store for `$CODEX_HOME/auth.json`.
///
/// The legacy `AuthDotJson` format is still accepted on load and is migrated
/// into this structure in-memory.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct AuthStore {
    pub version: u32,

    #[serde(
        rename = "OPENAI_API_KEY",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub openai_api_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_account_id: Option<String>,

    #[serde(default)]
    pub accounts: Vec<StoredAccount>,
}

impl Default for AuthStore {
    fn default() -> Self {
        Self {
            version: AUTH_STORE_VERSION,
            openai_api_key: None,
            active_account_id: None,
            accounts: Vec::new(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct StoredAccount {
    pub id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    pub tokens: TokenData,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AccountUsageCache>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
pub struct AccountUsageCache {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rate_limits: Option<RateLimitSnapshot>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhausted_until: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
}

impl AuthStore {
    pub fn validate(&self) -> std::io::Result<()> {
        if self.version != AUTH_STORE_VERSION {
            return Err(std::io::Error::other(format!(
                "unsupported auth store version {} (expected {AUTH_STORE_VERSION})",
                self.version
            )));
        }

        let mut ids = HashSet::with_capacity(self.accounts.len());
        for account in &self.accounts {
            if !ids.insert(account.id.as_str()) {
                return Err(std::io::Error::other(format!(
                    "duplicate auth account id '{}'",
                    account.id
                )));
            }
        }

        match self.active_account_id.as_deref() {
            Some(active) => {
                if !ids.contains(active) {
                    return Err(std::io::Error::other(format!(
                        "active_account_id '{active}' does not exist in stored accounts",
                    )));
                }
            }
            None => {
                if !self.accounts.is_empty() {
                    return Err(std::io::Error::other(
                        "active_account_id must be set when accounts are present",
                    ));
                }
            }
        }

        if self.accounts.is_empty() && self.active_account_id.is_some() {
            return Err(std::io::Error::other(
                "active_account_id must be unset when accounts are empty",
            ));
        }

        Ok(())
    }

    pub fn from_legacy(legacy: AuthDotJson) -> Self {
        let mut store = AuthStore {
            version: AUTH_STORE_VERSION,
            openai_api_key: legacy.openai_api_key,
            active_account_id: None,
            accounts: Vec::new(),
        };

        if let Some(tokens) = legacy.tokens {
            let id = tokens
                .account_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            store.accounts.push(StoredAccount {
                id: id.clone(),
                label: None,
                tokens,
                last_refresh: legacy.last_refresh,
                usage: None,
            });
            store.active_account_id = Some(id);
        }

        store
    }
}

pub(super) fn get_auth_file(codex_home: &Path) -> PathBuf {
    codex_home.join("auth.json")
}

#[derive(Debug)]
pub(super) struct AuthStoreLock {
    _file: File,
}

pub(super) fn lock_auth_store(codex_home: &Path) -> std::io::Result<AuthStoreLock> {
    std::fs::create_dir_all(codex_home)?;
    let canonical = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf());
    let lock_file_path = canonical.join(AUTH_STORE_LOCKFILE_NAME);

    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }

    let lock_file = options.open(&lock_file_path)?;

    for _ in 0..AUTH_STORE_LOCK_MAX_RETRIES {
        match lock_file.try_lock() {
            Ok(()) => return Ok(AuthStoreLock { _file: lock_file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                thread::sleep(AUTH_STORE_LOCK_RETRY_SLEEP);
            }
            Err(err) => return Err(err.into()),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "could not acquire exclusive lock on auth store after multiple attempts (lock file: {})",
            lock_file_path.display()
        ),
    ))
}

pub(super) fn delete_file_if_exists(codex_home: &Path) -> std::io::Result<bool> {
    let auth_file = get_auth_file(codex_home);
    match std::fs::remove_file(&auth_file) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

pub(super) trait AuthStorageBackend: Debug + Send + Sync {
    fn load(&self) -> std::io::Result<Option<AuthStore>>;
    fn save(&self, auth: &AuthStore) -> std::io::Result<()>;
    fn delete(&self) -> std::io::Result<bool>;
}

#[derive(Clone, Debug)]
pub(super) struct FileAuthStorage {
    codex_home: PathBuf,
}

impl FileAuthStorage {
    pub(super) fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    /// Attempt to read and parse the `auth.json` file in the given `CODEX_HOME` directory.
    pub(super) fn try_read_auth_store(&self, auth_file: &Path) -> std::io::Result<AuthStore> {
        let mut file = File::open(auth_file)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        parse_auth_store(&contents)
    }
}

impl AuthStorageBackend for FileAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthStore>> {
        let auth_file = get_auth_file(&self.codex_home);
        let store = match self.try_read_auth_store(&auth_file) {
            Ok(auth) => auth,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(Some(store))
    }

    fn save(&self, store: &AuthStore) -> std::io::Result<()> {
        let auth_file = get_auth_file(&self.codex_home);

        let parent = auth_file.parent().ok_or(std::io::Error::other(format!(
            "auth file path '{}' has no parent directory",
            auth_file.display()
        )))?;
        std::fs::create_dir_all(parent)?;
        let json_data = serde_json::to_string_pretty(store)?;

        let mut tmp = NamedTempFile::new_in(parent)?;
        #[cfg(unix)]
        {
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
        }

        tmp.as_file_mut().write_all(json_data.as_bytes())?;
        tmp.as_file_mut().flush()?;
        tmp.as_file().sync_all()?;
        tmp.persist(&auth_file)?;
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        delete_file_if_exists(&self.codex_home)
    }
}

const KEYRING_SERVICE: &str = "Codex Auth";

// turns codex_home path into a stable, short key string
fn compute_store_key(codex_home: &Path) -> std::io::Result<String> {
    let canonical = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf());
    let path_str = canonical.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let truncated = hex.get(..16).unwrap_or(&hex);
    Ok(format!("cli|{truncated}"))
}

#[derive(Clone, Debug)]
struct KeyringAuthStorage {
    codex_home: PathBuf,
    keyring_store: Arc<dyn KeyringStore>,
}

impl KeyringAuthStorage {
    fn new(codex_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        Self {
            codex_home,
            keyring_store,
        }
    }

    fn load_from_keyring(&self, key: &str) -> std::io::Result<Option<AuthStore>> {
        match self.keyring_store.load(KEYRING_SERVICE, key) {
            Ok(Some(serialized)) => parse_auth_store(&serialized).map(Some),
            Ok(None) => Ok(None),
            Err(error) => Err(std::io::Error::other(format!(
                "failed to load CLI auth from keyring: {}",
                error.message()
            ))),
        }
    }

    fn save_to_keyring(&self, key: &str, value: &str) -> std::io::Result<()> {
        match self.keyring_store.save(KEYRING_SERVICE, key, value) {
            Ok(()) => Ok(()),
            Err(error) => {
                let message = format!(
                    "failed to write OAuth tokens to keyring: {}",
                    error.message()
                );
                warn!("{message}");
                Err(std::io::Error::other(message))
            }
        }
    }
}

impl AuthStorageBackend for KeyringAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthStore>> {
        let key = compute_store_key(&self.codex_home)?;
        self.load_from_keyring(&key)
    }

    fn save(&self, store: &AuthStore) -> std::io::Result<()> {
        let key = compute_store_key(&self.codex_home)?;
        // Simpler error mapping per style: prefer method reference over closure
        let serialized = serde_json::to_string(store).map_err(std::io::Error::other)?;
        self.save_to_keyring(&key, &serialized)?;
        if let Err(err) = delete_file_if_exists(&self.codex_home) {
            warn!("failed to remove CLI auth fallback file: {err}");
        }
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        let key = compute_store_key(&self.codex_home)?;
        let keyring_removed = self
            .keyring_store
            .delete(KEYRING_SERVICE, &key)
            .map_err(|err| {
                std::io::Error::other(format!("failed to delete auth from keyring: {err}"))
            })?;
        let file_removed = delete_file_if_exists(&self.codex_home)?;
        Ok(keyring_removed || file_removed)
    }
}

#[derive(Clone, Debug)]
struct AutoAuthStorage {
    keyring_storage: Arc<KeyringAuthStorage>,
    file_storage: Arc<FileAuthStorage>,
}

impl AutoAuthStorage {
    fn new(codex_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        Self {
            keyring_storage: Arc::new(KeyringAuthStorage::new(codex_home.clone(), keyring_store)),
            file_storage: Arc::new(FileAuthStorage::new(codex_home)),
        }
    }
}

impl AuthStorageBackend for AutoAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthStore>> {
        match self.keyring_storage.load() {
            Ok(Some(auth)) => Ok(Some(auth)),
            Ok(None) => self.file_storage.load(),
            Err(err) => {
                warn!("failed to load CLI auth from keyring, falling back to file storage: {err}");
                self.file_storage.load()
            }
        }
    }

    fn save(&self, auth: &AuthStore) -> std::io::Result<()> {
        match self.keyring_storage.save(auth) {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!("failed to save auth to keyring, falling back to file storage: {err}");
                self.file_storage.save(auth)
            }
        }
    }

    fn delete(&self) -> std::io::Result<bool> {
        // Keyring storage will delete from disk as well
        self.keyring_storage.delete()
    }
}

// A global in-memory store for mapping codex_home -> AuthStore.
static EPHEMERAL_AUTH_STORE: Lazy<Mutex<HashMap<String, AuthStore>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
struct EphemeralAuthStorage {
    codex_home: PathBuf,
}

impl EphemeralAuthStorage {
    fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    fn with_store<F, T>(&self, action: F) -> std::io::Result<T>
    where
        F: FnOnce(&mut HashMap<String, AuthStore>, String) -> std::io::Result<T>,
    {
        let key = compute_store_key(&self.codex_home)?;
        let mut store = EPHEMERAL_AUTH_STORE
            .lock()
            .map_err(|_| std::io::Error::other("failed to lock ephemeral auth storage"))?;
        action(&mut store, key)
    }
}

impl AuthStorageBackend for EphemeralAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthStore>> {
        self.with_store(|store, key| Ok(store.get(&key).cloned()))
    }

    fn save(&self, auth: &AuthStore) -> std::io::Result<()> {
        self.with_store(|store, key| {
            store.insert(key, auth.clone());
            Ok(())
        })
    }

    fn delete(&self) -> std::io::Result<bool> {
        self.with_store(|store, key| Ok(store.remove(&key).is_some()))
    }
}

pub(super) fn create_auth_storage(
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
) -> Arc<dyn AuthStorageBackend> {
    let keyring_store: Arc<dyn KeyringStore> = Arc::new(DefaultKeyringStore);
    create_auth_storage_with_keyring_store(codex_home, mode, keyring_store)
}

fn parse_auth_store(contents: &str) -> std::io::Result<AuthStore> {
    match serde_json::from_str::<AuthStore>(contents) {
        Ok(store) => {
            store.validate()?;
            Ok(store)
        }
        Err(_) => {
            let legacy: AuthDotJson = serde_json::from_str(contents).map_err(|err| {
                std::io::Error::other(format!("failed to parse auth.json: {err}"))
            })?;
            Ok(AuthStore::from_legacy(legacy))
        }
    }
}

fn create_auth_storage_with_keyring_store(
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> Arc<dyn AuthStorageBackend> {
    match mode {
        AuthCredentialsStoreMode::File => Arc::new(FileAuthStorage::new(codex_home)),
        AuthCredentialsStoreMode::Keyring => {
            Arc::new(KeyringAuthStorage::new(codex_home, keyring_store))
        }
        AuthCredentialsStoreMode::Auto => Arc::new(AutoAuthStorage::new(codex_home, keyring_store)),
        AuthCredentialsStoreMode::Ephemeral => Arc::new(EphemeralAuthStorage::new(codex_home)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use codex_keyring_store::tests::MockKeyringStore;
    use keyring::Error as KeyringError;

    #[tokio::test]
    async fn file_storage_load_returns_auth_store() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
        let store = AuthStore {
            openai_api_key: Some("test-key".to_string()),
            ..AuthStore::default()
        };

        storage.save(&store).context("failed to save auth file")?;

        let loaded = storage.load().context("failed to load auth file")?;
        assert_eq!(Some(store), loaded);
        Ok(())
    }

    #[tokio::test]
    async fn file_storage_save_persists_auth_store() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
        let store = AuthStore {
            openai_api_key: Some("test-key".to_string()),
            ..AuthStore::default()
        };

        let file = get_auth_file(codex_home.path());
        storage.save(&store).context("failed to save auth file")?;

        let same_store = storage
            .try_read_auth_store(&file)
            .context("failed to read auth file after save")?;
        assert_eq!(store, same_store);
        Ok(())
    }

    #[test]
    fn file_storage_load_migrates_legacy_auth_dot_json() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
        let id_token_raw = "e30.eyJzdWIiOiJ1c2VyIn0.sig";
        let id_token = crate::token_data::parse_chatgpt_jwt_claims(id_token_raw)
            .expect("minimal JWT should parse");
        let legacy = AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token,
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some("acct".to_string()),
            }),
            last_refresh: Some(Utc::now()),
        };

        let file = get_auth_file(codex_home.path());
        std::fs::write(&file, serde_json::to_string_pretty(&legacy)?)?;

        let loaded = storage.load()?.context("store should load")?;
        assert_eq!(loaded.active_account_id.as_deref(), Some("acct"));
        assert_eq!(loaded.accounts.len(), 1);
        assert_eq!(loaded.accounts[0].tokens.access_token, "access");
        Ok(())
    }

    #[test]
    fn file_storage_delete_removes_auth_file() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let store = AuthStore {
            openai_api_key: Some("sk-test-key".to_string()),
            ..AuthStore::default()
        };
        let storage = create_auth_storage(dir.path().to_path_buf(), AuthCredentialsStoreMode::File);
        storage.save(&store)?;
        assert!(dir.path().join("auth.json").exists());
        let storage = FileAuthStorage::new(dir.path().to_path_buf());
        let removed = storage.delete()?;
        assert!(removed);
        assert!(!dir.path().join("auth.json").exists());
        Ok(())
    }

    #[test]
    fn ephemeral_storage_save_load_delete_is_in_memory_only() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let storage = create_auth_storage(
            dir.path().to_path_buf(),
            AuthCredentialsStoreMode::Ephemeral,
        );
        let store = AuthStore {
            openai_api_key: Some("sk-ephemeral".to_string()),
            ..AuthStore::default()
        };

        storage.save(&store)?;
        let loaded = storage.load()?;
        assert_eq!(Some(store), loaded);

        let removed = storage.delete()?;
        assert!(removed);
        let loaded = storage.load()?;
        assert_eq!(None, loaded);
        assert!(!get_auth_file(dir.path()).exists());
        Ok(())
    }

    fn seed_keyring_and_fallback_auth_file_for_delete<F>(
        mock_keyring: &MockKeyringStore,
        codex_home: &Path,
        compute_key: F,
    ) -> anyhow::Result<(String, PathBuf)>
    where
        F: FnOnce() -> std::io::Result<String>,
    {
        let key = compute_key()?;
        mock_keyring.save(KEYRING_SERVICE, &key, "{}")?;
        let auth_file = get_auth_file(codex_home);
        std::fs::write(&auth_file, "stale")?;
        Ok((key, auth_file))
    }

    fn seed_keyring_with_auth<F>(
        mock_keyring: &MockKeyringStore,
        compute_key: F,
        store: &AuthStore,
    ) -> anyhow::Result<()>
    where
        F: FnOnce() -> std::io::Result<String>,
    {
        let key = compute_key()?;
        let serialized = serde_json::to_string(store)?;
        mock_keyring.save(KEYRING_SERVICE, &key, &serialized)?;
        Ok(())
    }

    fn assert_keyring_saved_auth_and_removed_fallback(
        mock_keyring: &MockKeyringStore,
        key: &str,
        codex_home: &Path,
        expected: &AuthStore,
    ) {
        let saved_value = mock_keyring
            .saved_value(key)
            .expect("keyring entry should exist");
        let expected_serialized = serde_json::to_string(expected).expect("serialize expected auth");
        assert_eq!(saved_value, expected_serialized);
        let auth_file = get_auth_file(codex_home);
        assert!(
            !auth_file.exists(),
            "fallback auth.json should be removed after keyring save"
        );
    }

    fn store_with_prefix(prefix: &str) -> AuthStore {
        AuthStore {
            openai_api_key: Some(format!("{prefix}-api-key")),
            ..AuthStore::default()
        }
    }

    #[test]
    fn keyring_auth_storage_load_returns_deserialized_auth() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = KeyringAuthStorage::new(
            codex_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let expected = AuthStore {
            openai_api_key: Some("sk-test".to_string()),
            ..AuthStore::default()
        };
        seed_keyring_with_auth(
            &mock_keyring,
            || compute_store_key(codex_home.path()),
            &expected,
        )?;

        let loaded = storage.load()?;
        assert_eq!(Some(expected), loaded);
        Ok(())
    }

    #[test]
    fn keyring_auth_storage_compute_store_key_for_home_directory() -> anyhow::Result<()> {
        let codex_home = PathBuf::from("~/.codex");

        let key = compute_store_key(codex_home.as_path())?;

        assert_eq!(key, "cli|940db7b1d0e4eb40");
        Ok(())
    }

    #[test]
    fn keyring_auth_storage_save_persists_and_removes_fallback_file() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = KeyringAuthStorage::new(
            codex_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let auth_file = get_auth_file(codex_home.path());
        std::fs::write(&auth_file, "stale")?;
        let auth = AuthStore {
            openai_api_key: None,
            active_account_id: Some("account".to_string()),
            accounts: vec![StoredAccount {
                id: "account".to_string(),
                label: None,
                tokens: TokenData {
                    id_token: Default::default(),
                    access_token: "access".to_string(),
                    refresh_token: "refresh".to_string(),
                    account_id: Some("account".to_string()),
                },
                last_refresh: Some(Utc::now()),
                usage: None,
            }],
            ..AuthStore::default()
        };

        storage.save(&auth)?;

        let key = compute_store_key(codex_home.path())?;
        assert_keyring_saved_auth_and_removed_fallback(
            &mock_keyring,
            &key,
            codex_home.path(),
            &auth,
        );
        Ok(())
    }

    #[test]
    fn keyring_auth_storage_delete_removes_keyring_and_file() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = KeyringAuthStorage::new(
            codex_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let (key, auth_file) = seed_keyring_and_fallback_auth_file_for_delete(
            &mock_keyring,
            codex_home.path(),
            || compute_store_key(codex_home.path()),
        )?;

        let removed = storage.delete()?;

        assert!(removed, "delete should report removal");
        assert!(
            !mock_keyring.contains(&key),
            "keyring entry should be removed"
        );
        assert!(
            !auth_file.exists(),
            "fallback auth.json should be removed after keyring delete"
        );
        Ok(())
    }

    #[test]
    fn auto_auth_storage_load_prefers_keyring_value() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            codex_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let keyring_auth = store_with_prefix("keyring");
        seed_keyring_with_auth(
            &mock_keyring,
            || compute_store_key(codex_home.path()),
            &keyring_auth,
        )?;

        let file_auth = store_with_prefix("file");
        storage.file_storage.save(&file_auth)?;

        let loaded = storage.load()?;
        assert_eq!(loaded, Some(keyring_auth));
        Ok(())
    }

    #[test]
    fn auto_auth_storage_load_uses_file_when_keyring_empty() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(codex_home.path().to_path_buf(), Arc::new(mock_keyring));

        let expected = store_with_prefix("file-only");
        storage.file_storage.save(&expected)?;

        let loaded = storage.load()?;
        assert_eq!(loaded, Some(expected));
        Ok(())
    }

    #[test]
    fn auto_auth_storage_load_falls_back_when_keyring_errors() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            codex_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let key = compute_store_key(codex_home.path())?;
        mock_keyring.set_error(&key, KeyringError::Invalid("error".into(), "load".into()));

        let expected = store_with_prefix("fallback");
        storage.file_storage.save(&expected)?;

        let loaded = storage.load()?;
        assert_eq!(loaded, Some(expected));
        Ok(())
    }

    #[test]
    fn auto_auth_storage_save_prefers_keyring() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            codex_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let key = compute_store_key(codex_home.path())?;

        let stale = store_with_prefix("stale");
        storage.file_storage.save(&stale)?;

        let expected = store_with_prefix("to-save");
        storage.save(&expected)?;

        assert_keyring_saved_auth_and_removed_fallback(
            &mock_keyring,
            &key,
            codex_home.path(),
            &expected,
        );
        Ok(())
    }

    #[test]
    fn auto_auth_storage_save_falls_back_when_keyring_errors() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            codex_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let key = compute_store_key(codex_home.path())?;
        mock_keyring.set_error(&key, KeyringError::Invalid("error".into(), "save".into()));

        let auth = store_with_prefix("fallback");
        storage.save(&auth)?;

        let auth_file = get_auth_file(codex_home.path());
        assert!(
            auth_file.exists(),
            "fallback auth.json should be created when keyring save fails"
        );
        let saved = storage
            .file_storage
            .load()?
            .context("fallback auth should exist")?;
        assert_eq!(saved, auth);
        assert!(
            mock_keyring.saved_value(&key).is_none(),
            "keyring should not contain value when save fails"
        );
        Ok(())
    }

    #[test]
    fn auto_auth_storage_delete_removes_keyring_and_file() -> anyhow::Result<()> {
        let codex_home = tempdir()?;
        let mock_keyring = MockKeyringStore::default();
        let storage = AutoAuthStorage::new(
            codex_home.path().to_path_buf(),
            Arc::new(mock_keyring.clone()),
        );
        let (key, auth_file) = seed_keyring_and_fallback_auth_file_for_delete(
            &mock_keyring,
            codex_home.path(),
            || compute_store_key(codex_home.path()),
        )?;

        let removed = storage.delete()?;

        assert!(removed, "delete should report removal");
        assert!(
            !mock_keyring.contains(&key),
            "keyring entry should be removed"
        );
        assert!(
            !auth_file.exists(),
            "fallback auth.json should be removed after delete"
        );
        Ok(())
    }
}
