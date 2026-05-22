use crate::{config, error::ServerError};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use memo_core::{
    sha256_hex, AttachmentBlobFetchRequest, AttachmentBlobManifestRequest, AttachmentBlobPayload,
    AttachmentBlobRelayRequest, PullRequest, PushRequest, SyncOperationKind, SYNC_PROTOCOL_VERSION,
};

const ALLOWED_IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

pub(crate) fn ensure_protocol(version: u16) -> Result<(), ServerError> {
    if version == SYNC_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ServerError::UnsupportedProtocol(version))
    }
}

pub(crate) fn validate_push_request(request: &PushRequest) -> Result<(), ServerError> {
    ensure_protocol(request.protocol_version)?;
    if request.device_id.trim().is_empty() {
        return Err(ServerError::bad_request("device_id is required"));
    }
    if request.device_id.len() > config::MAX_DEVICE_ID_BYTES {
        return Err(ServerError::bad_request(format!(
            "device_id accepts at most {} bytes",
            config::MAX_DEVICE_ID_BYTES
        )));
    }
    if request.operations.len() > config::MAX_PUSH_OPERATIONS {
        return Err(ServerError::bad_request(format!(
            "push accepts at most {} operations",
            config::MAX_PUSH_OPERATIONS
        )));
    }
    for operation in &request.operations {
        if operation.device_id != request.device_id {
            return Err(ServerError::bad_request(
                "operation device_id must match request device_id",
            ));
        }
        if operation.repository_id.is_none() {
            return Err(ServerError::bad_request(
                "operation repository_id is required",
            ));
        }
        if operation.hlc.wall_time_ms < 0 {
            return Err(ServerError::bad_request(
                "operation hlc wall_time_ms must be non-negative",
            ));
        }
        if let SyncOperationKind::UpsertAttachment(attachment) = &operation.kind {
            validate_attachment_metadata_fields(
                &attachment.file_name,
                &attachment.media_type,
                attachment.byte_len,
                &attachment.content_sha256,
            )?;
            if !attachment.data_base64.is_empty() {
                validate_attachment_content(
                    attachment.byte_len,
                    &attachment.content_sha256,
                    &attachment.data_base64,
                )?;
            }
        }
        if let SyncOperationKind::PatchMemo { patch, .. } = &operation.kind {
            if patch.title.as_ref().is_some_and(|title| title.len() > 1024) {
                return Err(ServerError::bad_request(
                    "patch title accepts at most 1024 bytes",
                ));
            }
        }
    }
    Ok(())
}

fn validate_attachment_metadata_fields(
    file_name: &str,
    media_type: &str,
    byte_len: usize,
    content_sha256: &str,
) -> Result<(), ServerError> {
    if file_name.trim().is_empty() {
        return Err(ServerError::bad_request("attachment file_name is required"));
    }
    if file_name.len() > 180 {
        return Err(ServerError::bad_request(
            "attachment file_name accepts at most 180 bytes",
        ));
    }
    if !ALLOWED_IMAGE_TYPES.contains(&media_type) {
        return Err(ServerError::bad_request(
            "attachment media_type must be image/png, image/jpeg, image/webp, or image/gif",
        ));
    }
    if byte_len == 0 {
        return Err(ServerError::bad_request("attachment content is empty"));
    }
    if byte_len > config::MAX_ATTACHMENT_BYTES {
        return Err(ServerError::bad_request(format!(
            "attachment content accepts at most {} bytes",
            config::MAX_ATTACHMENT_BYTES
        )));
    }
    if content_sha256.len() != 64 || !content_sha256.chars().all(|char| char.is_ascii_hexdigit()) {
        return Err(ServerError::bad_request(
            "attachment content_sha256 must be a sha-256 hex digest",
        ));
    }
    Ok(())
}

fn validate_attachment_content(
    byte_len: usize,
    content_sha256: &str,
    data_base64: &str,
) -> Result<(), ServerError> {
    let decoded = BASE64_STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|_| ServerError::bad_request("attachment data_base64 is invalid"))?;
    if decoded.len() != byte_len {
        return Err(ServerError::bad_request(
            "attachment byte_len must match decoded content length",
        ));
    }
    if sha256_hex(&decoded) != content_sha256.to_ascii_lowercase() {
        return Err(ServerError::bad_request(
            "attachment content_sha256 must match decoded content",
        ));
    }
    Ok(())
}

pub(crate) fn validate_blob_manifest_request(
    request: &AttachmentBlobManifestRequest,
) -> Result<(), ServerError> {
    ensure_protocol(request.protocol_version)?;
    validate_hash_list(
        &request.content_sha256,
        config::MAX_ATTACHMENT_BLOB_MANIFEST_ITEMS,
        "manifest",
    )
}

pub(crate) fn validate_blob_fetch_request(
    request: &AttachmentBlobFetchRequest,
) -> Result<(), ServerError> {
    ensure_protocol(request.protocol_version)?;
    validate_hash_list(
        &request.content_sha256,
        config::MAX_ATTACHMENT_BLOB_FETCH_ITEMS,
        "blob fetch",
    )
}

pub(crate) fn validate_blob_relay_request(
    request: &AttachmentBlobRelayRequest,
) -> Result<(), ServerError> {
    ensure_protocol(request.protocol_version)?;
    if request.device_id.trim().is_empty() {
        return Err(ServerError::bad_request("device_id is required"));
    }
    if request.device_id.len() > config::MAX_DEVICE_ID_BYTES {
        return Err(ServerError::bad_request(format!(
            "device_id accepts at most {} bytes",
            config::MAX_DEVICE_ID_BYTES
        )));
    }
    validate_blob_payloads(
        &request.blobs,
        config::MAX_ATTACHMENT_BLOB_RELAY_ITEMS,
        "blob relay",
    )
}

pub(crate) fn validate_blob_payloads(
    blobs: &[AttachmentBlobPayload],
    limit: usize,
    label: &str,
) -> Result<(), ServerError> {
    if blobs.len() > limit {
        return Err(ServerError::bad_request(format!(
            "{label} accepts at most {limit} attachment blobs"
        )));
    }
    let total_bytes = blobs
        .iter()
        .map(|blob| blob.descriptor.byte_len)
        .try_fold(0usize, |total, len| total.checked_add(len))
        .ok_or_else(|| ServerError::bad_request(format!("{label} payload is too large")))?;
    if total_bytes > config::MAX_ATTACHMENT_BLOB_RELAY_REQUEST_BYTES {
        return Err(ServerError::bad_request(format!(
            "{label} accepts at most {} bytes per request",
            config::MAX_ATTACHMENT_BLOB_RELAY_REQUEST_BYTES
        )));
    }
    for blob in blobs {
        validate_attachment_metadata_fields(
            "relay",
            &blob.descriptor.media_type,
            blob.descriptor.byte_len,
            &blob.descriptor.content_sha256,
        )?;
        validate_attachment_content(
            blob.descriptor.byte_len,
            &blob.descriptor.content_sha256,
            &blob.data_base64,
        )?;
    }
    Ok(())
}

fn validate_hash_list(hashes: &[String], limit: usize, label: &str) -> Result<(), ServerError> {
    if hashes.len() > limit {
        return Err(ServerError::bad_request(format!(
            "{label} accepts at most {limit} attachment blob hashes"
        )));
    }
    for hash in hashes {
        if hash.len() != 64 || !hash.chars().all(|char| char.is_ascii_hexdigit()) {
            return Err(ServerError::bad_request(
                "attachment blob hash must be a sha-256 hex digest",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_pull_request(request: &PullRequest) -> Result<(), ServerError> {
    ensure_protocol(request.protocol_version)?;
    if request.since_sequence < 0 {
        return Err(ServerError::bad_request(
            "since_sequence must be non-negative",
        ));
    }
    if request.repository_ids.len() > config::MAX_PULL_REPOSITORIES {
        return Err(ServerError::bad_request(format!(
            "pull accepts at most {} repositories",
            config::MAX_PULL_REPOSITORIES
        )));
    }
    if request
        .exclude_device_id
        .as_deref()
        .is_some_and(|device_id| device_id.len() > config::MAX_DEVICE_ID_BYTES)
    {
        return Err(ServerError::bad_request(format!(
            "exclude_device_id accepts at most {} bytes",
            config::MAX_DEVICE_ID_BYTES
        )));
    }
    Ok(())
}
