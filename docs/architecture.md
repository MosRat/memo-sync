# Architecture

Memo Sync uses an operation-log sync model rather than last-write-only document replacement.

## Local-first data flow

1. The desktop app writes every edit into local SQLite and appends a signed logical operation.
2. The sync worker batches pending operations and pushes them to the server.
3. The server stores operations in an append-only SQLite WAL table with unique operation IDs.
4. Clients pull operations newer than their last acknowledged sequence and apply them with deterministic conflict rules.

## Conflict handling

- Every operation carries a hybrid logical clock (`wall_time_ms`, `counter`, `device_id`).
- Field-level memo updates win by the highest HLC, then by device ID for a stable tie break.
- Deletes are tombstones and win over older edits.
- Delete and patch operations carry `repository_id`, so scoped pulls still receive tombstones.
- Temporary repositories are deliberately local-only, are not pushed, and their memos are purged on app startup.

## Protocol

- `POST /api/v1/sync/push`: idempotently append client operations.
- `POST /api/v1/sync/pull`: fetch operations after a server sequence.
- `GET /api/v1/events`: WebSocket notification channel for low-latency pulls.

SQLite is configured for WAL, busy timeout, normalized synchronous mode, and bounded connection pools so it behaves well in small VPS/container deployments.

## App Settings

Desktop settings are stored in the Tauri app data directory as `settings.json`.

- Sync endpoint persists across restarts.
- Quick capture and clipboard capture shortcuts are user-configurable.
- Shortcut updates are validated before saving; if OS registration fails, the app attempts to restore the previous working shortcuts.
- The web preview keeps equivalent settings in `localStorage` and hides desktop-only shortcut controls.
