use anyhow::Context;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use memo_core::{
    attachment_content_sha256, default_sync_protocol_version, sha256_hex, AttachmentBlobDescriptor,
    AttachmentBlobFetchRequest, AttachmentBlobFetchResponse, AttachmentBlobManifestRequest,
    AttachmentBlobManifestResponse, AttachmentBlobPayload, AttachmentBlobRelayRequest,
    AttachmentBlobRelayResponse, ClientInfo, HybridLogicalClock, Memo, MemoAttachment,
    MemoAttachmentMeta, MemoFilter, MemoMeta, MemoSource, PullRequest, PullResponse, PushRequest,
    Repository, RepositoryKind, ServerOperation, SnapshotResponse, SyncOperation,
    SyncOperationKind, DEFAULT_PULL_LIMIT, SYNC_PROTOCOL_VERSION,
};
use serde::Serialize as SerdeSerialize;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    QueryBuilder, Row, Sqlite, SqlitePool, Transaction,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};
use uuid::Uuid;

const PUSH_BATCH_LIMIT: i64 = 500;
const ATTACHMENT_BLOB_FETCH_LIMIT: usize = 128;
const ATTACHMENT_BLOB_RELAY_LIMIT: usize = 32;
const ATTACHMENT_BLOB_RELAY_JSON_BUDGET_BYTES: usize = 24 * 1024 * 1024;
const DEFAULT_ATTACHMENT_BLOB_RELAY_TTL_SECS: u64 = 10 * 60;
const SYNC_HTTP_RETRIES: usize = 2;
const DEFAULTS_SEEDED_KEY: &str = "defaults_seeded_version";
const DEFAULTS_SEEDED_VERSION: &str = "1";
const MAX_ATTACHMENT_BYTES: usize = 16 * 1024 * 1024;
const ALLOWED_IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

#[derive(Clone)]
pub struct LocalStore {
    pool: SqlitePool,
    sync_client: reqwest::Client,
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

#[derive(Debug, Clone, Serialize)]
pub struct LocalStats {
    pub memo_count: i64,
    pub repository_count: i64,
    pub attachment_count: i64,
    pub attachment_blob_count: i64,
    pub attachment_blob_bytes: i64,
    pub missing_attachment_blobs: i64,
    pub attachment_metadata_mismatches: i64,
    pub pending_operations: i64,
    pub last_server_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveAttachmentInput {
    pub memo_id: Uuid,
    pub file_name: String,
    pub media_type: String,
    pub data_base64: String,
}

#[derive(Debug, Deserialize)]
pub struct WaitForChangeResponse {
    pub changed: bool,
    pub server_sequence: i64,
    #[serde(default = "default_sync_protocol_version")]
    pub protocol_version: u16,
}

#[derive(Debug, Deserialize)]
struct SyncServerErrorResponse {
    code: String,
    message: String,
}

#[derive(Debug, Clone)]
struct EntityVersion {
    device_id: String,
    hlc: HybridLogicalClock,
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
        let store = Self {
            pool,
            sync_client: build_sync_client()?,
        };
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
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, repository_id, title, body_md, tags, pinned, archived, deleted, created_at, updated_at, source, meta FROM memos WHERE deleted = 0",
        );
        if let Some(repository_id) = filter.repository_id {
            query.push(" AND repository_id = ");
            query.push_bind(repository_id.to_string());
        }
        if let Some(pinned) = filter.pinned {
            query.push(" AND pinned = ");
            query.push_bind(pinned);
        }
        if let Some(archived) = filter.archived {
            query.push(" AND archived = ");
            query.push_bind(archived);
        }
        if let Some(source) = &filter.source {
            query.push(" AND source = ");
            query.push_bind(source_to_str(source));
        }
        if let Some(input) = filter
            .query
            .as_ref()
            .map(|query| query.trim())
            .filter(|query| !query.is_empty())
        {
            let pattern = format!("%{}%", input.to_lowercase());
            query.push(" AND (lower(title) LIKE ");
            query.push_bind(pattern.clone());
            query.push(" OR lower(body_md) LIKE ");
            query.push_bind(pattern.clone());
            query.push(" OR lower(tags) LIKE ");
            query.push_bind(pattern.clone());
            query.push(" OR lower(meta) LIKE ");
            query.push_bind(pattern);
            query.push(")");
        }
        query.push(" ORDER BY pinned DESC, updated_at DESC");

        let rows = query.build().fetch_all(&self.pool).await?;

        let mut memos = rows
            .into_iter()
            .map(memo_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;
        memos.retain(|memo| filter.matches(memo));
        Ok(memos)
    }

    pub async fn attachments(&self) -> anyhow::Result<Vec<MemoAttachmentMeta>> {
        let rows = sqlx::query(
            "SELECT id, memo_id, repository_id, file_name, media_type, byte_len, content_sha256, deleted, created_at, updated_at FROM attachments WHERE deleted = 0 ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(attachment_meta_from_row).collect()
    }

    pub async fn attachment_bytes(&self, id: Uuid) -> anyhow::Result<Option<(String, Vec<u8>)>> {
        let row = sqlx::query(
            r#"
            SELECT attachments.media_type, attachments.content_sha256,
                   COALESCE(attachment_blobs.data_base64, attachments.data_base64) AS data_base64
            FROM attachments
            LEFT JOIN attachment_blobs ON attachment_blobs.content_sha256 = attachments.content_sha256
            WHERE attachments.id = ?1 AND attachments.deleted = 0
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let media_type = row.get::<String, _>("media_type");
            let data_base64 = row.get::<String, _>("data_base64");
            let expected_hash = row.get::<String, _>("content_sha256");
            let bytes = BASE64_STANDARD.decode(data_base64.as_bytes())?;
            anyhow::ensure!(
                expected_hash.is_empty() || sha256_hex(&bytes) == expected_hash,
                "attachment content hash mismatch"
            );
            Ok((media_type, bytes))
        })
        .transpose()
    }

    pub async fn stats(&self) -> anyhow::Result<LocalStats> {
        let memo_count = scalar_i64(
            &self.pool,
            "SELECT COUNT(*) AS value FROM memos WHERE deleted = 0",
        )
        .await?;
        let repository_count =
            scalar_i64(&self.pool, "SELECT COUNT(*) AS value FROM repositories").await?;
        let attachment_count = scalar_i64(
            &self.pool,
            "SELECT COUNT(*) AS value FROM attachments WHERE deleted = 0",
        )
        .await?;
        let attachment_blob_count =
            scalar_i64(&self.pool, "SELECT COUNT(*) AS value FROM attachment_blobs").await?;
        let attachment_blob_bytes = scalar_i64(
            &self.pool,
            "SELECT COALESCE(SUM(byte_len), 0) AS value FROM attachment_blobs",
        )
        .await?;
        let missing_attachment_blobs = scalar_i64(
            &self.pool,
            r#"
            SELECT COUNT(*) AS value
            FROM attachments
            LEFT JOIN attachment_blobs ON attachment_blobs.content_sha256 = attachments.content_sha256
            WHERE attachments.deleted = 0
              AND attachments.data_base64 = ''
              AND attachment_blobs.content_sha256 IS NULL
            "#,
        )
        .await?;
        let attachment_metadata_mismatches = scalar_i64(
            &self.pool,
            r#"
            SELECT COUNT(*) AS value
            FROM attachments
            JOIN attachment_blobs ON attachment_blobs.content_sha256 = attachments.content_sha256
            WHERE attachments.deleted = 0
              AND (
                attachment_blobs.byte_len != attachments.byte_len
                OR attachment_blobs.media_type != attachments.media_type
              )
            "#,
        )
        .await?;
        let pending_operations = scalar_i64(
            &self.pool,
            "SELECT COUNT(*) AS value FROM local_operations WHERE server_sequence IS NULL",
        )
        .await?;
        Ok(LocalStats {
            memo_count,
            repository_count,
            attachment_count,
            attachment_blob_count,
            attachment_blob_bytes,
            missing_attachment_blobs,
            attachment_metadata_mismatches,
            pending_operations,
            last_server_sequence: self.last_server_sequence().await?,
        })
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
        let repo = Repository::new(cleaned_repository_name(name), kind, cleaned_color(color));
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

    pub async fn update_repository(
        &self,
        id: Uuid,
        name: String,
        color: String,
        sync_enabled: bool,
        device_id: &str,
    ) -> anyhow::Result<Repository> {
        let mut repo = self
            .repository_by_id(id)
            .await?
            .context("repository not found")?;
        repo.name = cleaned_repository_name(name);
        repo.color = cleaned_color(color);
        repo.sync_enabled = matches!(repo.kind, RepositoryKind::Persistent) && sync_enabled;
        repo.updated_at = Utc::now();
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
        let mut memo = match input.id {
            Some(id) => match self.memo_by_id(id).await? {
                Some(memo) => memo,
                None => {
                    let mut memo = Memo::new(input.repository_id, "", "");
                    memo.id = id;
                    memo
                }
            },
            None => Memo::new(input.repository_id, "", ""),
        };

        memo.repository_id = input.repository_id;
        memo.title = if input.title.trim().is_empty() {
            title_from_body(&input.body_md)
        } else {
            cleaned_memo_title(input.title)
        };
        memo.body_md = input.body_md;
        memo.tags = cleaned_tags(input.tags);
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

    pub async fn save_attachment(
        &self,
        input: SaveAttachmentInput,
        device_id: &str,
    ) -> anyhow::Result<MemoAttachmentMeta> {
        let memo = self
            .memo_by_id(input.memo_id)
            .await?
            .context("memo not found")?;
        let file_name = cleaned_file_name(input.file_name);
        let media_type = cleaned_media_type(input.media_type)?;
        let bytes = BASE64_STANDARD.decode(input.data_base64.as_bytes())?;
        anyhow::ensure!(!bytes.is_empty(), "attachment is empty");
        anyhow::ensure!(
            bytes.len() <= MAX_ATTACHMENT_BYTES,
            "attachment accepts at most {} bytes",
            MAX_ATTACHMENT_BYTES
        );
        let attachment = MemoAttachment::new(
            memo.id,
            memo.repository_id,
            file_name,
            media_type,
            bytes.len(),
            BASE64_STANDARD.encode(&bytes),
        );
        self.upsert_attachment(&attachment).await?;
        if self
            .repository_sync_enabled(attachment.repository_id)
            .await?
        {
            let op = SyncOperation::new(
                device_id,
                HybridLogicalClock::now(),
                SyncOperationKind::UpsertAttachment(attachment.clone()),
            );
            self.append_local_operation(&op).await?;
        }
        Ok(MemoAttachmentMeta::from(&attachment))
    }

    pub async fn delete_attachment(&self, id: Uuid, device_id: &str) -> anyhow::Result<()> {
        let existing = self.attachment_by_id(id).await?;
        sqlx::query("UPDATE attachments SET deleted = 1, updated_at = ?2 WHERE id = ?1")
            .bind(id.to_string())
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await?;
        if let Some(attachment) = existing {
            if self
                .repository_sync_enabled(attachment.repository_id)
                .await?
            {
                let op = SyncOperation::new(
                    device_id,
                    HybridLogicalClock::now(),
                    SyncOperationKind::DeleteAttachment {
                        repository_id: attachment.repository_id,
                        attachment_id: id,
                    },
                );
                self.append_local_operation(&op).await?;
            }
        }
        self.cleanup_attachment_blobs().await?;
        Ok(())
    }

    pub async fn wait_for_remote_change(
        &self,
        server_url: &str,
        since_sequence: i64,
        timeout: Duration,
    ) -> anyhow::Result<WaitForChangeResponse> {
        let timeout_ms = timeout.as_millis().clamp(1_000, 60_000);
        let base_url = sync_base_url(server_url);
        let response = self
            .sync_client
            .get(format!(
                "{}/api/v1/sync/wait?protocol_version={}&since_sequence={}&timeout_ms={}",
                base_url, SYNC_PROTOCOL_VERSION, since_sequence, timeout_ms
            ))
            .timeout(timeout + Duration::from_secs(8))
            .send()
            .await?;
        let response: WaitForChangeResponse = decode_sync_response(response).await?;
        anyhow::ensure!(
            response.protocol_version == SYNC_PROTOCOL_VERSION,
            "sync protocol mismatch: client {}, server {}",
            SYNC_PROTOCOL_VERSION,
            response.protocol_version
        );
        Ok(response)
    }

    pub async fn sync_now(&self, server_url: &str, device_id: &str) -> anyhow::Result<SyncSummary> {
        let mut pushed = 0usize;
        let mut server_sequence = self.last_server_sequence().await?;
        let base_url = sync_base_url(server_url);

        loop {
            let pending = self.pending_operations(PUSH_BATCH_LIMIT).await?;
            if pending.is_empty() {
                break;
            }
            let relay_available = self
                .relay_pending_attachment_blobs(&base_url, device_id, &pending)
                .await?;
            let push_operations = if relay_available {
                metadata_only_operations(&pending)
            } else {
                pending.clone()
            };
            let response = self
                .sync_client
                .post(format!("{base_url}/api/v1/sync/push"))
                .timeout(Duration::from_secs(20))
                .json(&PushRequest {
                    protocol_version: SYNC_PROTOCOL_VERSION,
                    device_id: device_id.to_string(),
                    client: Some(desktop_client_info()),
                    operations: push_operations,
                })
                .send()
                .await?;
            let push_response: memo_core::PushResponse = decode_sync_response(response).await?;
            pushed += push_response.accepted;
            server_sequence = server_sequence.max(push_response.server_sequence);
            self.mark_operations_pushed(&pending, push_response.server_sequence)
                .await?;
        }

        let mut pulled = 0usize;
        if self.last_server_sequence().await? == 0 {
            let snapshot = self.fetch_snapshot(&base_url).await?;
            server_sequence = server_sequence.max(snapshot.server_sequence);
            pulled += self.apply_snapshot(snapshot).await?;
        }

        loop {
            let since_sequence = self.last_server_sequence().await?;
            let response = self
                .sync_client
                .post(format!("{base_url}/api/v1/sync/pull"))
                .timeout(Duration::from_secs(20))
                .json(&PullRequest {
                    protocol_version: SYNC_PROTOCOL_VERSION,
                    since_sequence,
                    repository_ids: vec![],
                    exclude_device_id: Some(device_id.to_string()),
                    limit: DEFAULT_PULL_LIMIT,
                    client: Some(desktop_client_info()),
                })
                .send()
                .await?;
            let pull: PullResponse = decode_sync_response(response).await?;

            if pull.snapshot_required {
                let snapshot = self.fetch_snapshot(&base_url).await?;
                server_sequence = server_sequence.max(snapshot.server_sequence);
                pulled += self.apply_snapshot(snapshot).await?;
                continue;
            }
            pulled += pull.operations.len();
            server_sequence = server_sequence.max(pull.server_sequence);
            if let Some(applied_sequence) = self.apply_remote_operations(pull.operations).await? {
                server_sequence = server_sequence.max(applied_sequence);
            } else {
                self.set_last_server_sequence(server_sequence).await?;
            }
            if !pull.has_more {
                break;
            }
        }

        self.hydrate_missing_attachment_blobs(&base_url).await?;
        self.maintain().await?;
        Ok(SyncSummary {
            pushed,
            pulled,
            server_sequence,
        })
    }

    async fn fetch_snapshot(&self, base_url: &str) -> anyhow::Result<SnapshotResponse> {
        let response = self
            .sync_client
            .get(format!(
                "{base_url}/api/v1/sync/snapshot?protocol_version={SYNC_PROTOCOL_VERSION}"
            ))
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        decode_sync_response(response).await
    }

    async fn ensure_defaults(&self) -> anyhow::Result<()> {
        if !self.repositories().await?.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        let inbox = Repository::new("Inbox", RepositoryKind::Persistent, "#c86f52");
        let scratch = Repository::new("Scratch", RepositoryKind::Temporary, "#6f8f83");
        upsert_repository_tx(&mut tx, &inbox).await?;
        upsert_repository_tx(&mut tx, &scratch).await?;

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
        upsert_memo_tx(&mut tx, &welcome).await?;
        set_meta_tx(&mut tx, DEFAULTS_SEEDED_KEY, DEFAULTS_SEEDED_VERSION).await?;
        tx.commit().await?;
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

    async fn attachment_by_id(&self, id: Uuid) -> anyhow::Result<Option<MemoAttachment>> {
        let row = sqlx::query(
            r#"
            SELECT attachments.id, attachments.memo_id, attachments.repository_id, attachments.file_name,
                   attachments.media_type, attachments.byte_len, attachments.content_sha256,
                   COALESCE(attachment_blobs.data_base64, attachments.data_base64) AS data_base64,
                   attachments.deleted, attachments.created_at, attachments.updated_at
            FROM attachments
            LEFT JOIN attachment_blobs ON attachment_blobs.content_sha256 = attachments.content_sha256
            WHERE attachments.id = ?1
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(attachment_from_row).transpose()
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

    async fn repository_by_id(&self, id: Uuid) -> anyhow::Result<Option<Repository>> {
        let row = sqlx::query(
            "SELECT id, name, kind, sync_enabled, color, created_at, updated_at FROM repositories WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(repository_from_row).transpose()
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

    async fn upsert_attachment(&self, attachment: &MemoAttachment) -> anyhow::Result<()> {
        upsert_attachment_blob(&self.pool, attachment).await?;
        sqlx::query(
            r#"
            INSERT INTO attachments (id, memo_id, repository_id, file_name, media_type, byte_len, content_sha256, data_base64, deleted, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(id) DO UPDATE SET
              memo_id = excluded.memo_id,
              repository_id = excluded.repository_id,
              file_name = excluded.file_name,
              media_type = excluded.media_type,
              byte_len = excluded.byte_len,
              content_sha256 = excluded.content_sha256,
              data_base64 = excluded.data_base64,
              deleted = excluded.deleted,
              updated_at = excluded.updated_at
            "#,
        )
        .bind(attachment.id.to_string())
        .bind(attachment.memo_id.to_string())
        .bind(attachment.repository_id.to_string())
        .bind(&attachment.file_name)
        .bind(&attachment.media_type)
        .bind(i64::try_from(attachment.byte_len)?)
        .bind(&attachment.content_sha256)
        .bind("")
        .bind(attachment.deleted)
        .bind(attachment.created_at.to_rfc3339())
        .bind(attachment.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn append_local_operation(&self, op: &SyncOperation) -> anyhow::Result<()> {
        let (entity_kind, entity_id) = operation_entity(op);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO local_operations (op_id, payload, entity_kind, entity_id, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(entity_kind, entity_id)
            WHERE server_sequence IS NULL AND entity_kind IS NOT NULL AND entity_id IS NOT NULL
            DO UPDATE SET
              op_id = excluded.op_id,
              payload = excluded.payload
            "#,
        )
        .bind(op.op_id.to_string())
        .bind(serde_json::to_string(op)?)
        .bind(entity_kind)
        .bind(entity_id.to_string())
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
        set_entity_version_tx(&mut tx, entity_kind, entity_id, &op.device_id, op.hlc).await?;
        tx.commit().await?;
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
        if operations.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;
        let mut query =
            QueryBuilder::<Sqlite>::new("UPDATE local_operations SET server_sequence = ");
        query.push_bind(sequence).push(" WHERE op_id IN (");
        let mut separated = query.separated(", ");
        for op in operations {
            separated.push_bind(op.op_id.to_string());
        }
        separated.push_unseparated(")");
        query.build().execute(&mut *tx).await?;

        sqlx::query("DELETE FROM local_operations WHERE server_sequence IS NOT NULL")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn apply_remote_operations(
        &self,
        items: Vec<ServerOperation>,
    ) -> anyhow::Result<Option<i64>> {
        if items.is_empty() {
            return Ok(None);
        }

        let mut tx = self.pool.begin().await?;
        let mut last_sequence = 0;
        for item in items {
            last_sequence = item.sequence;
            apply_remote_operation_tx(&mut tx, item.operation).await?;
        }
        set_last_server_sequence_tx(&mut tx, last_sequence).await?;
        tx.commit().await?;
        self.cleanup_attachment_blobs().await?;
        Ok(Some(last_sequence))
    }

    async fn apply_snapshot(&self, snapshot: SnapshotResponse) -> anyhow::Result<usize> {
        anyhow::ensure!(
            snapshot.protocol_version == SYNC_PROTOCOL_VERSION,
            "sync protocol mismatch: client {}, server {}",
            SYNC_PROTOCOL_VERSION,
            snapshot.protocol_version
        );
        let mut applied = 0usize;
        let snapshot_has_remote_data = snapshot.server_sequence > 0
            && (!snapshot.repositories.is_empty()
                || !snapshot.memos.is_empty()
                || !snapshot.attachments.is_empty());
        let mut tx = self.pool.begin().await?;
        discard_seed_defaults_if_safe_tx(&mut tx, snapshot_has_remote_data).await?;

        for item in snapshot.repositories {
            let entity_id = item.repository.id;
            if operation_wins_tx(&mut tx, "repository", entity_id, &item.device_id, item.hlc)
                .await?
            {
                upsert_repository_tx(&mut tx, &item.repository).await?;
                set_entity_version_tx(&mut tx, "repository", entity_id, &item.device_id, item.hlc)
                    .await?;
                applied += 1;
            }
        }

        for item in snapshot.memos {
            let entity_id = item.memo.id;
            if operation_wins_tx(&mut tx, "memo", entity_id, &item.device_id, item.hlc).await? {
                upsert_memo_tx(&mut tx, &item.memo).await?;
                set_entity_version_tx(&mut tx, "memo", entity_id, &item.device_id, item.hlc)
                    .await?;
                applied += 1;
            }
        }

        for item in snapshot.attachments {
            let entity_id = item.attachment.id;
            if operation_wins_tx(&mut tx, "attachment", entity_id, &item.device_id, item.hlc)
                .await?
            {
                upsert_attachment_tx(&mut tx, &item.attachment).await?;
                set_entity_version_tx(&mut tx, "attachment", entity_id, &item.device_id, item.hlc)
                    .await?;
                applied += 1;
            }
        }

        set_last_server_sequence_tx(&mut tx, snapshot.server_sequence).await?;
        tx.commit().await?;
        self.cleanup_attachment_blobs().await?;
        Ok(applied)
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

    async fn maintain(&self) -> anyhow::Result<()> {
        self.cleanup_attachment_blobs().await?;
        sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
            .execute(&self.pool)
            .await?;
        sqlx::query("PRAGMA optimize").execute(&self.pool).await?;
        Ok(())
    }

    async fn cleanup_attachment_blobs(&self) -> anyhow::Result<u64> {
        let protected_hashes = self.pending_attachment_blob_hashes().await?;
        let mut query = QueryBuilder::<Sqlite>::new(
            r#"
            DELETE FROM attachment_blobs
            WHERE NOT EXISTS (
              SELECT 1
              FROM attachments
              WHERE attachments.content_sha256 = attachment_blobs.content_sha256
                AND attachments.deleted = 0
            )
            "#,
        );
        if !protected_hashes.is_empty() {
            query.push(" AND content_sha256 NOT IN (");
            let mut separated = query.separated(", ");
            for hash in protected_hashes {
                separated.push_bind(hash);
            }
            separated.push_unseparated(")");
        }
        Ok(query.build().execute(&self.pool).await?.rows_affected())
    }

    async fn relay_pending_attachment_blobs(
        &self,
        base_url: &str,
        device_id: &str,
        operations: &[SyncOperation],
    ) -> anyhow::Result<bool> {
        let payloads = self.pending_attachment_blob_payloads(operations).await?;
        if payloads.is_empty() {
            return Ok(true);
        }

        for chunk in attachment_blob_relay_chunks(payloads) {
            let expected = chunk.len();
            let hashes = chunk
                .iter()
                .map(|blob| blob.descriptor.content_sha256.clone())
                .collect::<Vec<_>>();
            let response = self
                .post_sync_json(
                    format!("{base_url}/api/v1/sync/attachment-blobs/relay"),
                    &AttachmentBlobRelayRequest {
                        protocol_version: SYNC_PROTOCOL_VERSION,
                        device_id: device_id.to_string(),
                        blobs: chunk,
                        ttl_secs: Some(DEFAULT_ATTACHMENT_BLOB_RELAY_TTL_SECS),
                    },
                    Duration::from_secs(60),
                )
                .await?;
            if matches!(
                response.status(),
                reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
            ) {
                return Ok(false);
            }
            let relay: AttachmentBlobRelayResponse = decode_sync_response(response).await?;
            anyhow::ensure!(
                relay.protocol_version == SYNC_PROTOCOL_VERSION,
                "sync protocol mismatch: client {}, server {}",
                SYNC_PROTOCOL_VERSION,
                relay.protocol_version
            );
            anyhow::ensure!(
                relay.accepted == expected,
                "sync blob relay accepted {} of {} blobs",
                relay.accepted,
                expected
            );

            let response = self
                .post_sync_json(
                    format!("{base_url}/api/v1/sync/attachment-blobs/manifest"),
                    &AttachmentBlobManifestRequest {
                        protocol_version: SYNC_PROTOCOL_VERSION,
                        content_sha256: hashes.clone(),
                    },
                    Duration::from_secs(20),
                )
                .await?;
            let manifest: AttachmentBlobManifestResponse = decode_sync_response(response).await?;
            anyhow::ensure!(
                manifest.protocol_version == SYNC_PROTOCOL_VERSION,
                "sync protocol mismatch: client {}, server {}",
                SYNC_PROTOCOL_VERSION,
                manifest.protocol_version
            );
            anyhow::ensure!(
                manifest.missing.is_empty() && manifest.present.len() == expected,
                "sync blob relay confirmation missed {} of {} blobs",
                manifest.missing.len(),
                expected
            );
        }

        Ok(true)
    }

    async fn post_sync_json<T: SerdeSerialize + ?Sized>(
        &self,
        url: String,
        body: &T,
        timeout: Duration,
    ) -> anyhow::Result<reqwest::Response> {
        let mut last_error = None;
        for attempt in 0..=SYNC_HTTP_RETRIES {
            match self
                .sync_client
                .post(&url)
                .timeout(timeout)
                .json(body)
                .send()
                .await
            {
                Ok(response)
                    if is_retryable_status(response.status()) && attempt < SYNC_HTTP_RETRIES =>
                {
                    tokio::time::sleep(retry_delay(attempt)).await;
                    continue;
                }
                Ok(response) => return Ok(response),
                Err(error) if attempt < SYNC_HTTP_RETRIES => {
                    last_error = Some(error);
                    tokio::time::sleep(retry_delay(attempt)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(last_error
            .map(anyhow::Error::from)
            .unwrap_or_else(|| anyhow::anyhow!("sync request failed")))
    }

    async fn pending_attachment_blob_payloads(
        &self,
        operations: &[SyncOperation],
    ) -> anyhow::Result<Vec<AttachmentBlobPayload>> {
        let mut payloads = BTreeMap::<String, AttachmentBlobPayload>::new();
        for operation in operations {
            let SyncOperationKind::UpsertAttachment(attachment) = &operation.kind else {
                continue;
            };
            if attachment.content_sha256.is_empty()
                || payloads.contains_key(&attachment.content_sha256)
            {
                continue;
            }
            let data_base64 = if attachment.data_base64.is_empty() {
                self.attachment_blob_data_base64(&attachment.content_sha256)
                    .await?
                    .with_context(|| {
                        format!(
                            "attachment blob {} is missing from local cache",
                            attachment.content_sha256
                        )
                    })?
            } else {
                attachment.data_base64.clone()
            };
            payloads.insert(
                attachment.content_sha256.clone(),
                AttachmentBlobPayload {
                    descriptor: AttachmentBlobDescriptor {
                        content_sha256: attachment.content_sha256.clone(),
                        media_type: attachment.media_type.clone(),
                        byte_len: attachment.byte_len,
                    },
                    data_base64,
                },
            );
        }
        Ok(payloads.into_values().collect())
    }

    async fn attachment_blob_data_base64(
        &self,
        content_sha256: &str,
    ) -> anyhow::Result<Option<String>> {
        let row = sqlx::query(
            r#"
            SELECT data_base64
            FROM attachment_blobs
            WHERE content_sha256 = ?1
            UNION ALL
            SELECT data_base64
            FROM attachments
            WHERE content_sha256 = ?1
              AND data_base64 != ''
            LIMIT 1
            "#,
        )
        .bind(content_sha256)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get("data_base64")))
    }

    async fn hydrate_missing_attachment_blobs(&self, base_url: &str) -> anyhow::Result<usize> {
        let missing = self.missing_attachment_blob_hashes().await?;
        let mut hydrated = 0usize;
        for chunk in missing.chunks(ATTACHMENT_BLOB_FETCH_LIMIT) {
            let response = self
                .post_sync_json(
                    format!("{base_url}/api/v1/sync/attachment-blobs/fetch"),
                    &AttachmentBlobFetchRequest {
                        protocol_version: SYNC_PROTOCOL_VERSION,
                        content_sha256: chunk.to_vec(),
                    },
                    Duration::from_secs(30),
                )
                .await?;
            if matches!(
                response.status(),
                reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
            ) {
                return Ok(hydrated);
            }
            let fetched: AttachmentBlobFetchResponse = decode_sync_response(response).await?;
            anyhow::ensure!(
                fetched.protocol_version == SYNC_PROTOCOL_VERSION,
                "sync protocol mismatch: client {}, server {}",
                SYNC_PROTOCOL_VERSION,
                fetched.protocol_version
            );
            hydrated += store_attachment_blob_payloads(&self.pool, fetched.blobs).await?;
            if fetched.missing.len() == chunk.len() {
                break;
            }
        }
        Ok(hydrated)
    }

    async fn missing_attachment_blob_hashes(&self) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT attachments.content_sha256
            FROM attachments
            LEFT JOIN attachment_blobs ON attachment_blobs.content_sha256 = attachments.content_sha256
            WHERE attachments.deleted = 0
              AND attachments.content_sha256 != ''
              AND attachment_blobs.content_sha256 IS NULL
            ORDER BY attachments.content_sha256
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| row.get::<String, _>("content_sha256"))
            .collect())
    }

    async fn pending_attachment_blob_hashes(&self) -> anyhow::Result<Vec<String>> {
        let pending = self.pending_operations(PUSH_BATCH_LIMIT).await?;
        let mut hashes = BTreeSet::new();
        for op in pending {
            if let SyncOperationKind::UpsertAttachment(attachment) = op.kind {
                hashes.insert(attachment.content_sha256);
            }
        }
        Ok(hashes.into_iter().collect())
    }
}

async fn scalar_i64(pool: &SqlitePool, sql: &str) -> anyhow::Result<i64> {
    let row = sqlx::query(sql).fetch_one(pool).await?;
    Ok(row.get("value"))
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(150 * u64::try_from(attempt + 1).unwrap_or(1))
}

fn metadata_only_operations(operations: &[SyncOperation]) -> Vec<SyncOperation> {
    operations
        .iter()
        .cloned()
        .map(metadata_only_operation)
        .collect()
}

fn metadata_only_operation(mut operation: SyncOperation) -> SyncOperation {
    if let SyncOperationKind::UpsertAttachment(attachment) = &mut operation.kind {
        attachment.data_base64.clear();
    }
    operation
}

fn attachment_blob_relay_chunks(
    payloads: Vec<AttachmentBlobPayload>,
) -> Vec<Vec<AttachmentBlobPayload>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut current_bytes = 0usize;

    for payload in payloads {
        let payload_bytes = payload.data_base64.len()
            + payload.descriptor.content_sha256.len()
            + payload.descriptor.media_type.len()
            + 128;
        let should_flush = !current.is_empty()
            && (current.len() >= ATTACHMENT_BLOB_RELAY_LIMIT
                || current_bytes + payload_bytes > ATTACHMENT_BLOB_RELAY_JSON_BUDGET_BYTES);
        if should_flush {
            chunks.push(current);
            current = Vec::new();
            current_bytes = 0;
        }
        current_bytes += payload_bytes;
        current.push(payload);
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

async fn apply_remote_operation_tx(
    tx: &mut Transaction<'_, Sqlite>,
    op: SyncOperation,
) -> anyhow::Result<()> {
    let (entity_kind, entity_id) = operation_entity(&op);
    if !operation_wins_tx(tx, entity_kind, entity_id, &op.device_id, op.hlc).await? {
        return Ok(());
    }

    let device_id = op.device_id.clone();
    let hlc = op.hlc;
    match op.kind {
        SyncOperationKind::UpsertRepository(repo) => upsert_repository_tx(tx, &repo).await?,
        SyncOperationKind::UpsertMemo(memo) => upsert_memo_tx(tx, &memo).await?,
        SyncOperationKind::UpsertAttachment(attachment) => {
            upsert_attachment_tx(tx, &attachment).await?
        }
        SyncOperationKind::DeleteMemo { memo_id, .. } => {
            sqlx::query("UPDATE memos SET deleted = 1, updated_at = ?2 WHERE id = ?1")
                .bind(memo_id.to_string())
                .bind(Utc::now().to_rfc3339())
                .execute(&mut **tx)
                .await?;
        }
        SyncOperationKind::PatchMemo { memo_id, patch, .. } => {
            if let Some(mut memo) = memo_by_id_tx(tx, memo_id).await? {
                memo.apply_patch(patch);
                upsert_memo_tx(tx, &memo).await?;
            }
        }
        SyncOperationKind::DeleteAttachment { attachment_id, .. } => {
            sqlx::query("UPDATE attachments SET deleted = 1, updated_at = ?2 WHERE id = ?1")
                .bind(attachment_id.to_string())
                .bind(Utc::now().to_rfc3339())
                .execute(&mut **tx)
                .await?;
        }
    }
    set_entity_version_tx(tx, entity_kind, entity_id, &device_id, hlc).await?;
    Ok(())
}

async fn operation_wins_tx(
    tx: &mut Transaction<'_, Sqlite>,
    entity_kind: &str,
    entity_id: Uuid,
    device_id: &str,
    hlc: HybridLogicalClock,
) -> anyhow::Result<bool> {
    let existing = entity_version_tx(tx, entity_kind, entity_id).await?;
    Ok(match existing {
        Some(existing) => {
            hlc > existing.hlc || (hlc == existing.hlc && device_id > existing.device_id.as_str())
        }
        None => true,
    })
}

async fn entity_version_tx(
    tx: &mut Transaction<'_, Sqlite>,
    entity_kind: &str,
    entity_id: Uuid,
) -> anyhow::Result<Option<EntityVersion>> {
    let table = match entity_kind {
        "repository" => "repositories",
        "memo" => "memos",
        "attachment" => "attachments",
        _ => return Ok(None),
    };
    let row = sqlx::query(&format!(
        "SELECT sync_device_id, sync_hlc_wall_time_ms, sync_hlc_counter FROM {table} WHERE id = ?1"
    ))
    .bind(entity_id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.and_then(|row| {
        let device_id = row.get::<Option<String>, _>("sync_device_id")?;
        Some(EntityVersion {
            device_id,
            hlc: HybridLogicalClock {
                wall_time_ms: row
                    .get::<Option<i64>, _>("sync_hlc_wall_time_ms")
                    .unwrap_or(0),
                counter: row
                    .get::<Option<i64>, _>("sync_hlc_counter")
                    .and_then(|value| u16::try_from(value).ok())
                    .unwrap_or(0),
            },
        })
    }))
}

async fn set_entity_version_tx(
    tx: &mut Transaction<'_, Sqlite>,
    entity_kind: &str,
    entity_id: Uuid,
    device_id: &str,
    hlc: HybridLogicalClock,
) -> anyhow::Result<()> {
    let table = match entity_kind {
        "repository" => "repositories",
        "memo" => "memos",
        "attachment" => "attachments",
        _ => return Ok(()),
    };
    sqlx::query(&format!(
        "UPDATE {table} SET sync_device_id = ?2, sync_hlc_wall_time_ms = ?3, sync_hlc_counter = ?4 WHERE id = ?1"
    ))
    .bind(entity_id.to_string())
    .bind(device_id)
    .bind(hlc.wall_time_ms)
    .bind(i64::from(hlc.counter))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_repository_tx(
    tx: &mut Transaction<'_, Sqlite>,
    repo: &Repository,
) -> anyhow::Result<()> {
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
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_memo_tx(tx: &mut Transaction<'_, Sqlite>, memo: &Memo) -> anyhow::Result<()> {
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
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_attachment_tx(
    tx: &mut Transaction<'_, Sqlite>,
    attachment: &MemoAttachment,
) -> anyhow::Result<()> {
    upsert_attachment_blob_tx(tx, attachment).await?;
    sqlx::query(
        r#"
        INSERT INTO attachments (id, memo_id, repository_id, file_name, media_type, byte_len, content_sha256, data_base64, deleted, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT(id) DO UPDATE SET
          memo_id = excluded.memo_id,
          repository_id = excluded.repository_id,
          file_name = excluded.file_name,
          media_type = excluded.media_type,
          byte_len = excluded.byte_len,
          content_sha256 = excluded.content_sha256,
          data_base64 = excluded.data_base64,
          deleted = excluded.deleted,
          updated_at = excluded.updated_at
        "#,
    )
    .bind(attachment.id.to_string())
    .bind(attachment.memo_id.to_string())
    .bind(attachment.repository_id.to_string())
    .bind(&attachment.file_name)
    .bind(&attachment.media_type)
    .bind(i64::try_from(attachment.byte_len)?)
    .bind(&attachment.content_sha256)
    .bind("")
    .bind(attachment.deleted)
    .bind(attachment.created_at.to_rfc3339())
    .bind(attachment.updated_at.to_rfc3339())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_attachment_blob(
    pool: &SqlitePool,
    attachment: &MemoAttachment,
) -> anyhow::Result<()> {
    if attachment.data_base64.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"
        INSERT INTO attachment_blobs (content_sha256, media_type, byte_len, data_base64, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(content_sha256) DO UPDATE SET
          media_type = excluded.media_type,
          byte_len = excluded.byte_len,
          data_base64 = excluded.data_base64
        "#,
    )
    .bind(&attachment.content_sha256)
    .bind(&attachment.media_type)
    .bind(i64::try_from(attachment.byte_len)?)
    .bind(&attachment.data_base64)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

async fn upsert_attachment_blob_tx(
    tx: &mut Transaction<'_, Sqlite>,
    attachment: &MemoAttachment,
) -> anyhow::Result<()> {
    if attachment.data_base64.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"
        INSERT INTO attachment_blobs (content_sha256, media_type, byte_len, data_base64, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(content_sha256) DO UPDATE SET
          media_type = excluded.media_type,
          byte_len = excluded.byte_len,
          data_base64 = excluded.data_base64
        "#,
    )
    .bind(&attachment.content_sha256)
    .bind(&attachment.media_type)
    .bind(i64::try_from(attachment.byte_len)?)
    .bind(&attachment.data_base64)
    .bind(Utc::now().to_rfc3339())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn store_attachment_blob_payloads(
    pool: &SqlitePool,
    blobs: Vec<AttachmentBlobPayload>,
) -> anyhow::Result<usize> {
    let mut stored = 0usize;
    let mut tx = pool.begin().await?;
    for blob in blobs {
        let content_sha256 = blob.descriptor.content_sha256.to_ascii_lowercase();
        let bytes = BASE64_STANDARD.decode(blob.data_base64.as_bytes())?;
        anyhow::ensure!(
            bytes.len() == blob.descriptor.byte_len,
            "attachment blob byte_len mismatch"
        );
        anyhow::ensure!(
            sha256_hex(&bytes) == content_sha256,
            "attachment blob content hash mismatch"
        );
        sqlx::query(
            r#"
            INSERT INTO attachment_blobs (content_sha256, media_type, byte_len, data_base64, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(content_sha256) DO UPDATE SET
              media_type = excluded.media_type,
              byte_len = excluded.byte_len,
              data_base64 = excluded.data_base64
            "#,
        )
        .bind(content_sha256)
        .bind(blob.descriptor.media_type)
        .bind(i64::try_from(blob.descriptor.byte_len)?)
        .bind(blob.data_base64)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
        stored += 1;
    }
    tx.commit().await?;
    Ok(stored)
}

async fn memo_by_id_tx(tx: &mut Transaction<'_, Sqlite>, id: Uuid) -> anyhow::Result<Option<Memo>> {
    let row = sqlx::query(
        "SELECT id, repository_id, title, body_md, tags, pinned, archived, deleted, created_at, updated_at, source, meta FROM memos WHERE id = ?1",
    )
    .bind(id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    row.map(memo_from_row).transpose()
}

async fn set_last_server_sequence_tx(
    tx: &mut Transaction<'_, Sqlite>,
    sequence: i64,
) -> anyhow::Result<()> {
    set_meta_tx(tx, "last_server_sequence", &sequence.to_string()).await
}

async fn set_meta_tx(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn meta_value_tx(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
) -> anyhow::Result<Option<String>> {
    Ok(sqlx::query("SELECT value FROM meta WHERE key = ?1")
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?
        .map(|row| row.get("value")))
}

async fn count_tx(tx: &mut Transaction<'_, Sqlite>, sql: &str) -> anyhow::Result<i64> {
    let row = sqlx::query(sql).fetch_one(&mut **tx).await?;
    Ok(row.get("value"))
}

async fn discard_seed_defaults_if_safe_tx(
    tx: &mut Transaction<'_, Sqlite>,
    snapshot_has_remote_data: bool,
) -> anyhow::Result<()> {
    if !snapshot_has_remote_data {
        return Ok(());
    }
    if meta_value_tx(tx, DEFAULTS_SEEDED_KEY).await?.as_deref() != Some(DEFAULTS_SEEDED_VERSION) {
        return Ok(());
    }
    let last_sequence = meta_value_tx(tx, "last_server_sequence")
        .await?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    if last_sequence != 0 {
        return Ok(());
    }
    let pending = count_tx(
        tx,
        "SELECT COUNT(*) AS value FROM local_operations WHERE server_sequence IS NULL",
    )
    .await?;
    let repositories = count_tx(tx, "SELECT COUNT(*) AS value FROM repositories").await?;
    let memos = count_tx(tx, "SELECT COUNT(*) AS value FROM memos").await?;
    let attachments = count_tx(tx, "SELECT COUNT(*) AS value FROM attachments").await?;
    if pending == 0 && repositories == 2 && memos == 1 && attachments == 0 {
        sqlx::query("DELETE FROM memos").execute(&mut **tx).await?;
        sqlx::query("DELETE FROM repositories")
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM meta WHERE key = ?1")
            .bind(DEFAULTS_SEEDED_KEY)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

fn sync_base_url(server_url: &str) -> String {
    server_url.trim_end_matches('/').to_string()
}

fn build_sync_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .tcp_keepalive(Duration::from_secs(60))
        .build()?)
}

async fn decode_sync_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> anyhow::Result<T> {
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        if let Ok(error) = serde_json::from_slice::<SyncServerErrorResponse>(&bytes) {
            anyhow::bail!(
                "sync server rejected request ({} {}): {}",
                status,
                error.code,
                error.message
            );
        }
        let body = String::from_utf8_lossy(&bytes);
        anyhow::bail!("sync server rejected request ({}): {}", status, body);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

async fn ensure_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    definition: &str,
) -> anyhow::Result<()> {
    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await?;
    let exists = rows
        .iter()
        .any(|row| row.get::<String, _>("name") == column);
    if !exists {
        sqlx::query(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))
        .execute(pool)
        .await?;
    }
    Ok(())
}

fn operation_entity(op: &SyncOperation) -> (&'static str, Uuid) {
    match &op.kind {
        SyncOperationKind::UpsertRepository(repo) => ("repository", repo.id),
        SyncOperationKind::UpsertMemo(memo) => ("memo", memo.id),
        SyncOperationKind::UpsertAttachment(attachment) => ("attachment", attachment.id),
        SyncOperationKind::PatchMemo { memo_id, .. }
        | SyncOperationKind::DeleteMemo { memo_id, .. } => ("memo", *memo_id),
        SyncOperationKind::DeleteAttachment { attachment_id, .. } => ("attachment", *attachment_id),
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
        "CREATE INDEX IF NOT EXISTS memos_deleted_updated_idx ON memos(deleted, updated_at)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS memos_repo_deleted_updated_idx ON memos(repository_id, deleted, updated_at)",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS memos_source_updated_idx ON memos(source, updated_at)")
        .execute(pool)
        .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS attachments (
          id TEXT PRIMARY KEY,
          memo_id TEXT NOT NULL,
          repository_id TEXT NOT NULL,
          file_name TEXT NOT NULL,
          media_type TEXT NOT NULL,
          byte_len INTEGER NOT NULL,
          content_sha256 TEXT NOT NULL DEFAULT '',
          data_base64 TEXT NOT NULL,
          deleted INTEGER NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          FOREIGN KEY(memo_id) REFERENCES memos(id),
          FOREIGN KEY(repository_id) REFERENCES repositories(id)
        );
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS attachments_memo_idx ON attachments(memo_id)")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS attachments_repo_updated_idx ON attachments(repository_id, updated_at)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS attachment_blobs (
          content_sha256 TEXT PRIMARY KEY,
          media_type TEXT NOT NULL,
          byte_len INTEGER NOT NULL,
          data_base64 TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS local_operations (
          op_id TEXT PRIMARY KEY,
          payload TEXT NOT NULL,
          entity_kind TEXT,
          entity_id TEXT,
          server_sequence INTEGER,
          created_at TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;
    ensure_column(pool, "local_operations", "entity_kind", "TEXT").await?;
    ensure_column(pool, "local_operations", "entity_id", "TEXT").await?;
    ensure_column(pool, "repositories", "sync_device_id", "TEXT").await?;
    ensure_column(
        pool,
        "repositories",
        "sync_hlc_wall_time_ms",
        "INTEGER DEFAULT 0",
    )
    .await?;
    ensure_column(
        pool,
        "repositories",
        "sync_hlc_counter",
        "INTEGER DEFAULT 0",
    )
    .await?;
    ensure_column(pool, "memos", "sync_device_id", "TEXT").await?;
    ensure_column(pool, "memos", "sync_hlc_wall_time_ms", "INTEGER DEFAULT 0").await?;
    ensure_column(pool, "memos", "sync_hlc_counter", "INTEGER DEFAULT 0").await?;
    ensure_column(pool, "attachments", "sync_device_id", "TEXT").await?;
    ensure_column(
        pool,
        "attachments",
        "content_sha256",
        "TEXT NOT NULL DEFAULT ''",
    )
    .await?;
    ensure_column(
        pool,
        "attachments",
        "sync_hlc_wall_time_ms",
        "INTEGER DEFAULT 0",
    )
    .await?;
    ensure_column(pool, "attachments", "sync_hlc_counter", "INTEGER DEFAULT 0").await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS attachments_content_idx ON attachments(content_sha256)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS local_operations_pending_idx ON local_operations(server_sequence, created_at)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS local_operations_pending_entity_idx
        ON local_operations(entity_kind, entity_id)
        WHERE server_sequence IS NULL AND entity_kind IS NOT NULL AND entity_id IS NOT NULL
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(pool)
        .await?;
    backfill_attachment_hashes(pool).await?;
    backfill_attachment_blobs(pool).await?;
    Ok(())
}

async fn backfill_attachment_hashes(pool: &SqlitePool) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT id, data_base64 FROM attachments WHERE content_sha256 = ''")
        .fetch_all(pool)
        .await?;
    for row in rows {
        let id = row.get::<String, _>("id");
        let data_base64 = row.get::<String, _>("data_base64");
        sqlx::query("UPDATE attachments SET content_sha256 = ?2 WHERE id = ?1")
            .bind(id)
            .bind(attachment_content_sha256(&data_base64))
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn backfill_attachment_blobs(pool: &SqlitePool) -> anyhow::Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT content_sha256, media_type, byte_len, data_base64, created_at
        FROM attachments
        WHERE data_base64 != ''
        "#,
    )
    .fetch_all(pool)
    .await?;
    for row in rows {
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO attachment_blobs (content_sha256, media_type, byte_len, data_base64, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(row.get::<String, _>("content_sha256"))
        .bind(row.get::<String, _>("media_type"))
        .bind(row.get::<i64, _>("byte_len"))
        .bind(row.get::<String, _>("data_base64"))
        .bind(row.get::<String, _>("created_at"))
        .execute(pool)
        .await?;
    }
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

fn attachment_meta_from_row(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<MemoAttachmentMeta> {
    Ok(MemoAttachmentMeta {
        id: Uuid::parse_str(&row.get::<String, _>("id"))?,
        memo_id: Uuid::parse_str(&row.get::<String, _>("memo_id"))?,
        repository_id: Uuid::parse_str(&row.get::<String, _>("repository_id"))?,
        file_name: row.get("file_name"),
        media_type: row.get("media_type"),
        byte_len: usize::try_from(row.get::<i64, _>("byte_len"))?,
        content_sha256: row.get::<String, _>("content_sha256"),
        deleted: row.get("deleted"),
        created_at: parse_dt(&row.get::<String, _>("created_at"))?,
        updated_at: parse_dt(&row.get::<String, _>("updated_at"))?,
    })
}

fn attachment_from_row(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<MemoAttachment> {
    let data_base64 = row.get::<String, _>("data_base64");
    let content_sha256 = row.get::<String, _>("content_sha256");
    Ok(MemoAttachment {
        id: Uuid::parse_str(&row.get::<String, _>("id"))?,
        memo_id: Uuid::parse_str(&row.get::<String, _>("memo_id"))?,
        repository_id: Uuid::parse_str(&row.get::<String, _>("repository_id"))?,
        file_name: row.get("file_name"),
        media_type: row.get("media_type"),
        byte_len: usize::try_from(row.get::<i64, _>("byte_len"))?,
        content_sha256: if content_sha256.is_empty() {
            attachment_content_sha256(&data_base64)
        } else {
            content_sha256
        },
        data_base64,
        deleted: row.get("deleted"),
        created_at: parse_dt(&row.get::<String, _>("created_at"))?,
        updated_at: parse_dt(&row.get::<String, _>("updated_at"))?,
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

fn cleaned_repository_name(name: String) -> String {
    let cleaned = name.trim();
    if cleaned.is_empty() {
        "Untitled repository".to_string()
    } else {
        cleaned.chars().take(80).collect()
    }
}

fn cleaned_memo_title(title: String) -> String {
    let cleaned = title.trim();
    if cleaned.is_empty() {
        "Untitled memo".to_string()
    } else {
        cleaned.chars().take(128).collect()
    }
}

fn cleaned_color(color: String) -> String {
    let color = color.trim();
    if color.len() == 7
        && color.starts_with('#')
        && color.chars().skip(1).all(|char| char.is_ascii_hexdigit())
    {
        color.to_ascii_lowercase()
    } else {
        "#c86f52".to_string()
    }
}

fn cleaned_tags(tags: BTreeSet<String>) -> BTreeSet<String> {
    tags.into_iter()
        .map(|tag| tag.trim().chars().take(64).collect::<String>())
        .filter(|tag| !tag.is_empty())
        .collect()
}

fn cleaned_file_name(name: String) -> String {
    let cleaned = name
        .trim()
        .chars()
        .map(|char| match char {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect::<String>();
    let cleaned = cleaned.trim_matches('.');
    if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned.chars().take(180).collect()
    }
}

fn cleaned_media_type(media_type: String) -> anyhow::Result<String> {
    let media_type = media_type.trim().to_ascii_lowercase();
    anyhow::ensure!(
        ALLOWED_IMAGE_TYPES.contains(&media_type.as_str()),
        "attachment media type must be png, jpeg, webp, or gif"
    );
    Ok(media_type)
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

    fn test_blob_payload(hash_suffix: usize) -> AttachmentBlobPayload {
        AttachmentBlobPayload {
            descriptor: AttachmentBlobDescriptor {
                content_sha256: format!("{hash_suffix:064x}"),
                media_type: "image/png".to_string(),
                byte_len: 4,
            },
            data_base64: BASE64_STANDARD.encode([1, 2, 3, 4]),
        }
    }

    #[test]
    fn relay_payloads_are_chunked_by_server_item_limit() {
        let payloads = (0..(ATTACHMENT_BLOB_RELAY_LIMIT + 1))
            .map(test_blob_payload)
            .collect::<Vec<_>>();
        let chunks = attachment_blob_relay_chunks(payloads);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), ATTACHMENT_BLOB_RELAY_LIMIT);
        assert_eq!(chunks[1].len(), 1);
    }

    #[test]
    fn attachment_metadata_pushes_do_not_include_blob_content() {
        let memo_id = Uuid::now_v7();
        let repository_id = Uuid::now_v7();
        let attachment = MemoAttachment::new(
            memo_id,
            repository_id,
            "figure.png",
            "image/png",
            4,
            BASE64_STANDARD.encode([1, 2, 3, 4]),
        );
        let operations = metadata_only_operations(&[SyncOperation::new(
            "device-a",
            HybridLogicalClock::now(),
            SyncOperationKind::UpsertAttachment(attachment),
        )]);

        let SyncOperationKind::UpsertAttachment(attachment) = &operations[0].kind else {
            panic!("operation should stay an attachment upsert");
        };
        assert!(attachment.data_base64.is_empty());
        assert!(!attachment.content_sha256.is_empty());
    }

    #[test]
    fn sync_retries_only_transient_http_statuses() {
        assert!(is_retryable_status(reqwest::StatusCode::REQUEST_TIMEOUT));
        assert!(is_retryable_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(!is_retryable_status(reqwest::StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(reqwest::StatusCode::NOT_FOUND));
    }

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

        assert_eq!(
            store
                .pending_operations(PUSH_BATCH_LIMIT)
                .await
                .unwrap()
                .len(),
            0
        );
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
    async fn updating_persistent_repository_queues_sync_operation() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memo.sqlite");
        let store = LocalStore::open(&db).await.unwrap();
        let repo = store
            .create_repository(
                "Writing".to_string(),
                false,
                "#c86f52".to_string(),
                "device-a",
            )
            .await
            .unwrap();
        let saved = store
            .update_repository(
                repo.id,
                "Longform".to_string(),
                "#5f7597".to_string(),
                true,
                "device-a",
            )
            .await
            .unwrap();

        assert_eq!(saved.name, "Longform");
        assert_eq!(saved.color, "#5f7597");
        assert!(saved.sync_enabled);
        let pending = store.pending_operations(PUSH_BATCH_LIMIT).await.unwrap();
        assert!(pending.iter().any(|op| matches!(
            &op.kind,
            SyncOperationKind::UpsertRepository(repository)
                if repository.id == repo.id && repository.name == "Longform"
        )));
    }

    #[tokio::test]
    async fn repository_input_is_sanitized_consistently() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();

        let created = store
            .create_repository(
                "  ".to_string(),
                false,
                "not-a-color".to_string(),
                "device-a",
            )
            .await
            .unwrap();

        assert_eq!(created.name, "Untitled repository");
        assert_eq!(created.color, "#c86f52");
    }

    #[tokio::test]
    async fn save_memo_preserves_client_supplied_new_id_and_cleans_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let repo = store
            .create_repository(
                "Synced".to_string(),
                false,
                "#C86F52".to_string(),
                "device-a",
            )
            .await
            .unwrap();
        let memo_id = Uuid::now_v7();

        let saved = store
            .save_memo(
                SaveMemoInput {
                    id: Some(memo_id),
                    repository_id: repo.id,
                    title: "  Client title  ".to_string(),
                    body_md: "body".to_string(),
                    tags: BTreeSet::from([" sync ".to_string(), "".to_string()]),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();

        assert_eq!(saved.id, memo_id);
        assert_eq!(saved.title, "Client title");
        assert_eq!(saved.tags, BTreeSet::from(["sync".to_string()]));
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

    #[tokio::test]
    async fn stats_report_local_queue_and_counts() {
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
        store
            .save_memo(
                SaveMemoInput {
                    id: None,
                    repository_id: repo.id,
                    title: "Stats".to_string(),
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

        let stats = store.stats().await.unwrap();
        assert!(stats.memo_count >= 2);
        assert!(stats.repository_count >= 3);
        assert_eq!(stats.attachment_count, 0);
        assert_eq!(stats.attachment_blob_count, 0);
        assert_eq!(stats.attachment_blob_bytes, 0);
        assert_eq!(stats.missing_attachment_blobs, 0);
        assert_eq!(stats.attachment_metadata_mismatches, 0);
        assert_eq!(stats.pending_operations, 2);
        assert_eq!(stats.last_server_sequence, 0);
    }

    #[tokio::test]
    async fn pending_operations_are_compacted_by_entity() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let repo = store
            .create_repository(
                "Drafts".to_string(),
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
                    title: "First".to_string(),
                    body_md: "one".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();

        store
            .save_memo(
                SaveMemoInput {
                    id: Some(memo.id),
                    repository_id: repo.id,
                    title: "Second".to_string(),
                    body_md: "two".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();
        store
            .save_memo(
                SaveMemoInput {
                    id: Some(memo.id),
                    repository_id: repo.id,
                    title: "Third".to_string(),
                    body_md: "three".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();

        let pending = store.pending_operations(PUSH_BATCH_LIMIT).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().any(|op| {
            matches!(
                &op.kind,
                SyncOperationKind::UpsertMemo(memo) if memo.title == "Third" && memo.body_md == "three"
            )
        }));
    }

    #[tokio::test]
    async fn memos_apply_database_filters_before_domain_filtering() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let work = store
            .create_repository("Work".to_string(), false, "#c86f52".to_string(), "device-a")
            .await
            .unwrap();
        let personal = store
            .create_repository(
                "Personal".to_string(),
                false,
                "#6f8f83".to_string(),
                "device-a",
            )
            .await
            .unwrap();
        store
            .save_memo(
                SaveMemoInput {
                    id: None,
                    repository_id: work.id,
                    title: "Latency notes".to_string(),
                    body_md: "Query SQLite before React".to_string(),
                    tags: BTreeSet::from(["perf".to_string()]),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();
        store
            .save_memo(
                SaveMemoInput {
                    id: None,
                    repository_id: personal.id,
                    title: "Garden".to_string(),
                    body_md: "Low latency watering".to_string(),
                    tags: BTreeSet::from(["home".to_string()]),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Clipboard,
                "device-a",
            )
            .await
            .unwrap();

        let results = store
            .memos(MemoFilter {
                repository_id: Some(work.id),
                query: Some("latency".to_string()),
                tags: BTreeSet::from(["perf".to_string()]),
                source: Some(MemoSource::Manual),
                ..MemoFilter::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Latency notes");
    }

    #[tokio::test]
    async fn remote_operations_do_not_overwrite_newer_local_versions() {
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
                    title: "Local wins".to_string(),
                    body_md: "new".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();

        let local_version = store
            .pending_operations(PUSH_BATCH_LIMIT)
            .await
            .unwrap()
            .into_iter()
            .find(
                |op| matches!(&op.kind, SyncOperationKind::UpsertMemo(item) if item.id == memo.id),
            )
            .unwrap()
            .hlc;
        let older_remote = SyncOperation::new(
            "device-b",
            HybridLogicalClock {
                wall_time_ms: local_version.wall_time_ms.saturating_sub(1),
                counter: local_version.counter,
            },
            SyncOperationKind::UpsertMemo({
                let mut remote = memo.clone();
                remote.title = "Remote loses".to_string();
                remote.body_md = "old".to_string();
                remote
            }),
        );

        store
            .apply_remote_operations(vec![ServerOperation {
                sequence: 42,
                operation: older_remote,
            }])
            .await
            .unwrap();

        let reloaded = store.memo_by_id(memo.id).await.unwrap().unwrap();
        assert_eq!(reloaded.title, "Local wins");
        assert_eq!(store.last_server_sequence().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn first_snapshot_replaces_untouched_seed_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        assert!(store
            .repositories()
            .await
            .unwrap()
            .iter()
            .any(|repo| repo.name == "Inbox"));

        let repo = Repository::new("Remote", RepositoryKind::Persistent, "#5f7597");
        let memo = Memo::new(repo.id, "Synced note", "from another device");
        let applied = store
            .apply_snapshot(SnapshotResponse {
                protocol_version: SYNC_PROTOCOL_VERSION,
                server_sequence: 7,
                min_available_sequence: 0,
                repositories: vec![memo_core::SnapshotRepository {
                    repository: repo.clone(),
                    device_id: "device-b".to_string(),
                    hlc: HybridLogicalClock {
                        wall_time_ms: 500,
                        counter: 0,
                    },
                    sequence: 6,
                }],
                memos: vec![memo_core::SnapshotMemo {
                    memo: memo.clone(),
                    device_id: "device-b".to_string(),
                    hlc: HybridLogicalClock {
                        wall_time_ms: 501,
                        counter: 0,
                    },
                    sequence: 7,
                }],
                attachments: vec![],
            })
            .await
            .unwrap();

        let repositories = store.repositories().await.unwrap();
        let memos = store.memos(MemoFilter::default()).await.unwrap();
        assert_eq!(applied, 2);
        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].id, repo.id);
        assert_eq!(memos.len(), 1);
        assert_eq!(memos[0].id, memo.id);
        assert_eq!(store.last_server_sequence().await.unwrap(), 7);
    }

    #[tokio::test]
    async fn first_empty_snapshot_keeps_seed_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();

        let applied = store
            .apply_snapshot(SnapshotResponse {
                protocol_version: SYNC_PROTOCOL_VERSION,
                server_sequence: 0,
                min_available_sequence: 0,
                repositories: vec![],
                memos: vec![],
                attachments: vec![],
            })
            .await
            .unwrap();

        assert_eq!(applied, 0);
        assert!(store
            .repositories()
            .await
            .unwrap()
            .iter()
            .any(|repo| repo.name == "Inbox"));
        assert_eq!(store.last_server_sequence().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn image_attachment_is_stored_and_queued_for_sync() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let repo = store
            .create_repository(
                "Images".to_string(),
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
                    title: "Visual note".to_string(),
                    body_md: "diagram".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();

        let attachment = store
            .save_attachment(
                SaveAttachmentInput {
                    memo_id: memo.id,
                    file_name: " diagram?.png ".to_string(),
                    media_type: "image/png".to_string(),
                    data_base64: BASE64_STANDARD.encode([1, 2, 3, 4]),
                },
                "device-a",
            )
            .await
            .unwrap();

        assert_eq!(attachment.file_name, "diagram_.png");
        assert_eq!(attachment.byte_len, 4);
        assert_eq!(attachment.content_sha256, sha256_hex(&[1, 2, 3, 4]));
        let (media_type, bytes) = store
            .attachment_bytes(attachment.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(media_type, "image/png");
        assert_eq!(bytes, vec![1, 2, 3, 4]);
        let pending = store.pending_operations(PUSH_BATCH_LIMIT).await.unwrap();
        assert!(pending.iter().any(|op| matches!(
            &op.kind,
            SyncOperationKind::UpsertAttachment(item) if item.id == attachment.id
        )));
        let inline_copies = scalar_i64(
            &store.pool,
            "SELECT COUNT(*) AS value FROM attachments WHERE data_base64 != ''",
        )
        .await
        .unwrap();
        let blobs = scalar_i64(
            &store.pool,
            "SELECT COUNT(*) AS value FROM attachment_blobs",
        )
        .await
        .unwrap();
        assert_eq!(inline_copies, 0);
        assert_eq!(blobs, 1);
    }

    #[tokio::test]
    async fn duplicate_attachment_content_reuses_one_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let repo = store
            .create_repository(
                "Images".to_string(),
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
                    title: "Visual note".to_string(),
                    body_md: "diagram".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();

        for name in ["one.png", "two.png"] {
            store
                .save_attachment(
                    SaveAttachmentInput {
                        memo_id: memo.id,
                        file_name: name.to_string(),
                        media_type: "image/png".to_string(),
                        data_base64: BASE64_STANDARD.encode([9, 8, 7, 6]),
                    },
                    "device-a",
                )
                .await
                .unwrap();
        }

        let blobs = scalar_i64(
            &store.pool,
            "SELECT COUNT(*) AS value FROM attachment_blobs",
        )
        .await
        .unwrap();
        let attachments = scalar_i64(&store.pool, "SELECT COUNT(*) AS value FROM attachments")
            .await
            .unwrap();
        assert_eq!(attachments, 2);
        assert_eq!(blobs, 1);
    }

    #[tokio::test]
    async fn deleted_attachments_release_unused_blob_cache() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let repo = store
            .create_repository(
                "Images".to_string(),
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
                    title: "Visual note".to_string(),
                    body_md: "diagram".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();
        let first = store
            .save_attachment(
                SaveAttachmentInput {
                    memo_id: memo.id,
                    file_name: "one.png".to_string(),
                    media_type: "image/png".to_string(),
                    data_base64: BASE64_STANDARD.encode([5, 4, 3, 2]),
                },
                "device-a",
            )
            .await
            .unwrap();
        let second = store
            .save_attachment(
                SaveAttachmentInput {
                    memo_id: memo.id,
                    file_name: "two.png".to_string(),
                    media_type: "image/png".to_string(),
                    data_base64: BASE64_STANDARD.encode([5, 4, 3, 2]),
                },
                "device-a",
            )
            .await
            .unwrap();

        store.delete_attachment(first.id, "device-a").await.unwrap();
        assert_eq!(
            scalar_i64(
                &store.pool,
                "SELECT COUNT(*) AS value FROM attachment_blobs",
            )
            .await
            .unwrap(),
            1
        );

        store
            .delete_attachment(second.id, "device-a")
            .await
            .unwrap();
        assert_eq!(
            scalar_i64(
                &store.pool,
                "SELECT COUNT(*) AS value FROM attachment_blobs",
            )
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn pending_attachment_upsert_protects_blob_cache() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let repo = store
            .create_repository(
                "Images".to_string(),
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
                    title: "Visual note".to_string(),
                    body_md: "diagram".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();
        let attachment = store
            .save_attachment(
                SaveAttachmentInput {
                    memo_id: memo.id,
                    file_name: "one.png".to_string(),
                    media_type: "image/png".to_string(),
                    data_base64: BASE64_STANDARD.encode([8, 6, 4, 2]),
                },
                "device-a",
            )
            .await
            .unwrap();

        sqlx::query("DELETE FROM attachments WHERE id = ?1")
            .bind(attachment.id.to_string())
            .execute(&store.pool)
            .await
            .unwrap();

        assert_eq!(store.cleanup_attachment_blobs().await.unwrap(), 0);
        assert_eq!(
            scalar_i64(
                &store.pool,
                "SELECT COUNT(*) AS value FROM attachment_blobs",
            )
            .await
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn missing_attachment_blobs_can_be_hydrated_from_server_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let repo = Repository::new("Remote", RepositoryKind::Persistent, "#5f7597");
        let memo = Memo::new(repo.id, "Remote image", "memo-attachment");
        let mut attachment = MemoAttachment::new(
            memo.id,
            repo.id,
            "remote.png",
            "image/png",
            4,
            BASE64_STANDARD.encode([3, 1, 4, 1]),
        );
        let data_base64 = attachment.data_base64.clone();
        attachment.data_base64.clear();

        store
            .apply_snapshot(SnapshotResponse {
                protocol_version: SYNC_PROTOCOL_VERSION,
                server_sequence: 9,
                min_available_sequence: 0,
                repositories: vec![memo_core::SnapshotRepository {
                    repository: repo.clone(),
                    device_id: "device-b".to_string(),
                    hlc: HybridLogicalClock {
                        wall_time_ms: 900,
                        counter: 0,
                    },
                    sequence: 7,
                }],
                memos: vec![memo_core::SnapshotMemo {
                    memo: memo.clone(),
                    device_id: "device-b".to_string(),
                    hlc: HybridLogicalClock {
                        wall_time_ms: 901,
                        counter: 0,
                    },
                    sequence: 8,
                }],
                attachments: vec![memo_core::SnapshotAttachment {
                    attachment: attachment.clone(),
                    device_id: "device-b".to_string(),
                    hlc: HybridLogicalClock {
                        wall_time_ms: 902,
                        counter: 0,
                    },
                    sequence: 9,
                }],
            })
            .await
            .unwrap();

        assert_eq!(
            store.missing_attachment_blob_hashes().await.unwrap().len(),
            1
        );
        let stats = store.stats().await.unwrap();
        assert_eq!(stats.missing_attachment_blobs, 1);

        let stored = store_attachment_blob_payloads(
            &store.pool,
            vec![AttachmentBlobPayload {
                descriptor: memo_core::AttachmentBlobDescriptor {
                    content_sha256: attachment.content_sha256.clone(),
                    media_type: attachment.media_type.clone(),
                    byte_len: attachment.byte_len,
                },
                data_base64,
            }],
        )
        .await
        .unwrap();

        assert_eq!(stored, 1);
        assert!(store
            .missing_attachment_blob_hashes()
            .await
            .unwrap()
            .is_empty());
        let (_, bytes) = store
            .attachment_bytes(attachment.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes, vec![3, 1, 4, 1]);
    }

    #[tokio::test]
    async fn invalid_hydrated_attachment_blob_payload_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let result = store_attachment_blob_payloads(
            &store.pool,
            vec![AttachmentBlobPayload {
                descriptor: memo_core::AttachmentBlobDescriptor {
                    content_sha256: "0".repeat(64),
                    media_type: "image/png".to_string(),
                    byte_len: 4,
                },
                data_base64: BASE64_STANDARD.encode([1, 2, 3, 4]),
            }],
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            scalar_i64(
                &store.pool,
                "SELECT COUNT(*) AS value FROM attachment_blobs",
            )
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn rejects_unsupported_or_large_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::open(dir.path().join("memo.sqlite"))
            .await
            .unwrap();
        let repo = store
            .create_repository(
                "Images".to_string(),
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
                    title: "Visual note".to_string(),
                    body_md: "diagram".to_string(),
                    tags: BTreeSet::new(),
                    pinned: false,
                    archived: false,
                },
                MemoSource::Manual,
                "device-a",
            )
            .await
            .unwrap();

        let unsupported = store
            .save_attachment(
                SaveAttachmentInput {
                    memo_id: memo.id,
                    file_name: "script.svg".to_string(),
                    media_type: "image/svg+xml".to_string(),
                    data_base64: BASE64_STANDARD.encode([1]),
                },
                "device-a",
            )
            .await;
        assert!(unsupported.is_err());

        let large = store
            .save_attachment(
                SaveAttachmentInput {
                    memo_id: memo.id,
                    file_name: "large.png".to_string(),
                    media_type: "image/png".to_string(),
                    data_base64: BASE64_STANDARD.encode(vec![0u8; MAX_ATTACHMENT_BYTES + 1]),
                },
                "device-a",
            )
            .await;
        assert!(large.is_err());
    }
}
