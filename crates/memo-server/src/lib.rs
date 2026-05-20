use axum::{
    extract::{ws::WebSocketUpgrade, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use memo_core::{
    PullRequest, PullResponse, PushRequest, PushResponse, ServerOperation, SyncOperation,
    SYNC_PROTOCOL_VERSION,
};
use serde::Serialize;
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::{path::Path, time::Duration};
use tokio::sync::broadcast;
use tower_http::{compression::CompressionLayer, cors::CorsLayer, trace::TraceLayer};

#[derive(Clone)]
pub struct AppState {
    pool: SqlitePool,
    tx: broadcast::Sender<ServerEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerEvent {
    pub kind: &'static str,
    pub server_sequence: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("operation payload error: {0}")]
    Payload(#[from] serde_json::Error),
    #[error("unsupported sync protocol version {0}")]
    UnsupportedProtocol(u16),
}

impl IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            ServerError::Database(_) | ServerError::Payload(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ServerError::UnsupportedProtocol(_) => StatusCode::BAD_REQUEST,
        };
        (status, self.to_string()).into_response()
    }
}

pub async fn open_pool(database_url: impl AsRef<str>) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url.as_ref())
        .await?;
    configure_sqlite(&pool).await?;
    migrate(&pool).await?;
    Ok(pool)
}

pub async fn open_file_pool(path: impl AsRef<Path>) -> anyhow::Result<SqlitePool> {
    let url = format!("sqlite://{}?mode=rwc", path.as_ref().display());
    open_pool(url).await
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/sync/push", post(push))
        .route("/api/v1/sync/pull", post(pull))
        .route("/api/v1/events", get(events))
        .layer(CorsLayer::permissive())
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub fn state(pool: SqlitePool) -> AppState {
    let (tx, _) = broadcast::channel(1024);
    AppState { pool, tx }
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

    Ok(())
}

async fn health(State(state): State<AppState>) -> Result<Json<Health>, ServerError> {
    let sequence = latest_sequence(&state.pool).await?;
    Ok(Json(Health {
        ok: true,
        server_sequence: sequence,
    }))
}

#[derive(Serialize)]
struct Health {
    ok: bool,
    server_sequence: i64,
}

async fn push(
    State(state): State<AppState>,
    Json(request): Json<PushRequest>,
) -> Result<Json<PushResponse>, ServerError> {
    ensure_protocol(request.protocol_version)?;
    let mut tx = state.pool.begin().await?;
    let mut accepted = 0usize;

    for operation in request.operations {
        let payload = serde_json::to_string(&operation)?;
        let result = sqlx::query(
            r#"
            INSERT OR IGNORE INTO operation_log
            (op_id, repository_id, device_id, hlc_wall_time_ms, hlc_counter, payload)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(operation.op_id.to_string())
        .bind(operation.repository_id.map(|id| id.to_string()))
        .bind(operation.device_id)
        .bind(operation.hlc.wall_time_ms)
        .bind(i64::from(operation.hlc.counter))
        .bind(payload)
        .execute(&mut *tx)
        .await?;

        accepted += usize::try_from(result.rows_affected()).unwrap_or(0);
    }

    tx.commit().await?;
    let server_sequence = latest_sequence(&state.pool).await?;

    if accepted > 0 {
        let _ = state.tx.send(ServerEvent {
            kind: "operations",
            server_sequence,
        });
    }

    Ok(Json(PushResponse {
        protocol_version: SYNC_PROTOCOL_VERSION,
        accepted,
        server_sequence,
    }))
}

async fn pull(
    State(state): State<AppState>,
    Json(request): Json<PullRequest>,
) -> Result<Json<PullResponse>, ServerError> {
    ensure_protocol(request.protocol_version)?;
    let limit = i64::from(request.limit.clamp(1, 1000));
    let fetch_limit = limit + 1;
    let rows = if request.repository_ids.is_empty() {
        sqlx::query(
            r#"
            SELECT sequence, payload FROM operation_log
            WHERE sequence > ?1
            ORDER BY sequence ASC
            LIMIT ?2
            "#,
        )
        .bind(request.since_sequence)
        .bind(fetch_limit)
        .fetch_all(&state.pool)
        .await?
    } else {
        let repo_ids = request
            .repository_ids
            .iter()
            .map(uuid::Uuid::to_string)
            .collect::<Vec<_>>();
        let placeholders = std::iter::repeat("?")
            .take(repo_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT sequence, payload FROM operation_log WHERE sequence > ? AND repository_id IN ({placeholders}) ORDER BY sequence ASC LIMIT ?"
        );
        let mut query = sqlx::query(&sql).bind(request.since_sequence);
        for repo_id in repo_ids {
            query = query.bind(repo_id);
        }
        query = query.bind(fetch_limit);
        query.fetch_all(&state.pool).await?
    };

    let has_more = rows.len() > usize::try_from(limit).unwrap_or(1000);
    let rows = rows
        .into_iter()
        .take(usize::try_from(limit).unwrap_or(1000))
        .collect::<Vec<_>>();
    let mut operations = Vec::with_capacity(rows.len());
    for row in rows {
        let sequence: i64 = row.get("sequence");
        let payload: String = row.get("payload");
        let operation: SyncOperation = serde_json::from_str(&payload)?;
        operations.push(ServerOperation {
            sequence,
            operation,
        });
    }

    let server_sequence = operations
        .last()
        .map(|item| item.sequence)
        .unwrap_or(latest_sequence(&state.pool).await?);

    Ok(Json(PullResponse {
        protocol_version: SYNC_PROTOCOL_VERSION,
        operations,
        server_sequence,
        has_more,
    }))
}

async fn events(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    let mut rx = state.tx.subscribe();
    ws.on_upgrade(move |mut socket| async move {
        while let Ok(event) = rx.recv().await {
            let Ok(payload) = serde_json::to_string(&event) else {
                continue;
            };
            if socket
                .send(axum::extract::ws::Message::Text(payload.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

async fn latest_sequence(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let row = sqlx::query("SELECT COALESCE(MAX(sequence), 0) AS sequence FROM operation_log")
        .fetch_one(pool)
        .await?;
    Ok(row.get("sequence"))
}

fn ensure_protocol(version: u16) -> Result<(), ServerError> {
    if version == SYNC_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ServerError::UnsupportedProtocol(version))
    }
}
