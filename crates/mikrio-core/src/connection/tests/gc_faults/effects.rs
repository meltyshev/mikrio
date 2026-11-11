//! Independently discovered collector schedules preserve the same retry and ownership assertions.

use super::*;

#[test]
fn kill_single_gc_selected_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.selected", 1)], Layout::Single)
}

#[test]
fn kill_single_gc_mark_begun_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.mark.begun", 1)], Layout::Single)
}

#[test]
fn kill_single_gc_mark_commit_before_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.mark.commit.before", 1)], Layout::Single)
}

#[test]
fn kill_single_gc_mark_commit_after_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.mark.commit.after", 1)], Layout::Single)
}

#[test]
fn kill_single_gc_finish_begun_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.finish.begun", 1)], Layout::Single)
}

#[test]
fn kill_single_gc_finish_commit_before_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.finish.commit.before", 1)], Layout::Single)
}

#[test]
fn kill_single_gc_finish_commit_after_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.finish.commit.after", 1)], Layout::Single)
}

#[test]
fn kill_single_gc_settled_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.settled", 1)], Layout::Single)
}

#[test]
fn kill_single_fs_unlink_before_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("fs.Unlink.Before", 1)], Layout::Single)
}

#[test]
fn kill_single_fs_unlink_after_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("fs.Unlink.After", 1)], Layout::Single)
}

#[test]
fn kill_single_fs_unlink_sync_before_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("fs.UnlinkSync.Before", 1)], Layout::Single)
}

#[test]
fn kill_single_fs_unlink_sync_after_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("fs.UnlinkSync.After", 1)], Layout::Single)
}

#[test]
fn kill_single_fs_unlink_before_2_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("fs.Unlink.Before", 2)], Layout::Single)
}

#[test]
fn kill_single_fs_unlink_after_2_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("fs.Unlink.After", 2)], Layout::Single)
}

#[test]
fn kill_single_fs_unlink_sync_before_2_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("fs.UnlinkSync.Before", 2)], Layout::Single)
}

#[test]
fn kill_single_fs_unlink_sync_after_2_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("fs.UnlinkSync.After", 2)], Layout::Single)
}

#[test]
fn error_single_gc_mark_begun_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("gc.mark.begun", 1)], Layout::Single)
}

#[test]
fn error_single_gc_mark_commit_before_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("gc.mark.commit.before", 1)], Layout::Single)
}

#[test]
fn error_single_gc_mark_commit_after_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("gc.mark.commit.after", 1)], Layout::Single)
}

#[test]
fn error_single_fs_unlink_before_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("fs.Unlink.Before", 1)], Layout::Single)
}

#[test]
fn error_single_fs_unlink_after_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("fs.Unlink.After", 1)], Layout::Single)
}

#[test]
fn error_single_fs_unlink_sync_before_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("fs.UnlinkSync.Before", 1)], Layout::Single)
}

#[test]
fn error_single_fs_unlink_sync_after_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("fs.UnlinkSync.After", 1)], Layout::Single)
}

#[test]
fn error_single_fs_unlink_before_2_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("fs.Unlink.Before", 2)], Layout::Single)
}

#[test]
fn error_single_fs_unlink_after_2_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("fs.Unlink.After", 2)], Layout::Single)
}

#[test]
fn error_single_fs_unlink_sync_before_2_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("fs.UnlinkSync.Before", 2)], Layout::Single)
}

#[test]
fn error_single_fs_unlink_sync_after_2_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("fs.UnlinkSync.After", 2)], Layout::Single)
}

#[test]
fn error_single_gc_finish_begun_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("gc.finish.begun", 1)], Layout::Single)
}

#[test]
fn error_single_gc_finish_commit_before_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("gc.finish.commit.before", 1)], Layout::Single)
}

#[test]
fn error_single_gc_finish_commit_after_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("gc.finish.commit.after", 1)], Layout::Single)
}

#[test]
fn kill_completed_gc_finish_begun_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.finish.begun", 1)], Layout::Completed)
}

#[test]
fn kill_completed_gc_finish_commit_before_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.finish.commit.before", 1)], Layout::Completed)
}

#[test]
fn kill_completed_gc_finish_commit_after_1_preserves_retry_authority() -> TestResult {
    interruptions("kill", &[("gc.finish.commit.after", 1)], Layout::Completed)
}

#[test]
fn error_completed_gc_finish_begun_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("gc.finish.begun", 1)], Layout::Completed)
}

#[test]
fn error_completed_gc_finish_commit_before_1_preserves_retry_authority() -> TestResult {
    interruptions(
        "error",
        &[("gc.finish.commit.before", 1)],
        Layout::Completed,
    )
}

#[test]
fn error_completed_gc_finish_commit_after_1_preserves_retry_authority() -> TestResult {
    interruptions("error", &[("gc.finish.commit.after", 1)], Layout::Completed)
}
