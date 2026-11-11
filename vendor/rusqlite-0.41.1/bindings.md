# List of SQLite functions supported

- [ ] `sqlite3_version`
- [x] `sqlite3_libversion`
- [ ] `sqlite3_sourceid`
- [x] `sqlite3_libversion_number`

- [ ] `sqlite3_compileoption_used`
- [ ] `sqlite3_compileoption_get`

- [x] `sqlite3_threadsafe` (internal use only)

- [x] `sqlite3_close`
- [ ] `sqlite3_close_v2`

- [ ] `sqlite3_exec`

- [ ] `sqlite3_initialize`
- [ ] `sqlite3_shutdown`
- [ ] `sqlite3_os_init`
- [ ] `sqlite3_os_end`

- [ ] `sqlite3_config` (partially, `fn` callback for SQLITE_CONFIG_LOG) (cannot
      be used by a loadable extension)
- [x] `sqlite3_db_config`

- [x] `sqlite3_extended_result_codes` (not public, internal use only)

- [x] `sqlite3_last_insert_rowid`
- [ ] `sqlite3_set_last_insert_rowid`

- [x] `sqlite3_changes`
- [x] `sqlite3_changes64`
- [x] `sqlite3_total_changes`
- [x] `sqlite3_total_changes64`

- [x] `sqlite3_interrupt`
- [x] `sqlite3_is_interrupted`

- [ ] `sqlite3_complete`

- [x] `sqlite3_busy_handler` (Boxed callback, destroyed by SQLite)
- [x] `sqlite3_busy_timeout`

- [ ] `sqlite3_get_table`

- [ ] `sqlite3_mprintf`
- [ ] `sqlite3_vmprintf`
- [ ] `sqlite3_snprintf`
- [ ] `sqlite3_vsnprintf`

- [ ] `sqlite3_malloc`
- [x] `sqlite3_malloc64` (not public, internal use only)
- [ ] `sqlite3_realloc`
- [ ] `sqlite3_realloc64`
- [x] `sqlite3_free` (not public, internal use only)
- [ ] `sqlite3_msize`

- [ ] `sqlite3_memory_used`
- [ ] `sqlite3_memory_highwater`

- [ ] `sqlite3_randomness`

- [x] `sqlite3_set_authorizer` (Boxed callback, destroyed by SQLite)
- [ ] `sqlite3_trace` deprecated
- [ ] `sqlite3_profile` deprecated
- [x] `sqlite3_trace_v2` (Boxed callback, destroyed by SQLite)
- [x] `sqlite3_progress_handler` (Boxed callback, destroyed by SQLite)

- [ ] `sqlite3_open`
- [x] `sqlite3_open_v2`
- [ ] `sqlite3_uri_parameter`
- [ ] `sqlite3_uri_boolean`
- [ ] `sqlite3_uri_int64`
- [ ] `sqlite3_uri_key`

- [ ] `sqlite3_filename_database`
- [ ] `sqlite3_filename_journal`
- [ ] `sqlite3_filename_wal`
- [ ] `sqlite3_database_file_object`
- [ ] `sqlite3_create_filename`
- [ ] `sqlite3_free_filename`

- [x] `sqlite3_errcode`
- [x] `sqlite3_extended_errcode`
- [x] `sqlite3_errmsg` (not public, internal use only)
- [x] `sqlite3_errstr` (not public, internal use only)
- [x] `sqlite3_error_offset`

- [x] `sqlite3_limit`

- [ ] `sqlite3_prepare`
- [x] `sqlite3_prepare_v2`
- [x] `sqlite3_prepare_v3`

- [x] `sqlite3_sql` (not public, internal use only)
- [x] `sqlite3_expanded_sql`
- [ ] `sqlite3_normalized_sql`

- [x] `sqlite3_stmt_readonly`
- [x] `sqlite3_stmt_isexplain`
- [ ] `sqlite3_stmt_explain`
- [x] `sqlite3_stmt_busy`

- [ ] `sqlite3_bind_blob`
- [x] `sqlite3_bind_blob64`
- [x] `sqlite3_bind_double`
- [ ] `sqlite3_bind_int`
- [x] `sqlite3_bind_int64`
- [x] `sqlite3_bind_null`
- [ ] `sqlite3_bind_text`
- [x] `sqlite3_bind_text64`
- [ ] `sqlite3_bind_value`
- [x] `sqlite3_bind_pointer`
- [ ] `sqlite3_bind_zeroblob`
- [x] `sqlite3_bind_zeroblob64`

- [x] `sqlite3_bind_parameter_count`
- [x] `sqlite3_bind_parameter_name`
- [x] `sqlite3_bind_parameter_index`
- [x] `sqlite3_clear_bindings`

- [x] `sqlite3_column_count`
- [ ] `sqlite3_data_count`
- [x] `sqlite3_column_name`
- [x] `sqlite3_column_database_name`
- [x] `sqlite3_column_table_name`
- [x] `sqlite3_column_origin_name`
- [x] `sqlite3_column_decltype`

- [x] `sqlite3_step`

- [x] `sqlite3_column_blob`
- [x] `sqlite3_column_double`
- [ ] `sqlite3_column_int`
- [x] `sqlite3_column_int64`
- [x] `sqlite3_column_text`
- [x] `sqlite3_column_value` (not public, internal use only)
- [x] `sqlite3_column_bytes` (not public, internal use only)
- [x] `sqlite3_column_type`

- [x] `sqlite3_finalize`
- [x] `sqlite3_reset` (not public, internal use only)

- [ ] `sqlite3_create_function`
- [x] `sqlite3_create_function_v2` (Boxed callback, destroyed by SQLite)
- [x] `sqlite3_create_window_function` (Boxed callback, destroyed by SQLite)

- [x] `sqlite3_value_blob`
- [x] `sqlite3_value_double`
- [ ] `sqlite3_value_int`
- [x] `sqlite3_value_int64`
- [x] `sqlite3_value_pointer`
- [x] `sqlite3_value_text`
- [x] `sqlite3_value_bytes` (not public, internal use only)
- [x] `sqlite3_value_type`
- [ ] `sqlite3_value_numeric_type`
- [x] `sqlite3_value_nochange`
- [ ] `sqlite3_value_frombind`
- [ ] `sqlite3_value_encoding`
- [x] `sqlite3_value_subtype`

- [ ] `sqlite3_value_dup`
- [ ] `sqlite3_value_free`

- [x] `sqlite3_aggregate_context` (not public, internal use only)
- [x] `sqlite3_user_data` (not public, internal use only)
- [x] `sqlite3_context_db_handle` (Connection ref)
- [x] `sqlite3_get_auxdata`
- [x] `sqlite3_set_auxdata`
- [x] `sqlite3_get_clientdata`
- [x] `sqlite3_set_clientdata`

- [ ] `sqlite3_result_blob`
- [x] `sqlite3_result_blob64`
- [x] `sqlite3_result_double`
- [x] `sqlite3_result_error`
- [x] `sqlite3_result_error_toobig`
- [x] `sqlite3_result_error_nomem`
- [x] `sqlite3_result_error_code`
- [ ] `sqlite3_result_int`
- [x] `sqlite3_result_int64`
- [x] `sqlite3_result_null`
- [ ] `sqlite3_result_text`
- [x] `sqlite3_result_text64`
- [x] `sqlite3_result_value`
- [x] `sqlite3_result_pointer`
- [ ] `sqlite3_result_zeroblob`
- [x] `sqlite3_result_zeroblob64`
- [x] `sqlite3_result_subtype`

- [ ] `sqlite3_create_collation`
- [x] `sqlite3_create_collation_v2` (Boxed callback, destroyed by SQLite)
- [x] `sqlite3_collation_needed` (`fn` callback)

- [ ] `sqlite3_sleep`

- [x] `sqlite3_get_autocommit`

- [x] `sqlite3_db_handle` (not public, internal use only, Connection ref)
- [x] `sqlite3_db_name`
- [x] `sqlite3_db_filename`
- [x] `sqlite3_db_readonly`
- [x] `sqlite3_txn_state`
- [x] `sqlite3_next_stmt` (not public, internal use only)

- [x] `sqlite3_commit_hook` (Boxed callback, destroyed by SQLite)
- [x] `sqlite3_rollback_hook` (Boxed callback, destroyed by SQLite)
- [ ] `sqlite3_autovacuum_pages`
- [x] `sqlite3_update_hook` (Boxed callback, destroyed by SQLite)

- [ ] `sqlite3_enable_shared_cache`
- [ ] `sqlite3_release_memory`
- [x] `sqlite3_db_release_memory`
- [ ] `sqlite3_soft_heap_limit64`
- [ ] `sqlite3_hard_heap_limit64`

- [x] `sqlite3_table_column_metadata`

- [x] `sqlite3_load_extension`
- [x] `sqlite3_enable_load_extension` (cannot be used by a loadable extension)
- [x] `sqlite3_auto_extension` (`fn` callbak with Connection ref)
- [x] `sqlite3_cancel_auto_extension`
- [x] `sqlite3_reset_auto_extension`

- [ ] `sqlite3_create_module`
- [x] `sqlite3_create_module_v2`
- [ ] `sqlite3_drop_modules`
- [x] `sqlite3_declare_vtab`
- [ ] `sqlite3_overload_function`

- [x] `sqlite3_blob_open`
- [x] `sqlite3_blob_reopen`
- [x] `sqlite3_blob_close`
- [x] `sqlite3_blob_bytes`
- [x] `sqlite3_blob_read`
- [x] `sqlite3_blob_write`

- [ ] `sqlite3_vfs_find`
- [ ] `sqlite3_vfs_register`
- [ ] `sqlite3_vfs_unregister`

- [ ] `sqlite3_mutex_alloc`
- [ ] `sqlite3_mutex_free`
- [ ] `sqlite3_mutex_enter`
- [ ] `sqlite3_mutex_try`
- [ ] `sqlite3_mutex_leave`
- [ ] `sqlite3_mutex_held`
- [ ] `sqlite3_mutex_notheld`
- [ ] `sqlite3_db_mutex`

- [x] `sqlite3_file_control`
- [ ] `sqlite3_test_control`

- [ ] `sqlite3_keyword_count`
- [ ] `sqlite3_keyword_name`
- [ ] `sqlite3_keyword_check`

- [ ] `sqlite3_str_new`
- [ ] `sqlite3_str_finish`
- [ ] `sqlite3_str_append`
- [ ] `sqlite3_str_reset`
- [ ] `sqlite3_str_errcode`
- [ ] `sqlite3_str_length`
- [ ] `sqlite3_str_value`

- [ ] `sqlite3_status`
- [ ] `sqlite3_status64`
- [ ] `sqlite3_db_status`
- [x] `sqlite3_stmt_status`

- [x] `sqlite3_backup_init`
- [x] `sqlite3_backup_step`
- [x] `sqlite3_backup_finish`
- [x] `sqlite3_backup_remaining`
- [x] `sqlite3_backup_pagecount`

- [x] `sqlite3_unlock_notify` (`fn` callback, internal use only)

- [ ] `sqlite3_stricmp`
- [ ] `sqlite3_strnicmp`
- [ ] `sqlite3_strglob`
- [ ] `sqlite3_strlike`

- [x] `sqlite3_log`

- [x] `sqlite3_wal_hook` (Boxed callback, destroyed by SQLite)
- [ ] `sqlite3_wal_autocheckpoint`
- [ ] `sqlite3_wal_checkpoint`
- [x] `sqlite3_wal_checkpoint_v2`

- [x] `sqlite3_vtab_config`
- [x] `sqlite3_vtab_on_conflict`
- [x] `sqlite3_vtab_nochange`
- [x] `sqlite3_vtab_collation`
- [x] `sqlite3_vtab_distinct`
- [x] `sqlite3_vtab_in`
- [x] `sqlite3_vtab_in_first`
- [x] `sqlite3_vtab_in_next`
- [x] `sqlite3_vtab_rhs_value`

- [ ] `sqlite3_stmt_scanstatus`
- [ ] `sqlite3_stmt_scanstatus_v2`
- [ ] `sqlite3_stmt_scanstatus_reset`

- [x] `sqlite3_db_cacheflush`

- [x] `sqlite3_preupdate_hook` (Boxed callback with Connection ref, destroyed by
      SQLite) (cannot be used by a loadable extension)
- [x] `sqlite3_preupdate_old`
- [x] `sqlite3_preupdate_count`
- [x] `sqlite3_preupdate_depth`
- [x] `sqlite3_preupdate_new`
- [ ] `sqlite3_preupdate_blobwrite`

- [ ] `sqlite3_system_errno`

- [ ] `sqlite3_snapshot_get`
- [ ] `sqlite3_snapshot_open`
- [ ] `sqlite3_snapshot_free`
- [ ] `sqlite3_snapshot_cmp`
- [ ] `sqlite3_snapshot_recover`

- [x] `sqlite3_serialize`
- [x] `sqlite3_deserialize`

- [ ] `sqlite3_rtree_geometry_callback`
- [ ] `sqlite3_rtree_query_callback`

- [x] `sqlite3session_create`
- [x] `sqlite3session_delete`
- [ ] `sqlite3session_object_config`
- [x] `sqlite3session_enable`
- [x] `sqlite3session_indirect`
- [x] `sqlite3session_attach`
- [x] `sqlite3session_table_filter` (Boxed callback, reference kept)
- [x] `sqlite3session_changeset`
- [ ] `sqlite3session_changeset_size`
- [x] `sqlite3session_diff`
- [x] `sqlite3session_patchset`
- [x] `sqlite3session_isempty`
- [ ] `sqlite3session_memory_used`
- [x] `sqlite3changeset_start`
- [ ] `sqlite3changeset_start_v2`
- [x] `sqlite3changeset_next`
- [x] `sqlite3changeset_op`
- [x] `sqlite3changeset_pk`
- [x] `sqlite3changeset_old`
- [x] `sqlite3changeset_new`
- [x] `sqlite3changeset_conflict`
- [x] `sqlite3changeset_fk_conflicts`
- [x] `sqlite3changeset_finalize`
- [x] `sqlite3changeset_invert`
- [x] `sqlite3changeset_concat`
- [ ] `sqlite3changeset_upgrade`
- [x] `sqlite3changegroup_new`
- [ ] `sqlite3changegroup_schema`
- [x] `sqlite3changegroup_add`
- [ ] `sqlite3changegroup_add_change`
- [x] `sqlite3changegroup_output`
- [x] `sqlite3changegroup_delete`
- [x] `sqlite3changeset_apply`
- [ ] `sqlite3changeset_apply_v2`
- [ ] `sqlite3rebaser_create`
- [ ] `sqlite3rebaser_configure`
- [ ] `sqlite3rebaser_rebase`
- [ ] `sqlite3rebaser_delete`
- [x] `sqlite3changeset_apply_strm`
- [ ] `sqlite3changeset_apply_v2_strm`
- [x] `sqlite3changeset_concat_strm`
- [x] `sqlite3changeset_invert_strm`
- [x] `sqlite3changeset_start_strm`
- [ ] `sqlite3changeset_start_v2_strm`
- [x] `sqlite3session_changeset_strm`
- [x] `sqlite3session_patchset_strm`
- [x] `sqlite3changegroup_add_strm`
- [x] `sqlite3changegroup_add_strm`
- [x] `sqlite3changegroup_output_strm`
- [ ] `sqlite3rebaser_rebase_strm`
- [ ] `sqlite3session_config`

## List of virtual table methods supported

- [x] `xCreate`
- [x] `xConnect`
- [x] `xBestIndex`
- [x] `xDisconnect`
- [x] `xDestroy`
- [x] `xOpen`
- [x] `xClose`
- [x] `xFilter`
- [x] `xNext`
- [x] `xEof`
- [x] `xColumn`
- [x] `xRowid`
- [x] `xUpdate`
- [x] `xBegin`
- [x] `xSync`
- [x] `xCommit`
- [x] `xRollback`
- [ ] `xFindFunction`
- [ ] `xRename`
- [ ] `xSavepoint`
- [ ] `xRelease`
- [ ] `xRollbackTo`
- [ ] `xShadowName`
- [ ] `xIntegrity`
