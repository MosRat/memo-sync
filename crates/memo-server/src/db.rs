use crate::{config, projection};
use sqlx::{sqlite::SqlitePoolOptions, Row, Sqlite, SqlitePool, Transaction};
use std::{path::Path, time::Duration};

pub async fn open_pool(database_url: impl AsRef<str>) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url.as_ref())
        .await?;
    configure_sqlite(&pool).await?;
    migrate(&pool).await?;
    tracing::info!("database pool opened");
    Ok(pool)
}

pub async fn open_file_pool(path: impl AsRef<Path>) -> anyhow::Result<SqlitePool> {
    let url = format!("sqlite://{}?mode=rwc", path.as_ref().display());
    open_pool(url).await
}

async fn configure_sqlite(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA synchronous = NORMAL")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA busy_timeout = 5000")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA temp_store = MEMORY")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA cache_size = -16000")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA mmap_size = 268435456")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA journal_size_limit = 67108864")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA analysis_limit = 1000")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA optimize").execute(pool).await?;
    Ok(())
}

async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS operation_log (
          sequence INTEGER PRIMARY KEY AUTOINCREMENT,
          op_id TEXT NOT NULL UNIQUE,
          repository_id TEXT,
          device_id TEXT NOT NULL,
          hlc_wall_time_ms INTEGER NOT NULL,
          hlc_counter INTEGER NOT NULL,
          payload TEXT NOT NULL,
          created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS operation_log_repo_sequence_idx
        ON operation_log(repository_id, sequence);
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS repository_state (
          id TEXT PRIMARY KEY,
          payload TEXT NOT NULL,
          device_id TEXT NOT NULL,
          hlc_wall_time_ms INTEGER NOT NULL,
          hlc_counter INTEGER NOT NULL,
          sequence INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS memo_state (
          id TEXT PRIMARY KEY,
          repository_id TEXT NOT NULL,
          payload TEXT NOT NULL,
          device_id TEXT NOT NULL,
          hlc_wall_time_ms INTEGER NOT NULL,
          hlc_counter INTEGER NOT NULL,
          sequence INTEGER NOT NULL,
          deleted INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS memo_state_repo_idx ON memo_state(repository_id)")
        .execute(pool)
        .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS attachment_state (
          id TEXT PRIMARY KEY,
          memo_id TEXT NOT NULL,
          repository_id TEXT NOT NULL,
          payload TEXT NOT NULL,
          device_id TEXT NOT NULL,
          hlc_wall_time_ms INTEGER NOT NULL,
          hlc_counter INTEGER NOT NULL,
          sequence INTEGER NOT NULL,
          deleted INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS attachment_state_memo_idx ON attachment_state(memo_id)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS attachment_state_repo_idx ON attachment_state(repository_id)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS attachment_blob_state (
          content_sha256 TEXT PRIMARY KEY,
          media_type TEXT NOT NULL,
          byte_len INTEGER NOT NULL,
          data_base64 TEXT NOT NULL,
          created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
          updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS sync_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(pool)
        .await?;
    projection::rebuild_state_if_needed(pool).await?;
    Ok(())
}

pub(crate) async fn latest_sequence_from_db(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT max(value) AS value
        FROM (
          SELECT COALESCE(MAX(sequence), 0) AS value FROM operation_log
          UNION ALL
          SELECT COALESCE(MAX(sequence), 0) AS value FROM repository_state
          UNION ALL
          SELECT COALESCE(MAX(sequence), 0) AS value FROM memo_state
          UNION ALL
          SELECT COALESCE(MAX(sequence), 0) AS value FROM attachment_state
        )
        "#,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.get::<i64, _>("value"))
}

pub(crate) async fn min_available_sequence(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    Ok(meta_i64(pool, config::MIN_AVAILABLE_SEQUENCE_KEY)
        .await?
        .unwrap_or(0))
}

pub(crate) async fn meta_i64(pool: &SqlitePool, key: &str) -> Result<Option<i64>, sqlx::Error> {
    let row = sqlx::query("SELECT value FROM sync_meta WHERE key = ?1")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.and_then(|row| row.get::<String, _>("value").parse::<i64>().ok()))
}

pub(crate) async fn meta_string(
    pool: &SqlitePool,
    key: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query("SELECT value FROM sync_meta WHERE key = ?1")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|row| row.get("value")))
}

pub(crate) async fn set_meta_i64(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    value: i64,
) -> Result<(), sqlx::Error> {
    set_meta_string(tx, key, &value.to_string()).await
}

pub(crate) async fn set_meta_string(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    value: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO sync_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
