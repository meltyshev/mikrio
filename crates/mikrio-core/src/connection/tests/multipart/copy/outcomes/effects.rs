//! Part-copy fault schedules retain independent outer watchdogs.

use super::*;

#[test]
fn kill_part_publication_blob_ready_preserves_complete_part() -> TestResult {
    publication_effect("kill:part.publication.blob.ready")
}

#[test]
fn error_part_publication_blob_ready_preserves_complete_part() -> TestResult {
    publication_effect("error:part.publication.blob.ready")
}

#[test]
fn kill_part_publication_part_replaced_preserves_complete_part() -> TestResult {
    publication_effect("kill:part.publication.part.replaced")
}

#[test]
fn error_part_publication_part_replaced_preserves_complete_part() -> TestResult {
    publication_effect("error:part.publication.part.replaced")
}

#[test]
fn kill_part_publication_predecessor_retired_preserves_complete_part() -> TestResult {
    publication_effect("kill:part.publication.predecessor.retired")
}

#[test]
fn error_part_publication_predecessor_retired_preserves_complete_part() -> TestResult {
    publication_effect("error:part.publication.predecessor.retired")
}

#[test]
fn kill_copy_selected_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:copy.selected")
}

#[test]
fn kill_part_reservation_begun_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:part.reservation.begun")
}

#[test]
fn kill_part_reservation_commit_before_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:part.reservation.commit.before")
}

#[test]
fn kill_part_reservation_commit_after_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:part.reservation.commit.after")
}

#[test]
fn kill_part_copy_admitted_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:part.copy.admitted")
}

#[test]
fn kill_stored_read_before_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:stored.read.before")
}

#[test]
fn kill_stored_read_after_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:stored.read.after")
}

#[test]
fn kill_staging_create_after_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:staging.create.after")
}

#[test]
fn kill_staging_write_after_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:staging.write.after")
}

#[test]
fn kill_integrity_verified_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:integrity.verified")
}

#[test]
fn kill_fs_file_sync_before_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:fs.FileSync.Before")
}

#[test]
fn kill_fs_file_sync_after_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:fs.FileSync.After")
}

#[test]
fn kill_fs_rename_before_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:fs.Rename.Before")
}

#[test]
fn kill_fs_rename_after_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:fs.Rename.After")
}

#[test]
fn kill_fs_destination_sync_after_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:fs.DestinationSync.After")
}

#[test]
fn kill_fs_source_sync_after_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:fs.SourceSync.After")
}

#[test]
fn kill_payload_published_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:payload.published")
}

#[test]
fn kill_part_commit_fence_entered_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:part.commit_fence.entered")
}

#[test]
fn kill_part_publication_begun_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:part.publication.begun")
}

#[test]
fn kill_part_publication_commit_before_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:part.publication.commit.before")
}

#[test]
fn kill_part_publication_commit_after_preserves_one_complete_part() -> TestResult {
    publication_effect("kill:part.publication.commit.after")
}

#[test]
fn error_part_reservation_commit_after_preserves_one_complete_part() -> TestResult {
    publication_effect("error:part.reservation.commit.after")
}

#[test]
fn error_stored_read_after_preserves_one_complete_part() -> TestResult {
    publication_effect("error:stored.read.after")
}

#[test]
fn error_part_publication_commit_before_preserves_one_complete_part() -> TestResult {
    publication_effect("error:part.publication.commit.before")
}

#[test]
fn error_part_publication_commit_after_preserves_one_complete_part() -> TestResult {
    publication_effect("error:part.publication.commit.after")
}

#[test]
fn unknown_witness_preserves_one_complete_part() -> TestResult {
    publication_effect("unknown:witness")
}
