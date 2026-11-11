//! Every publication checkpoint has an independent outer watchdog.

use super::*;

#[test]
fn kill_publication_blob_ready_preserves_complete_destination() -> TestResult {
    publication_effect("kill:publication.blob.ready")
}

#[test]
fn error_publication_blob_ready_preserves_complete_destination() -> TestResult {
    publication_effect("error:publication.blob.ready")
}

#[test]
fn kill_publication_object_replaced_preserves_complete_destination() -> TestResult {
    publication_effect("kill:publication.object.replaced")
}

#[test]
fn error_publication_object_replaced_preserves_complete_destination() -> TestResult {
    publication_effect("error:publication.object.replaced")
}

#[test]
fn kill_publication_predecessor_retired_preserves_complete_destination() -> TestResult {
    publication_effect("kill:publication.predecessor.retired")
}

#[test]
fn error_publication_predecessor_retired_preserves_complete_destination() -> TestResult {
    publication_effect("error:publication.predecessor.retired")
}

#[test]
fn kill_copy_selected_recovers_complete_destination() -> TestResult {
    publication_effect("kill:copy.selected")
}

#[test]
fn kill_reservation_begun_recovers_complete_destination() -> TestResult {
    publication_effect("kill:reservation.begun")
}

#[test]
fn kill_reservation_commit_before_recovers_complete_destination() -> TestResult {
    publication_effect("kill:reservation.commit.before")
}

#[test]
fn kill_reservation_commit_after_recovers_complete_destination() -> TestResult {
    publication_effect("kill:reservation.commit.after")
}

#[test]
fn kill_copy_admitted_recovers_complete_destination() -> TestResult {
    publication_effect("kill:copy.admitted")
}

#[test]
fn kill_stored_read_before_recovers_complete_destination() -> TestResult {
    publication_effect("kill:stored.read.before")
}

#[test]
fn kill_stored_read_after_recovers_complete_destination() -> TestResult {
    publication_effect("kill:stored.read.after")
}

#[test]
fn kill_staging_create_after_recovers_complete_destination() -> TestResult {
    publication_effect("kill:staging.create.after")
}

#[test]
fn kill_staging_write_after_recovers_complete_destination() -> TestResult {
    publication_effect("kill:staging.write.after")
}

#[test]
fn kill_integrity_verified_recovers_complete_destination() -> TestResult {
    publication_effect("kill:integrity.verified")
}

#[test]
fn kill_fs_file_sync_before_recovers_complete_destination() -> TestResult {
    publication_effect("kill:fs.FileSync.Before")
}

#[test]
fn kill_fs_file_sync_after_recovers_complete_destination() -> TestResult {
    publication_effect("kill:fs.FileSync.After")
}

#[test]
fn kill_fs_rename_before_recovers_complete_destination() -> TestResult {
    publication_effect("kill:fs.Rename.Before")
}

#[test]
fn kill_fs_rename_after_recovers_complete_destination() -> TestResult {
    publication_effect("kill:fs.Rename.After")
}

#[test]
fn kill_fs_destination_sync_after_recovers_complete_destination() -> TestResult {
    publication_effect("kill:fs.DestinationSync.After")
}

#[test]
fn kill_fs_source_sync_after_recovers_complete_destination() -> TestResult {
    publication_effect("kill:fs.SourceSync.After")
}

#[test]
fn kill_payload_published_recovers_complete_destination() -> TestResult {
    publication_effect("kill:payload.published")
}

#[test]
fn kill_copy_commit_fence_recovers_complete_destination() -> TestResult {
    publication_effect("kill:copy.commit_fence")
}

#[test]
fn kill_publication_begun_recovers_complete_destination() -> TestResult {
    publication_effect("kill:publication.begun")
}

#[test]
fn kill_publication_commit_before_recovers_complete_destination() -> TestResult {
    publication_effect("kill:publication.commit.before")
}

#[test]
fn kill_publication_commit_after_recovers_complete_destination() -> TestResult {
    publication_effect("kill:publication.commit.after")
}

#[test]
fn error_reservation_commit_after_recovers_complete_destination() -> TestResult {
    publication_effect("error:reservation.commit.after")
}

#[test]
fn error_stored_read_after_recovers_complete_destination() -> TestResult {
    publication_effect("error:stored.read.after")
}

#[test]
fn error_publication_commit_before_recovers_complete_destination() -> TestResult {
    publication_effect("error:publication.commit.before")
}

#[test]
fn error_publication_commit_after_recovers_complete_destination() -> TestResult {
    publication_effect("error:publication.commit.after")
}

#[test]
fn unknown_witness_recovers_complete_destination() -> TestResult {
    publication_effect("unknown:witness")
}
