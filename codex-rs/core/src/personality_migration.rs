use crate::config::edit::ConfigEditsBuilder;
use codex_config::config_toml::ConfigToml;
use codex_protocol::config_types::Personality;
use codex_thread_store::ListThreadsParams;
use codex_thread_store::LocalThreadStore;
use codex_thread_store::ThreadSortKey;
use codex_thread_store::ThreadStore;
use std::io;
use std::path::Path;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

// Merge-safety anchor: personality migration must keep rollout/session followers aligned with persisted subagent file-mutation mode.

pub const PERSONALITY_MIGRATION_FILENAME: &str = ".personality_migration";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonalityMigrationStatus {
    SkippedMarker,
    SkippedExplicitPersonality,
    SkippedNoSessions,
    Applied,
}

pub async fn maybe_migrate_personality(
    codex_home: &Path,
    config_toml: &ConfigToml,
) -> io::Result<PersonalityMigrationStatus> {
    let marker_path = codex_home.join(PERSONALITY_MIGRATION_FILENAME);
    if tokio::fs::try_exists(&marker_path).await? {
        return Ok(PersonalityMigrationStatus::SkippedMarker);
    }

    let config_profile = config_toml
        .get_config_profile(/*override_profile*/ None)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if config_toml.personality.is_some() || config_profile.personality.is_some() {
        create_marker(&marker_path).await?;
        return Ok(PersonalityMigrationStatus::SkippedExplicitPersonality);
    }

    let model_provider_id = config_profile
        .model_provider
        .or_else(|| config_toml.model_provider.clone())
        .unwrap_or_else(|| "openai".to_string());

    if !has_recorded_sessions(codex_home, model_provider_id.as_str()).await? {
        create_marker(&marker_path).await?;
        return Ok(PersonalityMigrationStatus::SkippedNoSessions);
    }

    ConfigEditsBuilder::new(codex_home)
        .set_personality(Some(Personality::Pragmatic))
        .apply()
        .await
        .map_err(|err| {
            io::Error::other(format!("failed to persist personality migration: {err}"))
        })?;

    create_marker(&marker_path).await?;
    Ok(PersonalityMigrationStatus::Applied)
}

async fn has_recorded_sessions(codex_home: &Path, default_provider: &str) -> io::Result<bool> {
    let store = LocalThreadStore::new(codex_rollout::RolloutConfig {
        codex_home: codex_home.to_path_buf(),
        sqlite_home: codex_home.to_path_buf(),
        cwd: codex_home.to_path_buf(),
        model_provider_id: default_provider.to_string(),
        generate_memories: false,
        subagent_file_mutation_mode: Default::default(),
    });
    if has_threads(&store, /*archived*/ false).await? {
        return Ok(true);
    }
    has_threads(&store, /*archived*/ true).await
}

async fn has_threads(store: &LocalThreadStore, archived: bool) -> io::Result<bool> {
    store
        .list_threads(ListThreadsParams {
            page_size: 1,
            cursor: None,
            sort_key: ThreadSortKey::CreatedAt,
            sort_direction: codex_thread_store::SortDirection::Desc,
            allowed_sources: Vec::new(),
            model_providers: None,
            archived,
            search_term: None,
        })
        .await
        .map(|page| !page.items.is_empty())
        .map_err(io::Error::other)
}

async fn create_marker(marker_path: &Path) -> io::Result<()> {
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(marker_path)
        .await
    {
        Ok(mut file) => file.write_all(b"v1\n").await,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::RolloutItem;
    use codex_protocol::protocol::RolloutLine;
    use codex_protocol::protocol::SessionMeta;
    use codex_protocol::protocol::SessionMetaLine;
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::UserMessageEvent;
    use codex_rollout::SESSIONS_SUBDIR;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    const TEST_TIMESTAMP: &str = "2025-01-01T00-00-00";

    async fn read_config_toml(codex_home: &Path) -> io::Result<ConfigToml> {
        let contents = tokio::fs::read_to_string(codex_home.join("config.toml")).await?;
        toml::from_str(&contents).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    async fn write_session_with_user_event(codex_home: &Path) -> io::Result<()> {
        let thread_id = ThreadId::new();
        let dir = codex_home
            .join(SESSIONS_SUBDIR)
            .join("2025")
            .join("01")
            .join("01");
        tokio::fs::create_dir_all(&dir).await?;
        let file_path = dir.join(format!("rollout-{TEST_TIMESTAMP}-{thread_id}.jsonl"));
        let mut file = tokio::fs::File::create(&file_path).await?;

        let session_meta = SessionMetaLine {
            meta: SessionMeta {
                id: thread_id,
                forked_from_id: None,
                timestamp: TEST_TIMESTAMP.to_string(),
                cwd: std::path::PathBuf::from("."),
                config_path: None,
                originator: "test_originator".to_string(),
                cli_version: "test_version".to_string(),
                source: SessionSource::Cli,
                agent_nickname: None,
                agent_role: None,
                agent_path: None,
                subagent_file_mutation_mode: Default::default(),
                model_provider: None,
                base_instructions: None,
                dynamic_tools: None,
                memory_mode: None,
            },
            git: None,
        };
        let meta_line = RolloutLine {
            timestamp: TEST_TIMESTAMP.to_string(),
            item: RolloutItem::SessionMeta(session_meta),
        };
        let user_event = RolloutLine {
            timestamp: TEST_TIMESTAMP.to_string(),
            item: RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: "hello".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            })),
        };

        file.write_all(format!("{}\n", serde_json::to_string(&meta_line)?).as_bytes())
            .await?;
        file.write_all(format!("{}\n", serde_json::to_string(&user_event)?).as_bytes())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn applies_when_sessions_exist_and_no_personality() -> io::Result<()> {
        let temp = TempDir::new()?;
        write_session_with_user_event(temp.path()).await?;

        let config_toml = ConfigToml::default();
        let status = maybe_migrate_personality(temp.path(), &config_toml).await?;

        assert_eq!(status, PersonalityMigrationStatus::Applied);
        assert!(temp.path().join(PERSONALITY_MIGRATION_FILENAME).exists());

        let persisted = read_config_toml(temp.path()).await?;
        assert_eq!(persisted.personality, Some(Personality::Pragmatic));
        Ok(())
    }

    #[tokio::test]
    async fn skips_when_marker_exists() -> io::Result<()> {
        let temp = TempDir::new()?;
        create_marker(&temp.path().join(PERSONALITY_MIGRATION_FILENAME)).await?;

        let config_toml = ConfigToml::default();
        let status = maybe_migrate_personality(temp.path(), &config_toml).await?;

        assert_eq!(status, PersonalityMigrationStatus::SkippedMarker);
        assert!(!temp.path().join("config.toml").exists());
        Ok(())
    }

    #[tokio::test]
    async fn skips_when_personality_explicit() -> io::Result<()> {
        let temp = TempDir::new()?;
        ConfigEditsBuilder::new(temp.path())
            .set_personality(Some(Personality::Friendly))
            .apply()
            .await
            .map_err(|err| io::Error::other(format!("failed to write config: {err}")))?;

        let config_toml = read_config_toml(temp.path()).await?;
        let status = maybe_migrate_personality(temp.path(), &config_toml).await?;

        assert_eq!(
            status,
            PersonalityMigrationStatus::SkippedExplicitPersonality
        );
        assert!(temp.path().join(PERSONALITY_MIGRATION_FILENAME).exists());

        let persisted = read_config_toml(temp.path()).await?;
        assert_eq!(persisted.personality, Some(Personality::Friendly));
        Ok(())
    }

    #[tokio::test]
    async fn skips_when_no_sessions() -> io::Result<()> {
        let temp = TempDir::new()?;
        let config_toml = ConfigToml::default();
        let status = maybe_migrate_personality(temp.path(), &config_toml).await?;

        assert_eq!(status, PersonalityMigrationStatus::SkippedNoSessions);
        assert!(temp.path().join(PERSONALITY_MIGRATION_FILENAME).exists());
        assert!(!temp.path().join("config.toml").exists());
        Ok(())
    }
}
