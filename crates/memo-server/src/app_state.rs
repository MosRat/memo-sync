use chrono::{DateTime, Utc};
use memo_core::AttachmentBlobPayload;
use sqlx::SqlitePool;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
};
use tokio::sync::{broadcast, RwLock};

#[derive(Clone)]
pub struct AppState {
    pub(crate) pool: SqlitePool,
    pub(crate) tx: broadcast::Sender<ServerEvent>,
    pub(crate) latest_sequence: Arc<AtomicI64>,
    pub(crate) blob_relay: Arc<RwLock<BTreeMap<String, RelayBlob>>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerEvent {
    pub kind: &'static str,
    pub server_sequence: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct RelayBlob {
    pub payload: AttachmentBlobPayload,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

pub fn state(pool: SqlitePool) -> AppState {
    let (tx, _) = broadcast::channel(1024);
    AppState {
        pool,
        tx,
        latest_sequence: Arc::new(AtomicI64::new(0)),
        blob_relay: Arc::new(RwLock::new(BTreeMap::new())),
    }
}

pub(crate) fn remember_latest(state: &AppState, sequence: i64) {
    state.latest_sequence.fetch_max(sequence, Ordering::Relaxed);
}

pub(crate) fn cached_latest(state: &AppState) -> i64 {
    state.latest_sequence.load(Ordering::Relaxed)
}

pub(crate) fn store_latest(state: &AppState, sequence: i64) {
    state.latest_sequence.store(sequence, Ordering::Relaxed);
}
