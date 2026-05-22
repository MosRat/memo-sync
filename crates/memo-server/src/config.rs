pub(crate) const MAX_JSON_BODY_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_PUSH_OPERATIONS: usize = 500;
pub(crate) const MAX_PULL_REPOSITORIES: usize = 256;
pub(crate) const MAX_DEVICE_ID_BYTES: usize = 128;
pub(crate) const MAX_OPERATION_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_BLOB_MANIFEST_ITEMS: usize = 2048;
pub(crate) const MAX_ATTACHMENT_BLOB_FETCH_ITEMS: usize = 128;
pub(crate) const MAX_ATTACHMENT_BLOB_RELAY_ITEMS: usize = 32;
pub(crate) const MAX_ATTACHMENT_BLOB_RELAY_REQUEST_BYTES: usize = 24 * 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_BLOB_RELAY_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_BLOB_RELAY_DEVICE_BYTES: usize = 96 * 1024 * 1024;
pub(crate) const DEFAULT_ATTACHMENT_BLOB_RELAY_TTL_SECS: u64 = 10 * 60;
pub(crate) const MAX_ATTACHMENT_BLOB_RELAY_TTL_SECS: u64 = 30 * 60;

pub(crate) const STATE_PROJECTION_VERSION: &str = "2";
pub(crate) const STATE_PROJECTION_VERSION_KEY: &str = "state_projection_version";
pub(crate) const MIN_AVAILABLE_SEQUENCE_KEY: &str = "min_available_sequence";

pub(crate) const COMPACT_OPERATION_LOG_AFTER: i64 = 4096;
pub(crate) const COMPACT_OPERATION_LOG_RETAIN: i64 = 2048;
