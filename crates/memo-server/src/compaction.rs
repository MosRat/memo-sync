use crate::{config, db, error::ServerError};
use sqlx::{Row, SqlitePool};

pub(crate) async fn compact_operation_log(pool: &SqlitePool) -> Result<(), ServerError> {
    let latest = db::latest_sequence_from_db(pool).await?;
    if latest <= config::COMPACT_OPERATION_LOG_RETAIN {
        return Ok(());
    }
    let row = sqlx::query("SELECT COUNT(*) AS value FROM operation_log")
        .fetch_one(pool)
        .await?;
    let operation_count = row.get::<i64, _>("value");
    if operation_count <= config::COMPACT_OPERATION_LOG_AFTER {
        return Ok(());
    }

    let cutoff = latest - config::COMPACT_OPERATION_LOG_RETAIN;
    let current_min = db::min_available_sequence(pool).await?;
    if cutoff <= current_min {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    let result = sqlx::query("DELETE FROM operation_log WHERE sequence <= ?1")
        .bind(cutoff)
        .execute(&mut *tx)
        .await?;
    let deleted = result.rows_affected();
    if deleted > 0 {
        db::set_meta_i64(&mut tx, config::MIN_AVAILABLE_SEQUENCE_KEY, cutoff).await?;
    }
    tx.commit().await?;
    tracing::info!(
        latest_sequence = latest,
        min_available_sequence = cutoff,
        deleted,
        "operation log compacted"
    );
    Ok(())
}
