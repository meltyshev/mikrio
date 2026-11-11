//! Completion fault schedules retain independent outer watchdogs.

use super::*;

#[test]
fn error_multipart_complete_layout_part_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.layout.part")
}

#[test]
fn error_multipart_complete_object_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.object")
}

#[test]
fn error_multipart_complete_parts_retired_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.parts-retired")
}

#[test]
fn error_multipart_complete_receipt_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.receipt")
}

#[test]
fn kill_multipart_complete_blob_ready_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.blob.ready")
}

#[test]
fn error_multipart_complete_blob_ready_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.blob.ready")
}

#[test]
fn kill_multipart_complete_parts_marked_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.parts.marked")
}

#[test]
fn error_multipart_complete_parts_marked_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.parts.marked")
}

#[test]
fn kill_multipart_complete_upload_removed_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.upload.removed")
}

#[test]
fn error_multipart_complete_upload_removed_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.upload.removed")
}

#[test]
fn kill_multipart_complete_predecessor_retired_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.predecessor.retired")
}

#[test]
fn error_multipart_complete_predecessor_retired_preserves_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.predecessor.retired")
}

#[test]
fn kill_multipart_complete_selected_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.selected")
}

#[test]
fn kill_multipart_claim_begun_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.claim.begun")
}

#[test]
fn kill_multipart_claim_commit_before_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.claim.commit.before")
}

#[test]
fn kill_multipart_claim_commit_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.claim.commit.after")
}

#[test]
fn kill_multipart_complete_claimed_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.claimed")
}

#[test]
fn kill_stored_read_before_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:stored.read.before")
}

#[test]
fn kill_stored_read_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:stored.read.after")
}

#[test]
fn kill_staging_create_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:staging.create.after")
}

#[test]
fn kill_staging_write_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:staging.write.after")
}

#[test]
fn kill_integrity_verified_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:integrity.verified")
}

#[test]
fn kill_fs_file_sync_before_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:fs.FileSync.Before")
}

#[test]
fn kill_fs_file_sync_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:fs.FileSync.After")
}

#[test]
fn kill_fs_rename_before_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:fs.Rename.Before")
}

#[test]
fn kill_fs_rename_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:fs.Rename.After")
}

#[test]
fn kill_fs_destination_sync_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:fs.DestinationSync.After")
}

#[test]
fn kill_fs_source_sync_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:fs.SourceSync.After")
}

#[test]
fn kill_payload_published_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:payload.published")
}

#[test]
fn kill_multipart_complete_commit_fence_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.commit_fence")
}

#[test]
fn kill_multipart_complete_begun_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.begun")
}

#[test]
fn kill_multipart_complete_layout_part_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.layout.part")
}

#[test]
fn kill_multipart_complete_object_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.object")
}

#[test]
fn kill_multipart_complete_parts_retired_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.parts-retired")
}

#[test]
fn kill_multipart_complete_receipt_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.receipt")
}

#[test]
fn kill_multipart_complete_commit_before_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.commit.before")
}

#[test]
fn kill_multipart_complete_commit_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("kill:multipart.complete.commit.after")
}

#[test]
fn error_multipart_claim_commit_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.claim.commit.after")
}

#[test]
fn error_stored_read_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("error:stored.read.after")
}

#[test]
fn error_multipart_complete_commit_before_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.commit.before")
}

#[test]
fn error_multipart_complete_commit_after_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("error:multipart.complete.commit.after")
}

#[test]
fn unknown_receipt_recovers_parts_or_complete_object() -> TestResult {
    completion_effect("unknown:receipt")
}
