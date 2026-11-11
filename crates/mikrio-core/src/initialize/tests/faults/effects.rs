//! Every initializer interruption retains its own command watchdog.

use super::*;

#[test]
fn kill_initialize_guard_create_before_preserves_state() -> TestResult {
    kills(&[("initialize.guard.create.before", None)])
}

#[test]
fn kill_initialize_guard_create_after_preserves_state() -> TestResult {
    kills(&[(
        "initialize.guard.create.after",
        Some(OpenFailure::InvalidStoreDirectory),
    )])
}

#[test]
fn kill_initialize_guard_file_sync_after_preserves_state() -> TestResult {
    kills(&[(
        "initialize.guard.file_sync.after",
        Some(OpenFailure::InvalidStoreDirectory),
    )])
}

#[test]
fn kill_initialize_guard_directory_sync_after_preserves_state() -> TestResult {
    kills(&[(
        "initialize.guard.directory_sync.after",
        Some(OpenFailure::InvalidStoreDirectory),
    )])
}

#[test]
fn kill_initializing_marker_create_before_preserves_state() -> TestResult {
    kills(&[(
        "initializing.marker.create.before",
        Some(OpenFailure::InvalidStoreDirectory),
    )])
}

#[test]
fn kill_initializing_marker_create_after_preserves_state() -> TestResult {
    kills(&[(
        "initializing.marker.create.after",
        Some(OpenFailure::InvalidStoreDirectory),
    )])
}

#[test]
fn kill_initializing_marker_write_after_preserves_state() -> TestResult {
    kills(&[(
        "initializing.marker.write.after",
        Some(OpenFailure::InvalidStoreDirectory),
    )])
}

#[test]
fn kill_initializing_marker_file_sync_after_preserves_state() -> TestResult {
    kills(&[(
        "initializing.marker.file_sync.after",
        Some(OpenFailure::InvalidStoreDirectory),
    )])
}

#[test]
fn kill_initializing_marker_publish_before_preserves_state() -> TestResult {
    kills(&[(
        "initializing.marker.publish.before",
        Some(OpenFailure::InvalidStoreDirectory),
    )])
}

#[test]
fn kill_initializing_marker_publish_after_preserves_state() -> TestResult {
    kills(&[("initializing.marker.publish.after", None)])
}

#[test]
fn kill_bootstrap_database_create_before_preserves_state() -> TestResult {
    kills(&[("bootstrap.database.create.before", None)])
}

#[test]
fn kill_bootstrap_database_create_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.database.create.after", None)])
}

#[test]
fn kill_bootstrap_database_file_sync_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.database.file_sync.after", None)])
}

#[test]
fn kill_bootstrap_database_directory_sync_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.database.directory_sync.after", None)])
}

#[test]
fn kill_bootstrap_layout_parent_create_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.layout.parent_create.after", None)])
}

#[test]
fn kill_bootstrap_layout_shard_create_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.layout.shard_create.after", None)])
}

#[test]
fn kill_bootstrap_layout_shard_sync_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.layout.shard_sync.after", None)])
}

#[test]
fn kill_bootstrap_layout_parent_sync_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.layout.parent_sync.after", None)])
}

#[test]
fn kill_bootstrap_layout_guard_create_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.layout.guard_create.after", None)])
}

#[test]
fn kill_bootstrap_layout_guard_sync_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.layout.guard_sync.after", None)])
}

#[test]
fn kill_bootstrap_layout_locks_sync_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.layout.locks_sync.after", None)])
}

#[test]
fn kill_bootstrap_sql_begin_before_preserves_state() -> TestResult {
    kills(&[("bootstrap.sql.begin.before", None)])
}

#[test]
fn kill_bootstrap_sql_begin_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.sql.begin.after", None)])
}

#[test]
fn kill_bootstrap_sql_schema_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.sql.schema.after", None)])
}

#[test]
fn kill_bootstrap_sql_singleton_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.sql.singleton.after", None)])
}

#[test]
fn kill_bootstrap_sql_sweeps_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.sql.sweeps.after", None)])
}

#[test]
fn kill_bootstrap_sql_commit_before_preserves_state() -> TestResult {
    kills(&[("bootstrap.sql.commit.before", None)])
}

#[test]
fn kill_bootstrap_sql_commit_after_preserves_state() -> TestResult {
    kills(&[("bootstrap.sql.commit.after", None)])
}

#[test]
fn kill_ready_layout_directory_sync_after_preserves_state() -> TestResult {
    kills(&[("ready.layout.directory_sync.after", None)])
}

#[test]
fn kill_ready_marker_create_before_preserves_state() -> TestResult {
    kills(&[("ready.marker.create.before", None)])
}

#[test]
fn kill_ready_marker_create_after_preserves_state() -> TestResult {
    kills(&[("ready.marker.create.after", Some(OpenFailure::StoreCorrupt))])
}

#[test]
fn kill_ready_marker_write_after_preserves_state() -> TestResult {
    kills(&[("ready.marker.write.after", None)])
}

#[test]
fn kill_ready_marker_file_sync_after_preserves_state() -> TestResult {
    kills(&[("ready.marker.file_sync.after", None)])
}

#[test]
fn kill_ready_marker_rename_before_preserves_state() -> TestResult {
    kills(&[("ready.marker.rename.before", None)])
}

#[test]
fn kill_ready_marker_rename_after_preserves_state() -> TestResult {
    kills(&[("ready.marker.rename.after", None)])
}

#[test]
fn kill_ready_marker_directory_sync_after_preserves_state() -> TestResult {
    kills(&[("ready.marker.directory_sync.after", None)])
}
