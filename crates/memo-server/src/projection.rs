use crate::{config, db, error::ServerError};
use memo_core::{
    attachment_content_sha256, HybridLogicalClock, Memo, MemoAttachment, SyncOperation,
    SyncOperationKind,
};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

pub(crate) async fn rebuild_state_if_needed(pool: &SqlitePool) -> anyhow::Result<()> {
    let operation_count: i64 = sqlx::query("SELECT COUNT(*) AS value FROM operation_log")
        .fetch_one(pool)
        .await?
        .get("value");
    let state_count: i64 = sqlx::query(
        "SELECT (SELECT COUNT(*) FROM repository_state) + (SELECT COUNT(*) FROM memo_state) + (SELECT COUNT(*) FROM attachment_state) + (SELECT COUNT(*) FROM attachment_blob_state) AS value",
    )
    .fetch_one(pool)
    .await?
    .get("value");
    let projection_version = db::meta_string(pool, config::STATE_PROJECTION_VERSION_KEY).await?;
    let projection_is_current =
        projection_version.as_deref() == Some(config::STATE_PROJECTION_VERSION);
    if projection_is_current && (operation_count == 0 || state_count > 0) {
        return Ok(());
    }

    let rows = sqlx::query("SELECT sequence, payload FROM operation_log ORDER BY sequence ASC")
        .fetch_all(pool)
        .await?;
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM repository_state")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM memo_state")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM attachment_state")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM attachment_blob_state")
        .execute(&mut *tx)
        .await?;
    for row in rows {
        let sequence = row.get("sequence");
        let payload: String = row.get("payload");
        let operation: SyncOperation = serde_json::from_str(&payload)?;
        apply_state_operation(&mut tx, operation, sequence)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    db::set_meta_string(
        &mut tx,
        config::STATE_PROJECTION_VERSION_KEY,
        config::STATE_PROJECTION_VERSION,
    )
    .await?;
    tx.commit().await?;
    tracing::info!(operations = operation_count, "state projection rebuilt");
    Ok(())
}

pub(crate) async fn apply_state_operation(
    tx: &mut Transaction<'_, Sqlite>,
    op: SyncOperation,
    sequence: i64,
) -> Result<(), ServerError> {
    match op.kind {
        SyncOperationKind::UpsertRepository(repo) => {
            if state_operation_wins(
                tx,
                "repository_state",
                &repo.id.to_string(),
                &op.device_id,
                op.hlc,
            )
            .await?
            {
                sqlx::query(
                    r#"
                    INSERT INTO repository_state (id, payload, device_id, hlc_wall_time_ms, hlc_counter, sequence)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    ON CONFLICT(id) DO UPDATE SET
                      payload = excluded.payload,
                      device_id = excluded.device_id,
                      hlc_wall_time_ms = excluded.hlc_wall_time_ms,
                      hlc_counter = excluded.hlc_counter,
                      sequence = excluded.sequence
                    "#,
                )
                .bind(repo.id.to_string())
                .bind(serde_json::to_string(&repo)?)
                .bind(&op.device_id)
                .bind(op.hlc.wall_time_ms)
                .bind(i64::from(op.hlc.counter))
                .bind(sequence)
                .execute(&mut **tx)
                .await?;
            }
        }
        SyncOperationKind::UpsertMemo(memo) => {
            if state_operation_wins(
                tx,
                "memo_state",
                &memo.id.to_string(),
                &op.device_id,
                op.hlc,
            )
            .await?
            {
                upsert_memo_state(tx, &memo, &op.device_id, op.hlc, sequence).await?;
            }
        }
        SyncOperationKind::UpsertAttachment(mut attachment) => {
            if attachment.content_sha256.is_empty() {
                attachment.content_sha256 = attachment_content_sha256(&attachment.data_base64);
            }
            attachment.data_base64.clear();
            if state_operation_wins(
                tx,
                "attachment_state",
                &attachment.id.to_string(),
                &op.device_id,
                op.hlc,
            )
            .await?
            {
                upsert_attachment_state(tx, &attachment, &op.device_id, op.hlc, sequence).await?;
            }
        }
        SyncOperationKind::PatchMemo { memo_id, patch, .. } => {
            if state_operation_wins(
                tx,
                "memo_state",
                &memo_id.to_string(),
                &op.device_id,
                op.hlc,
            )
            .await?
            {
                let Some(mut memo) = memo_state(tx, memo_id).await? else {
                    return Ok(());
                };
                memo.apply_patch(patch);
                upsert_memo_state(tx, &memo, &op.device_id, op.hlc, sequence).await?;
            }
        }
        SyncOperationKind::DeleteMemo {
            memo_id,
            repository_id,
        } => {
            if state_operation_wins(
                tx,
                "memo_state",
                &memo_id.to_string(),
                &op.device_id,
                op.hlc,
            )
            .await?
            {
                if let Some(mut memo) = memo_state(tx, memo_id).await? {
                    memo.deleted = true;
                    upsert_memo_state(tx, &memo, &op.device_id, op.hlc, sequence).await?;
                } else {
                    sqlx::query(
                        r#"
                        INSERT INTO memo_state (id, repository_id, payload, device_id, hlc_wall_time_ms, hlc_counter, sequence, deleted)
                        VALUES (?1, ?2, '{}', ?3, ?4, ?5, ?6, 1)
                        ON CONFLICT(id) DO UPDATE SET
                          device_id = excluded.device_id,
                          hlc_wall_time_ms = excluded.hlc_wall_time_ms,
                          hlc_counter = excluded.hlc_counter,
                          sequence = excluded.sequence,
                          deleted = 1
                        "#,
                    )
                    .bind(memo_id.to_string())
                    .bind(repository_id.to_string())
                    .bind(&op.device_id)
                    .bind(op.hlc.wall_time_ms)
                    .bind(i64::from(op.hlc.counter))
                    .bind(sequence)
                    .execute(&mut **tx)
                    .await?;
                }
            }
        }
        SyncOperationKind::DeleteAttachment {
            attachment_id,
            repository_id,
        } => {
            if state_operation_wins(
                tx,
                "attachment_state",
                &attachment_id.to_string(),
                &op.device_id,
                op.hlc,
            )
            .await?
            {
                if let Some(mut attachment) = attachment_state(tx, attachment_id).await? {
                    attachment.deleted = true;
                    upsert_attachment_state(&mut *tx, &attachment, &op.device_id, op.hlc, sequence)
                        .await?;
                    cleanup_attachment_blob_state(&mut *tx).await?;
                } else {
                    sqlx::query(
                        r#"
                        INSERT INTO attachment_state (id, memo_id, repository_id, payload, device_id, hlc_wall_time_ms, hlc_counter, sequence, deleted)
                        VALUES (?1, '', ?2, '{}', ?3, ?4, ?5, ?6, 1)
                        ON CONFLICT(id) DO UPDATE SET
                          device_id = excluded.device_id,
                          hlc_wall_time_ms = excluded.hlc_wall_time_ms,
                          hlc_counter = excluded.hlc_counter,
                          sequence = excluded.sequence,
                          deleted = 1
                        "#,
                    )
                    .bind(attachment_id.to_string())
                    .bind(repository_id.to_string())
                    .bind(&op.device_id)
                    .bind(op.hlc.wall_time_ms)
                    .bind(i64::from(op.hlc.counter))
                    .bind(sequence)
                    .execute(&mut **tx)
                    .await?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) async fn cleanup_attachment_blob_state(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        DELETE FROM attachment_blob_state
        WHERE NOT EXISTS (
          SELECT 1
          FROM attachment_state
          WHERE attachment_state.deleted = 0
            AND json_extract(attachment_state.payload, '$.content_sha256') = attachment_blob_state.content_sha256
        )
        "#,
    )
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

pub(crate) fn row_hlc(row: &sqlx::sqlite::SqliteRow) -> HybridLogicalClock {
    HybridLogicalClock {
        wall_time_ms: row.get("hlc_wall_time_ms"),
        counter: row
            .get::<i64, _>("hlc_counter")
            .try_into()
            .unwrap_or(u16::MAX),
    }
}

async fn state_operation_wins(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    id: &str,
    device_id: &str,
    hlc: HybridLogicalClock,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(&format!(
        "SELECT device_id, hlc_wall_time_ms, hlc_counter FROM {table} WHERE id = ?1"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(match row {
        Some(row) => {
            let existing_hlc = row_hlc(&row);
            let existing_device: String = row.get("device_id");
            hlc > existing_hlc || (hlc == existing_hlc && device_id > existing_device.as_str())
        }
        None => true,
    })
}

async fn upsert_memo_state(
    tx: &mut Transaction<'_, Sqlite>,
    memo: &Memo,
    device_id: &str,
    hlc: HybridLogicalClock,
    sequence: i64,
) -> Result<(), ServerError> {
    sqlx::query(
        r#"
        INSERT INTO memo_state (id, repository_id, payload, device_id, hlc_wall_time_ms, hlc_counter, sequence, deleted)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(id) DO UPDATE SET
          repository_id = excluded.repository_id,
          payload = excluded.payload,
          device_id = excluded.device_id,
          hlc_wall_time_ms = excluded.hlc_wall_time_ms,
          hlc_counter = excluded.hlc_counter,
          sequence = excluded.sequence,
          deleted = excluded.deleted
        "#,
    )
    .bind(memo.id.to_string())
    .bind(memo.repository_id.to_string())
    .bind(serde_json::to_string(memo)?)
    .bind(device_id)
    .bind(hlc.wall_time_ms)
    .bind(i64::from(hlc.counter))
    .bind(sequence)
    .bind(memo.deleted)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn memo_state(
    tx: &mut Transaction<'_, Sqlite>,
    memo_id: uuid::Uuid,
) -> Result<Option<Memo>, ServerError> {
    let row = sqlx::query("SELECT payload FROM memo_state WHERE id = ?1")
        .bind(memo_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
    row.map(|row| row.get::<String, _>("payload"))
        .filter(|payload| payload != "{}")
        .map(|payload| serde_json::from_str(&payload))
        .transpose()
        .map_err(ServerError::from)
}

async fn upsert_attachment_state(
    tx: &mut Transaction<'_, Sqlite>,
    attachment: &MemoAttachment,
    device_id: &str,
    hlc: HybridLogicalClock,
    sequence: i64,
) -> Result<(), ServerError> {
    sqlx::query(
        r#"
        INSERT INTO attachment_state (id, memo_id, repository_id, payload, device_id, hlc_wall_time_ms, hlc_counter, sequence, deleted)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(id) DO UPDATE SET
          memo_id = excluded.memo_id,
          repository_id = excluded.repository_id,
          payload = excluded.payload,
          device_id = excluded.device_id,
          hlc_wall_time_ms = excluded.hlc_wall_time_ms,
          hlc_counter = excluded.hlc_counter,
          sequence = excluded.sequence,
          deleted = excluded.deleted
        "#,
    )
    .bind(attachment.id.to_string())
    .bind(attachment.memo_id.to_string())
    .bind(attachment.repository_id.to_string())
    .bind(serde_json::to_string(attachment)?)
    .bind(device_id)
    .bind(hlc.wall_time_ms)
    .bind(i64::from(hlc.counter))
    .bind(sequence)
    .bind(attachment.deleted)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn attachment_state(
    tx: &mut Transaction<'_, Sqlite>,
    attachment_id: uuid::Uuid,
) -> Result<Option<MemoAttachment>, ServerError> {
    let row = sqlx::query("SELECT payload FROM attachment_state WHERE id = ?1")
        .bind(attachment_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
    row.map(|row| row.get::<String, _>("payload"))
        .filter(|payload| payload != "{}")
        .map(|payload| serde_json::from_str(&payload))
        .transpose()
        .map_err(ServerError::from)
}
