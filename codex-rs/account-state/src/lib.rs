//! SQLite-backed persisted runtime state for saved-account usage truth.
//!
//! This crate owns account usage observations that need to survive process
//! restarts and be shared across concurrent Codex sessions without reusing the
//! legacy auth-store cache fields as runtime truth.

use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use codex_protocol::protocol::RateLimitSnapshot;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

// Merge-safety anchor: WS12 account usage truth now persists under sqlite_home
// instead of the legacy auth-store cache fields; future autoswitch/accounts
// work must keep this owner aligned with login/core/TUI account-state readers.

pub const ACCOUNTS_DB_FILENAME: &str = "accounts";
pub const ACCOUNTS_DB_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsageState {
    pub last_rate_limits: Option<RateLimitSnapshot>,
    pub exhausted_until: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLeaseConflict {
    pub owner_session_id: String,
    pub owner_codex_session_id: Option<String>,
    pub lease_until: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionActiveAccountSetOutcome {
    Assigned,
    Conflict(AccountLeaseConflict),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionActiveAccountRefresh {
    None,
    Active(String),
    LostToOtherSession {
        account_id: String,
        owner_session_id: String,
        owner_codex_session_id: Option<String>,
        lease_until: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceReleasedAccountOwners {
    pub session_ids: Vec<String>,
    pub codex_session_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForceReleaseAccountOutcome {
    NotFound,
    Released(ForceReleasedAccountOwners),
}

#[derive(Clone)]
pub struct AccountStateStore {
    sqlite_home: PathBuf,
    connection: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for AccountStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountStateStore")
            .field("sqlite_home", &self.sqlite_home)
            .finish_non_exhaustive()
    }
}

impl AccountStateStore {
    pub fn open(sqlite_home: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&sqlite_home)?;
        let path = accounts_db_path(&sqlite_home);
        let connection = Connection::open(path.as_path())?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS account_usage_state (
    account_id TEXT PRIMARY KEY,
    rate_limits_json TEXT,
    exhausted_until INTEGER,
    last_seen_at INTEGER,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS session_active_account (
    session_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    codex_session_id TEXT,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS account_leases (
    account_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    codex_session_id TEXT,
    lease_until INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
            "#,
        )?;
        ensure_optional_text_column(&connection, "session_active_account", "codex_session_id")?;
        ensure_optional_text_column(&connection, "account_leases", "codex_session_id")?;
        Ok(Self {
            sqlite_home,
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn sqlite_home(&self) -> &Path {
        self.sqlite_home.as_path()
    }

    pub fn is_usage_state_empty(&self) -> Result<bool> {
        let connection = self.lock_connection()?;
        let count =
            connection.query_row("SELECT COUNT(*) FROM account_usage_state", [], |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(count == 0)
    }

    pub fn load_usage_states_for_accounts(
        &self,
        account_ids: &[String],
    ) -> Result<HashMap<String, AccountUsageState>> {
        if account_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let connection = self.lock_connection()?;
        let mut statement = connection.prepare(
            "SELECT account_id, rate_limits_json, exhausted_until, last_seen_at FROM account_usage_state WHERE account_id = ?",
        )?;
        let mut usage_by_account = HashMap::with_capacity(account_ids.len());
        for account_id in account_ids {
            let maybe_state = statement
                .query_row([account_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                })
                .optional()?;
            if let Some((account_id, serialized_rate_limits, exhausted_until, last_seen_at)) =
                maybe_state
            {
                usage_by_account.insert(
                    account_id,
                    AccountUsageState {
                        last_rate_limits: deserialize_snapshot(serialized_rate_limits)?,
                        exhausted_until: epoch_seconds_to_datetime(exhausted_until),
                        last_seen_at: epoch_seconds_to_datetime(last_seen_at),
                    },
                );
            }
        }
        Ok(usage_by_account)
    }

    pub fn replace_usage_states(
        &self,
        usage_by_account: &HashMap<String, AccountUsageState>,
    ) -> Result<()> {
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM account_usage_state", [])?;
        let mut statement = transaction.prepare(
            r#"
INSERT INTO account_usage_state (
    account_id,
    rate_limits_json,
    exhausted_until,
    last_seen_at,
    updated_at
) VALUES (?, ?, ?, ?, ?)
            "#,
        )?;
        let now = Utc::now().timestamp();
        for (account_id, state) in usage_by_account {
            statement.execute(params![
                account_id,
                serialize_snapshot(state.last_rate_limits.as_ref())?,
                datetime_to_epoch_seconds(state.exhausted_until.as_ref()),
                datetime_to_epoch_seconds(state.last_seen_at.as_ref()),
                now,
            ])?;
        }
        drop(statement);
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert_usage_states(
        &self,
        usage_by_account: &HashMap<String, AccountUsageState>,
    ) -> Result<()> {
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction()?;
        let mut statement = transaction.prepare(
            r#"
INSERT INTO account_usage_state (
    account_id,
    rate_limits_json,
    exhausted_until,
    last_seen_at,
    updated_at
) VALUES (?, ?, ?, ?, ?)
ON CONFLICT(account_id) DO UPDATE SET
    rate_limits_json = excluded.rate_limits_json,
    exhausted_until = excluded.exhausted_until,
    last_seen_at = excluded.last_seen_at,
    updated_at = excluded.updated_at
            "#,
        )?;
        let now = Utc::now().timestamp();
        for (account_id, state) in usage_by_account {
            statement.execute(params![
                account_id,
                serialize_snapshot(state.last_rate_limits.as_ref())?,
                datetime_to_epoch_seconds(state.exhausted_until.as_ref()),
                datetime_to_epoch_seconds(state.last_seen_at.as_ref()),
                now,
            ])?;
        }
        drop(statement);
        transaction.commit()?;
        Ok(())
    }

    pub fn refresh_session_active_account(
        &self,
        session_id: &str,
        codex_session_id: Option<&str>,
        now: DateTime<Utc>,
        lease_ttl_seconds: i64,
    ) -> Result<SessionActiveAccountRefresh> {
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction()?;
        let now_epoch = now.timestamp();
        let Some(account_id) = load_session_active_account_id(&transaction, session_id)? else {
            transaction.commit()?;
            return Ok(SessionActiveAccountRefresh::None);
        };
        if let Some(conflict) =
            load_unexpired_lease_conflict(&transaction, account_id.as_str(), session_id, now_epoch)?
        {
            if conflict_matches_codex_session(&conflict, codex_session_id) {
                collapse_logical_session_runtime_rows(
                    &transaction,
                    session_id,
                    codex_session_id,
                    account_id.as_str(),
                )?;
                upsert_session_active_account(
                    &transaction,
                    session_id,
                    account_id.as_str(),
                    codex_session_id,
                    now_epoch,
                )?;
                upsert_account_lease(
                    &transaction,
                    account_id.as_str(),
                    session_id,
                    codex_session_id,
                    lease_until_epoch(now_epoch, lease_ttl_seconds),
                    now_epoch,
                )?;
                transaction.commit()?;
                return Ok(SessionActiveAccountRefresh::Active(account_id));
            }
            transaction.execute(
                "DELETE FROM session_active_account WHERE session_id = ?",
                [session_id],
            )?;
            transaction.commit()?;
            return Ok(SessionActiveAccountRefresh::LostToOtherSession {
                account_id,
                owner_session_id: conflict.owner_session_id,
                owner_codex_session_id: conflict.owner_codex_session_id,
                lease_until: conflict.lease_until,
            });
        }
        collapse_logical_session_runtime_rows(
            &transaction,
            session_id,
            codex_session_id,
            account_id.as_str(),
        )?;
        upsert_session_active_account(
            &transaction,
            session_id,
            account_id.as_str(),
            codex_session_id,
            now_epoch,
        )?;
        upsert_account_lease(
            &transaction,
            account_id.as_str(),
            session_id,
            codex_session_id,
            lease_until_epoch(now_epoch, lease_ttl_seconds),
            now_epoch,
        )?;
        transaction.commit()?;
        Ok(SessionActiveAccountRefresh::Active(account_id))
    }

    pub fn set_session_active_account(
        &self,
        session_id: &str,
        codex_session_id: Option<&str>,
        account_id: &str,
        now: DateTime<Utc>,
        lease_ttl_seconds: i64,
    ) -> Result<SessionActiveAccountSetOutcome> {
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction()?;
        let now_epoch = now.timestamp();
        if let Some(conflict) =
            load_unexpired_lease_conflict(&transaction, account_id, session_id, now_epoch)?
            && !conflict_matches_codex_session(&conflict, codex_session_id)
        {
            transaction.commit()?;
            return Ok(SessionActiveAccountSetOutcome::Conflict(conflict));
        }
        if let Some(previous_account_id) = load_session_active_account_id(&transaction, session_id)?
            && previous_account_id != account_id
        {
            transaction.execute(
                "DELETE FROM account_leases WHERE account_id = ? AND session_id = ?",
                params![previous_account_id, session_id],
            )?;
        }
        collapse_logical_session_runtime_rows(
            &transaction,
            session_id,
            codex_session_id,
            account_id,
        )?;
        upsert_session_active_account(
            &transaction,
            session_id,
            account_id,
            codex_session_id,
            now_epoch,
        )?;
        upsert_account_lease(
            &transaction,
            account_id,
            session_id,
            codex_session_id,
            lease_until_epoch(now_epoch, lease_ttl_seconds),
            now_epoch,
        )?;
        transaction.commit()?;
        Ok(SessionActiveAccountSetOutcome::Assigned)
    }

    pub fn clear_session_active_account(&self, session_id: &str) -> Result<Option<String>> {
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction()?;
        let account_id = load_session_active_account_id(&transaction, session_id)?;
        if let Some(account_id) = account_id.as_deref() {
            transaction.execute(
                "DELETE FROM account_leases WHERE account_id = ? AND session_id = ?",
                params![account_id, session_id],
            )?;
        }
        transaction.execute(
            "DELETE FROM session_active_account WHERE session_id = ?",
            [session_id],
        )?;
        transaction.commit()?;
        Ok(account_id)
    }

    pub fn force_release_account(&self, account_id: &str) -> Result<ForceReleaseAccountOutcome> {
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction()?;
        let released_owners = load_force_released_account_owners(&transaction, account_id)?;
        if released_owners.session_ids.is_empty() && released_owners.codex_session_ids.is_empty() {
            transaction.commit()?;
            return Ok(ForceReleaseAccountOutcome::NotFound);
        }
        transaction.execute(
            "DELETE FROM account_leases WHERE account_id = ?",
            [account_id],
        )?;
        transaction.execute(
            "DELETE FROM session_active_account WHERE account_id = ?",
            [account_id],
        )?;
        transaction.commit()?;
        Ok(ForceReleaseAccountOutcome::Released(released_owners))
    }

    pub fn account_is_leased_by_other(
        &self,
        session_id: &str,
        codex_session_id: Option<&str>,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let connection = self.lock_connection()?;
        Ok(
            load_unexpired_lease_conflict(&connection, account_id, session_id, now.timestamp())?
                .is_some_and(|conflict| {
                    !conflict_matches_codex_session(&conflict, codex_session_id)
                }),
        )
    }

    pub fn account_ids_leased_by_other(
        &self,
        session_id: &str,
        codex_session_id: Option<&str>,
        account_ids: &[String],
        now: DateTime<Utc>,
    ) -> Result<HashSet<String>> {
        if account_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let connection = self.lock_connection()?;
        let now_epoch = now.timestamp();
        let mut leased_account_ids = HashSet::new();
        for account_id in account_ids {
            if load_unexpired_lease_conflict(&connection, account_id, session_id, now_epoch)?
                .is_some_and(|conflict| {
                    !conflict_matches_codex_session(&conflict, codex_session_id)
                })
            {
                leased_account_ids.insert(account_id.clone());
            }
        }
        Ok(leased_account_ids)
    }

    fn lock_connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock account state connection"))
    }
}

pub fn accounts_db_filename() -> String {
    format!("{ACCOUNTS_DB_FILENAME}_{ACCOUNTS_DB_VERSION}.sqlite")
}

pub fn accounts_db_path(sqlite_home: &Path) -> PathBuf {
    sqlite_home.join(accounts_db_filename())
}

fn serialize_snapshot(snapshot: Option<&RateLimitSnapshot>) -> Result<Option<String>> {
    snapshot
        .map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

fn deserialize_snapshot(serialized: Option<String>) -> Result<Option<RateLimitSnapshot>> {
    serialized
        .map(|serialized| serde_json::from_str::<RateLimitSnapshot>(serialized.as_str()))
        .transpose()
        .map_err(Into::into)
}

fn datetime_to_epoch_seconds(value: Option<&DateTime<Utc>>) -> Option<i64> {
    value.map(DateTime::timestamp)
}

fn epoch_seconds_to_datetime(value: Option<i64>) -> Option<DateTime<Utc>> {
    value.and_then(|value| DateTime::<Utc>::from_timestamp(value, 0))
}

fn load_session_active_account_id(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT account_id FROM session_active_account WHERE session_id = ?",
            [session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
}

fn load_force_released_account_owners(
    connection: &Connection,
    account_id: &str,
) -> Result<ForceReleasedAccountOwners> {
    let mut session_ids = BTreeSet::new();
    let mut codex_session_ids = BTreeSet::new();

    let mut session_rows = connection.prepare(
        "SELECT session_id, codex_session_id FROM session_active_account WHERE account_id = ?",
    )?;
    let session_rows = session_rows.query_map([account_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in session_rows {
        let (session_id, codex_session_id) = row?;
        session_ids.insert(session_id);
        if let Some(codex_session_id) = codex_session_id {
            codex_session_ids.insert(codex_session_id);
        }
    }

    let mut lease_rows = connection
        .prepare("SELECT session_id, codex_session_id FROM account_leases WHERE account_id = ?")?;
    let lease_rows = lease_rows.query_map([account_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in lease_rows {
        let (session_id, codex_session_id) = row?;
        session_ids.insert(session_id);
        if let Some(codex_session_id) = codex_session_id {
            codex_session_ids.insert(codex_session_id);
        }
    }

    Ok(ForceReleasedAccountOwners {
        session_ids: session_ids.into_iter().collect(),
        codex_session_ids: codex_session_ids.into_iter().collect(),
    })
}

fn load_unexpired_lease_conflict(
    connection: &Connection,
    account_id: &str,
    session_id: &str,
    now_epoch: i64,
) -> Result<Option<AccountLeaseConflict>> {
    connection
        .query_row(
            r#"
SELECT session_id, codex_session_id, lease_until
FROM account_leases
WHERE account_id = ? AND lease_until > ? AND session_id != ?
            "#,
            params![account_id, now_epoch, session_id],
            |row| {
                let lease_until_epoch = row.get::<_, i64>(2)?;
                let lease_until =
                    epoch_seconds_to_datetime(Some(lease_until_epoch)).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Integer,
                            format!("invalid lease_until epoch {lease_until_epoch}").into(),
                        )
                    })?;
                Ok(AccountLeaseConflict {
                    owner_session_id: row.get::<_, String>(0)?,
                    owner_codex_session_id: row.get::<_, Option<String>>(1)?,
                    lease_until,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn upsert_session_active_account(
    connection: &Connection,
    session_id: &str,
    account_id: &str,
    codex_session_id: Option<&str>,
    now_epoch: i64,
) -> Result<()> {
    connection.execute(
        r#"
INSERT INTO session_active_account (session_id, account_id, codex_session_id, updated_at)
VALUES (?, ?, ?, ?)
ON CONFLICT(session_id) DO UPDATE SET
    account_id = excluded.account_id,
    codex_session_id = excluded.codex_session_id,
    updated_at = excluded.updated_at
        "#,
        params![session_id, account_id, codex_session_id, now_epoch],
    )?;
    Ok(())
}

fn upsert_account_lease(
    connection: &Connection,
    account_id: &str,
    session_id: &str,
    codex_session_id: Option<&str>,
    lease_until: i64,
    now_epoch: i64,
) -> Result<()> {
    connection.execute(
        r#"
INSERT INTO account_leases (account_id, session_id, codex_session_id, lease_until, updated_at)
VALUES (?, ?, ?, ?, ?)
ON CONFLICT(account_id) DO UPDATE SET
    session_id = excluded.session_id,
    codex_session_id = excluded.codex_session_id,
    lease_until = excluded.lease_until,
    updated_at = excluded.updated_at
        "#,
        params![
            account_id,
            session_id,
            codex_session_id,
            lease_until,
            now_epoch
        ],
    )?;
    Ok(())
}

fn ensure_optional_text_column(
    connection: &Connection,
    table_name: &str,
    column_name: &str,
) -> Result<()> {
    let pragma = format!("PRAGMA table_info({table_name})");
    let mut statement = connection.prepare(pragma.as_str())?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column_name {
            return Ok(());
        }
    }
    let alter = format!("ALTER TABLE {table_name} ADD COLUMN {column_name} TEXT");
    connection.execute(alter.as_str(), [])?;
    Ok(())
}

fn collapse_logical_session_runtime_rows(
    connection: &Connection,
    session_id: &str,
    codex_session_id: Option<&str>,
    account_id: &str,
) -> Result<()> {
    let Some(codex_session_id) = codex_session_id else {
        return Ok(());
    };
    connection.execute(
        r#"
DELETE FROM session_active_account
WHERE codex_session_id = ? AND session_id != ?
        "#,
        params![codex_session_id, session_id],
    )?;
    connection.execute(
        r#"
DELETE FROM account_leases
WHERE codex_session_id = ? AND account_id != ?
        "#,
        params![codex_session_id, account_id],
    )?;
    Ok(())
}

fn conflict_matches_codex_session(
    conflict: &AccountLeaseConflict,
    codex_session_id: Option<&str>,
) -> bool {
    codex_session_id.is_some_and(|codex_session_id| {
        conflict.owner_codex_session_id.as_deref() == Some(codex_session_id)
    })
}

fn lease_until_epoch(now_epoch: i64, lease_ttl_seconds: i64) -> i64 {
    now_epoch + lease_ttl_seconds
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::RateLimitWindow;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_763_233_549, 0).expect("fixed timestamp")
    }

    fn test_snapshot(primary_remaining_percent: f64) -> RateLimitSnapshot {
        RateLimitSnapshot {
            limit_id: Some("limit-1".to_string()),
            limit_name: Some("primary".to_string()),
            primary: Some(RateLimitWindow {
                remaining_percent: primary_remaining_percent,
                window_minutes: Some(300),
                resets_at: Some((Utc::now() + chrono::Duration::hours(1)).timestamp()),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct SessionActiveAccountRow {
        session_id: String,
        account_id: String,
        codex_session_id: Option<String>,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct AccountLeaseRow {
        account_id: String,
        session_id: String,
        codex_session_id: Option<String>,
    }

    fn load_session_active_rows(store: &AccountStateStore) -> Vec<SessionActiveAccountRow> {
        let connection = store.lock_connection().expect("lock connection");
        let mut statement = connection
            .prepare(
                r#"
SELECT session_id, account_id, codex_session_id
FROM session_active_account
ORDER BY session_id
                "#,
            )
            .expect("prepare session_active_account query");
        let rows = statement
            .query_map([], |row| {
                Ok(SessionActiveAccountRow {
                    session_id: row.get::<_, String>(0)?,
                    account_id: row.get::<_, String>(1)?,
                    codex_session_id: row.get::<_, Option<String>>(2)?,
                })
            })
            .expect("query session_active_account rows");
        rows.map(|row| row.expect("read session_active_account row"))
            .collect()
    }

    fn load_account_lease_rows(store: &AccountStateStore) -> Vec<AccountLeaseRow> {
        let connection = store.lock_connection().expect("lock connection");
        let mut statement = connection
            .prepare(
                r#"
SELECT account_id, session_id, codex_session_id
FROM account_leases
ORDER BY account_id
                "#,
            )
            .expect("prepare account_leases query");
        let rows = statement
            .query_map([], |row| {
                Ok(AccountLeaseRow {
                    account_id: row.get::<_, String>(0)?,
                    session_id: row.get::<_, String>(1)?,
                    codex_session_id: row.get::<_, Option<String>>(2)?,
                })
            })
            .expect("query account_leases rows");
        rows.map(|row| row.expect("read account_leases row"))
            .collect()
    }

    #[test]
    fn replace_usage_states_round_trips_and_clears_removed_accounts() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();
        let mut usage_by_account = HashMap::new();
        usage_by_account.insert(
            "account-a".to_string(),
            AccountUsageState {
                last_rate_limits: Some(test_snapshot(25.0)),
                exhausted_until: Some(now + chrono::Duration::minutes(5)),
                last_seen_at: Some(now),
            },
        );
        store
            .replace_usage_states(&usage_by_account)
            .expect("persist usage states");

        let loaded = store
            .load_usage_states_for_accounts(&["account-a".to_string(), "account-b".to_string()])
            .expect("load usage states");
        assert_eq!(loaded.get("account-a"), usage_by_account.get("account-a"));
        assert_eq!(loaded.get("account-b"), None);

        store
            .replace_usage_states(&HashMap::new())
            .expect("clear usage states");
        assert!(store.is_usage_state_empty().expect("empty check"));
    }

    #[test]
    fn upsert_usage_states_preserves_existing_rows() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();
        let mut initial_usage_by_account = HashMap::new();
        initial_usage_by_account.insert(
            "account-a".to_string(),
            AccountUsageState {
                last_rate_limits: Some(test_snapshot(25.0)),
                exhausted_until: Some(now + chrono::Duration::minutes(5)),
                last_seen_at: Some(now),
            },
        );
        store
            .replace_usage_states(&initial_usage_by_account)
            .expect("persist initial usage states");

        let mut incremental_usage_by_account = HashMap::new();
        incremental_usage_by_account.insert(
            "account-b".to_string(),
            AccountUsageState {
                last_rate_limits: Some(test_snapshot(10.0)),
                exhausted_until: Some(now + chrono::Duration::minutes(15)),
                last_seen_at: Some(now),
            },
        );
        store
            .upsert_usage_states(&incremental_usage_by_account)
            .expect("upsert incremental usage states");

        let loaded = store
            .load_usage_states_for_accounts(&["account-a".to_string(), "account-b".to_string()])
            .expect("load usage states after upsert");
        assert_eq!(
            loaded.get("account-a"),
            initial_usage_by_account.get("account-a")
        );
        assert_eq!(
            loaded.get("account-b"),
            incremental_usage_by_account.get("account-b")
        );
    }

    #[test]
    fn set_session_active_account_acquires_and_refreshes_lease() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();

        assert_eq!(
            store
                .set_session_active_account("session-a", None, "account-a", now, 300)
                .expect("assign initial lease"),
            SessionActiveAccountSetOutcome::Assigned
        );
        assert_eq!(
            store
                .refresh_session_active_account(
                    "session-a",
                    None,
                    now + chrono::Duration::seconds(30),
                    300,
                )
                .expect("refresh owned lease"),
            SessionActiveAccountRefresh::Active("account-a".to_string())
        );
    }

    #[test]
    fn set_session_active_account_conflicts_with_other_live_session() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();

        store
            .set_session_active_account("session-a", None, "account-a", now, 300)
            .expect("assign initial lease");
        let outcome = store
            .set_session_active_account(
                "session-b",
                None,
                "account-a",
                now + chrono::Duration::seconds(1),
                300,
            )
            .expect("second assignment should not error");
        assert_eq!(
            outcome,
            SessionActiveAccountSetOutcome::Conflict(AccountLeaseConflict {
                owner_session_id: "session-a".to_string(),
                owner_codex_session_id: None,
                lease_until: now + chrono::Duration::seconds(300),
            })
        );
    }

    #[test]
    fn clear_session_active_account_releases_owned_lease() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();

        store
            .set_session_active_account("session-a", None, "account-a", now, 300)
            .expect("assign initial lease");
        assert_eq!(
            store
                .clear_session_active_account("session-a")
                .expect("clear session active account"),
            Some("account-a".to_string())
        );
        assert_eq!(
            store
                .set_session_active_account(
                    "session-b",
                    None,
                    "account-a",
                    now + chrono::Duration::seconds(1),
                    300,
                )
                .expect("reassign released lease"),
            SessionActiveAccountSetOutcome::Assigned
        );
    }

    #[test]
    fn refresh_session_active_account_clears_row_after_losing_expired_lease() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();

        store
            .set_session_active_account("session-a", None, "account-a", now, 60)
            .expect("assign initial lease");
        assert_eq!(
            store
                .set_session_active_account(
                    "session-b",
                    None,
                    "account-a",
                    now + chrono::Duration::seconds(61),
                    60,
                )
                .expect("expired lease should be reusable"),
            SessionActiveAccountSetOutcome::Assigned
        );
        assert_eq!(
            store
                .refresh_session_active_account(
                    "session-a",
                    None,
                    now + chrono::Duration::seconds(62),
                    60,
                )
                .expect("refresh after losing lease"),
            SessionActiveAccountRefresh::LostToOtherSession {
                account_id: "account-a".to_string(),
                owner_session_id: "session-b".to_string(),
                owner_codex_session_id: None,
                lease_until: now + chrono::Duration::seconds(121),
            }
        );
        assert_eq!(
            store
                .refresh_session_active_account(
                    "session-a",
                    None,
                    now + chrono::Duration::seconds(63),
                    60,
                )
                .expect("row should be cleared after lost lease"),
            SessionActiveAccountRefresh::None
        );
    }

    #[test]
    fn account_ids_leased_by_other_returns_only_live_foreign_leases() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();

        store
            .set_session_active_account("session-a", None, "account-a", now, 300)
            .expect("assign foreign live lease");
        store
            .set_session_active_account("session-b", None, "account-b", now, 300)
            .expect("assign owned live lease");
        store
            .set_session_active_account("session-c", None, "account-c", now, 60)
            .expect("assign expiring foreign lease");

        let leased_account_ids = store
            .account_ids_leased_by_other(
                "session-b",
                None,
                &[
                    "account-a".to_string(),
                    "account-b".to_string(),
                    "account-c".to_string(),
                    "account-d".to_string(),
                ],
                now + chrono::Duration::seconds(61),
            )
            .expect("load bulk lease conflicts");

        assert_eq!(leased_account_ids, HashSet::from(["account-a".to_string()]),);
    }

    #[test]
    fn same_codex_session_takeover_collapses_old_runtime_rows() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();

        assert_eq!(
            store
                .set_session_active_account(
                    "runtime-a",
                    Some("codex-session-1"),
                    "account-a",
                    now,
                    300,
                )
                .expect("assign initial lease"),
            SessionActiveAccountSetOutcome::Assigned
        );
        assert_eq!(
            store
                .set_session_active_account(
                    "runtime-b",
                    Some("codex-session-1"),
                    "account-a",
                    now + chrono::Duration::seconds(1),
                    300,
                )
                .expect("same-session takeover should succeed"),
            SessionActiveAccountSetOutcome::Assigned
        );
        assert!(
            !store
                .account_is_leased_by_other(
                    "runtime-b",
                    Some("codex-session-1"),
                    "account-a",
                    now + chrono::Duration::seconds(2),
                )
                .expect("same-session lease should not be foreign"),
        );
        assert_eq!(
            load_session_active_rows(&store),
            vec![SessionActiveAccountRow {
                session_id: "runtime-b".to_string(),
                account_id: "account-a".to_string(),
                codex_session_id: Some("codex-session-1".to_string()),
            }]
        );
        assert_eq!(
            load_account_lease_rows(&store),
            vec![AccountLeaseRow {
                account_id: "account-a".to_string(),
                session_id: "runtime-b".to_string(),
                codex_session_id: Some("codex-session-1".to_string()),
            }]
        );
        assert_eq!(
            store
                .refresh_session_active_account(
                    "runtime-a",
                    Some("codex-session-1"),
                    now + chrono::Duration::seconds(2),
                    300,
                )
                .expect("collapsed runtime row should stay gone"),
            SessionActiveAccountRefresh::None
        );
        assert!(
            !store
                .account_is_leased_by_other(
                    "runtime-a",
                    Some("codex-session-1"),
                    "account-a",
                    now + chrono::Duration::seconds(3),
                )
                .expect("reclaimed lease should stay local"),
        );
    }

    #[test]
    fn same_codex_session_switching_accounts_releases_old_lease() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();

        assert_eq!(
            store
                .set_session_active_account(
                    "runtime-a",
                    Some("codex-session-1"),
                    "account-a",
                    now,
                    300,
                )
                .expect("assign initial lease"),
            SessionActiveAccountSetOutcome::Assigned
        );
        assert_eq!(
            store
                .set_session_active_account(
                    "runtime-b",
                    Some("codex-session-1"),
                    "account-b",
                    now + chrono::Duration::seconds(1),
                    300,
                )
                .expect("same-session account switch should succeed"),
            SessionActiveAccountSetOutcome::Assigned
        );
        assert_eq!(
            load_session_active_rows(&store),
            vec![SessionActiveAccountRow {
                session_id: "runtime-b".to_string(),
                account_id: "account-b".to_string(),
                codex_session_id: Some("codex-session-1".to_string()),
            }]
        );
        assert_eq!(
            load_account_lease_rows(&store),
            vec![AccountLeaseRow {
                account_id: "account-b".to_string(),
                session_id: "runtime-b".to_string(),
                codex_session_id: Some("codex-session-1".to_string()),
            }]
        );
        assert_eq!(
            store
                .set_session_active_account(
                    "runtime-c",
                    None,
                    "account-a",
                    now + chrono::Duration::seconds(2),
                    300,
                )
                .expect("released old lease should be reusable"),
            SessionActiveAccountSetOutcome::Assigned
        );
    }

    #[test]
    fn force_release_account_clears_lease_and_session_rows() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");
        let now = fixed_now();

        assert_eq!(
            store
                .set_session_active_account("session-a", None, "account-a", now, 300)
                .expect("assign initial lease"),
            SessionActiveAccountSetOutcome::Assigned
        );
        {
            let connection = store.lock_connection().expect("lock connection");
            upsert_session_active_account(
                &connection,
                "session-b",
                "account-a",
                Some("codex-session-2"),
                now.timestamp(),
            )
            .expect("seed duplicate runtime row");
            upsert_session_active_account(
                &connection,
                "session-c",
                "account-b",
                None,
                now.timestamp(),
            )
            .expect("seed unrelated session row");
        }

        assert_eq!(
            store
                .force_release_account("account-a")
                .expect("force release owned account"),
            ForceReleaseAccountOutcome::Released(ForceReleasedAccountOwners {
                session_ids: vec!["session-a".to_string(), "session-b".to_string()],
                codex_session_ids: vec!["codex-session-2".to_string()],
            })
        );
        assert_eq!(
            load_session_active_rows(&store),
            vec![SessionActiveAccountRow {
                session_id: "session-c".to_string(),
                account_id: "account-b".to_string(),
                codex_session_id: None,
            }]
        );
        assert!(load_account_lease_rows(&store).is_empty());
        assert_eq!(
            store
                .set_session_active_account(
                    "session-d",
                    None,
                    "account-a",
                    now + chrono::Duration::seconds(1),
                    300,
                )
                .expect("force-released account should be reusable"),
            SessionActiveAccountSetOutcome::Assigned
        );
    }

    #[test]
    fn force_release_account_is_loud_when_account_has_no_runtime_state() {
        let sqlite_home = TempDir::new().expect("tempdir");
        let store = AccountStateStore::open(sqlite_home.path().to_path_buf()).expect("open store");

        assert_eq!(
            store
                .force_release_account("account-a")
                .expect("force release should not error on missing account"),
            ForceReleaseAccountOutcome::NotFound
        );
    }
}
