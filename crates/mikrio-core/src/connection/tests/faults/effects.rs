//! Recovery at each write interruption and I/O failure point.

use super::*;

#[test]
fn kill_publication_blob_ready_preserves_complete_generation() -> TestResult {
    kill_publications(&["publication.blob.ready"])
}

#[test]
fn error_publication_blob_ready_preserves_complete_generation() -> TestResult {
    publication_error("publication.blob.ready")
}

#[test]
fn kill_publication_object_replaced_preserves_complete_generation() -> TestResult {
    kill_publications(&["publication.object.replaced"])
}

#[test]
fn error_publication_object_replaced_preserves_complete_generation() -> TestResult {
    publication_error("publication.object.replaced")
}

#[test]
fn kill_publication_predecessor_retired_preserves_complete_generation() -> TestResult {
    kill_publications(&["publication.predecessor.retired"])
}

#[test]
fn error_publication_predecessor_retired_preserves_complete_generation() -> TestResult {
    publication_error("publication.predecessor.retired")
}

#[test]
fn kill_reservation_begun_recovers_complete_generation() -> TestResult {
    kill_publications(&["reservation.begun"])
}

#[test]
fn kill_reservation_commit_before_recovers_complete_generation() -> TestResult {
    kill_publications(&["reservation.commit.before"])
}

#[test]
fn kill_reservation_commit_after_recovers_complete_generation() -> TestResult {
    kill_publications(&["reservation.commit.after"])
}

#[test]
fn kill_staging_create_before_recovers_complete_generation() -> TestResult {
    kill_publications(&["staging.create.before"])
}

#[test]
fn kill_staging_create_after_recovers_complete_generation() -> TestResult {
    kill_publications(&["staging.create.after"])
}

#[test]
fn kill_staging_write_before_recovers_complete_generation() -> TestResult {
    kill_publications(&["staging.write.before"])
}

#[test]
fn kill_staging_write_after_recovers_complete_generation() -> TestResult {
    kill_publications(&["staging.write.after"])
}

#[test]
fn kill_integrity_verified_recovers_complete_generation() -> TestResult {
    kill_publications(&["integrity.verified"])
}

#[test]
fn kill_fs_file_sync_before_recovers_complete_generation() -> TestResult {
    kill_publications(&["fs.FileSync.Before"])
}

#[test]
fn kill_fs_file_sync_after_recovers_complete_generation() -> TestResult {
    kill_publications(&["fs.FileSync.After"])
}

#[test]
fn kill_fs_rename_before_recovers_complete_generation() -> TestResult {
    kill_publications(&["fs.Rename.Before"])
}

#[test]
fn kill_fs_rename_after_recovers_complete_generation() -> TestResult {
    kill_publications(&["fs.Rename.After"])
}

#[test]
fn kill_fs_destination_sync_before_recovers_complete_generation() -> TestResult {
    kill_publications(&["fs.DestinationSync.Before"])
}

#[test]
fn kill_fs_destination_sync_after_recovers_complete_generation() -> TestResult {
    kill_publications(&["fs.DestinationSync.After"])
}

#[test]
fn kill_fs_source_sync_before_recovers_complete_generation() -> TestResult {
    kill_publications(&["fs.SourceSync.Before"])
}

#[test]
fn kill_fs_source_sync_after_recovers_complete_generation() -> TestResult {
    kill_publications(&["fs.SourceSync.After"])
}

#[test]
fn kill_payload_published_recovers_complete_generation() -> TestResult {
    kill_publications(&["payload.published"])
}

#[test]
fn kill_commit_fence_entered_recovers_complete_generation() -> TestResult {
    kill_publications(&["commit_fence.entered"])
}

#[test]
fn kill_publication_begun_recovers_complete_generation() -> TestResult {
    kill_publications(&["publication.begun"])
}

#[test]
fn kill_publication_commit_before_recovers_complete_generation() -> TestResult {
    kill_publications(&["publication.commit.before"])
}

#[test]
fn kill_publication_commit_after_recovers_complete_generation() -> TestResult {
    kill_publications(&["publication.commit.after"])
}

#[test]
fn error_reservation_commit_after_preserves_mutation_outcome() -> TestResult {
    publication_error("reservation.commit.after")
}

#[test]
fn error_staging_write_after_preserves_mutation_outcome() -> TestResult {
    publication_error("staging.write.after")
}

#[test]
fn error_fs_file_sync_after_preserves_mutation_outcome() -> TestResult {
    publication_error("fs.FileSync.After")
}

#[test]
fn error_fs_rename_after_preserves_mutation_outcome() -> TestResult {
    publication_error("fs.Rename.After")
}

#[test]
fn error_fs_destination_sync_after_preserves_mutation_outcome() -> TestResult {
    publication_error("fs.DestinationSync.After")
}

#[test]
fn error_fs_source_sync_after_preserves_mutation_outcome() -> TestResult {
    publication_error("fs.SourceSync.After")
}

#[test]
fn error_publication_commit_before_preserves_mutation_outcome() -> TestResult {
    publication_error("publication.commit.before")
}

#[test]
fn error_publication_commit_after_preserves_mutation_outcome() -> TestResult {
    publication_error("publication.commit.after")
}
