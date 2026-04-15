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
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

// Merge-safety anchor: WS12 account usage truth now persists under sqlite_home
// instead of the legacy auth-store cache fields; future autoswitch/accounts
// work must keep this owner aligned with login/core/TUI account-state readers.

pub const ACCOUNTS_DB_FILENAME: &str = "accounts";
pub const ACCOUNTS_DB_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsageState {
    pub last_rate_limits: Option<RateLimitSnapshot>,
    pub exhausted_until: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
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
            "#,
        )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::RateLimitWindow;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_763_233_549, 0).expect("fixed timestamp")
    }

    fn test_snapshot(primary_used_percent: f64) -> RateLimitSnapshot {
        RateLimitSnapshot {
            limit_id: Some("limit-1".to_string()),
            limit_name: Some("primary".to_string()),
            primary: Some(RateLimitWindow {
                used_percent: primary_used_percent,
                window_minutes: Some(300),
                resets_at: Some((Utc::now() + chrono::Duration::hours(1)).timestamp()),
            }),
            secondary: None,
            credits: None,
            plan_type: None,
        }
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
}
