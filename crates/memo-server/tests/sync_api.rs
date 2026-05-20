use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use memo_core::{
    HybridLogicalClock, Memo, PullRequest, PullResponse, PushRequest, PushResponse, Repository,
    RepositoryKind, SyncOperation, SyncOperationKind, DEFAULT_PULL_LIMIT, SYNC_PROTOCOL_VERSION,
};
use memo_server::{open_pool, router, state};
use serde::{de::DeserializeOwned, Deserialize};
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
                limit: DEFAULT_PULL_LIMIT,
                client: None,
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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

#[derive(Deserialize)]
struct WaitResponse {
    changed: bool,
    server_sequence: i64,
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
