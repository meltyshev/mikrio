/* Exercise every implemented expected-owner option through the installed ABI.
 * Native owner assertions do not represent HTTP authentication or permissions.
 */
#include "mikrio.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef NDEBUG
#error "FFI fixtures require assertions, including calls with side effects"
#endif

static const char POLICY_DOCUMENT[] =
    "{\"Version\":\"2012-10-17\",\"Statement\":{\"Effect\":\"Deny\","
    "\"Principal\":\"*\",\"Action\":\"s3:GetObject\","
    "\"Resource\":\"arn:aws:s3:::bucket/*\"}}";

static mikrio_v1_span_t text(const char *value) {
  return (mikrio_v1_span_t){(const uint8_t *)value, strlen(value)};
}

static int equals(mikrio_v1_span_t span, const char *value) {
  return span.len == strlen(value) &&
         (span.len == 0 || memcmp(span.data, value, span.len) == 0);
}

static void succeeded(uint32_t status, mikrio_v1_error_t **error) {
  if (status != 0) {
    mikrio_v1_error_view_t view = mikrio_v1_error_view(*error);

    fprintf(stderr, "unexpected error: %.*s (outcome %u)\n", (int)view.code.len,
            view.code.data, view.mutation_outcome);

    mikrio_v1_error_release(*error);
    abort();
  }

  assert(*error == NULL);
}

static mikrio_v1_store_t *open_store(const char *path) {
  mikrio_v1_open_options_t options = {0};
  options.struct_size = sizeof(options);
  options.directory = text(path);
  mikrio_v1_store_t *store = NULL;
  mikrio_v1_error_t *error = NULL;
  succeeded(mikrio_v1_store_open(&options, &store, &error), &error);
  assert(store != NULL);
  return store;
}

struct retained_error {
  mikrio_v1_error_t *error;
  const char *code;
  uint32_t outcome;
};

struct rejected_calls {
  struct retained_error errors[80];
  size_t count;
};

static void check_error(const struct retained_error *retained) {
  mikrio_v1_error_view_t view = mikrio_v1_error_view(retained->error);
  assert(equals(view.code, retained->code));
  assert(view.mutation_outcome == retained->outcome);
  assert(view.message.data != NULL && view.message.len > 0);
  assert(view.reserved == 0);
  assert(mikrio_v1_error_delete_objects_progress_count(retained->error) == 0);
}

static void rejected(struct rejected_calls *calls, const char *operation,
                     uint32_t status, mikrio_v1_error_t **error,
                     const char *code, uint32_t outcome) {
  if (status != 1 || *error == NULL) {
    fprintf(stderr, "%s did not reject %s\n", operation, code);
    abort();
  }

  assert(calls->count < sizeof(calls->errors) / sizeof(calls->errors[0]));
  mikrio_v1_error_view_t actual = mikrio_v1_error_view(*error);

  if (!equals(actual.code, code) || actual.mutation_outcome != outcome) {
    fprintf(stderr, "%s: expected %s/%u, received %.*s/%u\n", operation, code,
            outcome, (int)actual.code.len, actual.code.data,
            actual.mutation_outcome);
  }

  struct retained_error retained = {*error, code, outcome};
  check_error(&retained);
  calls->errors[calls->count++] = retained;
  *error = NULL;
  printf("CASE %s %s\n", operation, code);
}

static void seed(mikrio_v1_store_t *store) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("bucket");
  succeeded(mikrio_v1_store_create_bucket(store, &bucket, &error), &error);

  mikrio_v1_put_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = bucket.bucket;
  input.key = text("key");
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *put = NULL;

  succeeded(mikrio_v1_store_begin_put_object(store, &input, &writer, &error),
            &error);

  succeeded(mikrio_v1_writer_write(writer, text("original"), &error), &error);
  succeeded(mikrio_v1_writer_finish(writer, &put, &error), &error);
  mikrio_v1_put_output_release(put);
  mikrio_v1_writer_release(writer);

  mikrio_v1_put_bucket_policy_input_t policy = {0};
  policy.struct_size = sizeof(policy);
  policy.bucket = bucket.bucket;
  policy.document = text(POLICY_DOCUMENT);
  succeeded(mikrio_v1_store_put_bucket_policy(store, &policy, &error), &error);

  mikrio_v1_span_t origin = text("https://app.example");
  mikrio_v1_span_t method = text("GET");
  mikrio_v1_cors_rule_input_t rule = {0};
  rule.struct_size = sizeof(rule);
  rule.allowed_origins = (mikrio_v1_span_list_t){&origin, 1};
  rule.allowed_methods = (mikrio_v1_span_list_t){&method, 1};
  mikrio_v1_put_bucket_cors_input_t cors = {0};
  cors.struct_size = sizeof(cors);
  cors.bucket = bucket.bucket;
  cors.cors_rules = &rule;
  cors.cors_rule_count = 1;
  succeeded(mikrio_v1_store_put_bucket_cors(store, &cors, &error), &error);
}

static void reject_owner(mikrio_v1_store_t *store, mikrio_v1_span_t owner,
                         const char *code, struct rejected_calls *calls) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("bucket");
  bucket.expected_bucket_owner = owner;
  mikrio_v1_bucket_output_t *region = NULL;

  rejected(calls, "head_bucket",
           mikrio_v1_store_head_bucket(store, &bucket, &region, &error), &error,
           code, 0);

  assert(region == NULL);

  rejected(calls, "get_bucket_location",
           mikrio_v1_store_get_bucket_location(store, &bucket, &region, &error),
           &error, code, 0);

  assert(region == NULL);

  rejected(calls, "delete_bucket",
           mikrio_v1_store_delete_bucket(store, &bucket, &error), &error, code,
           1);

  mikrio_v1_put_input_t put = {0};
  put.struct_size = sizeof(put);
  put.bucket = bucket.bucket;
  put.key = text("key");
  put.expected_bucket_owner = owner;
  mikrio_v1_writer_t *writer = NULL;

  uint32_t status =
      mikrio_v1_store_begin_put_object(store, &put, &writer, &error);

  if (owner.len != 0) {
    /* Begin admits an asynchronous worker, not the storage operation itself.
     * Finish joins that worker and reports its owner rejection. */
    succeeded(status, &error);
    assert(writer != NULL);
    mikrio_v1_put_output_t *output = NULL;
    status = mikrio_v1_writer_finish(writer, &output, &error);
    assert(output == NULL);
    mikrio_v1_writer_release(writer);
  } else {
    assert(writer == NULL);
  }

  rejected(calls, "put_object", status, &error, code, 1);

  mikrio_v1_read_input_t read = {0};
  read.struct_size = sizeof(read);
  read.bucket = bucket.bucket;
  read.key = put.key;
  read.expected_bucket_owner = owner;
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;

  rejected(calls, "get_object",
           mikrio_v1_store_get_object(store, &read, &facts, &body, &error),
           &error, code, 0);

  assert(facts == NULL && body == NULL);

  rejected(calls, "head_object",
           mikrio_v1_store_head_object(store, &read, &facts, &error), &error,
           code, 0);

  assert(facts == NULL);

  mikrio_v1_delete_input_t remove = {0};
  remove.struct_size = sizeof(remove);
  remove.bucket = bucket.bucket;
  remove.key = put.key;
  remove.expected_bucket_owner = owner;

  rejected(calls, "delete_object",
           mikrio_v1_store_delete_object(store, &remove, &error), &error, code,
           1);

  mikrio_v1_object_identifier_t object = {text("key"), {0}};
  mikrio_v1_delete_objects_input_t batch = {0};
  batch.struct_size = sizeof(batch);
  batch.bucket = bucket.bucket;
  batch.expected_bucket_owner = owner;
  batch.objects = &object;
  batch.objects_count = 1;
  mikrio_v1_delete_objects_output_t *deleted = NULL;

  rejected(calls, "delete_objects",
           mikrio_v1_store_delete_objects(store, &batch, &deleted, &error),
           &error, code, 1);

  assert(deleted == NULL);

  mikrio_v1_list_objects_v2_input_t list = {0};
  list.struct_size = sizeof(list);
  list.bucket = bucket.bucket;
  list.expected_bucket_owner = owner;
  mikrio_v1_list_objects_v2_output_t *page = NULL;

  rejected(calls, "list_objects_v2",
           mikrio_v1_store_list_objects_v2(store, &list, &page, &error), &error,
           code, 0);

  assert(page == NULL);

  mikrio_v1_put_bucket_policy_input_t put_policy = {0};
  put_policy.struct_size = sizeof(put_policy);
  put_policy.bucket = bucket.bucket;
  put_policy.expected_bucket_owner = owner;
  put_policy.document = text(POLICY_DOCUMENT);

  rejected(calls, "put_bucket_policy",
           mikrio_v1_store_put_bucket_policy(store, &put_policy, &error),
           &error, code, 1);

  mikrio_v1_get_bucket_policy_input_t get_policy = {0};
  get_policy.struct_size = sizeof(get_policy);
  get_policy.bucket = bucket.bucket;
  get_policy.expected_bucket_owner = owner;
  mikrio_v1_get_bucket_policy_output_t *policy = NULL;

  rejected(
      calls, "get_bucket_policy",
      mikrio_v1_store_get_bucket_policy(store, &get_policy, &policy, &error),
      &error, code, 0);

  assert(policy == NULL);

  mikrio_v1_delete_bucket_policy_input_t delete_policy = {0};
  delete_policy.struct_size = sizeof(delete_policy);
  delete_policy.bucket = bucket.bucket;
  delete_policy.expected_bucket_owner = owner;

  rejected(calls, "delete_bucket_policy",
           mikrio_v1_store_delete_bucket_policy(store, &delete_policy, &error),
           &error, code, 1);

  mikrio_v1_get_bucket_policy_status_input_t get_status = {0};
  get_status.struct_size = sizeof(get_status);
  get_status.bucket = bucket.bucket;
  get_status.expected_bucket_owner = owner;
  mikrio_v1_get_bucket_policy_status_output_t *policy_status = NULL;

  rejected(calls, "get_bucket_policy_status",
           mikrio_v1_store_get_bucket_policy_status(store, &get_status,
                                                    &policy_status, &error),
           &error, code, 0);

  assert(policy_status == NULL);

  mikrio_v1_put_public_access_block_input_t put_flags = {0};
  put_flags.struct_size = sizeof(put_flags);
  put_flags.bucket = bucket.bucket;
  put_flags.expected_bucket_owner = owner;

  rejected(calls, "put_public_access_block",
           mikrio_v1_store_put_public_access_block(store, &put_flags, &error),
           &error, code, 1);

  mikrio_v1_get_public_access_block_input_t get_flags = {0};
  get_flags.struct_size = sizeof(get_flags);
  get_flags.bucket = bucket.bucket;
  get_flags.expected_bucket_owner = owner;
  mikrio_v1_get_public_access_block_output_t *flags = NULL;

  rejected(calls, "get_public_access_block",
           mikrio_v1_store_get_public_access_block(store, &get_flags, &flags,
                                                   &error),
           &error, code, 0);

  assert(flags == NULL);

  mikrio_v1_delete_public_access_block_input_t delete_flags = {0};
  delete_flags.struct_size = sizeof(delete_flags);
  delete_flags.bucket = bucket.bucket;
  delete_flags.expected_bucket_owner = owner;

  rejected(
      calls, "delete_public_access_block",
      mikrio_v1_store_delete_public_access_block(store, &delete_flags, &error),
      &error, code, 1);

  mikrio_v1_put_bucket_ownership_controls_input_t put_ownership = {0};
  put_ownership.struct_size = sizeof(put_ownership);
  put_ownership.bucket = bucket.bucket;
  put_ownership.expected_bucket_owner = owner;
  put_ownership.object_ownership = MIKRIO_V1_BUCKET_OWNER_ENFORCED;

  rejected(calls, "put_bucket_ownership_controls",
           mikrio_v1_store_put_bucket_ownership_controls(store, &put_ownership,
                                                         &error),
           &error, code, 1);

  mikrio_v1_get_bucket_ownership_controls_input_t get_ownership = {0};
  get_ownership.struct_size = sizeof(get_ownership);
  get_ownership.bucket = bucket.bucket;
  get_ownership.expected_bucket_owner = owner;
  mikrio_v1_get_bucket_ownership_controls_output_t *ownership = NULL;

  rejected(calls, "get_bucket_ownership_controls",
           mikrio_v1_store_get_bucket_ownership_controls(store, &get_ownership,
                                                         &ownership, &error),
           &error, code, 0);

  assert(ownership == NULL);

  mikrio_v1_get_bucket_acl_input_t get_acl = {0};
  get_acl.struct_size = sizeof(get_acl);
  get_acl.bucket = bucket.bucket;
  get_acl.expected_bucket_owner = owner;
  mikrio_v1_get_bucket_acl_output_t *acl = NULL;

  rejected(calls, "get_bucket_acl",
           mikrio_v1_store_get_bucket_acl(store, &get_acl, &acl, &error),
           &error, code, 0);

  assert(acl == NULL);

  mikrio_v1_get_object_acl_input_t get_object_acl = {0};
  get_object_acl.struct_size = sizeof(get_object_acl);
  get_object_acl.bucket = bucket.bucket;
  get_object_acl.key = put.key;
  get_object_acl.expected_bucket_owner = owner;
  mikrio_v1_get_object_acl_output_t *object_acl = NULL;

  rejected(calls, "get_object_acl",
           mikrio_v1_store_get_object_acl(store, &get_object_acl, &object_acl,
                                          &error),
           &error, code, 0);

  assert(object_acl == NULL);

  mikrio_v1_span_t origin = text("https://changed.example");
  mikrio_v1_span_t method = text("PUT");
  mikrio_v1_cors_rule_input_t rule = {0};
  rule.struct_size = sizeof(rule);
  rule.allowed_origins = (mikrio_v1_span_list_t){&origin, 1};
  rule.allowed_methods = (mikrio_v1_span_list_t){&method, 1};
  mikrio_v1_put_bucket_cors_input_t put_cors = {0};
  put_cors.struct_size = sizeof(put_cors);
  put_cors.bucket = bucket.bucket;
  put_cors.expected_bucket_owner = owner;
  put_cors.cors_rules = &rule;
  put_cors.cors_rule_count = 1;

  rejected(calls, "put_bucket_cors",
           mikrio_v1_store_put_bucket_cors(store, &put_cors, &error), &error,
           code, 1);

  mikrio_v1_get_bucket_cors_input_t get_cors = {0};
  get_cors.struct_size = sizeof(get_cors);
  get_cors.bucket = bucket.bucket;
  get_cors.expected_bucket_owner = owner;
  mikrio_v1_get_bucket_cors_output_t *cors = NULL;

  rejected(calls, "get_bucket_cors",
           mikrio_v1_store_get_bucket_cors(store, &get_cors, &cors, &error),
           &error, code, 0);

  assert(cors == NULL);

  mikrio_v1_delete_bucket_cors_input_t delete_cors = {0};
  delete_cors.struct_size = sizeof(delete_cors);
  delete_cors.bucket = bucket.bucket;
  delete_cors.expected_bucket_owner = owner;

  rejected(calls, "delete_bucket_cors",
           mikrio_v1_store_delete_bucket_cors(store, &delete_cors, &error),
           &error, code, 1);
}

static void reject_tagging_lifecycle_and_multipart_owners(
    mikrio_v1_store_t *store, mikrio_v1_span_t owner, const char *code,
    struct rejected_calls *calls) {
  mikrio_v1_error_t *error = NULL;

  mikrio_v1_span_t bucket = text("bucket");
  mikrio_v1_span_t key = text("key");
  mikrio_v1_create_multipart_upload_input_t seed = {0};
  seed.struct_size = sizeof(seed);
  seed.bucket = bucket;
  seed.key = text("pending-owner");
  mikrio_v1_create_multipart_upload_output_t *upload = NULL;

  succeeded(
      mikrio_v1_store_create_multipart_upload(store, &seed, &upload, &error),
      &error);

  mikrio_v1_span_t upload_id =
      mikrio_v1_create_multipart_upload_output_view(upload).upload_id;

  mikrio_v1_upload_part_input_t seed_part = {0};
  seed_part.struct_size = sizeof(seed_part);
  seed_part.bucket = bucket;
  seed_part.key = seed.key;
  seed_part.upload_id = upload_id;
  seed_part.part_number = 1;
  mikrio_v1_writer_t *writer = NULL;

  succeeded(
      mikrio_v1_store_begin_upload_part(store, &seed_part, &writer, &error),
      &error);

  succeeded(mikrio_v1_writer_write(writer, text("original"), &error), &error);

  mikrio_v1_upload_part_output_t *written = NULL;
  succeeded(mikrio_v1_writer_finish_part(writer, &written, &error), &error);
  mikrio_v1_writer_release(writer);
  writer = NULL;
  mikrio_v1_completed_part_t selected = {0};
  selected.part_number = 1;
  selected.etag = mikrio_v1_upload_part_output_view(written).etag;
  mikrio_v1_span_t attribute = text("ObjectSize");
  mikrio_v1_lifecycle_rule_input_t rule = {0};
  rule.struct_size = sizeof(rule);
  rule.status = MIKRIO_V1_LIFECYCLE_ENABLED;
  rule.expiration_days = (mikrio_v1_optional_u64_t){1, 0, 7};

  {
    mikrio_v1_put_object_tagging_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = key;

    rejected(calls, "put_object_tagging",
             mikrio_v1_store_put_object_tagging(store, &input, &error), &error,
             code, 1);
  }

  {
    mikrio_v1_get_object_tagging_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = key;
    mikrio_v1_get_object_tagging_output_t *result = (void *)1;

    rejected(calls, "get_object_tagging",
             mikrio_v1_store_get_object_tagging(store, &input, &result, &error),
             &error, code, 0);

    assert(result == NULL);
  }

  {
    mikrio_v1_delete_object_tagging_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = key;

    rejected(calls, "delete_object_tagging",
             mikrio_v1_store_delete_object_tagging(store, &input, &error),
             &error, code, 1);
  }

  {
    mikrio_v1_put_bucket_lifecycle_configuration_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.lifecycle_rules = &rule;
    input.lifecycle_rule_count = 1;

    rejected(calls, "put_bucket_lifecycle_configuration",
             mikrio_v1_store_put_bucket_lifecycle_configuration(store, &input,
                                                                &error),
             &error, code, 1);
  }

  {
    mikrio_v1_get_bucket_lifecycle_configuration_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    mikrio_v1_get_bucket_lifecycle_configuration_output_t *result = (void *)1;

    rejected(calls, "get_bucket_lifecycle_configuration",
             mikrio_v1_store_get_bucket_lifecycle_configuration(
                 store, &input, &result, &error),
             &error, code, 0);

    assert(result == NULL);
  }

  {
    mikrio_v1_delete_bucket_lifecycle_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;

    rejected(calls, "delete_bucket_lifecycle",
             mikrio_v1_store_delete_bucket_lifecycle(store, &input, &error),
             &error, code, 1);
  }

  {
    mikrio_v1_copy_object_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = text("owner-copy");
    input.copy_source = (mikrio_v1_object_ref_t){bucket, key};
    mikrio_v1_copy_object_output_t *result = (void *)1;

    rejected(calls, "copy_object",
             mikrio_v1_store_copy_object(store, &input, &result, &error),
             &error, code, 1);

    assert(result == NULL);

    input.expected_bucket_owner = (mikrio_v1_span_t){0};
    input.expected_source_bucket_owner = owner;
    result = (void *)1;

    rejected(calls, "copy_object_source",
             mikrio_v1_store_copy_object(store, &input, &result, &error),
             &error, code, 1);

    assert(result == NULL);
  }

  {
    mikrio_v1_create_multipart_upload_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = seed.key;
    mikrio_v1_create_multipart_upload_output_t *result = (void *)1;

    rejected(
        calls, "create_multipart_upload",
        mikrio_v1_store_create_multipart_upload(store, &input, &result, &error),
        &error, code, 1);

    assert(result == NULL);
  }

  {
    mikrio_v1_upload_part_copy_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = seed.key;
    input.upload_id = upload_id;
    input.part_number = 1;
    input.copy_source = (mikrio_v1_object_ref_t){bucket, key};
    mikrio_v1_upload_part_copy_output_t *result = (void *)1;

    rejected(calls, "upload_part_copy",
             mikrio_v1_store_upload_part_copy(store, &input, &result, &error),
             &error, code, 1);

    assert(result == NULL);

    input.expected_bucket_owner = (mikrio_v1_span_t){0};
    input.expected_source_bucket_owner = owner;
    result = (void *)1;

    rejected(calls, "upload_part_copy_source",
             mikrio_v1_store_upload_part_copy(store, &input, &result, &error),
             &error, code, 1);

    assert(result == NULL);
  }

  {
    mikrio_v1_complete_multipart_upload_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = seed.key;
    input.upload_id = upload_id;
    input.parts = &selected;
    input.parts_count = 1;
    mikrio_v1_complete_multipart_upload_output_t *result = (void *)1;

    rejected(calls, "complete_multipart_upload",
             mikrio_v1_store_complete_multipart_upload(store, &input, &result,
                                                       &error),
             &error, code, 1);

    assert(result == NULL);
  }

  {
    mikrio_v1_abort_multipart_upload_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = seed.key;
    input.upload_id = upload_id;

    rejected(calls, "abort_multipart_upload",
             mikrio_v1_store_abort_multipart_upload(store, &input, &error),
             &error, code, 1);
  }

  {
    mikrio_v1_list_parts_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = seed.key;
    input.upload_id = upload_id;
    mikrio_v1_list_parts_output_t *result = (void *)1;

    rejected(calls, "list_parts",
             mikrio_v1_store_list_parts(store, &input, &result, &error), &error,
             code, 0);

    assert(result == NULL);
  }

  {
    mikrio_v1_list_multipart_uploads_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    mikrio_v1_list_multipart_uploads_output_t *result = (void *)1;

    rejected(
        calls, "list_multipart_uploads",
        mikrio_v1_store_list_multipart_uploads(store, &input, &result, &error),
        &error, code, 0);

    assert(result == NULL);
  }

  {
    mikrio_v1_get_object_attributes_input_t input = {0};
    input.struct_size = sizeof(input);
    input.bucket = bucket;
    input.expected_bucket_owner = owner;
    input.key = key;
    input.object_attributes = &attribute;
    input.object_attributes_count = 1;
    mikrio_v1_get_object_attributes_output_t *result = (void *)1;

    rejected(
        calls, "get_object_attributes",
        mikrio_v1_store_get_object_attributes(store, &input, &result, &error),
        &error, code, 0);

    assert(result == NULL);
  }

  seed_part.expected_bucket_owner = owner;

  uint32_t status =
      mikrio_v1_store_begin_upload_part(store, &seed_part, &writer, &error);

  if (owner.len != 0) {
    succeeded(status, &error);
    mikrio_v1_upload_part_output_t *result = (void *)1;
    status = mikrio_v1_writer_finish_part(writer, &result, &error);
    assert(result == NULL);
    mikrio_v1_writer_release(writer);
  } else {
    assert(writer == NULL);
  }

  rejected(calls, "upload_part", status, &error, code, 1);

  /* Rejected completion/abort/part replacement must retain the exact upload. */
  mikrio_v1_list_parts_input_t list = {0};
  list.struct_size = sizeof(list);
  list.bucket = bucket;
  list.key = seed.key;
  list.upload_id = upload_id;
  mikrio_v1_list_parts_output_t *parts = NULL;
  succeeded(mikrio_v1_store_list_parts(store, &list, &parts, &error), &error);
  assert(mikrio_v1_list_parts_output_view(parts).parts_count == 1);

  mikrio_v1_part_summary_view_t retained =
      mikrio_v1_list_parts_output_item(parts, 0);

  assert(retained.part_number == 1 && retained.size == 8);

  assert(retained.etag.len == selected.etag.len &&
         memcmp(retained.etag.data, selected.etag.data, selected.etag.len) ==
             0);

  mikrio_v1_list_parts_output_release(parts);

  mikrio_v1_abort_multipart_upload_input_t cleanup = {0};
  cleanup.struct_size = sizeof(cleanup);
  cleanup.bucket = bucket;
  cleanup.key = seed.key;
  cleanup.upload_id = upload_id;

  succeeded(mikrio_v1_store_abort_multipart_upload(store, &cleanup, &error),
            &error);

  mikrio_v1_create_multipart_upload_output_release(upload);
  mikrio_v1_upload_part_output_release(written);
}

static void unchanged_after_reopen(mikrio_v1_store_t *store) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_read_input_t read = {0};
  read.struct_size = sizeof(read);
  read.bucket = text("bucket");
  read.key = text("key");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;

  succeeded(mikrio_v1_store_get_object(store, &read, &facts, &body, &error),
            &error);

  assert(mikrio_v1_read_output_view(facts).content_length == 8);
  uint8_t bytes[16];
  uint64_t count = 0;

  succeeded(
      mikrio_v1_body_read_chunk(body, bytes, sizeof(bytes), &count, &error),
      &error);

  assert(count == 8 && memcmp(bytes, "original", 8) == 0);

  succeeded(
      mikrio_v1_body_read_chunk(body, bytes, sizeof(bytes), &count, &error),
      &error);

  assert(count == 0);
  succeeded(mikrio_v1_body_close(body, &error), &error);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);

  mikrio_v1_get_bucket_policy_input_t get_policy = {0};
  get_policy.struct_size = sizeof(get_policy);
  get_policy.bucket = read.bucket;
  mikrio_v1_get_bucket_policy_output_t *policy = NULL;

  succeeded(
      mikrio_v1_store_get_bucket_policy(store, &get_policy, &policy, &error),
      &error);

  assert(equals(mikrio_v1_get_bucket_policy_output_view(policy).document,
                POLICY_DOCUMENT));

  mikrio_v1_get_bucket_policy_output_release(policy);

  mikrio_v1_get_bucket_cors_input_t get_cors = {0};
  get_cors.struct_size = sizeof(get_cors);
  get_cors.bucket = read.bucket;
  mikrio_v1_get_bucket_cors_output_t *cors = NULL;

  succeeded(mikrio_v1_store_get_bucket_cors(store, &get_cors, &cors, &error),
            &error);

  assert(mikrio_v1_get_bucket_cors_output_view(cors).cors_rule_count == 1);

  assert(equals(mikrio_v1_get_bucket_cors_output_allowed_origin(cors, 0, 0),
                "https://app.example"));

  assert(equals(mikrio_v1_get_bucket_cors_output_allowed_method(cors, 0, 0),
                "GET"));

  mikrio_v1_get_bucket_cors_output_release(cors);

  mikrio_v1_get_public_access_block_input_t get_flags = {0};
  get_flags.struct_size = sizeof(get_flags);
  get_flags.bucket = read.bucket;
  mikrio_v1_get_public_access_block_output_t *flags = NULL;

  succeeded(mikrio_v1_store_get_public_access_block(store, &get_flags, &flags,
                                                    &error),
            &error);

  mikrio_v1_public_access_block_configuration_t configuration =
      mikrio_v1_get_public_access_block_output_view(flags).configuration;

  assert(configuration.block_public_acls == 1);
  assert(configuration.ignore_public_acls == 1);
  assert(configuration.block_public_policy == 1);
  assert(configuration.restrict_public_buckets == 1);
  mikrio_v1_get_public_access_block_output_release(flags);

  mikrio_v1_get_bucket_ownership_controls_input_t get_ownership = {0};
  get_ownership.struct_size = sizeof(get_ownership);
  get_ownership.bucket = read.bucket;
  mikrio_v1_get_bucket_ownership_controls_output_t *ownership = NULL;

  succeeded(mikrio_v1_store_get_bucket_ownership_controls(store, &get_ownership,
                                                          &ownership, &error),
            &error);

  assert(mikrio_v1_get_bucket_ownership_controls_output_view(ownership)
             .object_ownership == MIKRIO_V1_BUCKET_OWNER_ENFORCED);

  mikrio_v1_get_bucket_ownership_controls_output_release(ownership);
}

int main(int argc, char **argv) {
  assert(argc == 2 && mikrio_v1_abi_version() == 1);
  mikrio_v1_store_t *store = open_store(argv[1]);
  seed(store);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_owner_info_t *owner = NULL;
  succeeded(mikrio_v1_store_get_owner_info(store, &owner, &error), &error);
  mikrio_v1_span_t account = mikrio_v1_owner_info_view(owner).account_id;

  const char *wrong =
      equals(account, "000000000000") ? "000000000001" : "000000000000";

  struct rejected_calls calls = {0};
  reject_owner(store, text(wrong), "AccessDenied", &calls);
  reject_owner(store, text(""), "InvalidArgument", &calls);

  reject_tagging_lifecycle_and_multipart_owners(store, text(wrong),
                                                "AccessDenied", &calls);

  reject_tagging_lifecycle_and_multipart_owners(store, text(""),
                                                "InvalidArgument", &calls);

  assert(calls.count == 80);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  mikrio_v1_owner_info_release(owner);

  for (size_t index = 0; index < calls.count; ++index) {
    check_error(&calls.errors[index]);
    mikrio_v1_error_release(calls.errors[index].error);
  }

  store = open_store(argv[1]);
  unchanged_after_reopen(store);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  puts("PASS every_expected_owner_rejects_without_mutation");
  return 0;
}
