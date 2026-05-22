use crate::{
    app_state::{cached_latest, remember_latest, store_latest, AppState, RelayBlob, ServerEvent},
    compaction, config, db,
    error::ServerError,
    projection, validation,
};
use axum::{
    extract::{ws::WebSocketUpgrade, DefaultBodyLimit, Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{Duration as ChronoDuration, Utc};
use memo_core::{
    default_sync_protocol_version, AttachmentBlobDescriptor, AttachmentBlobFetchRequest,
    AttachmentBlobFetchResponse, AttachmentBlobManifestRequest, AttachmentBlobManifestResponse,
    AttachmentBlobPayload, AttachmentBlobRelayRequest, AttachmentBlobRelayResponse, PullRequest,
    PullResponse, PushRequest, PushResponse, ServerOperation, SnapshotAttachment, SnapshotMemo,
    SnapshotRepository, SnapshotResponse, SyncOperation, SyncOperationKind, SYNC_PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::broadcast;
use tower_http::{compression::CompressionLayer, cors::CorsLayer, trace::TraceLayer};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/sync/push", post(push))
        .route("/api/v1/sync/pull", post(pull))
        .route("/api/v1/sync/snapshot", get(snapshot))
        .route(
            "/api/v1/sync/attachment-blobs/manifest",
            post(attachment_blob_manifest),
        )
        .route(
            "/api/v1/sync/attachment-blobs/fetch",
            post(fetch_attachment_blobs),
        )
        .route(
            "/api/v1/sync/attachment-blobs/relay",
            post(relay_attachment_blobs),
        )
        .route("/api/v1/sync/wait", get(wait_for_change))
        .route("/api/v1/events", get(events))
        .layer(DefaultBodyLimit::max(config::MAX_JSON_BODY_BYTES))
        .layer(axum::middleware::from_fn(log_request_errors))
        .layer(CorsLayer::permissive())
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn log_request_errors(
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let response = next.run(request).await;
    if response.status().is_client_error() || response.status().is_server_error() {
        tracing::warn!(
            method = %method,
            path,
            status = response.status().as_u16(),
            "http request completed with error status"
        );
    }
    response
}

async fn health(State(state): State<AppState>) -> Result<Json<Health>, ServerError> {
    let sequence = latest_sequence(&state).await?;
    cleanup_expired_relay_blobs(&state).await;
    let (relay_blob_count, relay_blob_bytes, relay_device_count) = relay_blob_stats(&state).await;
    Ok(Json(Health {
        ok: true,
        server_sequence: sequence,
        min_available_sequence: db::min_available_sequence(&state.pool).await?,
        protocol_version: SYNC_PROTOCOL_VERSION,
        attachment_count: scalar_i64(
            &state,
            "SELECT COUNT(*) AS value FROM attachment_state WHERE deleted = 0",
        )
        .await?,
        attachment_blob_count: scalar_i64(
            &state,
            "SELECT COUNT(*) AS value FROM attachment_blob_state",
        )
        .await?,
        attachment_blob_bytes: scalar_i64(
            &state,
            "SELECT COALESCE(SUM(byte_len), 0) AS value FROM attachment_blob_state",
        )
        .await?,
        relay_blob_count,
        relay_blob_bytes,
        relay_device_count,
    }))
}

#[derive(Serialize)]
struct Health {
    ok: bool,
    server_sequence: i64,
    min_available_sequence: i64,
    protocol_version: u16,
    attachment_count: i64,
    attachment_blob_count: i64,
    attachment_blob_bytes: i64,
    relay_blob_count: i64,
    relay_blob_bytes: i64,
    relay_device_count: i64,
}

#[derive(Deserialize)]
struct WaitQuery {
    #[serde(default = "default_sync_protocol_version")]
    protocol_version: u16,
    since_sequence: i64,
    #[serde(default = "default_wait_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Deserialize)]
struct SnapshotQuery {
    #[serde(default = "default_sync_protocol_version")]
    protocol_version: u16,
}

#[derive(Serialize)]
struct WaitResponse {
    changed: bool,
    server_sequence: i64,
    protocol_version: u16,
}

fn default_wait_timeout_ms() -> u64 {
    25_000
}

async fn push(
    State(state): State<AppState>,
    Json(request): Json<PushRequest>,
) -> Result<Json<PushResponse>, ServerError> {
    validation::validate_push_request(&request)?;
    let device_id = request.device_id.clone();
    let operation_total = request.operations.len();
    let mut tx = state.pool.begin().await?;
    let mut accepted = 0usize;

    for operation in request.operations {
        relay_inline_attachment_blob(&state, &operation, &device_id).await?;
        let stored_operation = metadata_only_operation(operation);
        let payload = serde_json::to_string(&stored_operation)?;
        if payload.len() > config::MAX_OPERATION_PAYLOAD_BYTES {
            return Err(ServerError::bad_request(format!(
                "operation payload accepts at most {} bytes",
                config::MAX_OPERATION_PAYLOAD_BYTES
            )));
        }
        let result = sqlx::query(
            r#"
            INSERT OR IGNORE INTO operation_log
            (op_id, repository_id, device_id, hlc_wall_time_ms, hlc_counter, payload)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(stored_operation.op_id.to_string())
        .bind(stored_operation.repository_id.map(|id| id.to_string()))
        .bind(&stored_operation.device_id)
        .bind(stored_operation.hlc.wall_time_ms)
        .bind(i64::from(stored_operation.hlc.counter))
        .bind(payload)
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() > 0 {
            let row = sqlx::query("SELECT sequence FROM operation_log WHERE op_id = ?1")
                .bind(stored_operation.op_id.to_string())
                .fetch_one(&mut *tx)
                .await?;
            let sequence = row.get::<i64, _>("sequence");
            projection::apply_state_operation(&mut tx, stored_operation, sequence).await?;
            accepted += 1;
        }
    }

    tx.commit().await?;
    if accepted > 0 {
        compaction::compact_operation_log(&state.pool).await?;
    }
    let server_sequence = db::latest_sequence_from_db(&state.pool).await?;
    remember_latest(&state, server_sequence);

    if accepted > 0 {
        let _ = state.tx.send(ServerEvent {
            kind: "operations",
            server_sequence,
        });
    }

    tracing::info!(
        device_id,
        requested = operation_total,
        accepted,
        server_sequence,
        "sync push completed"
    );
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
    validation::validate_pull_request(&request)?;
    let min_available_sequence = db::min_available_sequence(&state.pool).await?;
    if request.since_sequence < min_available_sequence {
        let server_sequence = latest_sequence(&state).await?;
        tracing::info!(
            since_sequence = request.since_sequence,
            min_available_sequence,
            server_sequence,
            "pull requires snapshot"
        );
        return Ok(Json(PullResponse {
            protocol_version: SYNC_PROTOCOL_VERSION,
            operations: vec![],
            server_sequence,
            min_available_sequence,
            snapshot_required: true,
            has_more: false,
        }));
    }
    let limit = i64::from(request.limit.clamp(1, 1000));
    let fetch_limit = limit + 1;
    let rows = if request.repository_ids.is_empty() {
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT sequence, payload FROM operation_log WHERE sequence > ",
        );
        query.push_bind(request.since_sequence);
        if let Some(device_id) = request.exclude_device_id.as_deref() {
            query.push(" AND device_id != ");
            query.push_bind(device_id);
        }
        query.push(" ORDER BY sequence ASC LIMIT ");
        query.push_bind(fetch_limit);
        query.build().fetch_all(&state.pool).await?
    } else {
        let repo_ids = request
            .repository_ids
            .iter()
            .map(uuid::Uuid::to_string)
            .collect::<Vec<_>>();
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT sequence, payload FROM operation_log WHERE sequence > ",
        );
        query.push_bind(request.since_sequence);
        query.push(" AND repository_id IN (");
        let mut separated = query.separated(", ");
        for repo_id in repo_ids {
            separated.push_bind(repo_id);
        }
        separated.push_unseparated(")");
        if let Some(device_id) = request.exclude_device_id.as_deref() {
            query.push(" AND device_id != ");
            query.push_bind(device_id);
        }
        query.push(" ORDER BY sequence ASC LIMIT ");
        query.push_bind(fetch_limit);
        query.build().fetch_all(&state.pool).await?
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
            operation: metadata_only_operation(operation),
        });
    }

    let server_sequence = operations
        .last()
        .map(|item| item.sequence)
        .unwrap_or(latest_sequence(&state).await?);

    tracing::debug!(
        since_sequence = request.since_sequence,
        returned = operations.len(),
        has_more,
        server_sequence,
        "sync pull completed"
    );
    Ok(Json(PullResponse {
        protocol_version: SYNC_PROTOCOL_VERSION,
        operations,
        server_sequence,
        min_available_sequence,
        snapshot_required: false,
        has_more,
    }))
}

async fn snapshot(
    State(state): State<AppState>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<SnapshotResponse>, ServerError> {
    validation::ensure_protocol(query.protocol_version)?;
    let repo_rows = sqlx::query(
        "SELECT payload, device_id, hlc_wall_time_ms, hlc_counter, sequence FROM repository_state ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    let memo_rows = sqlx::query(
        "SELECT payload, device_id, hlc_wall_time_ms, hlc_counter, sequence FROM memo_state WHERE deleted = 0 ORDER BY repository_id, id",
    )
    .fetch_all(&state.pool)
    .await?;
    let attachment_rows = sqlx::query(
        "SELECT payload, device_id, hlc_wall_time_ms, hlc_counter, sequence FROM attachment_state WHERE deleted = 0 ORDER BY repository_id, memo_id, id",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut repositories = Vec::with_capacity(repo_rows.len());
    for row in repo_rows {
        repositories.push(SnapshotRepository {
            repository: serde_json::from_str(&row.get::<String, _>("payload"))?,
            device_id: row.get("device_id"),
            hlc: projection::row_hlc(&row),
            sequence: row.get("sequence"),
        });
    }

    let mut memos = Vec::with_capacity(memo_rows.len());
    for row in memo_rows {
        memos.push(SnapshotMemo {
            memo: serde_json::from_str(&row.get::<String, _>("payload"))?,
            device_id: row.get("device_id"),
            hlc: projection::row_hlc(&row),
            sequence: row.get("sequence"),
        });
    }

    let mut attachments = Vec::with_capacity(attachment_rows.len());
    for row in attachment_rows {
        let mut attachment: memo_core::MemoAttachment =
            serde_json::from_str(&row.get::<String, _>("payload"))?;
        attachment.data_base64.clear();
        attachments.push(SnapshotAttachment {
            attachment,
            device_id: row.get("device_id"),
            hlc: projection::row_hlc(&row),
            sequence: row.get("sequence"),
        });
    }

    let server_sequence = latest_sequence(&state).await?;
    let min_available_sequence = db::min_available_sequence(&state.pool).await?;
    tracing::info!(
        repositories = repositories.len(),
        memos = memos.len(),
        attachments = attachments.len(),
        server_sequence,
        min_available_sequence,
        "snapshot served"
    );
    Ok(Json(SnapshotResponse {
        protocol_version: SYNC_PROTOCOL_VERSION,
        server_sequence,
        min_available_sequence,
        repositories,
        memos,
        attachments,
    }))
}

async fn attachment_blob_manifest(
    State(state): State<AppState>,
    Json(request): Json<AttachmentBlobManifestRequest>,
) -> Result<Json<AttachmentBlobManifestResponse>, ServerError> {
    validation::validate_blob_manifest_request(&request)?;
    cleanup_expired_relay_blobs(&state).await;
    let requested = normalized_hashes(request.content_sha256);
    let found = load_attachment_blob_descriptors(&state, &requested).await?;
    let present = requested
        .iter()
        .filter_map(|hash| found.get(hash).cloned())
        .collect::<Vec<_>>();
    let missing = requested
        .into_iter()
        .filter(|hash| !found.contains_key(hash))
        .collect::<Vec<_>>();
    let server_sequence = latest_sequence(&state).await?;
    tracing::debug!(
        present = present.len(),
        missing = missing.len(),
        server_sequence,
        "attachment blob manifest served"
    );
    Ok(Json(AttachmentBlobManifestResponse {
        protocol_version: SYNC_PROTOCOL_VERSION,
        server_sequence,
        present,
        missing,
    }))
}

async fn fetch_attachment_blobs(
    State(state): State<AppState>,
    Json(request): Json<AttachmentBlobFetchRequest>,
) -> Result<Json<AttachmentBlobFetchResponse>, ServerError> {
    validation::validate_blob_fetch_request(&request)?;
    cleanup_expired_relay_blobs(&state).await;
    let requested = normalized_hashes(request.content_sha256);
    let found = load_attachment_blob_payloads(&state, &requested).await?;
    let blobs = requested
        .iter()
        .filter_map(|hash| found.get(hash).cloned())
        .collect::<Vec<_>>();
    let missing = requested
        .into_iter()
        .filter(|hash| !found.contains_key(hash))
        .collect::<Vec<_>>();
    let server_sequence = latest_sequence(&state).await?;
    tracing::info!(
        blobs = blobs.len(),
        missing = missing.len(),
        server_sequence,
        "attachment blobs fetched"
    );
    Ok(Json(AttachmentBlobFetchResponse {
        protocol_version: SYNC_PROTOCOL_VERSION,
        server_sequence,
        blobs,
        missing,
    }))
}

async fn relay_attachment_blobs(
    State(state): State<AppState>,
    Json(request): Json<AttachmentBlobRelayRequest>,
) -> Result<Json<AttachmentBlobRelayResponse>, ServerError> {
    validation::validate_blob_relay_request(&request)?;
    cleanup_expired_relay_blobs(&state).await;
    let ttl_secs = request
        .ttl_secs
        .unwrap_or(config::DEFAULT_ATTACHMENT_BLOB_RELAY_TTL_SECS)
        .clamp(30, config::MAX_ATTACHMENT_BLOB_RELAY_TTL_SECS);
    let accepted = store_relay_blobs(&state, &request.device_id, request.blobs, ttl_secs).await?;
    let server_sequence = latest_sequence(&state).await?;
    if accepted > 0 {
        let _ = state.tx.send(ServerEvent {
            kind: "attachment_blobs",
            server_sequence,
        });
    }
    tracing::info!(
        device_id = request.device_id,
        accepted,
        ttl_secs,
        server_sequence,
        "attachment blobs relayed"
    );
    Ok(Json(AttachmentBlobRelayResponse {
        protocol_version: SYNC_PROTOCOL_VERSION,
        server_sequence,
        accepted,
        ttl_secs,
    }))
}

async fn wait_for_change(
    State(state): State<AppState>,
    Query(query): Query<WaitQuery>,
) -> Result<Json<WaitResponse>, ServerError> {
    validation::ensure_protocol(query.protocol_version)?;
    let current = latest_sequence(&state).await?;
    if current > query.since_sequence {
        return Ok(Json(WaitResponse {
            changed: true,
            server_sequence: current,
            protocol_version: SYNC_PROTOCOL_VERSION,
        }));
    }

    let mut rx = state.tx.subscribe();
    let timeout_ms = query.timeout_ms.clamp(100, 60_000);
    let event = tokio::time::timeout(Duration::from_millis(timeout_ms), async move {
        loop {
            match rx.recv().await {
                Ok(event)
                    if event.server_sequence > query.since_sequence
                        || event.kind == "attachment_blobs" =>
                {
                    return Some(event)
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten();
    let changed = event.is_some();
    let sequence = match event {
        Some(event) => event.server_sequence,
        None => latest_sequence(&state).await?,
    };
    Ok(Json(WaitResponse {
        changed: changed || sequence > query.since_sequence,
        server_sequence: sequence,
        protocol_version: SYNC_PROTOCOL_VERSION,
    }))
}

async fn events(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl axum::response::IntoResponse {
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

async fn latest_sequence(state: &AppState) -> Result<i64, sqlx::Error> {
    let cached = cached_latest(state);
    if cached > 0 {
        return Ok(cached);
    }
    let sequence = db::latest_sequence_from_db(&state.pool).await?;
    store_latest(state, sequence);
    Ok(sequence)
}

async fn scalar_i64(state: &AppState, sql: &str) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(sql).fetch_one(&state.pool).await?;
    Ok(row.get("value"))
}

fn normalized_hashes(hashes: Vec<String>) -> Vec<String> {
    let mut result = Vec::with_capacity(hashes.len());
    for hash in hashes {
        let hash = hash.to_ascii_lowercase();
        if !result.contains(&hash) {
            result.push(hash);
        }
    }
    result
}

fn metadata_only_operation(mut operation: SyncOperation) -> SyncOperation {
    if let SyncOperationKind::UpsertAttachment(attachment) = &mut operation.kind {
        attachment.data_base64.clear();
    }
    operation
}

async fn relay_inline_attachment_blob(
    state: &AppState,
    operation: &SyncOperation,
    device_id: &str,
) -> Result<(), ServerError> {
    let SyncOperationKind::UpsertAttachment(attachment) = &operation.kind else {
        return Ok(());
    };
    if attachment.data_base64.is_empty() {
        return Ok(());
    }
    let payload = AttachmentBlobPayload {
        descriptor: AttachmentBlobDescriptor {
            content_sha256: attachment.content_sha256.to_ascii_lowercase(),
            media_type: attachment.media_type.clone(),
            byte_len: attachment.byte_len,
        },
        data_base64: attachment.data_base64.clone(),
    };
    validation::validate_blob_payloads(
        std::slice::from_ref(&payload),
        1,
        "inline attachment relay",
    )?;
    store_relay_blobs(
        state,
        device_id,
        vec![payload],
        config::DEFAULT_ATTACHMENT_BLOB_RELAY_TTL_SECS,
    )
    .await?;
    Ok(())
}

async fn cleanup_expired_relay_blobs(state: &AppState) {
    let now = Utc::now();
    let mut relay = state.blob_relay.write().await;
    relay.retain(|_, blob| blob.expires_at > now);
}

async fn store_relay_blobs(
    state: &AppState,
    device_id: &str,
    blobs: Vec<AttachmentBlobPayload>,
    ttl_secs: u64,
) -> Result<usize, ServerError> {
    let now = Utc::now();
    let expires_at = now + ChronoDuration::seconds(i64::try_from(ttl_secs).unwrap_or(i64::MAX / 2));
    let incoming_bytes = blobs
        .iter()
        .map(|blob| blob.descriptor.byte_len)
        .try_fold(0usize, |total, len| total.checked_add(len))
        .ok_or_else(|| ServerError::bad_request("attachment blob relay payload is too large"))?;
    if incoming_bytes > config::MAX_ATTACHMENT_BLOB_RELAY_DEVICE_BYTES {
        return Err(ServerError::bad_request(format!(
            "attachment blob relay accepts at most {} bytes per device window",
            config::MAX_ATTACHMENT_BLOB_RELAY_DEVICE_BYTES
        )));
    }
    let mut accepted = 0usize;
    let mut relay = state.blob_relay.write().await;
    for blob in &blobs {
        relay.remove(&blob.descriptor.content_sha256.to_ascii_lowercase());
    }
    while relay_device_bytes(&relay, device_id) + incoming_bytes
        > config::MAX_ATTACHMENT_BLOB_RELAY_DEVICE_BYTES
    {
        let Some(key) = oldest_relay_key(&relay, Some(device_id)) else {
            break;
        };
        relay.remove(&key);
    }
    while relay_total_bytes(&relay) + incoming_bytes > config::MAX_ATTACHMENT_BLOB_RELAY_BYTES {
        let Some(key) = oldest_relay_key(&relay, None) else {
            break;
        };
        relay.remove(&key);
    }
    if relay_device_bytes(&relay, device_id) + incoming_bytes
        > config::MAX_ATTACHMENT_BLOB_RELAY_DEVICE_BYTES
        || relay_total_bytes(&relay) + incoming_bytes > config::MAX_ATTACHMENT_BLOB_RELAY_BYTES
    {
        return Err(ServerError::bad_request(
            "attachment blob relay capacity is temporarily full",
        ));
    }
    for mut blob in blobs {
        blob.descriptor.content_sha256 = blob.descriptor.content_sha256.to_ascii_lowercase();
        relay.insert(
            blob.descriptor.content_sha256.clone(),
            RelayBlob {
                payload: blob,
                device_id: device_id.to_string(),
                created_at: now,
                expires_at,
            },
        );
        accepted += 1;
    }
    Ok(accepted)
}

async fn relay_blob_stats(state: &AppState) -> (i64, i64, i64) {
    let relay = state.blob_relay.read().await;
    let count = i64::try_from(relay.len()).unwrap_or(i64::MAX);
    let bytes = relay
        .values()
        .map(|blob| i64::try_from(blob.payload.descriptor.byte_len).unwrap_or(0))
        .sum();
    let mut device_ids = Vec::<&str>::new();
    for blob in relay.values() {
        if !device_ids.contains(&blob.device_id.as_str()) {
            device_ids.push(&blob.device_id);
        }
    }
    let device_count = i64::try_from(device_ids.len()).unwrap_or(i64::MAX);
    (count, bytes, device_count)
}

fn relay_total_bytes(relay: &BTreeMap<String, RelayBlob>) -> usize {
    relay
        .values()
        .map(|blob| blob.payload.descriptor.byte_len)
        .sum()
}

fn relay_device_bytes(relay: &BTreeMap<String, RelayBlob>, device_id: &str) -> usize {
    relay
        .values()
        .filter(|blob| blob.device_id == device_id)
        .map(|blob| blob.payload.descriptor.byte_len)
        .sum()
}

fn oldest_relay_key(
    relay: &BTreeMap<String, RelayBlob>,
    device_id: Option<&str>,
) -> Option<String> {
    relay
        .iter()
        .filter(|(_, blob)| device_id.is_none_or(|device_id| blob.device_id == device_id))
        .min_by(|(_, left), (_, right)| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.expires_at.cmp(&right.expires_at))
        })
        .map(|(key, _)| key.clone())
}

async fn load_attachment_blob_descriptors(
    state: &AppState,
    hashes: &[String],
) -> Result<BTreeMap<String, AttachmentBlobDescriptor>, ServerError> {
    if hashes.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT content_sha256, media_type, byte_len FROM attachment_blob_state WHERE content_sha256 IN (",
    );
    let mut separated = query.separated(", ");
    for hash in hashes {
        separated.push_bind(hash);
    }
    separated.push_unseparated(")");
    let rows = query.build().fetch_all(&state.pool).await?;
    let mut descriptors = rows
        .into_iter()
        .map(|row| {
            descriptor_from_row(&row)
                .map(|descriptor| (descriptor.content_sha256.clone(), descriptor))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let relay = state.blob_relay.read().await;
    let now = Utc::now();
    for hash in hashes {
        if descriptors.contains_key(hash) {
            continue;
        }
        if let Some(blob) = relay.get(hash).filter(|blob| blob.expires_at > now) {
            descriptors.insert(hash.clone(), blob.payload.descriptor.clone());
        }
    }
    Ok(descriptors)
}

async fn load_attachment_blob_payloads(
    state: &AppState,
    hashes: &[String],
) -> Result<BTreeMap<String, AttachmentBlobPayload>, ServerError> {
    if hashes.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT content_sha256, media_type, byte_len, data_base64 FROM attachment_blob_state WHERE content_sha256 IN (",
    );
    let mut separated = query.separated(", ");
    for hash in hashes {
        separated.push_bind(hash);
    }
    separated.push_unseparated(")");
    let rows = query.build().fetch_all(&state.pool).await?;
    let mut payloads = rows
        .into_iter()
        .map(|row| {
            let descriptor = descriptor_from_row(&row)?;
            Ok::<(String, AttachmentBlobPayload), ServerError>((
                descriptor.content_sha256.clone(),
                AttachmentBlobPayload {
                    descriptor,
                    data_base64: row.get("data_base64"),
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let relay = state.blob_relay.read().await;
    let now = Utc::now();
    for hash in hashes {
        if payloads.contains_key(hash) {
            continue;
        }
        if let Some(blob) = relay.get(hash).filter(|blob| blob.expires_at > now) {
            payloads.insert(hash.clone(), blob.payload.clone());
        }
    }
    Ok(payloads)
}

fn descriptor_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<AttachmentBlobDescriptor, ServerError> {
    Ok(AttachmentBlobDescriptor {
        content_sha256: row.get("content_sha256"),
        media_type: row.get("media_type"),
        byte_len: usize::try_from(row.get::<i64, _>("byte_len"))
            .map_err(|error| ServerError::bad_request(format!("invalid blob byte_len: {error}")))?,
    })
}
