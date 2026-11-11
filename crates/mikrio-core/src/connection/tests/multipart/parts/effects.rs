//! Part publication schedules retain independent outer watchdogs.

use super::*;

#[test]
fn kill_part_publication_blob_ready_preserves_complete_part() -> TestResult {
    part_death("part.publication.blob.ready")
}

#[test]
fn error_part_publication_blob_ready_preserves_complete_part() -> TestResult {
    part_error("error:part.publication.blob.ready")
}

#[test]
fn kill_part_publication_part_replaced_preserves_complete_part() -> TestResult {
    part_death("part.publication.part.replaced")
}

#[test]
fn error_part_publication_part_replaced_preserves_complete_part() -> TestResult {
    part_error("error:part.publication.part.replaced")
}

#[test]
fn kill_part_publication_predecessor_retired_preserves_complete_part() -> TestResult {
    part_death("part.publication.predecessor.retired")
}

#[test]
fn error_part_publication_predecessor_retired_preserves_complete_part() -> TestResult {
    part_error("error:part.publication.predecessor.retired")
}

#[test]
fn kill_part_reservation_begun_preserves_complete_part() -> TestResult {
    part_death("part.reservation.begun")
}

#[test]
fn kill_part_reservation_commit_before_preserves_complete_part() -> TestResult {
    part_death("part.reservation.commit.before")
}

#[test]
fn kill_part_reservation_commit_after_preserves_complete_part() -> TestResult {
    part_death("part.reservation.commit.after")
}

#[test]
fn kill_staging_create_before_preserves_complete_part() -> TestResult {
    part_death("staging.create.before")
}

#[test]
fn kill_staging_create_after_preserves_complete_part() -> TestResult {
    part_death("staging.create.after")
}

#[test]
fn kill_staging_write_before_preserves_complete_part() -> TestResult {
    part_death("staging.write.before")
}

#[test]
fn kill_staging_write_after_preserves_complete_part() -> TestResult {
    part_death("staging.write.after")
}

#[test]
fn kill_integrity_verified_preserves_complete_part() -> TestResult {
    part_death("integrity.verified")
}

#[test]
fn kill_fs_file_sync_before_preserves_complete_part() -> TestResult {
    part_death("fs.FileSync.Before")
}

#[test]
fn kill_fs_file_sync_after_preserves_complete_part() -> TestResult {
    part_death("fs.FileSync.After")
}

#[test]
fn kill_fs_rename_before_preserves_complete_part() -> TestResult {
    part_death("fs.Rename.Before")
}

#[test]
fn kill_fs_rename_after_preserves_complete_part() -> TestResult {
    part_death("fs.Rename.After")
}

#[test]
fn kill_fs_destination_sync_before_preserves_complete_part() -> TestResult {
    part_death("fs.DestinationSync.Before")
}

#[test]
fn kill_fs_destination_sync_after_preserves_complete_part() -> TestResult {
    part_death("fs.DestinationSync.After")
}

#[test]
fn kill_fs_source_sync_before_preserves_complete_part() -> TestResult {
    part_death("fs.SourceSync.Before")
}

#[test]
fn kill_fs_source_sync_after_preserves_complete_part() -> TestResult {
    part_death("fs.SourceSync.After")
}

#[test]
fn kill_payload_published_preserves_complete_part() -> TestResult {
    part_death("payload.published")
}

#[test]
fn kill_part_commit_fence_entered_preserves_complete_part() -> TestResult {
    part_death("part.commit_fence.entered")
}

#[test]
fn kill_part_publication_begun_preserves_complete_part() -> TestResult {
    part_death("part.publication.begun")
}

#[test]
fn kill_part_publication_commit_before_preserves_complete_part() -> TestResult {
    part_death("part.publication.commit.before")
}

#[test]
fn kill_part_publication_commit_after_preserves_complete_part() -> TestResult {
    part_death("part.publication.commit.after")
}

#[test]
fn kill_acknowledged_preserves_complete_part() -> TestResult {
    part_death("acknowledged")
}

#[test]
fn error_part_reservation_commit_after_preserves_complete_part() -> TestResult {
    part_error("error:part.reservation.commit.after")
}

#[test]
fn error_staging_write_after_preserves_complete_part() -> TestResult {
    part_error("error:staging.write.after")
}

#[test]
fn error_fs_file_sync_after_preserves_complete_part() -> TestResult {
    part_error("error:fs.FileSync.After")
}

#[test]
fn error_fs_rename_after_preserves_complete_part() -> TestResult {
    part_error("error:fs.Rename.After")
}

#[test]
fn error_fs_destination_sync_after_preserves_complete_part() -> TestResult {
    part_error("error:fs.DestinationSync.After")
}

#[test]
fn error_fs_source_sync_after_preserves_complete_part() -> TestResult {
    part_error("error:fs.SourceSync.After")
}

#[test]
fn error_part_publication_commit_before_preserves_complete_part() -> TestResult {
    part_error("error:part.publication.commit.before")
}

#[test]
fn error_part_publication_commit_after_preserves_complete_part() -> TestResult {
    part_error("error:part.publication.commit.after")
}

#[test]
fn unknown_publication_witness_before_preserves_complete_part() -> TestResult {
    part_error("unknown:publication.witness.before")
}
