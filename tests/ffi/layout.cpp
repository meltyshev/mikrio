#include "mikrio.h"
#include <cstddef>
#include <type_traits>

static_assert(sizeof(uint64_t) == 8);
static_assert(sizeof(mikrio_v1_span_t) == 16);
static_assert(sizeof(mikrio_v1_optional_i64_t) == 16);
static_assert(offsetof(mikrio_v1_open_options_t, directory) == 8);
static_assert(offsetof(mikrio_v1_open_options_t, create_if_missing) == 40);
static_assert(sizeof(mikrio_v1_open_options_t) == 56);
static_assert(sizeof(mikrio_v1_listen_options_t) == 88);
static_assert(offsetof(mikrio_v1_listen_options_t, public_endpoint) == 40);
static_assert(offsetof(mikrio_v1_listen_options_t, credentials) == 56);
static_assert(sizeof(mikrio_v1_server_info_view_t) == 64);
static_assert(sizeof(mikrio_v1_bucket_input_t) == 40);
static_assert(sizeof(mikrio_v1_put_input_t) == 344);
static_assert(sizeof(mikrio_v1_read_input_t) == 264);
static_assert(offsetof(mikrio_v1_read_input_t, range) == 72);
static_assert(offsetof(mikrio_v1_read_input_t, response_content_type) == 152);
static_assert(offsetof(mikrio_v1_read_input_t, response_expires) == 232);
static_assert(offsetof(mikrio_v1_read_input_t, part_number) == 248);
static_assert(sizeof(mikrio_v1_get_object_attributes_input_t) == 104);
static_assert(offsetof(mikrio_v1_get_object_attributes_input_t, max_parts) ==
              72);
static_assert(offsetof(mikrio_v1_get_object_attributes_input_t,
                       part_number_marker) == 88);
static_assert(sizeof(mikrio_v1_checksum_view_t) == 96);
static_assert(sizeof(mikrio_v1_object_parts_view_t) == 56);
static_assert(sizeof(mikrio_v1_object_part_view_t) == 104);
static_assert(sizeof(mikrio_v1_get_object_attributes_output_view_t) == 224);
static_assert(
    std::is_standard_layout_v<mikrio_v1_get_object_attributes_output_view_t>);
static_assert(sizeof(mikrio_v1_delete_input_t) == 72);
static_assert(sizeof(mikrio_v1_info_view_t) == 56);
static_assert(sizeof(mikrio_v1_error_view_t) == 56);
static_assert(sizeof(mikrio_v1_put_view_t) == 96);
static_assert(sizeof(mikrio_v1_read_view_t) == 280);
static_assert(std::is_standard_layout_v<mikrio_v1_read_view_t>);
static_assert(
    std::is_same_v<decltype(&mikrio_v1_writer_write),
                   uint32_t (*)(mikrio_v1_writer_t *, mikrio_v1_span_t,
                                mikrio_v1_error_t **)>);

static_assert(sizeof(mikrio_v1_create_principal_input_t) == 24);
static_assert(sizeof(mikrio_v1_set_principal_enabled_input_t) == 32);
static_assert(sizeof(mikrio_v1_list_principals_input_t) == 40);
static_assert(sizeof(mikrio_v1_list_access_keys_input_t) == 56);
static_assert(sizeof(mikrio_v1_owner_info_view_t) == 32);
static_assert(sizeof(mikrio_v1_principal_info_view_t) == 48);
static_assert(sizeof(mikrio_v1_access_key_info_view_t) == 40);
static_assert(sizeof(mikrio_v1_create_access_key_output_view_t) == 56);
static_assert(sizeof(mikrio_v1_list_principals_output_view_t) == 24);
static_assert(
    std::is_standard_layout_v<mikrio_v1_set_principal_enabled_input_t>);

static_assert(sizeof(mikrio_v1_public_access_block_configuration_t) == 16);
static_assert(sizeof(mikrio_v1_put_public_access_block_input_t) == 56);
static_assert(offsetof(mikrio_v1_put_public_access_block_input_t,
                       configuration) == 40);
static_assert(sizeof(mikrio_v1_put_bucket_policy_input_t) == 56);
static_assert(sizeof(mikrio_v1_get_bucket_policy_input_t) == 40);
static_assert(sizeof(mikrio_v1_get_bucket_policy_output_view_t) == 16);
static_assert(sizeof(mikrio_v1_get_bucket_policy_status_output_view_t) == 8);
static_assert(sizeof(mikrio_v1_get_public_access_block_output_view_t) == 16);
static_assert(sizeof(mikrio_v1_put_bucket_ownership_controls_input_t) == 48);
static_assert(sizeof(mikrio_v1_get_bucket_ownership_controls_output_view_t) ==
              8);
static_assert(sizeof(mikrio_v1_acl_owner_view_t) == 16);
static_assert(sizeof(mikrio_v1_acl_grant_view_t) == 24);
static_assert(sizeof(mikrio_v1_get_bucket_acl_output_view_t) == 24);
static_assert(sizeof(mikrio_v1_get_object_acl_input_t) == 56);
static_assert(sizeof(mikrio_v1_get_object_acl_output_view_t) == 24);
static_assert(
    std::is_standard_layout_v<mikrio_v1_put_public_access_block_input_t>);

static_assert(sizeof(mikrio_v1_span_list_t) == 16);
static_assert(sizeof(mikrio_v1_cors_rule_input_t) == 104);
static_assert(offsetof(mikrio_v1_cors_rule_input_t, max_age_seconds) == 88);
static_assert(sizeof(mikrio_v1_put_bucket_cors_input_t) == 56);
static_assert(sizeof(mikrio_v1_get_bucket_cors_input_t) == 40);
static_assert(sizeof(mikrio_v1_delete_bucket_cors_input_t) == 40);
static_assert(sizeof(mikrio_v1_get_bucket_cors_output_view_t) == 8);
static_assert(sizeof(mikrio_v1_cors_rule_view_t) == 80);
static_assert(std::is_standard_layout_v<mikrio_v1_cors_rule_input_t>);

static_assert(sizeof(mikrio_v1_list_buckets_input_t) == 56);
static_assert(sizeof(mikrio_v1_list_buckets_output_view_t) == 24);
static_assert(sizeof(mikrio_v1_bucket_summary_view_t) == 40);
static_assert(sizeof(mikrio_v1_list_objects_v2_input_t) == 120);
static_assert(sizeof(mikrio_v1_list_objects_v2_output_view_t) == 40);
static_assert(sizeof(mikrio_v1_object_summary_view_t) == 48);
static_assert(std::is_standard_layout_v<mikrio_v1_list_buckets_input_t>);
static_assert(std::is_standard_layout_v<mikrio_v1_list_objects_v2_input_t>);

static_assert(sizeof(mikrio_v1_get_object_tagging_input_t) == 56);
static_assert(sizeof(mikrio_v1_put_object_tagging_input_t) == 72);
static_assert(sizeof(mikrio_v1_delete_object_tagging_input_t) == 56);
static_assert(offsetof(mikrio_v1_put_object_tagging_input_t, tags) == 56);
static_assert(sizeof(mikrio_v1_get_object_tagging_output_view_t) == 8);

static_assert(sizeof(mikrio_v1_object_identifier_t) == 32);
static_assert(sizeof(mikrio_v1_delete_objects_input_t) == 64);
static_assert(sizeof(mikrio_v1_delete_objects_output_view_t) == 16);
static_assert(sizeof(mikrio_v1_delete_object_error_view_t) == 48);
static_assert(sizeof(mikrio_v1_delete_object_progress_view_t) == 64);
static_assert(std::is_standard_layout_v<mikrio_v1_delete_objects_input_t>);
static_assert(
    std::is_standard_layout_v<mikrio_v1_delete_object_progress_view_t>);

static_assert(sizeof(mikrio_v1_object_ref_t) == 32);
static_assert(sizeof(mikrio_v1_completed_part_t) == 104);
static_assert(offsetof(mikrio_v1_completed_part_t, etag) == 8);
static_assert(sizeof(mikrio_v1_copy_object_input_t) == 376);
static_assert(offsetof(mikrio_v1_copy_object_input_t, copy_source) == 40);
static_assert(sizeof(mikrio_v1_create_multipart_upload_input_t) == 216);
static_assert(sizeof(mikrio_v1_upload_part_input_t) == 208);
static_assert(offsetof(mikrio_v1_upload_part_input_t, part_number) == 56);
static_assert(sizeof(mikrio_v1_upload_part_copy_input_t) == 208);
static_assert(offsetof(mikrio_v1_upload_part_copy_input_t, copy_source) == 64);
static_assert(sizeof(mikrio_v1_complete_multipart_upload_input_t) == 232);
static_assert(offsetof(mikrio_v1_complete_multipart_upload_input_t, parts) ==
              56);
static_assert(offsetof(mikrio_v1_complete_multipart_upload_input_t,
                       parts_count) == 64);
static_assert(sizeof(mikrio_v1_abort_multipart_upload_input_t) == 72);
static_assert(sizeof(mikrio_v1_list_parts_input_t) == 104);
static_assert(sizeof(mikrio_v1_list_multipart_uploads_input_t) == 120);
static_assert(sizeof(mikrio_v1_copy_object_output_view_t) == 104);
static_assert(sizeof(mikrio_v1_create_multipart_upload_output_view_t) == 48);
static_assert(sizeof(mikrio_v1_upload_part_output_view_t) == 96);
static_assert(sizeof(mikrio_v1_upload_part_copy_output_view_t) == 104);
static_assert(sizeof(mikrio_v1_complete_multipart_upload_output_view_t) == 112);
static_assert(sizeof(mikrio_v1_part_summary_view_t) == 120);
static_assert(sizeof(mikrio_v1_multipart_upload_summary_view_t) == 72);
static_assert(sizeof(mikrio_v1_list_parts_output_view_t) == 64);
static_assert(sizeof(mikrio_v1_list_multipart_uploads_output_view_t) == 56);
static_assert(std::is_standard_layout_v<mikrio_v1_completed_part_t>);

static_assert(sizeof(mikrio_v1_lifecycle_rule_input_t) == 80);
static_assert(offsetof(mikrio_v1_lifecycle_rule_input_t, expiration_days) ==
              48);
static_assert(sizeof(mikrio_v1_lifecycle_rule_view_t) == 72);
static_assert(sizeof(mikrio_v1_put_bucket_lifecycle_configuration_input_t) ==
              56);
static_assert(sizeof(mikrio_v1_get_bucket_lifecycle_configuration_input_t) ==
              40);
static_assert(sizeof(mikrio_v1_delete_bucket_lifecycle_input_t) == 40);
static_assert(std::is_standard_layout_v<mikrio_v1_lifecycle_rule_input_t>);

int main() { return mikrio_v1_abi_version() == 1 ? 0 : 1; }
