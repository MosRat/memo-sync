use anyhow::Context;
use chrono::{DateTime, Utc};
use memo_core::{
    ClientInfo, HybridLogicalClock, Memo, MemoFilter, MemoMeta, MemoSource, PullRequest,
    PullResponse, PushRequest, Repository, RepositoryKind, SyncOperation, SyncOperationKind,
    DEFAULT_PULL_LIMIT, SYNC_PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
use std::{collections::BTreeSet, path::Path, time::Duration};
use uuid::Uuid;

const PUSH_BATCH_LIMIT: i64 = 500;

#[derive(Clone)]
pub struct LocalStore {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveMemoInput {
    pub id: Option<Uuid>,
    pub repository_id: Uuid,
    pub title: String,
    pub body_md: String,
    pub tags: BTreeSet<String>,
    pub pinned: bool,
    pub archived: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncSummary {
    pub pushed: usize,
    pub pulled: usize,
    pub server_sequence: i64,
}

impl LocalStore {
    pub async fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await?;
        configure_sqlite(&pool).await?;
        migrate(&pool).await?;
        let store = Self { pool };
        store.purge_temporary_memos().await?;
        store.ensure_defaults().await?;
        Ok(store)
    }

    pub async fn repositories(&self) -> anyhow::Result<Vec<Repository>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, sync_enabled, color, created_at, updated_at FROM repositories ORDER BY kind DESC, updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(repository_from_row).collect()
    }

    pub async fn memos(&self, filter: MemoFilter) -> anyhow::Result<Vec<Memo>> {
        let rows = sqlx::query(
            "SELECT id, repository_id, title, body_md, tags, pinned, archived, deleted, created_at, updated_at, source, meta FROM memos ORDER BY pinned DESC, updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut memos = rows
            .into_iter()
            .map(memo_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;
        memos.retain(|memo| filter.matches(memo));
        Ok(memos)
    }

    pub async fn create_repository(
        &self,
        name: String,
        temporary: bool,
        color: String,
        device_id: &str,
    ) -> anyhow::Result<Repository> {
        let kind = if temporary {
            RepositoryKind::Temporary
        } else {
            RepositoryKind::Persistent
        };
        let repo = Repository::new(name, kind, color);
        self.upsert_repository(&repo).await?;
        if repo.sync_enabled {
            let op = SyncOperation::new(
                device_id,
                HybridLogicalClock::now(),
                SyncOperationKind::UpsertRepository(repo.clone()),
            );
            self.append_local_operation(&op).await?;
        }
        Ok(repo)
    }

    pub async fn save_memo(
        &self,
        input: SaveMemoInput,
        source: MemoSource,
        device_id: &str,
    ) -> anyhow::Result<Memo> {
        let mut memo = if let Some(id) = input.id {
            self.memo_by_id(id)
                .await?
                .unwrap_or_else(|| Memo::new(input.repository_id, "", ""))
        } else {
            Memo::new(input.repository_id, "", "")
        };

        memo.repository_id = input.repository_id;
        memo.title = if input.title.trim().is_empty() {
            title_from_body(&input.body_md)
        } else {
            input.title
        };
        memo.body_md = input.body_md;
        memo.tags = input.tags;
        memo.pinned = input.pinned;
        memo.archived = input.archived;
        memo.deleted = false;
        memo.updated_at = Utc::now();
        memo.source = source;
        memo.meta.byte_len = memo.body_md.len();

        self.upsert_memo(&memo).await?;
        if self.repository_sync_enabled(memo.repository_id).await? {
            let op = SyncOperation::new(
                device_id,
                HybridLogicalClock::now(),
                SyncOperationKind::UpsertMemo(memo.clone()),
            );
            self.append_local_operation(&op).await?;
        }
        Ok(memo)
    }

    pub async fn delete_memo(&self, id: Uuid, device_id: &str) -> anyhow::Result<()> {
        let existing = self.memo_by_id(id).await?;
        sqlx::query("UPDATE memos SET deleted = 1, updated_at = ?2 WHERE id = ?1")
            .bind(id.to_string())
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await?;
        if let Some(memo) = existing {
            if self.repository_sync_enabled(memo.repository_id).await? {
                let op = SyncOperation::new(
                    device_id,
                    HybridLogicalClock::now(),
                    SyncOperationKind::DeleteMemo {
                        repository_id: memo.repository_id,
                        memo_id: id,
                    },
                );
                self.append_local_operation(&op).await?;
            }
        }
        Ok(())
    }

    pub async fn sync_now(&self, server_url: &str, device_id: &str) -> anyhow::Result<SyncSummary> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()?;
        let mut pushed = 0usize;
        let mut server_sequence = self.last_server_sequence().await?;

        loop {
            let pending = self.pending_operations(PUSH_BATCH_LIMIT).await?;
            if pending.is_empty() {
                break;
            }
            let push_response: memo_core::PushResponse = client
                .post(format!(
                    "{}/api/v1/sync/push",
                    server_url.trim_end_matches('/')
                ))
                .json(&PushRequest {
                    protocol_version: SYNC_PROTOCOL_VERSION,
                    device_id: device_id.to_string(),
                    client: Some(desktop_client_info()),
                    operations: pending.clone(),
                })
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            pushed += push_response.accepted;
            server_sequence = server_sequence.max(push_response.server_sequence);
            self.mark_operations_pushed(&pending, push_response.server_sequence)
                .await?;
        }

        let mut pulled = 0usize;
        loop {
            let since_sequence = self.last_server_sequence().await?;
            let pull: PullResponse = client
                .post(format!(
                    "{}/api/v1/sync/pull",
                    server_url.trim_end_matches('/')
                ))
                .json(&PullRequest {
                    protocol_version: SYNC_PROTOCOL_VERSION,
                    since_sequence,
                    repository_ids: vec![],
                    limit: DEFAULT_PULL_LIMIT,
                    client: Some(desktop_client_info()),
                })
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            pulled += pull.operations.len();
            server_sequence = server_sequence.max(pull.server_sequence);
            for item in pull.operations {
                self.apply_remote_operation(item.operation).await?;
                self.set_last_server_sequence(item.sequence).await?;
            }
            self.set_last_server_sequence(server_sequence).await?;
            if !pull.has_more {
                break;
            }
        }

        Ok(SyncSummary {
            pushed,
            pulled,
            server_sequence,
        })
    }

    async fn ensure_defaults(&self) -> anyhow::Result<()> {
        if !self.repositories().await?.is_empty() {
            return Ok(());
        }
        let inbox = Repository::new("Inbox", RepositoryKind::Persistent, "#c86f52");
        let scratch = Repository::new("Scratch", RepositoryKind::Temporary, "#6f8f83");
        self.upsert_repository(&inbox).await?;
        self.upsert_repository(&scratch).await?;

        let mut welcome = Memo::new(
            inbox.id,
            "晨间札记 / Morning Note",
            "把散落在剪贴板、会议和代码里的句子，收进一个可以同步的地方。\n\nUse **Markdown**, tags, repositories, quick capture, and background sync.\n\n```rust\nfn main() {\n    println!(\"quiet craft, fast notes\");\n}\n```",
        );
        welcome.tags = BTreeSet::from([
            "welcome".to_string(),
            "markdown".to_string(),
            "中文".to_string(),
        ]);
        self.upsert_memo(&welcome).await?;
        Ok(())
    }

    async fn purge_temporary_memos(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            DELETE FROM memos
            WHERE repository_id IN (
              SELECT id FROM repositories WHERE kind = 'temporary'
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn memo_by_id(&self, id: Uuid) -> anyhow::Result<Option<Memo>> {
        let row = sqlx::query(
            "SELECT id, repository_id, title, body_md, tags, pinned, archived, deleted, created_at, updated_at, source, meta FROM memos WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(memo_from_row).transpose()
    }

    async fn upsert_repository(&self, repo: &Repository) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO repositories (id, name, kind, sync_enabled, color, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
              name = excluded.name,
              kind = excluded.kind,
              sync_enabled = excluded.sync_enabled,
              color = excluded.color,
              updated_at = excluded.updated_at
            "#,
        )
        .bind(repo.id.to_string())
        .bind(&repo.name)
        .bind(kind_to_str(&repo.kind))
        .bind(repo.sync_enabled)
        .bind(&repo.color)
        .bind(repo.created_at.to_rfc3339())
        .bind(repo.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn upsert_memo(&self, memo: &Memo) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO memos (id, repository_id, title, body_md, tags, pinned, archived, deleted, created_at, updated_at, source, meta)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
              repository_id = excluded.repository_id,
              title = excluded.title,
              body_md = excluded.body_md,
              tags = excluded.tags,
              pinned = excluded.pinned,
              archived = excluded.archived,
              deleted = excluded.deleted,
              updated_at = excluded.updated_at,
              source = excluded.source,
              meta = excluded.meta
            "#,
        )
        .bind(memo.id.to_string())
        .bind(memo.repository_id.to_string())
        .bind(&memo.title)
        .bind(&memo.body_md)
        .bind(serde_json::to_string(&memo.tags)?)
        .bind(memo.pinned)
        .bind(memo.archived)
        .bind(memo.deleted)
        .bind(memo.created_at.to_rfc3339())
        .bind(memo.updated_at.to_rfc3339())
        .bind(source_to_str(&memo.source))
        .bind(serde_json::to_string(&memo.meta)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn append_local_operation(&self, op: &SyncOperation) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO local_operations (op_id, payload, created_at) VALUES (?1, ?2, ?3)",
        )
        .bind(op.op_id.to_string())
        .bind(serde_json::to_string(op)?)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn pending_operations(&self, limit: i64) -> anyhow::Result<Vec<SyncOperation>> {
        let rows = sqlx::query("SELECT payload FROM local_operations WHERE server_sequence IS NULL ORDER BY created_at ASC LIMIT ?1")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let payload: String = row.get("payload");
                serde_json::from_str(&payload).context("invalid local operation")
            })
            .collect()
    }

    async fn mark_operations_pushed(
        &self,
        operations: &[SyncOperation],
        sequence: i64,
    ) -> anyhow::Result<()> {
        for op in operations {
            sqlx::query("UPDATE local_operations SET server_sequence = ?2 WHERE op_id = ?1")
                .bind(op.op_id.to_string())
                .bind(sequence)
                .execute(&self.pool)
                .await?;
        }
        sqlx::query("DELETE FROM local_operations WHERE server_sequence IS NOT NULL")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn apply_remote_operation(&self, op: SyncOperation) -> anyhow::Result<()> {
        match op.kind {
            SyncOperationKind::UpsertRepository(repo) => self.upsert_repository(&repo).await?,
            SyncOperationKind::UpsertMemo(memo) => self.upsert_memo(&memo).await?,
            SyncOperationKind::DeleteMemo { memo_id, .. } => {
                sqlx::query("UPDATE memos SET deleted = 1, updated_at = ?2 WHERE id = ?1")
                    .bind(memo_id.to_string())
                    .bind(Utc::now().to_rfc3339())
                    .execute(&self.pool)
                    .await?;
            }
            SyncOperationKind::PatchMemo { memo_id, patch, .. } => {
                if let Some(mut memo) = self.memo_by_id(memo_id).await? {
                    memo.apply_patch(patch);
                    self.upsert_memo(&memo).await?;
                }
            }
        }
        Ok(())
    }

    async fn repository_sync_enabled(&self, id: Uuid) -> anyhow::Result<bool> {
        let row = sqlx::query("SELECT sync_enabled FROM repositories WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|row| row.get::<bool, _>("sync_enabled"))
            .unwrap_or(false))
    }

    async fn last_server_sequence(&self) -> anyhow::Result<i64> {
        let row = sqlx::query("SELECT value FROM meta WHERE key = 'last_server_sequence'")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .and_then(|row| row.get::<String, _>("value").parse::<i64>().ok())
            .unwrap_or(0))
    }

    async fn set_last_server_sequence(&self, sequence: i64) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO meta (key, value) VALUES ('last_server_sequence', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(sequence.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn desktop_client_info() -> ClientInfo {
    ClientInfo {
        name: "memo-sync-desktop".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS.to_string(),
    }
}

async fn configure_sqlite(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA temp_store = MEMORY")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA cache_size = -8000")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA mmap_size = 134217728")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA optimize").execute(pool).await?;
    Ok(())
}

async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(pool)
        .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS repositories (
          id TEXT PRIMARY KEY,
          name TEXT NOT NULL,
          kind TEXT NOT NULL,
          sync_enabled INTEGER NOT NULL,
          color TEXT NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS memos (
          id TEXT PRIMARY KEY,
          repository_id TEXT NOT NULL,
          title TEXT NOT NULL,
          body_md TEXT NOT NULL,
          tags TEXT NOT NULL,
          pinned INTEGER NOT NULL,
          archived INTEGER NOT NULL,
          deleted INTEGER NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          source TEXT NOT NULL,
          meta TEXT NOT NULL,
          FOREIGN KEY(repository_id) REFERENCES repositories(id)
        );
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS memos_repo_updated_idx ON memos(repository_id, updated_at)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS local_operations (
          op_id TEXT PRIMARY KEY,
          payload TEXT NOT NULL,
          server_sequence INTEGER,
          created_at TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(pool)
        .await?;
    Ok(())
}

fn repository_from_row(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<Repository> {
    Ok(Repository {
        id: Uuid::parse_str(&row.get::<String, _>("id"))?,
        name: row.get("name"),
        kind: kind_from_str(&row.get::<String, _>("kind")),
        sync_enabled: row.get("sync_enabled"),
        color: row.get("color"),
        created_at: parse_dt(&row.get::<String, _>("created_at"))?,
        updated_at: parse_dt(&row.get::<String, _>("updated_at"))?,
    })
}

fn memo_from_row(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<Memo> {
    let tags_json: String = row.get("tags");
    let meta_json: String = row.get("meta");
    Ok(Memo {
        id: Uuid::parse_str(&row.get::<String, _>("id"))?,
        repository_id: Uuid::parse_str(&row.get::<String, _>("repository_id"))?,
        title: row.get("title"),
        body_md: row.get("body_md"),
        tags: serde_json::from_str(&tags_json)?,
        pinned: row.get("pinned"),
        archived: row.get("archived"),
        deleted: row.get("deleted"),
        created_at: parse_dt(&row.get::<String, _>("created_at"))?,
        updated_at: parse_dt(&row.get::<String, _>("updated_at"))?,
        source: source_from_str(&row.get::<String, _>("source")),
        meta: serde_json::from_str::<MemoMeta>(&meta_json).unwrap_or_default(),
    })
}

fn parse_dt(input: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(input)?.with_timezone(&Utc))
}

fn kind_to_str(kind: &RepositoryKind) -> &'static str {
    match kind {
        RepositoryKind::Temporary => "temporary",
        RepositoryKind::Persistent => "persistent",
    }
}

fn kind_from_str(kind: &str) -> RepositoryKind {
    match kind {
        "temporary" => RepositoryKind::Temporary,
        _ => RepositoryKind::Persistent,
    }
}

fn source_to_str(source: &MemoSource) -> &'static str {
    match source {
        MemoSource::Manual => "manual",
        MemoSource::Clipboard => "clipboard",
        MemoSource::QuickCapture => "quick_capture",
        MemoSource::Import => "import",
    }
}

fn source_from_str(source: &str) -> MemoSource {
    match source {
        "clipboard" => MemoSource::Clipboard,
        "quick_capture" => MemoSource::QuickCapture,
        "import" => MemoSource::Import,
        _ => MemoSource::Manual,
    }
}

fn title_from_body(body: &str) -> String {
    body.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            line.trim()
                .trim_start_matches('#')
                .trim()
                .chars()
                .take(64)
                .collect()
        })
        .unwrap_or_else(|| "Untitled memo".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn temporary_repository_memos_are_not_synced_and_are_purged_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memo.sqlite");
        let store = LocalStore::open(&db).await.unwrap();
        let repo = store
            .create_repository(
                "Scratch test".to_string(),
                true,
                "#6f8f83".to_string(),
                "device-a",
            )
            .await
            .unwrap();

        let memo = store
            .save_memo(
                SaveMemoInput {
                    id: None,
                    repository_id: repo.id,
                    title: "Transient".to_string(),
                    body_md: "gone after restart".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::QuickCapture,
                "device-a",
            )
            .await
            .unwrap();

        assert_eq!(store.pending_operations(PUSH_BATCH_LIMIT).await.unwrap().len(), 0);
        assert!(store
            .memos(MemoFilter {
                repository_id: Some(repo.id),
                ..MemoFilter::default()
            })
            .await
            .unwrap()
            .iter()
            .any(|item| item.id == memo.id));

        drop(store);
        let reopened = LocalStore::open(&db).await.unwrap();
        assert!(reopened
            .memos(MemoFilter {
                repository_id: Some(repo.id),
                ..MemoFilter::default()
            })
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn persistent_memo_delete_appends_repository_scoped_operation() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let repo = store
            .create_repository(
                "Synced".to_string(),
                false,
                "#c86f52".to_string(),
                "device-a",
            )
            .await
            .unwrap();
        let memo = store
            .save_memo(
                SaveMemoInput {
                    id: None,
                    repository_id: repo.id,
                    title: "Keep scoped".to_string(),
                    body_md: "body".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();

        store.delete_memo(memo.id, "device-a").await.unwrap();
        let pending = store.pending_operations(PUSH_BATCH_LIMIT).await.unwrap();
        assert!(pending.iter().any(|op| {
            matches!(
                &op.kind,
                SyncOperationKind::DeleteMemo {
                    repository_id,
                    memo_id
                } if *repository_id == repo.id && *memo_id == memo.id
            )
        }));
    }
}
