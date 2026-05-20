use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub type DeviceId = String;
pub const SYNC_PROTOCOL_VERSION: u16 = 1;
pub const DEFAULT_PULL_LIMIT: u16 = 500;

pub fn default_sync_protocol_version() -> u16 {
    SYNC_PROTOCOL_VERSION
}

pub fn default_pull_limit() -> u16 {
    DEFAULT_PULL_LIMIT
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HybridLogicalClock {
    pub wall_time_ms: i64,
    pub counter: u16,
}

impl HybridLogicalClock {
    pub fn now() -> Self {
        Self {
            wall_time_ms: Utc::now().timestamp_millis(),
            counter: 0,
        }
    }

    pub fn tick(self) -> Self {
        let now = Utc::now().timestamp_millis();
        if now > self.wall_time_ms {
            Self {
                wall_time_ms: now,
                counter: 0,
            }
        } else {
            Self {
                wall_time_ms: self.wall_time_ms,
                counter: self.counter.saturating_add(1),
            }
        }
    }
}

impl Ord for HybridLogicalClock {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.wall_time_ms, self.counter).cmp(&(other.wall_time_ms, other.counter))
    }
}

impl PartialOrd for HybridLogicalClock {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepositoryKind {
    Temporary,
    Persistent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub id: Uuid,
    pub name: String,
    pub kind: RepositoryKind,
    pub sync_enabled: bool,
    pub color: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Repository {
    pub fn new(name: impl Into<String>, kind: RepositoryKind, color: impl Into<String>) -> Self {
        let now = Utc::now();
        let sync_enabled = matches!(kind, RepositoryKind::Persistent);
        Self {
            id: Uuid::now_v7(),
            name: name.into(),
            kind,
            sync_enabled,
            color: color.into(),
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memo {
    pub id: Uuid,
    pub repository_id: Uuid,
    pub title: String,
    pub body_md: String,
    pub tags: BTreeSet<String>,
    pub pinned: bool,
    pub archived: bool,
    pub deleted: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source: MemoSource,
    pub meta: MemoMeta,
}

impl Memo {
    pub fn new(repository_id: Uuid, title: impl Into<String>, body_md: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::now_v7(),
            repository_id,
            title: title.into(),
            body_md: body_md.into(),
            tags: BTreeSet::new(),
            pinned: false,
            archived: false,
            deleted: false,
            created_at: now,
            updated_at: now,
            source: MemoSource::Manual,
            meta: MemoMeta::default(),
        }
    }

    pub fn apply_patch(&mut self, patch: MemoPatch) {
        if let Some(title) = patch.title {
            self.title = title;
        }
        if let Some(body_md) = patch.body_md {
            self.body_md = body_md;
        }
        if let Some(tags) = patch.tags {
            self.tags = tags;
        }
        if let Some(pinned) = patch.pinned {
            self.pinned = pinned;
        }
        if let Some(archived) = patch.archived {
            self.archived = archived;
        }
        if let Some(deleted) = patch.deleted {
            self.deleted = deleted;
        }
        if let Some(source) = patch.source {
            self.source = source;
        }
        if let Some(meta) = patch.meta {
            self.meta = meta;
        }
        self.updated_at = Utc::now();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoSource {
    Manual,
    Clipboard,
    QuickCapture,
    Import,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemoMeta {
    pub language: Option<String>,
    pub url: Option<String>,
    pub device_name: Option<String>,
    pub byte_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoPatch {
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub tags: Option<BTreeSet<String>>,
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
    pub deleted: Option<bool>,
    pub source: Option<MemoSource>,
    pub meta: Option<MemoMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncOperationKind {
    UpsertRepository(Repository),
    UpsertMemo(Memo),
    PatchMemo {
        repository_id: Uuid,
        memo_id: Uuid,
        patch: MemoPatch,
    },
    DeleteMemo {
        repository_id: Uuid,
        memo_id: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOperation {
    pub op_id: Uuid,
    pub repository_id: Option<Uuid>,
    pub device_id: DeviceId,
    pub hlc: HybridLogicalClock,
    pub created_at: DateTime<Utc>,
    pub kind: SyncOperationKind,
}

impl SyncOperation {
    pub fn new(
        device_id: impl Into<DeviceId>,
        hlc: HybridLogicalClock,
        kind: SyncOperationKind,
    ) -> Self {
        let repository_id = match &kind {
            SyncOperationKind::UpsertRepository(repo) => Some(repo.id),
            SyncOperationKind::UpsertMemo(memo) => Some(memo.repository_id),
            SyncOperationKind::PatchMemo { repository_id, .. }
            | SyncOperationKind::DeleteMemo { repository_id, .. } => Some(*repository_id),
        };
        Self {
            op_id: Uuid::now_v7(),
            repository_id,
            device_id: device_id.into(),
            hlc,
            created_at: Utc::now(),
            kind,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncDocument {
    pub repositories: BTreeMap<Uuid, Versioned<Repository>>,
    pub memos: BTreeMap<Uuid, Versioned<Memo>>,
}

impl SyncDocument {
    pub fn apply(&mut self, op: SyncOperation) {
        match op.kind {
            SyncOperationKind::UpsertRepository(repo) => {
                merge_versioned(&mut self.repositories, repo.id, repo, &op.device_id, op.hlc);
            }
            SyncOperationKind::UpsertMemo(memo) => {
                merge_versioned(&mut self.memos, memo.id, memo, &op.device_id, op.hlc);
            }
            SyncOperationKind::PatchMemo { memo_id, patch, .. } => {
                if let Some(existing) = self.memos.get_mut(&memo_id) {
                    if wins(existing, &op.device_id, op.hlc) {
                        existing.value.apply_patch(patch);
                        existing.device_id = op.device_id;
                        existing.hlc = op.hlc;
                    }
                }
            }
            SyncOperationKind::DeleteMemo { memo_id, .. } => {
                if let Some(existing) = self.memos.get_mut(&memo_id) {
                    if wins(existing, &op.device_id, op.hlc) {
                        existing.value.deleted = true;
                        existing.value.updated_at = Utc::now();
                        existing.device_id = op.device_id;
                        existing.hlc = op.hlc;
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Versioned<T> {
    pub value: T,
    pub device_id: DeviceId,
    pub hlc: HybridLogicalClock,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoFilter {
    pub repository_id: Option<Uuid>,
    pub query: Option<String>,
    pub tags: BTreeSet<String>,
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
    pub source: Option<MemoSource>,
}

impl MemoFilter {
    pub fn matches(&self, memo: &Memo) -> bool {
        if memo.deleted {
            return false;
        }
        if self
            .repository_id
            .is_some_and(|id| id != memo.repository_id)
        {
            return false;
        }
        if self.pinned.is_some_and(|pinned| pinned != memo.pinned) {
            return false;
        }
        if self
            .archived
            .is_some_and(|archived| archived != memo.archived)
        {
            return false;
        }
        if self
            .source
            .as_ref()
            .is_some_and(|source| source != &memo.source)
        {
            return false;
        }
        if !self.tags.iter().all(|tag| memo.tags.contains(tag)) {
            return false;
        }
        if let Some(query) = &self.query {
            let query = query.to_lowercase();
            let haystack = format!(
                "{} {} {:?} {:?} {:?} {:?} {:?}",
                memo.title,
                memo.body_md,
                memo.tags,
                memo.source,
                memo.meta.language,
                memo.meta.url,
                memo.meta.device_name
            )
            .to_lowercase();
            return haystack.contains(&query);
        }
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushRequest {
    #[serde(default = "default_sync_protocol_version")]
    pub protocol_version: u16,
    pub device_id: DeviceId,
    #[serde(default)]
    pub client: Option<ClientInfo>,
    pub operations: Vec<SyncOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResponse {
    pub protocol_version: u16,
    pub accepted: usize,
    pub server_sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    #[serde(default = "default_sync_protocol_version")]
    pub protocol_version: u16,
    pub since_sequence: i64,
    pub repository_ids: Vec<Uuid>,
    #[serde(default = "default_pull_limit")]
    pub limit: u16,
    #[serde(default)]
    pub client: Option<ClientInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullResponse {
    pub protocol_version: u16,
    pub operations: Vec<ServerOperation>,
    pub server_sequence: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerOperation {
    pub sequence: i64,
    pub operation: SyncOperation,
}

fn merge_versioned<T>(
    map: &mut BTreeMap<Uuid, Versioned<T>>,
    id: Uuid,
    value: T,
    device_id: &str,
    hlc: HybridLogicalClock,
) {
    match map.get(&id) {
        Some(existing) if !wins(existing, device_id, hlc) => {}
        _ => {
            map.insert(
                id,
                Versioned {
                    value,
                    device_id: device_id.to_owned(),
                    hlc,
                },
            );
        }
    }
}

fn wins<T>(existing: &Versioned<T>, device_id: &str, hlc: HybridLogicalClock) -> bool {
    hlc > existing.hlc || (hlc == existing.hlc && device_id > existing.device_id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memo_filter_searches_body_and_tags() {
        let repo = Uuid::now_v7();
        let mut memo = Memo::new(repo, "Release notes", "Ship websocket sync");
        memo.tags.insert("sync".to_string());

        let filter = MemoFilter {
            query: Some("websocket".to_string()),
            tags: BTreeSet::from(["sync".to_string()]),
            ..MemoFilter::default()
        };

        assert!(filter.matches(&memo));
    }

    #[test]
    fn memo_filter_searches_metadata() {
        let repo = Uuid::now_v7();
        let mut memo = Memo::new(repo, "Snippet", "println");
        memo.meta.language = Some("rust".to_string());
        memo.meta.url = Some("https://example.test/spec".to_string());

        let language_filter = MemoFilter {
            query: Some("rust".to_string()),
            ..MemoFilter::default()
        };
        let url_filter = MemoFilter {
            query: Some("spec".to_string()),
            ..MemoFilter::default()
        };

        assert!(language_filter.matches(&memo));
        assert!(url_filter.matches(&memo));
    }

    #[test]
    fn higher_hlc_wins_conflicting_memo_updates() {
        let repo = Uuid::now_v7();
        let memo = Memo::new(repo, "Old", "body");
        let mut newer = memo.clone();
        newer.title = "New".to_string();

        let mut doc = SyncDocument::default();
        doc.apply(SyncOperation::new(
            "a",
            HybridLogicalClock {
                wall_time_ms: 10,
                counter: 0,
            },
            SyncOperationKind::UpsertMemo(newer),
        ));
        doc.apply(SyncOperation::new(
            "b",
            HybridLogicalClock {
                wall_time_ms: 9,
                counter: 0,
            },
            SyncOperationKind::UpsertMemo(memo),
        ));

        assert_eq!(doc.memos.values().next().unwrap().value.title, "New");
    }

    #[test]
    fn device_id_breaks_equal_clock_ties() {
        let repo = Uuid::now_v7();
        let mut first = Memo::new(repo, "Alpha", "");
        let mut second = first.clone();
        second.title = "Beta".to_string();
        first.title = "Alpha".to_string();

        let mut doc = SyncDocument::default();
        let hlc = HybridLogicalClock {
            wall_time_ms: 10,
            counter: 1,
        };
        doc.apply(SyncOperation::new(
            "device-a",
            hlc,
            SyncOperationKind::UpsertMemo(first),
        ));
        doc.apply(SyncOperation::new(
            "device-b",
            hlc,
            SyncOperationKind::UpsertMemo(second),
        ));

        assert_eq!(doc.memos.values().next().unwrap().value.title, "Beta");
    }

    #[test]
    fn patch_and_delete_operations_keep_repository_scope() {
        let repo = Uuid::now_v7();
        let memo = Memo::new(repo, "Scoped", "");
        let memo_id = memo.id;
        let mut doc = SyncDocument::default();
        doc.apply(SyncOperation::new(
            "device-a",
            HybridLogicalClock {
                wall_time_ms: 1,
                counter: 0,
            },
            SyncOperationKind::UpsertMemo(memo),
        ));

        let patch_op = SyncOperation::new(
            "device-a",
            HybridLogicalClock {
                wall_time_ms: 2,
                counter: 0,
            },
            SyncOperationKind::PatchMemo {
                repository_id: repo,
                memo_id,
                patch: MemoPatch {
                    title: Some("Renamed".to_string()),
                    body_md: None,
                    tags: None,
                    pinned: None,
                    archived: None,
                    deleted: None,
                    source: None,
                    meta: None,
                },
            },
        );
        assert_eq!(patch_op.repository_id, Some(repo));
        doc.apply(patch_op);
        assert_eq!(doc.memos.get(&memo_id).unwrap().value.title, "Renamed");

        let delete_op = SyncOperation::new(
            "device-a",
            HybridLogicalClock {
                wall_time_ms: 3,
                counter: 0,
            },
            SyncOperationKind::DeleteMemo {
                repository_id: repo,
                memo_id,
            },
        );
        assert_eq!(delete_op.repository_id, Some(repo));
        doc.apply(delete_op);
        assert!(doc.memos.get(&memo_id).unwrap().value.deleted);
    }
}
