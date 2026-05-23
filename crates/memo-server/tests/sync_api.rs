use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use http_body_util::BodyExt;
use memo_core::{
    AttachmentBlobFetchRequest, AttachmentBlobFetchResponse, AttachmentBlobManifestRequest,
    AttachmentBlobManifestResponse, AttachmentBlobPayload, AttachmentBlobRelayRequest,
    AttachmentBlobRelayResponse, HybridLogicalClock, Memo, MemoAttachment, MemoPatch, PullRequest,
    PullResponse, PushRequest, PushResponse, Repository, RepositoryKind, SnapshotResponse,
    SyncOperation, SyncOperationKind, DEFAULT_PULL_LIMIT, SYNC_PROTOCOL_VERSION,
};
use memo_server::{open_file_pool, open_pool, router, state};
use serde::{de::DeserializeOwned, Deserialize};
use sqlx::Row;
use tower::ServiceExt;

#[tokio::test]
async fn push_is_idempotent_and_pull_returns_ordered_operations() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "First", "Hello");
    let op = SyncOperation::new(
        "test-device",
        HybridLogicalClock {
            wall_time_ms: 100,
            counter: 0,
        },
        SyncOperationKind::UpsertMemo(memo),
    );

    let push = PushRequest {
        protocol_version: SYNC_PROTOCOL_VERSION,
        device_id: "test-device".to_string(),
        client: None,
        operations: vec![op.clone(), op],
    };
    let response = app
        .clone()
        .oneshot(json_request("/api/v1/sync/push", &push))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let pushed: PushResponse = read_json(response).await;
    assert_eq!(pushed.accepted, 1);

    let pull = PullRequest {
        protocol_version: SYNC_PROTOCOL_VERSION,
        since_sequence: 0,
        repository_ids: vec![],
        exclude_device_id: None,
        limit: DEFAULT_PULL_LIMIT,
        client: None,
    };
    let response = app
        .oneshot(json_request("/api/v1/sync/pull", &pull))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let pulled: PullResponse = read_json(response).await;
    assert_eq!(pulled.operations.len(), 1);
    assert_eq!(pulled.operations[0].sequence, 1);
}

#[tokio::test]
async fn pull_can_be_scoped_to_repository_including_deletes() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let first_repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let second_repo = Repository::new("Home", RepositoryKind::Persistent, "#6f8f83");
    let first_memo = Memo::new(first_repo.id, "First", "A");
    let second_memo = Memo::new(second_repo.id, "Second", "B");
    let delete_first = SyncOperation::new(
        "test-device",
        HybridLogicalClock {
            wall_time_ms: 102,
            counter: 0,
        },
        SyncOperationKind::DeleteMemo {
            repository_id: first_repo.id,
            memo_id: first_memo.id,
        },
    );

    let push = PushRequest {
        protocol_version: SYNC_PROTOCOL_VERSION,
        device_id: "test-device".to_string(),
        client: None,
        operations: vec![
            SyncOperation::new(
                "test-device",
                HybridLogicalClock {
                    wall_time_ms: 100,
                    counter: 0,
                },
                SyncOperationKind::UpsertMemo(first_memo),
            ),
            SyncOperation::new(
                "test-device",
                HybridLogicalClock {
                    wall_time_ms: 101,
                    counter: 0,
                },
                SyncOperationKind::UpsertMemo(second_memo),
            ),
            delete_first,
        ],
    };

    let pushed = app
        .clone()
        .oneshot(json_request("/api/v1/sync/push", &push))
        .await
        .unwrap();
    assert_eq!(pushed.status(), StatusCode::OK);

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                since_sequence: 0,
                repository_ids: vec![first_repo.id],
                exclude_device_id: None,
                limit: DEFAULT_PULL_LIMIT,
                client: None,
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let pulled: PullResponse = read_json(response).await;
    assert_eq!(pulled.operations.len(), 2);
    assert!(pulled
        .operations
        .iter()
        .all(|item| item.operation.repository_id == Some(first_repo.id)));
    assert!(pulled
        .operations
        .iter()
        .any(|item| matches!(&item.operation.kind, SyncOperationKind::DeleteMemo { .. })));
}

#[tokio::test]
async fn pull_respects_limit_and_reports_more_pages() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let operations = (0..3)
        .map(|index| {
            SyncOperation::new(
                "test-device",
                HybridLogicalClock {
                    wall_time_ms: 200 + index,
                    counter: 0,
                },
                SyncOperationKind::UpsertMemo(Memo::new(repo.id, format!("Memo {index}"), "Body")),
            )
        })
        .collect::<Vec<_>>();

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "test-device".to_string(),
                client: None,
                operations,
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                since_sequence: 0,
                repository_ids: vec![],
                exclude_device_id: None,
                limit: 2,
                client: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let pulled: PullResponse = read_json(response).await;
    assert_eq!(pulled.operations.len(), 2);
    assert!(pulled.has_more);
}

#[tokio::test]
async fn pull_can_exclude_requesting_device_echoes() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let own = SyncOperation::new(
        "device-a",
        HybridLogicalClock {
            wall_time_ms: 220,
            counter: 0,
        },
        SyncOperationKind::UpsertMemo(Memo::new(repo.id, "Own", "Body")),
    );
    let remote = SyncOperation::new(
        "device-b",
        HybridLogicalClock {
            wall_time_ms: 221,
            counter: 0,
        },
        SyncOperationKind::UpsertMemo(Memo::new(repo.id, "Remote", "Body")),
    );

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![own],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-b".to_string(),
                client: None,
                operations: vec![remote],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                since_sequence: 0,
                repository_ids: vec![],
                exclude_device_id: Some("device-a".to_string()),
                limit: DEFAULT_PULL_LIMIT,
                client: None,
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let pulled: PullResponse = read_json(response).await;
    assert_eq!(pulled.operations.len(), 1);
    assert_eq!(pulled.operations[0].operation.device_id, "device-b");
}

#[tokio::test]
async fn snapshot_returns_current_projected_state() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Snapshot", "Current");

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 230,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertRepository(repo.clone()),
                    ),
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 231,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertMemo(memo.clone()),
                    ),
                ],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(get_request("/api/v1/sync/snapshot?protocol_version=1"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let snapshot: SnapshotResponse = read_json(response).await;
    assert_eq!(snapshot.server_sequence, 2);
    assert!(snapshot
        .repositories
        .iter()
        .any(|item| item.repository.id == repo.id));
    assert!(snapshot.memos.iter().any(|item| item.memo.id == memo.id));
}

#[tokio::test]
async fn snapshot_excludes_deleted_memos() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Gone", "Deleted remotely");

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 232,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertMemo(memo.clone()),
                    ),
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 233,
                            counter: 0,
                        },
                        SyncOperationKind::DeleteMemo {
                            repository_id: repo.id,
                            memo_id: memo.id,
                        },
                    ),
                ],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(get_request("/api/v1/sync/snapshot?protocol_version=1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot: SnapshotResponse = read_json(response).await;
    assert!(snapshot.memos.iter().all(|item| item.memo.id != memo.id));
    assert_eq!(snapshot.server_sequence, 2);
}

#[tokio::test]
async fn snapshot_projects_patch_operations() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Draft", "old body");

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 234,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertMemo(memo.clone()),
                    ),
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 235,
                            counter: 0,
                        },
                        SyncOperationKind::PatchMemo {
                            repository_id: repo.id,
                            memo_id: memo.id,
                            patch: MemoPatch {
                                title: Some("Published".to_string()),
                                body_md: Some("new body".to_string()),
                                tags: None,
                                pinned: Some(true),
                                archived: None,
                                deleted: None,
                                source: None,
                                meta: None,
                            },
                        },
                    ),
                ],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(get_request("/api/v1/sync/snapshot?protocol_version=1"))
        .await
        .unwrap();
    let snapshot: SnapshotResponse = read_json(response).await;
    let projected = snapshot
        .memos
        .iter()
        .find(|item| item.memo.id == memo.id)
        .unwrap();
    assert_eq!(projected.memo.title, "Published");
    assert_eq!(projected.memo.body_md, "new body");
    assert!(projected.memo.pinned);
}

#[tokio::test]
async fn snapshot_projects_image_attachments_and_deletes() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "With image", "body");
    let kept = MemoAttachment::new(memo.id, repo.id, "kept.png", "image/png", 4, "AQIDBA==");
    let deleted = MemoAttachment::new(memo.id, repo.id, "deleted.png", "image/png", 4, "AQIDBA==");

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 236,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertMemo(memo),
                    ),
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 237,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertAttachment(kept.clone()),
                    ),
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 238,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertAttachment(deleted.clone()),
                    ),
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 239,
                            counter: 0,
                        },
                        SyncOperationKind::DeleteAttachment {
                            repository_id: repo.id,
                            attachment_id: deleted.id,
                        },
                    ),
                ],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(get_request("/api/v1/sync/snapshot?protocol_version=1"))
        .await
        .unwrap();
    let snapshot: SnapshotResponse = read_json(response).await;
    assert!(snapshot
        .attachments
        .iter()
        .any(|item| item.attachment.id == kept.id));
    assert!(snapshot
        .attachments
        .iter()
        .all(|item| item.attachment.id != deleted.id));
}

#[tokio::test]
async fn snapshot_keeps_newer_projection_when_older_operation_arrives_late() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let current = Memo::new(repo.id, "Current", "new");
    let mut stale = current.clone();
    stale.title = "Stale".to_string();
    stale.body_md = "old".to_string();

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-b".to_string(),
                client: None,
                operations: vec![SyncOperation::new(
                    "device-b",
                    HybridLogicalClock {
                        wall_time_ms: 240,
                        counter: 0,
                    },
                    SyncOperationKind::UpsertMemo(current.clone()),
                )],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-z".to_string(),
                client: None,
                operations: vec![SyncOperation::new(
                    "device-z",
                    HybridLogicalClock {
                        wall_time_ms: 239,
                        counter: 0,
                    },
                    SyncOperationKind::UpsertMemo(stale),
                )],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(get_request("/api/v1/sync/snapshot?protocol_version=1"))
        .await
        .unwrap();
    let snapshot: SnapshotResponse = read_json(response).await;
    let projected = snapshot
        .memos
        .iter()
        .find(|item| item.memo.id == current.id)
        .unwrap();
    assert_eq!(projected.memo.title, "Current");
    assert_eq!(projected.memo.body_md, "new");
}

#[tokio::test]
async fn snapshot_projection_rebuilds_from_existing_operation_log() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("server.sqlite");
    let pool = open_file_pool(&db).await.unwrap();
    let app = router(state(pool.clone()));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Recovered", "from log");

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![SyncOperation::new(
                    "device-a",
                    HybridLogicalClock {
                        wall_time_ms: 245,
                        counter: 0,
                    },
                    SyncOperationKind::UpsertMemo(memo.clone()),
                )],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    sqlx::query("DELETE FROM memo_state")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM repository_state")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM sync_meta")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let reopened = open_file_pool(&db).await.unwrap();
    let app = router(state(reopened));
    let response = app
        .oneshot(get_request("/api/v1/sync/snapshot?protocol_version=1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot: SnapshotResponse = read_json(response).await;
    assert!(snapshot.memos.iter().any(|item| item.memo.id == memo.id));
    assert_eq!(snapshot.server_sequence, 1);
}

#[tokio::test]
async fn compacted_log_requires_snapshot_for_stale_clients() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let mut pushed = 0usize;

    while pushed < 4100 {
        let batch_size = (4100 - pushed).min(500);
        let operations = (0..batch_size)
            .map(|offset| {
                let index = pushed + offset;
                SyncOperation::new(
                    "device-a",
                    HybridLogicalClock {
                        wall_time_ms: 1_000 + i64::try_from(index).unwrap(),
                        counter: 0,
                    },
                    SyncOperationKind::UpsertMemo(Memo::new(
                        repo.id,
                        format!("Memo {index}"),
                        "Body",
                    )),
                )
            })
            .collect::<Vec<_>>();
        let response = app
            .clone()
            .oneshot(json_request(
                "/api/v1/sync/push",
                &PushRequest {
                    protocol_version: SYNC_PROTOCOL_VERSION,
                    device_id: "device-a".to_string(),
                    client: None,
                    operations,
                },
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        pushed += batch_size;
    }

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                since_sequence: 0,
                repository_ids: vec![],
                exclude_device_id: None,
                limit: DEFAULT_PULL_LIMIT,
                client: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stale_pull: PullResponse = read_json(response).await;
    assert!(stale_pull.snapshot_required);
    assert!(stale_pull.operations.is_empty());
    assert_eq!(stale_pull.server_sequence, 4100);
    assert!(stale_pull.min_available_sequence > 0);

    let response = app
        .clone()
        .oneshot(get_request("/api/v1/sync/snapshot?protocol_version=1"))
        .await
        .unwrap();
    let snapshot: SnapshotResponse = read_json(response).await;
    assert_eq!(
        snapshot.min_available_sequence,
        stale_pull.min_available_sequence
    );
    assert_eq!(snapshot.server_sequence, 4100);

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                since_sequence: stale_pull.min_available_sequence,
                repository_ids: vec![],
                exclude_device_id: None,
                limit: 10,
                client: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let current_pull: PullResponse = read_json(response).await;
    assert!(!current_pull.snapshot_required);
    assert_eq!(current_pull.operations.len(), 10);
    assert_eq!(
        current_pull.operations[0].sequence,
        stale_pull.min_available_sequence + 1
    );
}

#[tokio::test]
async fn rejects_unsupported_protocol_version() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let response = app
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION + 1,
                since_sequence: 0,
                repository_ids: vec![],
                exclude_device_id: None,
                limit: DEFAULT_PULL_LIMIT,
                client: None,
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = read_json(response).await;
    assert_eq!(error.code, "unsupported_protocol");
    assert!(error.message.contains("unsupported sync protocol version"));
}

#[tokio::test]
async fn rejects_push_operations_from_a_different_device() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let op = SyncOperation::new(
        "other-device",
        HybridLogicalClock {
            wall_time_ms: 250,
            counter: 0,
        },
        SyncOperationKind::UpsertMemo(Memo::new(repo.id, "Mismatch", "Body")),
    );

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "request-device".to_string(),
                client: None,
                operations: vec![op],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rejects_malformed_push_metadata() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let mut op = SyncOperation::new(
        "test-device",
        HybridLogicalClock {
            wall_time_ms: -1,
            counter: 0,
        },
        SyncOperationKind::UpsertMemo(Memo::new(repo.id, "Bad clock", "Body")),
    );
    op.repository_id = None;

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "test-device".to_string(),
                client: None,
                operations: vec![op],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rejects_oversized_push_payload() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let op = SyncOperation::new(
        "test-device",
        HybridLogicalClock {
            wall_time_ms: 260,
            counter: 0,
        },
        SyncOperationKind::UpsertMemo(Memo::new(repo.id, "Large", "x".repeat(2200 * 1024))),
    );

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "test-device".to_string(),
                client: None,
                operations: vec![op],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = read_json(response).await;
    assert_eq!(error.code, "bad_request");
    assert!(error.message.contains("operation payload accepts at most"));
}

#[tokio::test]
async fn rejects_negative_pull_sequence() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                since_sequence: -1,
                repository_ids: vec![],
                exclude_device_id: None,
                limit: DEFAULT_PULL_LIMIT,
                client: None,
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rejects_too_many_repository_filters_with_structured_error() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let response = app
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                since_sequence: 0,
                repository_ids: (0..257).map(|_| uuid::Uuid::now_v7()).collect(),
                exclude_device_id: None,
                limit: DEFAULT_PULL_LIMIT,
                client: None,
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = read_json(response).await;
    assert_eq!(error.code, "bad_request");
    assert!(error.message.contains("pull accepts at most"));
}

#[tokio::test]
async fn rejects_oversized_patch_title() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Title", "Body");
    let op = SyncOperation::new(
        "device-a",
        HybridLogicalClock {
            wall_time_ms: 270,
            counter: 0,
        },
        SyncOperationKind::PatchMemo {
            repository_id: repo.id,
            memo_id: memo.id,
            patch: MemoPatch {
                title: Some("x".repeat(1025)),
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

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![op],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = read_json(response).await;
    assert_eq!(error.code, "bad_request");
    assert!(error.message.contains("patch title accepts at most"));
}

#[tokio::test]
async fn rejects_unsupported_attachment_media_type() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "With svg", "body");
    let attachment = MemoAttachment::new(
        memo.id,
        repo.id,
        "unsafe.svg",
        "image/svg+xml",
        11,
        "PHN2Zz48L3N2Zz4=",
    );

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![SyncOperation::new(
                    "device-a",
                    HybridLogicalClock {
                        wall_time_ms: 280,
                        counter: 0,
                    },
                    SyncOperationKind::UpsertAttachment(attachment),
                )],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = read_json(response).await;
    assert_eq!(error.code, "bad_request");
    assert!(error.message.contains("attachment media_type"));
}

#[tokio::test]
async fn rejects_malformed_attachment_base64() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Broken image", "body");
    let attachment = MemoAttachment::new(
        memo.id,
        repo.id,
        "broken.png",
        "image/png",
        4,
        "not base64!",
    );

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![SyncOperation::new(
                    "device-a",
                    HybridLogicalClock {
                        wall_time_ms: 281,
                        counter: 0,
                    },
                    SyncOperationKind::UpsertAttachment(attachment),
                )],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = read_json(response).await;
    assert_eq!(error.code, "bad_request");
    assert!(error.message.contains("data_base64 is invalid"));
}

#[tokio::test]
async fn rejects_attachment_byte_len_mismatch() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Mismatched image", "body");
    let attachment =
        MemoAttachment::new(memo.id, repo.id, "mismatch.png", "image/png", 5, "AQIDBA==");

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![SyncOperation::new(
                    "device-a",
                    HybridLogicalClock {
                        wall_time_ms: 282,
                        counter: 0,
                    },
                    SyncOperationKind::UpsertAttachment(attachment),
                )],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = read_json(response).await;
    assert_eq!(error.code, "bad_request");
    assert!(error.message.contains("byte_len must match"));
}

#[tokio::test]
async fn rejects_attachment_content_hash_mismatch() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Mismatched image hash", "body");
    let mut attachment =
        MemoAttachment::new(memo.id, repo.id, "mismatch.png", "image/png", 4, "AQIDBA==");
    attachment.content_sha256 = "0".repeat(64);

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![SyncOperation::new(
                    "device-a",
                    HybridLogicalClock {
                        wall_time_ms: 282,
                        counter: 1,
                    },
                    SyncOperationKind::UpsertAttachment(attachment),
                )],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = read_json(response).await;
    assert_eq!(error.code, "bad_request");
    assert!(error.message.contains("content_sha256 must match"));
}

#[tokio::test]
async fn accepts_small_image_attachment_with_inline_payload() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Inline image", "body");
    let bytes = vec![7u8; 600 * 1024];
    let attachment = MemoAttachment::new(
        memo.id,
        repo.id,
        "inline.png",
        "image/png",
        bytes.len(),
        BASE64_STANDARD.encode(bytes),
    );

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![SyncOperation::new(
                    "device-a",
                    HybridLogicalClock {
                        wall_time_ms: 283,
                        counter: 0,
                    },
                    SyncOperationKind::UpsertAttachment(attachment),
                )],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn attachment_blob_manifest_and_fetch_are_content_addressed() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool.clone()));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Image", "body");
    let data_base64 = BASE64_STANDARD.encode([1, 2, 3, 4]);
    let first = MemoAttachment::new(
        memo.id,
        repo.id,
        "first.png",
        "image/png",
        4,
        data_base64.clone(),
    );
    let second = MemoAttachment::new(memo.id, repo.id, "second.png", "image/png", 4, data_base64);
    let missing_hash = "f".repeat(64);

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 290,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertAttachment(first.clone()),
                    ),
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 291,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertAttachment(second),
                    ),
                ],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let blob_count: i64 = sqlx::query("SELECT COUNT(*) AS value FROM attachment_blob_state")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("value");
    assert_eq!(blob_count, 0);

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/attachment-blobs/manifest",
            &AttachmentBlobManifestRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                content_sha256: vec![first.content_sha256.clone(), missing_hash.clone()],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let manifest: AttachmentBlobManifestResponse = read_json(response).await;
    assert_eq!(manifest.present.len(), 1);
    assert_eq!(manifest.present[0].content_sha256, first.content_sha256);
    assert_eq!(manifest.present[0].byte_len, 4);
    assert_eq!(manifest.missing, vec![missing_hash.clone()]);

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/attachment-blobs/fetch",
            &AttachmentBlobFetchRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                content_sha256: vec![first.content_sha256.clone(), missing_hash.clone()],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched: AttachmentBlobFetchResponse = read_json(response).await;
    assert_eq!(fetched.blobs.len(), 1);
    assert_eq!(
        fetched.blobs[0].descriptor.content_sha256,
        first.content_sha256
    );
    assert_eq!(
        BASE64_STANDARD
            .decode(fetched.blobs[0].data_base64.as_bytes())
            .unwrap(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(fetched.missing, vec![missing_hash]);
}

#[tokio::test]
async fn inline_attachment_push_persists_metadata_only_and_relays_blob() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool.clone()));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Image", "body");
    let attachment = MemoAttachment::new(
        memo.id,
        repo.id,
        "inline.png",
        "image/png",
        4,
        BASE64_STANDARD.encode([9, 8, 7, 6]),
    );

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![SyncOperation::new(
                    "device-a",
                    HybridLogicalClock {
                        wall_time_ms: 291,
                        counter: 1,
                    },
                    SyncOperationKind::UpsertAttachment(attachment.clone()),
                )],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let payload: String = sqlx::query("SELECT payload FROM operation_log LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("payload");
    let stored_operation: SyncOperation = serde_json::from_str(&payload).unwrap();
    let SyncOperationKind::UpsertAttachment(stored_attachment) = stored_operation.kind else {
        panic!("stored operation should be an attachment upsert");
    };
    assert!(stored_attachment.data_base64.is_empty());
    assert_eq!(stored_attachment.content_sha256, attachment.content_sha256);

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                since_sequence: 0,
                repository_ids: vec![],
                exclude_device_id: None,
                limit: DEFAULT_PULL_LIMIT,
                client: None,
            },
        ))
        .await
        .unwrap();
    let pull: PullResponse = read_json(response).await;
    let SyncOperationKind::UpsertAttachment(pulled_attachment) = &pull.operations[0].operation.kind
    else {
        panic!("pulled operation should be an attachment upsert");
    };
    assert!(pulled_attachment.data_base64.is_empty());

    let response = app
        .clone()
        .oneshot(get_request("/api/v1/sync/snapshot?protocol_version=1"))
        .await
        .unwrap();
    let snapshot: SnapshotResponse = read_json(response).await;
    assert_eq!(snapshot.attachments.len(), 1);
    assert!(snapshot.attachments[0].attachment.data_base64.is_empty());

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/attachment-blobs/fetch",
            &AttachmentBlobFetchRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                content_sha256: vec![attachment.content_sha256.clone()],
            },
        ))
        .await
        .unwrap();
    let fetched: AttachmentBlobFetchResponse = read_json(response).await;
    assert_eq!(fetched.blobs.len(), 1);
    assert_eq!(
        BASE64_STANDARD
            .decode(fetched.blobs[0].data_base64.as_bytes())
            .unwrap(),
        vec![9, 8, 7, 6]
    );
}

#[tokio::test]
async fn attachment_blob_relay_accepts_metadata_first_uploads() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let data_base64 = BASE64_STANDARD.encode([6, 5, 4, 3]);
    let attachment = MemoAttachment::new(
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
        "relay.png",
        "image/png",
        4,
        data_base64.clone(),
    );

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/attachment-blobs/relay",
            &AttachmentBlobRelayRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                blobs: vec![AttachmentBlobPayload {
                    descriptor: memo_core::AttachmentBlobDescriptor {
                        content_sha256: attachment.content_sha256.clone(),
                        media_type: attachment.media_type.clone(),
                        byte_len: attachment.byte_len,
                    },
                    data_base64,
                }],
                ttl_secs: Some(60),
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let relay: AttachmentBlobRelayResponse = read_json(response).await;
    assert_eq!(relay.accepted, 1);

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/attachment-blobs/manifest",
            &AttachmentBlobManifestRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                content_sha256: vec![attachment.content_sha256.clone()],
            },
        ))
        .await
        .unwrap();
    let manifest: AttachmentBlobManifestResponse = read_json(response).await;
    assert_eq!(manifest.present.len(), 1);
    assert!(manifest.missing.is_empty());
}

#[tokio::test]
async fn wait_reports_attachment_blob_relay_events_without_sequence_change() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let data_base64 = BASE64_STANDARD.encode([1, 3, 5, 7]);
    let attachment = MemoAttachment::new(
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
        "relay-wakeup.png",
        "image/png",
        4,
        data_base64.clone(),
    );

    let wait_app = app.clone();
    let wait_task = tokio::spawn(async move {
        wait_app
            .oneshot(get_request(
                "/api/v1/sync/wait?protocol_version=1&since_sequence=0&timeout_ms=5000",
            ))
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/attachment-blobs/relay",
            &AttachmentBlobRelayRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                blobs: vec![AttachmentBlobPayload {
                    descriptor: memo_core::AttachmentBlobDescriptor {
                        content_sha256: attachment.content_sha256,
                        media_type: attachment.media_type,
                        byte_len: attachment.byte_len,
                    },
                    data_base64,
                }],
                ttl_secs: Some(60),
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = wait_task.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let changed: WaitResponse = read_json(response).await;
    assert!(changed.changed);
    assert_eq!(changed.server_sequence, 0);
}

#[tokio::test]
async fn deleted_attachment_releases_unreferenced_server_blob() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool.clone()));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Image", "body");
    let attachment = MemoAttachment::new(
        memo.id,
        repo.id,
        "first.png",
        "image/png",
        4,
        BASE64_STANDARD.encode([4, 3, 2, 1]),
    );

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 292,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertAttachment(attachment.clone()),
                    ),
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 293,
                            counter: 0,
                        },
                        SyncOperationKind::DeleteAttachment {
                            repository_id: repo.id,
                            attachment_id: attachment.id,
                        },
                    ),
                ],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let blob_count: i64 = sqlx::query("SELECT COUNT(*) AS value FROM attachment_blob_state")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("value");
    assert_eq!(blob_count, 0);
}

#[tokio::test]
async fn rejects_invalid_attachment_blob_manifest_hashes() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/attachment-blobs/manifest",
            &AttachmentBlobManifestRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                content_sha256: vec!["not-a-sha".to_string()],
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = read_json(response).await;
    assert_eq!(error.code, "bad_request");
    assert!(error.message.contains("sha-256 hex digest"));
}

#[tokio::test]
async fn wait_reports_existing_or_new_sequence_changes() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let operation = SyncOperation::new(
        "test-device",
        HybridLogicalClock {
            wall_time_ms: 300,
            counter: 0,
        },
        SyncOperationKind::UpsertMemo(Memo::new(repo.id, "Wait", "Body")),
    );

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "test-device".to_string(),
                client: None,
                operations: vec![operation],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(get_request(
            "/api/v1/sync/wait?protocol_version=1&since_sequence=0&timeout_ms=100",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let changed: WaitResponse = read_json(response).await;
    assert!(changed.changed);
    assert_eq!(changed.server_sequence, 1);

    let response = app
        .oneshot(get_request(
            "/api/v1/sync/wait?protocol_version=1&since_sequence=1&timeout_ms=100",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let unchanged: WaitResponse = read_json(response).await;
    assert!(!unchanged.changed);
    assert_eq!(unchanged.server_sequence, 1);
}

#[tokio::test]
async fn health_reports_sequence_and_relay_metrics() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let memo = Memo::new(repo.id, "Health", "Body");
    let data_base64 = BASE64_STANDARD.encode([1, 2, 3, 4, 5]);
    let attachment = MemoAttachment::new(
        memo.id,
        repo.id,
        "health.png",
        "image/png",
        5,
        data_base64.clone(),
    );

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations: vec![
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 310,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertMemo(memo),
                    ),
                    SyncOperation::new(
                        "device-a",
                        HybridLogicalClock {
                            wall_time_ms: 311,
                            counter: 0,
                        },
                        SyncOperationKind::UpsertAttachment(attachment),
                    ),
                ],
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/attachment-blobs/relay",
            &AttachmentBlobRelayRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-b".to_string(),
                blobs: vec![AttachmentBlobPayload {
                    descriptor: memo_core::AttachmentBlobDescriptor {
                        content_sha256:
                            "74f81fe167d99b4cb41d6d0ccda82278caee9f3e2f25d5e5a3936ff3dcec60d0"
                                .to_string(),
                        media_type: "image/png".to_string(),
                        byte_len: 5,
                    },
                    data_base64,
                }],
                ttl_secs: Some(60),
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app.oneshot(get_request("/health")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let health: HealthResponse = read_json(response).await;
    assert!(health.ok);
    assert_eq!(health.protocol_version, SYNC_PROTOCOL_VERSION);
    assert_eq!(health.server_sequence, 2);
    assert_eq!(health.min_available_sequence, 0);
    assert_eq!(health.attachment_count, 1);
    assert_eq!(health.attachment_blob_count, 0);
    assert_eq!(health.attachment_blob_bytes, 0);
    assert_eq!(health.relay_blob_count, 1);
    assert_eq!(health.relay_blob_bytes, 5);
    assert_eq!(health.relay_device_count, 1);
}

#[tokio::test]
async fn pull_clamps_zero_limit_to_one_operation() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let app = router(state(pool));
    let repo = Repository::new("Work", RepositoryKind::Persistent, "#cc785c");
    let operations = (0..2)
        .map(|index| {
            SyncOperation::new(
                "device-a",
                HybridLogicalClock {
                    wall_time_ms: 320 + index,
                    counter: 0,
                },
                SyncOperationKind::UpsertMemo(Memo::new(repo.id, format!("Memo {index}"), "Body")),
            )
        })
        .collect::<Vec<_>>();

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/v1/sync/push",
            &PushRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                device_id: "device-a".to_string(),
                client: None,
                operations,
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(json_request(
            "/api/v1/sync/pull",
            &PullRequest {
                protocol_version: SYNC_PROTOCOL_VERSION,
                since_sequence: 0,
                repository_ids: vec![],
                exclude_device_id: None,
                limit: 0,
                client: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let pulled: PullResponse = read_json(response).await;
    assert_eq!(pulled.operations.len(), 1);
    assert!(pulled.has_more);
    assert_eq!(pulled.server_sequence, 1);
}

#[derive(Deserialize)]
struct HealthResponse {
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
struct WaitResponse {
    changed: bool,
    server_sequence: i64,
}

#[derive(Deserialize)]
struct ErrorResponse {
    code: String,
    message: String,
}

fn json_request<T: serde::Serialize>(uri: &str, body: &T) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn get_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

async fn read_json<T: DeserializeOwned>(response: axum::response::Response) -> T {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
