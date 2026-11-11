#define _DEFAULT_SOURCE 1
#include "mikrio.h"
#include "platform.h"
#include <assert.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef NDEBUG
#error "FFI fixtures require assertions, including calls with side effects"
#endif

_Static_assert(sizeof(mikrio_v1_span_t) == 16, "64-bit span layout");
_Static_assert(sizeof(mikrio_v1_optional_u64_t) == 16, "optional layout");

static mikrio_v1_span_t text(const char *value) {
  mikrio_v1_span_t span = {(const uint8_t *)value, strlen(value)};
  return span;
}

static int equals(mikrio_v1_span_t span, const char *value) {
  return span.len == strlen(value) &&
         (span.len == 0 || memcmp(span.data, value, span.len) == 0);
}

static int same_span(mikrio_v1_span_t actual, mikrio_v1_span_t expected) {
  return actual.len == expected.len &&
         (actual.len == 0 ||
          memcmp(actual.data, expected.data, actual.len) == 0);
}

static void succeeded(uint32_t status, mikrio_v1_error_t **error) {
  if (status != 0) {
    mikrio_v1_error_view_t view = mikrio_v1_error_view(*error);

    fprintf(stderr, "unexpected error: %.*s: %.*s (outcome %u)\n",
            (int)view.code.len, view.code.data, (int)view.message.len,
            view.message.data, view.mutation_outcome);

    mikrio_v1_error_release(*error);
    abort();
  }

  assert(*error == NULL);
}

static void failed(uint32_t status, mikrio_v1_error_t **error, const char *code,
                   uint32_t outcome) {
  assert(status == 1 && *error != NULL);
  mikrio_v1_error_view_t view = mikrio_v1_error_view(*error);
  assert(equals(view.code, code));
  assert(view.mutation_outcome == outcome);
  mikrio_v1_error_release(*error);
  *error = NULL;
}

static mikrio_v1_store_t *open_store(const char *path) {
  mikrio_v1_open_options_t options = {0};
  options.struct_size = sizeof(options);
  options.directory = text(path);
  mikrio_v1_store_t *store = NULL;
  mikrio_v1_error_t *error = NULL;
  uint32_t status = mikrio_v1_store_open(&options, &store, &error);
  succeeded(status, &error);
  assert(store != NULL);
  return store;
}

static mikrio_v1_put_input_t put_input(const char *key) {
  mikrio_v1_put_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = text("bucket");
  input.key = text(key);
  return input;
}

static mikrio_v1_read_input_t read_input(const char *key) {
  mikrio_v1_read_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = text("bucket");
  input.key = text(key);
  return input;
}

static void write_object(mikrio_v1_store_t *store, const char *key,
                         const char *payload) {
  mikrio_v1_put_input_t input = put_input(key);
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *output = NULL;
  mikrio_v1_error_t *error = NULL;

  uint32_t status =
      mikrio_v1_store_begin_put_object(store, &input, &writer, &error);

  succeeded(status, &error);
  status = mikrio_v1_writer_write(writer, text(payload), &error);
  succeeded(status, &error);
  status = mikrio_v1_writer_finish(writer, &output, &error);
  succeeded(status, &error);
  mikrio_v1_put_output_release(output);
  mikrio_v1_writer_release(writer);
}

static void missing(mikrio_v1_store_t *store, const char *key) {
  mikrio_v1_read_input_t input = read_input(key);
  mikrio_v1_read_output_t *output = NULL;
  mikrio_v1_error_t *error = NULL;
  uint32_t status = mikrio_v1_store_head_object(store, &input, &output, &error);
  failed(status, &error, "NoSuchKey", 0);
  assert(output == NULL);
}

static void options_and_bucket(mikrio_v1_store_t *store, const char *path) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_store_t *rejected = NULL;
  mikrio_v1_open_options_t options = {0};
  options.directory = text(path);
  uint32_t status = mikrio_v1_store_open(&options, &rejected, &error);
  failed(status, &error, "InvalidArgument", 0);
  assert(rejected == NULL);

  char missing[4096];
  int length = snprintf(missing, sizeof(missing), "%s-existing-only", path);
  assert(length > 0 && (size_t)length < sizeof(missing));
  options.struct_size = sizeof(options);
  options.directory = text(missing);
  options.create_if_missing = (mikrio_v1_optional_u64_t){1, 0, 0};
  status = mikrio_v1_store_open(&options, &rejected, &error);
  failed(status, &error, "InvalidStoreDirectory", 0);
  assert(rejected == NULL);
  options.directory = text(path);

  options.struct_size = sizeof(options);
  options.create_if_missing = (mikrio_v1_optional_u64_t){1, 0, 0};
  status = mikrio_v1_store_open(&options, &rejected, &error);
  succeeded(status, &error);
  succeeded(mikrio_v1_store_close(rejected, &error), &error);
  mikrio_v1_store_release(rejected);
  rejected = NULL;
  options.create_if_missing.value = 2;
  status = mikrio_v1_store_open(&options, &rejected, &error);
  failed(status, &error, "InvalidArgument", 0);
  assert(rejected == NULL);

  struct {
    mikrio_v1_open_options_t known;
    uint64_t extension;
  } extended = {{0}, 0};

  extended.known.struct_size = sizeof(extended);
  extended.known.directory = text(path);
  status = mikrio_v1_store_open(&extended.known, &rejected, &error);
  succeeded(status, &error);
  status = mikrio_v1_store_close(rejected, &error);
  succeeded(status, &error);
  mikrio_v1_store_release(rejected);
  rejected = NULL;
  extended.extension = 1;
  status = mikrio_v1_store_open(&extended.known, &rejected, &error);
  failed(status, &error, "InvalidArgument", 0);
  assert(rejected == NULL);

  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("bucket");
  bucket.struct_size = 0;

  failed(mikrio_v1_store_create_bucket(store, &bucket, &error), &error,
         "InvalidArgument", 1);

  bucket.struct_size = sizeof(bucket);
  status = mikrio_v1_store_create_bucket(store, &bucket, &error);
  succeeded(status, &error);
  status = mikrio_v1_store_create_bucket(store, &bucket, &error);
  succeeded(status, &error);
  const char *unsupported_owners[] = {"", "000000000000"};

  for (size_t index = 0; index < 2; ++index) {
    bucket.expected_bucket_owner = text(unsupported_owners[index]);

    failed(mikrio_v1_store_create_bucket(store, &bucket, &error), &error,
           "InvalidArgument", 1);
  }

  bucket.expected_bucket_owner = (mikrio_v1_span_t){0};
  mikrio_v1_bucket_output_t *output = NULL;
  status = mikrio_v1_store_head_bucket(store, &bucket, &output, &error);
  succeeded(status, &error);
  assert(equals(mikrio_v1_bucket_output_view(output), "us-east-1"));
  mikrio_v1_bucket_output_release(output);
  output = NULL;
  status = mikrio_v1_store_get_bucket_location(store, &bucket, &output, &error);
  succeeded(status, &error);
  assert(output == NULL);
  bucket.expected_bucket_owner = text("");
  status = mikrio_v1_store_head_bucket(store, &bucket, &output, &error);
  failed(status, &error, "InvalidArgument", 0);
  assert(output == NULL);
  puts("PASS options_and_bucket");
}

static void input_prefixes_and_guarded_boundaries(mikrio_v1_store_t *store,
                                                  const char *path) {
  size_t page;
  uint8_t *mapping = test_guard_pages(&page);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_store_t *other = NULL;
  mikrio_v1_open_options_t options = {0};
  options.struct_size = 24;
  options.directory = text(path);
  options.region = text("omitted-region-must-not-be-read");
  void *prefix = mapping + page - 24;
  memcpy(prefix, &options, 24);
  succeeded(mikrio_v1_store_open(prefix, &other, &error), &error);
  assert(other != NULL);
  succeeded(mikrio_v1_store_close(other, &error), &error);
  mikrio_v1_store_release(other);
  other = NULL;

  const uint64_t rejected_sizes[] = {0,  1,  7,  8,     16,        23,
                                     25, 32, 39, 65537, UINT64_MAX};

  for (size_t i = 0; i < sizeof(rejected_sizes) / sizeof(rejected_sizes[0]);
       i++) {
    memcpy(prefix, &rejected_sizes[i], sizeof(uint64_t));

    failed(mikrio_v1_store_open(prefix, &other, &error), &error,
           "InvalidArgument", 0);

    assert(other == NULL);
  }

  uint8_t *extended = calloc(1, 65536);
  assert(extended != NULL);
  options.struct_size = 65536;
  options.region = (mikrio_v1_span_t){0};
  memcpy(extended, &options, sizeof(options));
  succeeded(mikrio_v1_store_open((void *)extended, &other, &error), &error);
  succeeded(mikrio_v1_store_close(other, &error), &error);
  mikrio_v1_store_release(other);
  other = NULL;
  extended[65535] = 1;

  failed(mikrio_v1_store_open((void *)extended, &other, &error), &error,
         "InvalidArgument", 0);

  assert(other == NULL);
  free(extended);

  mikrio_v1_put_input_t put = put_input("prefix-object");
  put.struct_size = 40;
  prefix = mapping + page - 40;
  memcpy(prefix, &put, 40);
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *written = NULL;
  const uint64_t empty_size = 0;
  memcpy(prefix, &empty_size, sizeof(empty_size));

  failed(mikrio_v1_store_begin_put_object(store, prefix, &writer, &error),
         &error, "InvalidArgument", 1);

  assert(writer == NULL);
  memcpy(prefix, &put, 40);

  succeeded(mikrio_v1_store_begin_put_object(store, prefix, &writer, &error),
            &error);

  succeeded(mikrio_v1_writer_write(writer, text("prefix payload"), &error),
            &error);

  succeeded(mikrio_v1_writer_finish(writer, &written, &error), &error);
  mikrio_v1_writer_release(writer);
  mikrio_v1_put_output_release(written);

  mikrio_v1_read_input_t read = read_input("prefix-object");
  read.struct_size = 40;
  memcpy(prefix, &read, 40);
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;
  memcpy(prefix, &empty_size, sizeof(empty_size));

  failed(mikrio_v1_store_get_object(store, prefix, &facts, &body, &error),
         &error, "InvalidArgument", 0);

  assert(facts == NULL && body == NULL);
  memcpy(prefix, &read, 40);

  succeeded(mikrio_v1_store_get_object(store, prefix, &facts, &body, &error),
            &error);

  struct {
    uint64_t before;
    mikrio_v1_read_view_t view;
    uint64_t after;
  } observed = {UINT64_MAX, {0}, UINT64_MAX};

  observed.view = mikrio_v1_read_output_view(facts);
  assert(observed.before == UINT64_MAX && observed.after == UINT64_MAX);
  assert(observed.view.content_length == 14);
  uint8_t bytes[32] = {0};
  uint64_t count = 0;

  succeeded(
      mikrio_v1_body_read_chunk(body, bytes, sizeof(bytes), &count, &error),
      &error);

  assert(count == 14 && memcmp(bytes, "prefix payload", 14) == 0);
  succeeded(mikrio_v1_body_close(body, &error), &error);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);

  mikrio_v1_list_buckets_output_t *buckets = NULL;
  uint64_t size_only = 8;
  prefix = mapping + page - 8;
  memcpy(prefix, &empty_size, sizeof(empty_size));

  failed(mikrio_v1_store_list_buckets(store, prefix, &buckets, &error), &error,
         "InvalidArgument", 0);

  assert(buckets == NULL);
  memcpy(prefix, &size_only, 8);

  succeeded(mikrio_v1_store_list_buckets(store, prefix, &buckets, &error),
            &error);

  assert(mikrio_v1_list_buckets_output_view(buckets).buckets_count > 0);
  mikrio_v1_list_buckets_output_release(buckets);

  mikrio_v1_delete_input_t remove = {0};
  remove.struct_size = 40;
  remove.bucket = text("bucket");
  remove.key = text("prefix-object");
  prefix = mapping + page - 40;
  memcpy(prefix, &remove, 40);
  memcpy(prefix, &empty_size, sizeof(empty_size));

  failed(mikrio_v1_store_delete_object(store, prefix, &error), &error,
         "InvalidArgument", 1);

  memcpy(prefix, &remove, 40);
  succeeded(mikrio_v1_store_delete_object(store, prefix, &error), &error);
  test_guard_pages_release(mapping, page);
  puts("PASS input_prefixes_and_guarded_boundaries");
}

static void bucket_listing_detached_pages(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  mikrio_v1_error_t *error = NULL;
  bucket.bucket = text("listing-bucket-a");
  succeeded(mikrio_v1_store_create_bucket(store, &bucket, &error), &error);
  bucket.bucket = text("listing-bucket-b");
  succeeded(mikrio_v1_store_create_bucket(store, &bucket, &error), &error);
  mikrio_v1_list_buckets_input_t input = {0};
  input.struct_size = sizeof(input);
  input.prefix = text("listing-bucket-");
  input.max_buckets = (mikrio_v1_optional_u64_t){1, 0, 1};
  mikrio_v1_list_buckets_output_t *first = NULL;

  succeeded(mikrio_v1_store_list_buckets(store, &input, &first, &error),
            &error);

  mikrio_v1_list_buckets_output_view_t view =
      mikrio_v1_list_buckets_output_view(first);

  assert(view.buckets_count == 1 && view.continuation_token.data != NULL);

  mikrio_v1_bucket_summary_view_t summary =
      mikrio_v1_list_buckets_output_item(first, 0);

  assert(equals(summary.name, "listing-bucket-a"));

  assert(equals(summary.bucket_region, "us-east-1") &&
         summary.creation_date_ms > 0);

  assert(mikrio_v1_list_buckets_output_item(first, UINT64_MAX).name.data ==
         NULL);

  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  store = open_store(path);
  input.continuation_token = view.continuation_token;
  mikrio_v1_list_buckets_output_t *last = NULL;
  succeeded(mikrio_v1_store_list_buckets(store, &input, &last, &error), &error);
  view = mikrio_v1_list_buckets_output_view(last);
  assert(view.buckets_count == 1 && view.continuation_token.data == NULL);

  assert(equals(mikrio_v1_list_buckets_output_item(last, 0).name,
                "listing-bucket-b"));

  input.prefix = (mikrio_v1_span_t){0};
  mikrio_v1_list_buckets_output_t *invalid = NULL;

  failed(mikrio_v1_store_list_buckets(store, &input, &invalid, &error), &error,
         "InvalidArgument", 0);

  assert(invalid == NULL);
  input.continuation_token = (mikrio_v1_span_t){0};
  input.max_buckets.value = UINT64_MAX;

  failed(mikrio_v1_store_list_buckets(store, &input, &invalid, &error), &error,
         "InvalidArgument", 0);

  assert(invalid == NULL);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  assert(equals(summary.name, "listing-bucket-a"));
  mikrio_v1_list_buckets_output_release(first);
  mikrio_v1_list_buckets_output_release(last);
  puts("PASS bucket_listing_detached_pages");
}

static void batch_deletion_detached_results(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  write_object(store, "batch-same", "payload");

  mikrio_v1_object_identifier_t objects[] = {
      {text("batch-same"), text("\"wrong\"")},
      {text("batch-same"), text("*")},
      {text("batch-same"), text("*")},
      {text("batch-same"), {0}},
  };

  mikrio_v1_delete_objects_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = text("bucket");
  input.objects = objects;
  input.objects_count = 4;
  mikrio_v1_delete_objects_output_t *output = NULL;
  mikrio_v1_error_t *error = NULL;

  input.struct_size = 0;

  failed(mikrio_v1_store_delete_objects(store, &input, &output, &error), &error,
         "InvalidArgument", 1);

  assert(output == NULL);
  input.struct_size = sizeof(input);

  succeeded(mikrio_v1_store_delete_objects(store, &input, &output, &error),
            &error);

  mikrio_v1_delete_objects_output_view_t view =
      mikrio_v1_delete_objects_output_view(output);

  assert(view.deleted_count == 2 && view.errors_count == 2);

  assert(
      equals(mikrio_v1_delete_objects_output_deleted(output, 0), "batch-same"));

  assert(equals(mikrio_v1_delete_objects_output_error(output, 0).code,
                "PreconditionFailed"));

  assert(equals(mikrio_v1_delete_objects_output_error(output, 1).code,
                "NoSuchKey"));

  mikrio_v1_span_t message =
      mikrio_v1_delete_objects_output_error(output, 0).message;

  assert(message.data != NULL && message.len > 0);

  assert(mikrio_v1_delete_objects_output_error(output, UINT64_MAX).key.data ==
         NULL);

  input.quiet = 1;
  mikrio_v1_delete_objects_output_t *quiet = NULL;

  succeeded(mikrio_v1_store_delete_objects(store, &input, &quiet, &error),
            &error);

  view = mikrio_v1_delete_objects_output_view(quiet);
  assert(view.deleted_count == 0 && view.errors_count == 3);
  mikrio_v1_delete_objects_output_t *invalid = NULL;
  input.quiet = 2;

  failed(mikrio_v1_store_delete_objects(store, &input, &invalid, &error),
         &error, "InvalidArgument", 1);

  input.quiet = 0;
  input.reserved = 1;

  failed(mikrio_v1_store_delete_objects(store, &input, &invalid, &error),
         &error, "InvalidArgument", 1);

  assert(invalid == NULL);
  input.reserved = 0;

  for (uint64_t count = 0; count <= 1001; count += 1001) {
    input.objects_count = count;

    failed(mikrio_v1_store_delete_objects(store, &input, &invalid, &error),
           &error, "InvalidArgument", 1);

    assert(invalid == NULL);
  }

  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);

  assert(equals(mikrio_v1_delete_objects_output_error(output, 0).key,
                "batch-same"));

  assert(
      equals(mikrio_v1_delete_objects_output_deleted(output, 1), "batch-same"));

  assert(mikrio_v1_error_delete_objects_progress_count(NULL) == 0);

  assert(mikrio_v1_error_delete_objects_progress_item(NULL, 0).key.data ==
         NULL);

  mikrio_v1_delete_objects_output_release(output);
  mikrio_v1_delete_objects_output_release(quiet);
  puts("PASS batch_deletion_detached_results");
}

struct batch_call {
  mikrio_v1_store_t *store;
  mikrio_v1_delete_objects_input_t input;
  mikrio_v1_delete_objects_output_t *output;
  mikrio_v1_error_t *error;
  uint32_t status;
};

static void *delete_batch(void *argument) {
  struct batch_call *call = argument;

  call->status = mikrio_v1_store_delete_objects(call->store, &call->input,
                                                &call->output, &call->error);

  return NULL;
}

static void batch_close_retains_error_progress(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  write_object(store, "batch-first", "payload");
  mikrio_v1_object_identifier_t objects[1000] = {0};

  for (size_t index = 0; index < 1000; ++index)
    objects[index].key = text(index == 0 ? "batch-first" : "batch-missing");

  struct batch_call call = {0};
  call.store = store;
  call.input.struct_size = sizeof(call.input);
  call.input.bucket = text("bucket");
  call.input.objects = objects;
  call.input.objects_count = 1000;
  call.input.quiet = 1;
  test_thread worker;
  assert(test_thread_start(&worker, delete_batch, &call) == 0);
  mikrio_v1_read_input_t read = read_input("batch-first");
  mikrio_v1_error_t *error = NULL;

  for (size_t attempts = 0;; ++attempts) {
    assert(attempts < 10000);
    mikrio_v1_read_output_t *facts = NULL;
    uint32_t status = mikrio_v1_store_head_object(store, &read, &facts, &error);

    if (status != 0) {
      failed(status, &error, "NoSuchKey", 0);
      break;
    }

    mikrio_v1_read_output_release(facts);
  }

  succeeded(mikrio_v1_store_close(store, &error), &error);
  assert(test_thread_join(worker) == 0);
  mikrio_v1_store_release(store);
  assert(call.status == 1 && call.output == NULL && call.error != NULL);
  mikrio_v1_error_view_t view = mikrio_v1_error_view(call.error);

  assert((equals(view.code, "StoreClosing") ||
          equals(view.code, "OperationCanceled")) &&
         view.mutation_outcome == 4);

  assert(mikrio_v1_error_delete_objects_progress_count(call.error) == 1000);

  for (uint64_t index = 0; index < 1000; ++index) {
    mikrio_v1_delete_object_progress_view_t item =
        mikrio_v1_error_delete_objects_progress_item(call.error, index);

    assert(item.index == index && item.reserved == 0);
    assert(item.mutation_outcome == 1 || item.mutation_outcome == 2);
  }

  mikrio_v1_delete_object_progress_view_t first =
      mikrio_v1_error_delete_objects_progress_item(call.error, 0);

  mikrio_v1_delete_object_progress_view_t last =
      mikrio_v1_error_delete_objects_progress_item(call.error, 999);

  assert(equals(first.key, "batch-first") && first.mutation_outcome == 2);

  assert(equals(last.key, "batch-missing") && last.mutation_outcome == 1 &&
         last.error_code.data == NULL);

  assert(last.error_message.data == NULL && last.error_message.len == 0);
  mikrio_v1_error_release(call.error);
  puts("PASS batch_close_retains_error_progress");
}

static void listing_detached_pages(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  write_object(store, "listing/a", "abc");
  write_object(store, "listing/b/one", "abc");
  write_object(store, "listing/b/two", "abc");
  write_object(store, "listing/c", "abc");
  mikrio_v1_list_objects_v2_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = text("bucket");
  input.prefix = text("listing/");
  input.delimiter = text("/");
  input.max_keys.present = 1;
  input.max_keys.value = 2;
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_list_objects_v2_output_t *first = NULL;

  input.struct_size = 0;

  failed(mikrio_v1_store_list_objects_v2(store, &input, &first, &error), &error,
         "InvalidArgument", 0);

  assert(first == NULL);
  input.struct_size = sizeof(input);

  succeeded(mikrio_v1_store_list_objects_v2(store, &input, &first, &error),
            &error);

  mikrio_v1_list_objects_v2_output_view_t view =
      mikrio_v1_list_objects_v2_output_view(first);

  assert(view.contents_count == 1 && view.common_prefixes_count == 1);
  assert(view.is_truncated == 1 && view.reserved == 0);

  mikrio_v1_object_summary_view_t object =
      mikrio_v1_list_objects_v2_output_item(first, 0);

  assert(equals(object.key, "listing/a") && object.size == 3);
  assert(equals(object.etag, "\"900150983cd24fb0d6963f7d28e17f72\""));
  mikrio_v1_read_input_t read = read_input("listing/a");
  mikrio_v1_read_output_t *facts = NULL;
  succeeded(mikrio_v1_store_head_object(store, &read, &facts, &error), &error);
  mikrio_v1_read_view_t metadata = mikrio_v1_read_output_view(facts);

  assert(equals(metadata.etag, "\"900150983cd24fb0d6963f7d28e17f72\""));
  assert(metadata.reserved == 0 && metadata.last_modified_ms > 0);
  assert(metadata.last_modified_ms == object.last_modified_at_ms);
  mikrio_v1_read_output_release(facts);

  assert(
      equals(mikrio_v1_list_objects_v2_output_prefix(first, 0), "listing/b/"));

  assert(mikrio_v1_list_objects_v2_output_item(first, UINT64_MAX).key.data ==
         NULL);

  assert(mikrio_v1_list_objects_v2_output_prefix(first, 1).data == NULL);
  input.continuation_token = view.next_continuation_token;
  input.max_keys.value = 1;
  mikrio_v1_list_objects_v2_output_t *last = NULL;

  succeeded(mikrio_v1_store_list_objects_v2(store, &input, &last, &error),
            &error);

  view = mikrio_v1_list_objects_v2_output_view(last);
  assert(view.is_truncated == 0 && view.next_continuation_token.data == NULL);

  assert(
      equals(mikrio_v1_list_objects_v2_output_item(last, 0).key, "listing/c"));

  input.delimiter = (mikrio_v1_span_t){0};
  mikrio_v1_list_objects_v2_output_t *invalid = NULL;

  failed(mikrio_v1_store_list_objects_v2(store, &input, &invalid, &error),
         &error, "InvalidArgument", 0);

  assert(invalid == NULL);
  input.continuation_token = (mikrio_v1_span_t){0};
  input.max_keys.value = UINT64_MAX;

  failed(mikrio_v1_store_list_objects_v2(store, &input, &invalid, &error),
         &error, "InvalidArgument", 0);

  input.delimiter = text("/");
  input.max_keys = (mikrio_v1_optional_u64_t){0};
  input.start_after = text("listing/b/");
  mikrio_v1_list_objects_v2_output_t *after = NULL;

  succeeded(mikrio_v1_store_list_objects_v2(store, &input, &after, &error),
            &error);

  mikrio_v1_list_objects_v2_output_view_t filtered =
      mikrio_v1_list_objects_v2_output_view(after);

  assert(filtered.contents_count == 1 && filtered.common_prefixes_count == 0);

  assert(filtered.is_truncated == 0 &&
         filtered.next_continuation_token.data == NULL);

  assert(
      equals(mikrio_v1_list_objects_v2_output_item(after, 0).key, "listing/c"));

  mikrio_v1_list_objects_v2_output_release(after);
  after = NULL;
  input.start_after = text("");
  input.max_keys = (mikrio_v1_optional_u64_t){1, 0, 0};

  succeeded(mikrio_v1_store_list_objects_v2(store, &input, &after, &error),
            &error);

  filtered = mikrio_v1_list_objects_v2_output_view(after);
  assert(filtered.contents_count == 0 && filtered.common_prefixes_count == 0);

  assert(filtered.is_truncated == 0 &&
         filtered.next_continuation_token.data == NULL);

  mikrio_v1_list_objects_v2_output_release(after);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  assert(equals(object.key, "listing/a"));

  assert(
      equals(mikrio_v1_list_objects_v2_output_item(last, 0).key, "listing/c"));

  mikrio_v1_list_objects_v2_output_release(first);
  mikrio_v1_list_objects_v2_output_release(last);
  puts("PASS listing_detached_pages");
}

static void writer_failure_and_release(mikrio_v1_store_t *store) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *output = NULL;
  mikrio_v1_put_input_t input = put_input("incomplete");
  input.content_length.present = 1;
  input.content_length.value = 5;

  uint32_t status =
      mikrio_v1_store_begin_put_object(store, &input, &writer, &error);

  succeeded(status, &error);
  status = mikrio_v1_writer_write(writer, text("four"), &error);
  succeeded(status, &error);
  status = mikrio_v1_writer_finish(writer, &output, &error);
  failed(status, &error, "IncompleteBody", 1);
  assert(output == NULL);
  mikrio_v1_writer_release(writer);
  missing(store, "incomplete");

  input = put_input("released");
  status = mikrio_v1_store_begin_put_object(store, &input, &writer, &error);
  succeeded(status, &error);
  status = mikrio_v1_writer_write(writer, text("unpublished"), &error);
  succeeded(status, &error);
  mikrio_v1_writer_release(writer);
  missing(store, "released");

  input = put_input("aborted");
  status = mikrio_v1_store_begin_put_object(store, &input, &writer, &error);
  succeeded(status, &error);
  status = mikrio_v1_writer_abort(writer, &error);
  succeeded(status, &error);
  status = mikrio_v1_writer_finish(writer, &output, &error);
  assert(status == 1 && output == NULL && error != NULL);
  assert(mikrio_v1_error_view(error).mutation_outcome == 1);
  mikrio_v1_error_release(error);
  error = NULL;
  mikrio_v1_writer_release(writer);
  missing(store, "aborted");

  input = put_input("finished");
  status = mikrio_v1_store_begin_put_object(store, &input, &writer, &error);
  succeeded(status, &error);
  status = mikrio_v1_writer_write(writer, text("committed"), &error);
  succeeded(status, &error);
  status = mikrio_v1_writer_finish(writer, &output, &error);
  succeeded(status, &error);
  assert(output != NULL);
  mikrio_v1_put_output_release(output);
  output = NULL;
  status = mikrio_v1_writer_abort(writer, &error);
  failed(status, &error, "BodyClosed", 2);
  status = mikrio_v1_writer_write(writer, text("late"), &error);
  failed(status, &error, "BodyClosed", 2);
  status = mikrio_v1_writer_finish(writer, &output, &error);
  failed(status, &error, "BodyClosed", 2);
  assert(output == NULL);
  mikrio_v1_writer_release(writer);

  mikrio_v1_read_input_t read = read_input("finished");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;

  succeeded(mikrio_v1_store_get_object(store, &read, &facts, &body, &error),
            &error);

  assert(mikrio_v1_read_output_view(facts).content_length == 9);
  uint8_t bytes[16] = {0};
  uint64_t count = 0;

  succeeded(
      mikrio_v1_body_read_chunk(body, bytes, sizeof(bytes), &count, &error),
      &error);

  assert(count == 9 && memcmp(bytes, "committed", 9) == 0);
  succeeded(mikrio_v1_body_close(body, &error), &error);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);

  input = put_input("invalid");
  input.content_length.present = 2;
  status = mikrio_v1_store_begin_put_object(store, &input, &writer, &error);
  failed(status, &error, "InvalidArgument", 1);
  assert(writer == NULL);
  puts("PASS writer_failure_and_release");
}

/* Check vectors are pinned independently in
 * tests/contract/checksum-vectors.json. */
static void checksum_and_representation_fields(mikrio_v1_store_t *store) {
  const char *algorithms[] = {"CRC32", "CRC32C", "SHA1", "SHA256", "CRC64NVME"};

  const char *values[] = {
      "y/Q5Jg==", "4waSgw==", "98O8HYCOBHMq32eZZczDTKeuNEE=",
      "FeKw08M4keuw8e9gnsQZQgwg4yDOlMZfvIwzEkSOsiU=", "rosUhgp5mIg="};

  for (unsigned index = 0; index < 5; ++index) {
    mikrio_v1_put_input_t input = put_input(algorithms[index]);

    mikrio_v1_span_t *checksums[] = {
        &input.checksum_crc32, &input.checksum_crc32c, &input.checksum_sha1,
        &input.checksum_sha256, &input.checksum_crc64_nvme};

    input.checksum_algorithm = text(algorithms[index]);
    *checksums[index] = text(values[index]);
    input.content_md5 = text("JfnnlDI7RTiF9RgfG2JNCw==");
    input.content_length = (mikrio_v1_optional_u64_t){1, 0, 9};
    input.content_type = text("application/example");
    input.content_encoding = text("identity");
    input.content_disposition = text("inline");
    input.content_language = text("en");
    input.cache_control = text("max-age=7");
    input.expires = (mikrio_v1_optional_i64_t){1, 0, 1833163506123};
    mikrio_v1_writer_t *writer = NULL;
    mikrio_v1_put_output_t *put = NULL;
    mikrio_v1_error_t *error = NULL;

    uint32_t status =
        mikrio_v1_store_begin_put_object(store, &input, &writer, &error);

    succeeded(status, &error);
    status = mikrio_v1_writer_write(writer, text("123456789"), &error);
    succeeded(status, &error);
    status = mikrio_v1_writer_finish(writer, &put, &error);
    succeeded(status, &error);
    mikrio_v1_put_view_t written = mikrio_v1_put_output_view(put);
    assert(equals(written.etag, "\"25f9e794323b453885f5181f1b624d0b\""));

    mikrio_v1_span_t observed[] = {
        written.checksum_crc32, written.checksum_crc32c, written.checksum_sha1,
        written.checksum_sha256, written.checksum_crc64_nvme};

    for (unsigned field = 0; field < 5; ++field) {
      if (field == index)
        assert(equals(observed[field], values[index]));
      else
        assert(observed[field].data == NULL);
    }

    mikrio_v1_put_output_release(put);
    put = NULL;
    status = mikrio_v1_writer_finish(writer, &put, &error);
    failed(status, &error, "BodyClosed", 2);
    assert(put == NULL);
    mikrio_v1_writer_release(writer);

    mikrio_v1_read_input_t read = read_input(algorithms[index]);
    mikrio_v1_read_output_t *facts = NULL;
    status = mikrio_v1_store_head_object(store, &read, &facts, &error);
    succeeded(status, &error);
    mikrio_v1_read_view_t view = mikrio_v1_read_output_view(facts);

    assert(view.checksum_type.data == NULL &&
           view.checksum_crc32.data == NULL &&
           view.checksum_sha256.data == NULL &&
           view.checksum_crc64_nvme.data == NULL);

    assert(view.content_length == 9 && view.tag_count == 0 &&
           view.metadata_count == 0);

    assert(view.content_range.data == NULL && view.parts_count.present == 0);
    assert(equals(view.content_type, "application/example"));
    assert(equals(view.content_encoding, "identity"));
    assert(equals(view.content_disposition, "inline"));
    assert(equals(view.content_language, "en"));
    assert(equals(view.cache_control, "max-age=7"));
    assert(view.expires.present == 1 && view.expires.value == 1833163506123);
    mikrio_v1_read_output_release(facts);
    read.checksum_mode = text("ENABLED");
    mikrio_v1_body_t *body = NULL;
    status = mikrio_v1_store_get_object(store, &read, &facts, &body, &error);
    succeeded(status, &error);
    view = mikrio_v1_read_output_view(facts);

    mikrio_v1_span_t returned[] = {view.checksum_crc32, view.checksum_crc32c,
                                   view.checksum_sha1, view.checksum_sha256,
                                   view.checksum_crc64_nvme};

    assert(equals(returned[index], values[index]));
    assert(equals(view.checksum_type, "FULL_OBJECT"));
    uint8_t buffer[10];
    uint64_t count;

    status =
        mikrio_v1_body_read_chunk(body, buffer, sizeof(buffer), &count, &error);

    succeeded(status, &error);
    assert(count == 9 && memcmp(buffer, "123456789", 9) == 0);
    status = mikrio_v1_body_close(body, &error);
    succeeded(status, &error);
    mikrio_v1_body_release(body);
    mikrio_v1_read_output_release(facts);
  }

  puts("PASS checksum_and_representation_fields");
}

static void ranges_and_response_overrides(mikrio_v1_store_t *store) {
  write_object(store, "range", "testcontent");
  mikrio_v1_read_input_t read = read_input("range");
  read.range = text("bytes=4-7");
  read.checksum_mode = text("ENABLED");
  read.response_content_type = text("application/pdf");
  read.response_content_encoding = text("identity");
  read.response_content_disposition = text("attachment");
  read.response_content_language = text("");
  read.response_cache_control = text("no-store");
  read.response_expires = (mikrio_v1_optional_i64_t){1, 0, 1833163506123};
  mikrio_v1_error_t *error = NULL;

  for (unsigned get = 0; get < 2; ++get) {
    mikrio_v1_read_output_t *facts = NULL;
    mikrio_v1_body_t *body = NULL;

    uint32_t status =
        get ? mikrio_v1_store_get_object(store, &read, &facts, &body, &error)
            : mikrio_v1_store_head_object(store, &read, &facts, &error);

    succeeded(status, &error);
    mikrio_v1_read_view_t view = mikrio_v1_read_output_view(facts);

    assert(view.content_length == 4 &&
           equals(view.content_range, "bytes 4-7/11"));

    assert(equals(view.content_type, "application/pdf"));
    assert(equals(view.content_encoding, "identity"));
    assert(equals(view.content_disposition, "attachment"));

    assert(view.content_language.data != NULL &&
           equals(view.content_language, ""));

    assert(equals(view.cache_control, "no-store"));
    assert(view.expires.present == 1 && view.expires.value == 1833163506123);

    assert(view.checksum_crc64_nvme.data == NULL &&
           view.checksum_type.data == NULL);

    if (get) {
      uint8_t bytes[32];
      uint64_t count = 0;

      succeeded(
          mikrio_v1_body_read_chunk(body, bytes, sizeof(bytes), &count, &error),
          &error);

      assert(count == 4 && memcmp(bytes, "cont", 4) == 0);

      succeeded(
          mikrio_v1_body_read_chunk(body, bytes, sizeof(bytes), &count, &error),
          &error);

      assert(count == 0);
      succeeded(mikrio_v1_body_close(body, &error), &error);
      mikrio_v1_body_release(body);
    }

    mikrio_v1_read_output_release(facts);
  }

  read.range = text("bytes=99-");
  mikrio_v1_read_output_t *facts = NULL;

  failed(mikrio_v1_store_head_object(store, &read, &facts, &error), &error,
         "InvalidRange", 0);

  assert(facts == NULL);
  read = read_input("range");
  succeeded(mikrio_v1_store_head_object(store, &read, &facts, &error), &error);
  mikrio_v1_read_view_t original = mikrio_v1_read_output_view(facts);
  assert(original.content_length == 11 && original.content_range.data == NULL);
  assert(equals(original.content_type, "binary/octet-stream"));
  mikrio_v1_read_output_release(facts);
  puts("PASS ranges_and_response_overrides");
}

static void terminal_integrity_rejects_replacement(mikrio_v1_store_t *store) {
  write_object(store, "terminal-validation", "predecessor");

  for (unsigned kind = 0; kind < 3; ++kind) {
    mikrio_v1_put_input_t input = put_input("terminal-validation");

    if (kind == 0)
      input.content_length = (mikrio_v1_optional_u64_t){1, 0, 2};

    if (kind == 1)
      input.content_md5 = text("1B2M2Y8AsgTpgAmY7PhCfg==");

    if (kind == 2)
      input.checksum_sha256 =
          text("47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=");

    mikrio_v1_writer_t *writer = NULL;
    mikrio_v1_put_output_t *put = NULL;
    mikrio_v1_error_t *error = NULL;

    uint32_t status =
        mikrio_v1_store_begin_put_object(store, &input, &writer, &error);

    succeeded(status, &error);
    status = mikrio_v1_writer_write(writer, text("extra bytes"), &error);

    if (status == 0) {
      succeeded(status, &error);
      status = mikrio_v1_writer_finish(writer, &put, &error);
    }

    failed(status, &error, kind == 0 ? "IncompleteBody" : "BadDigest", 1);
    assert(put == NULL);
    mikrio_v1_writer_release(writer);
    mikrio_v1_read_input_t read = read_input("terminal-validation");
    mikrio_v1_read_output_t *facts = NULL;
    mikrio_v1_body_t *body = NULL;
    status = mikrio_v1_store_get_object(store, &read, &facts, &body, &error);
    succeeded(status, &error);
    uint8_t buffer[32];
    uint64_t count;

    status =
        mikrio_v1_body_read_chunk(body, buffer, sizeof(buffer), &count, &error);

    succeeded(status, &error);
    assert(count == 11 && memcmp(buffer, "predecessor", 11) == 0);
    status = mikrio_v1_body_close(body, &error);
    succeeded(status, &error);
    mikrio_v1_body_release(body);
    mikrio_v1_read_output_release(facts);
  }

  puts("PASS terminal_integrity_rejects_replacement");
}

static void owner_conditions_and_bucket_retirement(mikrio_v1_store_t *store,
                                                   const char *owner) {
  const char *wrong =
      strcmp(owner, "000000000000") == 0 ? "000000000001" : "000000000000";

  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("retired");
  mikrio_v1_error_t *error = NULL;
  uint32_t status = mikrio_v1_store_create_bucket(store, &bucket, &error);
  succeeded(status, &error);
  bucket.expected_bucket_owner = text(owner);
  mikrio_v1_bucket_output_t *region = NULL;
  status = mikrio_v1_store_head_bucket(store, &bucket, &region, &error);
  succeeded(status, &error);
  mikrio_v1_bucket_output_release(region);
  status = mikrio_v1_store_get_bucket_location(store, &bucket, &region, &error);
  succeeded(status, &error);
  assert(region == NULL);
  status = mikrio_v1_store_delete_bucket(store, &bucket, &error);
  succeeded(status, &error);
  status = mikrio_v1_store_head_bucket(store, &bucket, &region, &error);
  failed(status, &error, "NoSuchBucket", 0);
  bucket.expected_bucket_owner = (mikrio_v1_span_t){0};
  status = mikrio_v1_store_create_bucket(store, &bucket, &error);
  succeeded(status, &error);
  bucket.expected_bucket_owner = text(wrong);
  status = mikrio_v1_store_delete_bucket(store, &bucket, &error);
  failed(status, &error, "AccessDenied", 1);

  mikrio_v1_put_input_t input = put_input("conditional");
  input.expected_bucket_owner = text(owner);
  input.if_none_match = text("*");
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *put = NULL;
  status = mikrio_v1_store_begin_put_object(store, &input, &writer, &error);
  succeeded(status, &error);
  status = mikrio_v1_writer_finish(writer, &put, &error);
  succeeded(status, &error);
  mikrio_v1_writer_release(writer);
  mikrio_v1_span_t etag = mikrio_v1_put_output_view(put).etag;
  mikrio_v1_read_input_t read = read_input("conditional");
  read.expected_bucket_owner = text(owner);
  read.if_match = etag;
  /* Matching ETag has precedence over this pre-epoch stale date. */
  read.if_unmodified_since = (mikrio_v1_optional_i64_t){1, 0, -1};
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;
  status = mikrio_v1_store_get_object(store, &read, &facts, &body, &error);
  succeeded(status, &error);
  assert(mikrio_v1_read_output_view(facts).content_length == 0);
  uint8_t buffer[1];
  uint64_t count = 99;
  status = mikrio_v1_body_read_chunk(body, buffer, 1, &count, &error);
  succeeded(status, &error);
  assert(count == 0);
  status = mikrio_v1_body_close(body, &error);
  succeeded(status, &error);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);
  read.if_match = (mikrio_v1_span_t){0};
  read.if_unmodified_since = (mikrio_v1_optional_i64_t){0};
  read.if_modified_since = (mikrio_v1_optional_i64_t){1, 0, 253402300799999};
  status = mikrio_v1_store_head_object(store, &read, &facts, &error);
  failed(status, &error, "NotModified", 0);
  assert(facts == NULL);
  read.if_modified_since = (mikrio_v1_optional_i64_t){0};
  read.if_none_match = etag;
  status = mikrio_v1_store_get_object(store, &read, &facts, &body, &error);
  failed(status, &error, "NotModified", 0);
  assert(facts == NULL && body == NULL);
  read.if_none_match = (mikrio_v1_span_t){0};
  read.expected_bucket_owner = text(wrong);
  status = mikrio_v1_store_head_object(store, &read, &facts, &error);
  failed(status, &error, "AccessDenied", 0);

  input.if_none_match = (mikrio_v1_span_t){0};
  input.if_match = etag;
  status = mikrio_v1_store_begin_put_object(store, &input, &writer, &error);
  succeeded(status, &error);
  mikrio_v1_put_output_t *replacement = NULL;
  status = mikrio_v1_writer_finish(writer, &replacement, &error);
  succeeded(status, &error);
  mikrio_v1_put_output_release(replacement);
  mikrio_v1_writer_release(writer);
  mikrio_v1_delete_input_t remove = {0};
  remove.struct_size = sizeof(remove);
  remove.bucket = text("bucket");
  remove.key = input.key;
  remove.expected_bucket_owner = text(owner);
  remove.if_match = text("\"mismatch\"");
  status = mikrio_v1_store_delete_object(store, &remove, &error);
  failed(status, &error, "PreconditionFailed", 1);
  remove.if_match = etag;
  status = mikrio_v1_store_delete_object(store, &remove, &error);
  succeeded(status, &error);
  mikrio_v1_put_output_release(put);
  missing(store, "conditional");
  puts("PASS owner_conditions_and_bucket_retirement");
}

static void
full_admission_keeps_writer_and_body_progress(mikrio_v1_store_t *store) {
  write_object(store, "saturation-source", "selected");
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_read_input_t read = read_input("saturation-source");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;

  uint32_t status =
      mikrio_v1_store_get_object(store, &read, &facts, &body, &error);

  succeeded(status, &error);
  mikrio_v1_writer_t *writers[32] = {0};

  for (unsigned index = 0; index < 32; ++index) {
    char key[32];
    assert(snprintf(key, sizeof(key), "saturated-%u", index) > 0);
    mikrio_v1_put_input_t input = put_input(key);

    status = mikrio_v1_store_begin_put_object(store, &input, &writers[index],
                                              &error);

    succeeded(status, &error);
  }

  mikrio_v1_put_input_t input = put_input("unadmitted");
  mikrio_v1_writer_t *extra = NULL;
  status = mikrio_v1_store_begin_put_object(store, &input, &extra, &error);
  failed(status, &error, "StoreBusy", 1);
  assert(extra == NULL);
  uint8_t buffer[16];
  uint64_t count;

  status =
      mikrio_v1_body_read_chunk(body, buffer, sizeof(buffer), &count, &error);

  succeeded(status, &error);
  assert(count == 8 && memcmp(buffer, "selected", 8) == 0);
  status = mikrio_v1_body_close(body, &error);
  succeeded(status, &error);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);

  for (unsigned index = 0; index < 32; ++index) {
    mikrio_v1_put_output_t *put = NULL;
    status = mikrio_v1_writer_write(writers[index], text("bounded"), &error);
    succeeded(status, &error);
    status = mikrio_v1_writer_finish(writers[index], &put, &error);
    succeeded(status, &error);
    mikrio_v1_put_output_release(put);
    mikrio_v1_writer_release(writers[index]);
  }

  puts("PASS full_admission_keeps_writer_and_body_progress");
}

static void selected_body_and_detached_outputs(mikrio_v1_store_t *store,
                                               mikrio_v1_store_t *peer) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *put = NULL;
  mikrio_v1_pair_t metadata[] = {{text("MiXeD"), text("value")}};
  mikrio_v1_pair_t tags[] = {{text("Case"), text("")}};
  mikrio_v1_put_input_t input = put_input("literal/%2F/雪");
  input.metadata = metadata;
  input.metadata_count = 1;
  input.tags = tags;
  input.tag_count = 1;
  input.content_type = text("");
  input.content_encoding = text("");
  input.expires.present = 1;
  input.expires.value = -1;
  input.checksum_algorithm = text("SHA256");

  uint32_t status =
      mikrio_v1_store_begin_put_object(store, &input, &writer, &error);

  succeeded(status, &error);
  uint8_t chunk[131073];
  memset(chunk, 'a', sizeof(chunk));
  mikrio_v1_span_t bytes = {chunk, sizeof(chunk)};
  status = mikrio_v1_writer_write(writer, bytes, &error);
  succeeded(status, &error);
  memset(chunk, 'b', sizeof(chunk));
  status = mikrio_v1_writer_write(writer, bytes, &error);
  succeeded(status, &error);
  memset(chunk, 'x', sizeof(chunk));
  status = mikrio_v1_writer_write(writer, text(""), &error);
  succeeded(status, &error);
  status = mikrio_v1_writer_finish(writer, &put, &error);
  succeeded(status, &error);
  assert(mikrio_v1_put_output_view(put).checksum_sha256.len == 44);
  mikrio_v1_writer_release(writer);

  mikrio_v1_read_input_t read = read_input("literal/%2F/雪");
  read.checksum_mode = text("ENABLED");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;
  status = mikrio_v1_store_get_object(peer, &read, &facts, &body, &error);
  succeeded(status, &error);
  mikrio_v1_read_view_t view = mikrio_v1_read_output_view(facts);
  assert(view.content_length == 2 * sizeof(chunk));
  assert(view.metadata_count == 1 && equals(view.metadata[0].key, "mixed"));
  assert(equals(view.metadata[0].value, "value"));

  assert(view.tag_count == 1 && view.expires.value == -1 &&
         view.expires.present == 1);

  assert(view.content_type.data != NULL && view.content_type.len == 0);
  assert(view.content_encoding.data != NULL && view.content_encoding.len == 0);
  assert(view.content_language.data == NULL);
  assert(equals(view.checksum_type, "FULL_OBJECT"));
  uint64_t count = 99;
  status = mikrio_v1_body_read_chunk(body, chunk, 0, &count, &error);
  failed(status, &error, "InvalidArgument", 0);
  assert(count == 0);

  write_object(store, "literal/%2F/雪", "replacement");
  mikrio_v1_delete_input_t remove = {0};
  remove.struct_size = sizeof(remove);
  remove.bucket = text("bucket");
  remove.key = text("literal/%2F/雪");
  status = mikrio_v1_store_delete_object(store, &remove, &error);
  succeeded(status, &error);
  status = mikrio_v1_store_close(store, &error);
  succeeded(status, &error);
  uint64_t offset = 0;

  do {
    status =
        mikrio_v1_body_read_chunk(body, chunk, sizeof(chunk), &count, &error);

    succeeded(status, &error);

    assert(count <= sizeof(chunk) && count <= 2 * sizeof(chunk) - offset);

    for (uint64_t index = 0; index < count; ++index) {
      assert(chunk[index] == (offset + index < sizeof(chunk) ? 'a' : 'b'));
    }

    offset += count;
  } while (count != 0);

  assert(offset == 2 * sizeof(chunk));

  status =
      mikrio_v1_body_read_chunk(body, chunk, sizeof(chunk), &count, &error);

  succeeded(status, &error);
  assert(count == 0);
  status = mikrio_v1_body_close(body, &error);
  succeeded(status, &error);

  status =
      mikrio_v1_body_read_chunk(body, chunk, sizeof(chunk), &count, &error);

  failed(status, &error, "BodyClosed", 0);
  mikrio_v1_body_release(body);
  assert(view.tag_count == 1 && equals(view.metadata[0].value, "value"));
  mikrio_v1_read_output_release(facts);
  mikrio_v1_put_output_release(put);
  write_object(peer, "survivor", "peer remains usable");
  puts("PASS selected_body_and_detached_outputs");
}

static void *close_connection(void *context) {
  mikrio_v1_error_t *error = NULL;
  uint32_t status = mikrio_v1_store_close(context, &error);
  succeeded(status, &error);
  return NULL;
}

static void concurrent_close(mikrio_v1_store_t *store) {
  test_thread threads[8];

  for (unsigned index = 0; index < 8; ++index) {
    assert(test_thread_start(&threads[index], close_connection, store) == 0);
  }

  for (unsigned index = 0; index < 8; ++index) {
    assert(test_thread_join(threads[index]) == 0);
  }

  puts("PASS concurrent_close");
}

static void truncated_body_is_terminal(const char *path) {
  char directory[4096];
  int length = snprintf(directory, sizeof(directory), "%s-truncated", path);
  assert(length > 0 && (size_t)length < sizeof(directory));
  mikrio_v1_store_t *store = open_store(directory);
  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("bucket");
  mikrio_v1_error_t *error = NULL;
  uint32_t status = mikrio_v1_store_create_bucket(store, &bucket, &error);
  succeeded(status, &error);
  char payload[131073];
  memset(payload, 'x', sizeof(payload) - 1);
  payload[sizeof(payload) - 1] = 0;
  write_object(store, "damaged", payload);
  mikrio_v1_read_input_t input = read_input("damaged");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;
  status = mikrio_v1_store_get_object(store, &input, &facts, &body, &error);
  succeeded(status, &error);
  uint64_t count = 0;

  status =
      mikrio_v1_body_read_chunk(body, (uint8_t *)payload, 1, &count, &error);

  succeeded(status, &error);
  assert(count == 1 && payload[0] == 'x');

  /* This owned corruption fixture has exactly one payload. No second SQLite
   * copy enters the consumer, and the selected descriptor remains live. */
  test_truncate_payload(directory);

  do {
    status = mikrio_v1_body_read_chunk(body, (uint8_t *)payload,
                                       sizeof(payload), &count, &error);

    assert(status != 0 || count > 0);
  } while (status == 0);

  assert(count == 0 && error != NULL);
  mikrio_v1_error_view_t failure = mikrio_v1_error_view(error);

  assert(equals(failure.code, "IoError") ||
         equals(failure.code, "CorruptStore") ||
         equals(failure.code, "BodyReadFailed"));

  assert(failure.mutation_outcome == 0);
  mikrio_v1_error_release(error);
  error = NULL;
  status = mikrio_v1_body_close(body, &error);
  succeeded(status, &error);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);
  status = mikrio_v1_store_close(store, &error);
  succeeded(status, &error);
  mikrio_v1_store_release(store);
  puts("PASS truncated_body_is_terminal");
}

struct diagnostic_context {
  test_mutex mutex;
  test_condition changed;
  mikrio_v1_store_t *store;
  mikrio_v1_diagnostics_t *observer;
  int has_entered;
  int can_finish;
};

static void observe(void *opaque, mikrio_v1_span_t message) {
  struct diagnostic_context *context = opaque;
  assert(message.len > 0 && message.len < 2048);
  /* The record contains categories/counts, never caller paths, keys or bytes.
   */
  char copy[2048];
  memcpy(copy, message.data, message.len);
  copy[message.len] = 0;
  assert(strstr(copy, "secret-diagnostic-canary") == NULL);
  assert(test_mutex_lock(&context->mutex) == 0);

  while (context->observer == NULL) {
    assert(test_condition_wait(&context->changed, &context->mutex) == 0);
  }

  assert(test_mutex_unlock(&context->mutex) == 0);

  mikrio_v1_error_t *error = NULL;
  uint32_t status = mikrio_v1_store_close(context->store, &error);
  succeeded(status, &error);
  status = mikrio_v1_diagnostics_unregister(context->observer, &error);
  failed(status, &error, "InvalidArgument", 0);

  assert(test_mutex_lock(&context->mutex) == 0);
  context->has_entered = 1;
  assert(test_condition_broadcast(&context->changed) == 0);

  while (!context->can_finish) {
    assert(test_condition_wait(&context->changed, &context->mutex) == 0);
  }

  assert(test_mutex_unlock(&context->mutex) == 0);
}

static void diagnostic_context_drain(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_store_t *peer = open_store(path);
  write_object(peer, "secret-diagnostic-canary", "secret-diagnostic-canary");
  mikrio_v1_read_input_t input = read_input("secret-diagnostic-canary");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;
  mikrio_v1_error_t *error = NULL;

  uint32_t status =
      mikrio_v1_store_get_object(peer, &input, &facts, &body, &error);

  succeeded(status, &error);
  mikrio_v1_delete_input_t remove = {0};
  remove.struct_size = sizeof(remove);
  remove.bucket = text("bucket");
  remove.key = input.key;
  status = mikrio_v1_store_delete_object(peer, &remove, &error);
  succeeded(status, &error);

  struct diagnostic_context context = {.mutex = TEST_MUTEX_INITIALIZER,
                                       .store = store};

  test_condition_init(&context.changed);

  mikrio_v1_diagnostics_t *observer = NULL;

  status = mikrio_v1_store_register_diagnostics(store, observe, &context,
                                                &observer, &error);

  succeeded(status, &error);
  assert(test_mutex_lock(&context.mutex) == 0);
  context.observer = observer;
  assert(test_condition_broadcast(&context.changed) == 0);
  uint64_t deadline = test_monotonic_ms() + 30000;

  while (!context.has_entered) {
    assert(test_condition_wait_until(&context.changed, &context.mutex,
                                     deadline) == 0);
  }

  assert(test_mutex_unlock(&context.mutex) == 0);

  /* The callback is blocked at an observed barrier. Closing/releasing its
   * Store neither waits for it nor invalidates its separately owned context. */
  status = mikrio_v1_store_close(store, &error);
  succeeded(status, &error);
  mikrio_v1_store_release(store);
  write_object(peer, "diagnostic-survivor", "peer progresses");
  assert(test_mutex_lock(&context.mutex) == 0);
  context.can_finish = 1;
  assert(test_condition_broadcast(&context.changed) == 0);
  assert(test_mutex_unlock(&context.mutex) == 0);
  status = mikrio_v1_diagnostics_unregister(observer, &error);
  succeeded(status, &error);
  status = mikrio_v1_diagnostics_release(observer, &error);
  succeeded(status, &error);
  test_synchronization_destroy(&context.changed, &context.mutex);
  status = mikrio_v1_body_close(body, &error);
  succeeded(status, &error);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);
  status = mikrio_v1_store_close(peer, &error);
  succeeded(status, &error);
  mikrio_v1_store_release(peer);
  puts("PASS diagnostic_context_drain");
}

static void administration(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_owner_info_t *owner = NULL;
  succeeded(mikrio_v1_store_get_owner_info(store, &owner, &error), &error);
  assert(mikrio_v1_owner_info_view(owner).account_id.len == 12);
  assert(mikrio_v1_owner_info_view(owner).canonical_id.len == 64);

  mikrio_v1_create_principal_input_t create = {0};
  create.struct_size = sizeof(create);
  create.name = text("c-administration");
  mikrio_v1_principal_info_t *principal = NULL;

  create.struct_size = 0;

  failed(mikrio_v1_store_create_principal(store, &create, &principal, &error),
         &error, "InvalidArgument", 1);

  assert(principal == NULL);
  create.struct_size = sizeof(create);

  succeeded(
      mikrio_v1_store_create_principal(store, &create, &principal, &error),
      &error);

  mikrio_v1_principal_info_view_t identity =
      mikrio_v1_principal_info_view(principal);

  assert(identity.principal_id.len == 32 && identity.is_enabled == 1);
  assert(equals(identity.name, "c-administration"));
  mikrio_v1_principal_info_t *duplicate = NULL;

  failed(mikrio_v1_store_create_principal(store, &create, &duplicate, &error),
         &error, "PrincipalAlreadyExists", 1);

  assert(duplicate == NULL);

  mikrio_v1_set_principal_enabled_input_t enabled = {0};
  enabled.struct_size = sizeof(enabled);
  enabled.principal_id = identity.principal_id;
  enabled.is_enabled = 2;

  failed(mikrio_v1_store_set_principal_enabled(store, &enabled, &duplicate,
                                               &error),
         &error, "InvalidArgument", 1);

  assert(duplicate == NULL);
  enabled.is_enabled = 1;
  enabled.reserved = 1;

  failed(mikrio_v1_store_set_principal_enabled(store, &enabled, &duplicate,
                                               &error),
         &error, "InvalidArgument", 1);

  assert(duplicate == NULL);
  enabled.reserved = 0;
  enabled.is_enabled = 0;

  enabled.struct_size = 0;

  failed(mikrio_v1_store_set_principal_enabled(store, &enabled, &duplicate,
                                               &error),
         &error, "InvalidArgument", 1);

  assert(duplicate == NULL);
  enabled.struct_size = sizeof(enabled);

  succeeded(mikrio_v1_store_set_principal_enabled(store, &enabled, &duplicate,
                                                  &error),
            &error);

  assert(mikrio_v1_principal_info_view(duplicate).is_enabled == 0);

  assert(mikrio_v1_principal_info_view(duplicate).created_at_ms ==
         identity.created_at_ms);

  mikrio_v1_principal_info_release(duplicate);
  duplicate = NULL;

  mikrio_v1_put_principal_policy_input_t put = {0};
  put.struct_size = sizeof(put);
  put.principal_id = identity.principal_id;

  const char *document =
      "{\"Version\":\"2012-10-17\",\"Statement\":{\"Effect\":\"Allow\","
      "\"Action\":\"s3:GetObject\",\"Resource\":\"*\"}}";

  put.document = text(document);
  put.struct_size = 0;

  failed(mikrio_v1_store_put_principal_policy(store, &put, &error), &error,
         "InvalidArgument", 1);

  put.struct_size = sizeof(put);

  succeeded(mikrio_v1_store_put_principal_policy(store, &put, &error), &error);
  mikrio_v1_get_principal_policy_input_t get_policy = {0};
  get_policy.struct_size = sizeof(get_policy);
  get_policy.principal_id = identity.principal_id;
  mikrio_v1_get_principal_policy_output_t *policy = NULL;

  get_policy.struct_size = 0;

  failed(
      mikrio_v1_store_get_principal_policy(store, &get_policy, &policy, &error),
      &error, "InvalidArgument", 0);

  assert(policy == NULL);
  get_policy.struct_size = sizeof(get_policy);

  succeeded(
      mikrio_v1_store_get_principal_policy(store, &get_policy, &policy, &error),
      &error);

  assert(equals(mikrio_v1_get_principal_policy_output_view(policy).document,
                document));

  mikrio_v1_create_access_key_input_t issue = {0};
  issue.struct_size = sizeof(issue);
  issue.principal_id = identity.principal_id;
  mikrio_v1_create_access_key_output_t *key = NULL;

  issue.struct_size = 0;

  failed(mikrio_v1_store_create_access_key(store, &issue, &key, &error), &error,
         "InvalidArgument", 1);

  assert(key == NULL);
  issue.struct_size = sizeof(issue);

  succeeded(mikrio_v1_store_create_access_key(store, &issue, &key, &error),
            &error);

  mikrio_v1_create_access_key_output_view_t credential =
      mikrio_v1_create_access_key_output_view(key);

  assert(credential.secret_access_key.len == 64);
  assert(credential.access_key.access_key_id.len == 40);

  mikrio_v1_delete_principal_input_t remove = {0};
  remove.struct_size = sizeof(remove);
  remove.principal_id = identity.principal_id;

  failed(mikrio_v1_store_delete_principal(store, &remove, &error), &error,
         "PrincipalHasAccessKeys", 1);

  mikrio_v1_list_access_keys_input_t list_keys = {0};
  list_keys.struct_size = sizeof(list_keys);
  list_keys.principal_id = identity.principal_id;
  list_keys.max_results.present = 1;
  list_keys.max_results.value = 1;
  mikrio_v1_list_access_keys_output_t *keys = NULL;

  list_keys.struct_size = 0;

  failed(mikrio_v1_store_list_access_keys(store, &list_keys, &keys, &error),
         &error, "InvalidArgument", 0);

  assert(keys == NULL);
  list_keys.struct_size = sizeof(list_keys);

  succeeded(mikrio_v1_store_list_access_keys(store, &list_keys, &keys, &error),
            &error);

  assert(mikrio_v1_list_access_keys_output_view(keys).count == 1);

  assert(mikrio_v1_list_access_keys_output_view(keys)
             .next_continuation_token.data == NULL);

  assert(mikrio_v1_list_access_keys_output_item(keys, 0).access_key_id.len ==
         40);

  assert(mikrio_v1_list_access_keys_output_item(keys, UINT64_MAX)
             .access_key_id.data == NULL);

  mikrio_v1_list_principals_input_t list = {0};
  list.struct_size = sizeof(list);
  list.max_results.present = 1;
  list.max_results.value = 1;
  mikrio_v1_list_principals_output_t *principals = NULL;

  list.struct_size = 0;

  failed(mikrio_v1_store_list_principals(store, &list, &principals, &error),
         &error, "InvalidArgument", 0);

  assert(principals == NULL);
  list.struct_size = sizeof(list);

  succeeded(mikrio_v1_store_list_principals(store, &list, &principals, &error),
            &error);

  assert(mikrio_v1_list_principals_output_view(principals).count == 1);

  assert(equals(mikrio_v1_list_principals_output_item(principals, 0).name,
                "c-administration"));

  assert(
      mikrio_v1_list_principals_output_item(principals, 1).principal_id.data ==
      NULL);

  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);

  assert(credential.secret_access_key.len == 64 &&
         credential.secret_access_key.data[0] != 0);

  assert(equals(mikrio_v1_get_principal_policy_output_view(policy).document,
                document));

  store = open_store(path);
  mikrio_v1_get_principal_input_t get = {0};
  get.struct_size = sizeof(get);
  get.principal_id = identity.principal_id;

  get.struct_size = 0;

  failed(mikrio_v1_store_get_principal(store, &get, &duplicate, &error), &error,
         "InvalidArgument", 0);

  assert(duplicate == NULL);
  get.struct_size = sizeof(get);

  succeeded(mikrio_v1_store_get_principal(store, &get, &duplicate, &error),
            &error);

  assert(mikrio_v1_principal_info_view(duplicate).is_enabled == 0);
  mikrio_v1_principal_info_release(duplicate);
  duplicate = NULL;

  mikrio_v1_delete_access_key_input_t revoke = {0};
  revoke.struct_size = sizeof(revoke);
  revoke.principal_id = identity.principal_id;
  revoke.access_key_id = credential.access_key.access_key_id;
  revoke.struct_size = 0;

  failed(mikrio_v1_store_delete_access_key(store, &revoke, &error), &error,
         "InvalidArgument", 1);

  revoke.struct_size = sizeof(revoke);

  succeeded(mikrio_v1_store_delete_access_key(store, &revoke, &error), &error);
  succeeded(mikrio_v1_store_delete_access_key(store, &revoke, &error), &error);
  mikrio_v1_delete_principal_policy_input_t clear = {0};
  clear.struct_size = sizeof(clear);
  clear.principal_id = identity.principal_id;

  clear.struct_size = 0;

  failed(mikrio_v1_store_delete_principal_policy(store, &clear, &error), &error,
         "InvalidArgument", 1);

  clear.struct_size = sizeof(clear);

  succeeded(mikrio_v1_store_delete_principal_policy(store, &clear, &error),
            &error);

  succeeded(mikrio_v1_store_delete_principal_policy(store, &clear, &error),
            &error);

  remove.struct_size = 0;

  failed(mikrio_v1_store_delete_principal(store, &remove, &error), &error,
         "InvalidArgument", 1);

  remove.struct_size = sizeof(remove);

  succeeded(mikrio_v1_store_delete_principal(store, &remove, &error), &error);

  failed(mikrio_v1_store_get_principal(store, &get, &duplicate, &error), &error,
         "NoSuchPrincipal", 0);

  assert(duplicate == NULL);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_owner_info_t *closed_owner = NULL;

  failed(mikrio_v1_store_get_owner_info(store, &closed_owner, &error), &error,
         "StoreClosed", 0);

  assert(closed_owner == NULL);
  assert(mikrio_v1_owner_info_view(owner).account_id.len == 12);
  mikrio_v1_store_release(store);
  mikrio_v1_list_principals_output_release(principals);
  mikrio_v1_list_access_keys_output_release(keys);
  mikrio_v1_get_principal_policy_output_release(policy);
  mikrio_v1_create_access_key_output_release(key);
  mikrio_v1_principal_info_release(principal);
  mikrio_v1_owner_info_release(owner);
  puts("PASS administration_detached_results_and_revocation");
}

static void administration_pagination(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_principal_info_t *principals[2] = {NULL, NULL};
  mikrio_v1_principal_info_view_t identities[2];
  const char *names[] = {"c-page-α", "c-page-β"};
  mikrio_v1_create_principal_input_t create = {0};
  create.struct_size = sizeof(create);

  for (size_t index = 0; index < 2; ++index) {
    create.name = text(names[index]);

    succeeded(mikrio_v1_store_create_principal(store, &create,
                                               &principals[index], &error),
              &error);

    identities[index] = mikrio_v1_principal_info_view(principals[index]);
    assert(identities[index].principal_id.len == 32);
    assert(equals(identities[index].name, names[index]));

    assert(identities[index].is_enabled == 1 &&
           identities[index].reserved == 0);

    assert(identities[index].created_at_ms > 0);
  }

  int principal_order = memcmp(identities[0].principal_id.data,
                               identities[1].principal_id.data, 32);

  assert(principal_order != 0);
  size_t first_principal = principal_order < 0 ? 0 : 1;
  mikrio_v1_create_access_key_input_t issue = {0};
  issue.struct_size = sizeof(issue);
  issue.principal_id = identities[0].principal_id;
  mikrio_v1_create_access_key_output_t *keys[2] = {NULL, NULL};
  mikrio_v1_access_key_info_view_t credentials[2];

  for (size_t index = 0; index < 2; ++index) {
    succeeded(
        mikrio_v1_store_create_access_key(store, &issue, &keys[index], &error),
        &error);

    credentials[index] =
        mikrio_v1_create_access_key_output_view(keys[index]).access_key;

    assert(credentials[index].access_key_id.len == 40);

    assert(
        same_span(credentials[index].principal_id, identities[0].principal_id));

    assert(credentials[index].created_at_ms >= identities[0].created_at_ms);
  }

  int key_order = memcmp(credentials[0].access_key_id.data,
                         credentials[1].access_key_id.data, 40);

  assert(key_order != 0);
  size_t first_key = key_order < 0 ? 0 : 1;
  mikrio_v1_list_principals_input_t list = {0};
  list.struct_size = sizeof(list);
  list.max_results = (mikrio_v1_optional_u64_t){1, 0, 1};
  mikrio_v1_list_principals_output_t *principal_pages[2] = {NULL, NULL};

  succeeded(mikrio_v1_store_list_principals(store, &list, &principal_pages[0],
                                            &error),
            &error);

  mikrio_v1_list_principals_output_view_t principal_page =
      mikrio_v1_list_principals_output_view(principal_pages[0]);

  assert(principal_page.count == 1);
  assert(principal_page.next_continuation_token.data != NULL);
  assert(principal_page.next_continuation_token.len > 0);
  mikrio_v1_list_access_keys_input_t list_keys = {0};
  list_keys.struct_size = sizeof(list_keys);
  list_keys.principal_id = identities[0].principal_id;
  list_keys.max_results = (mikrio_v1_optional_u64_t){1, 0, 1};
  mikrio_v1_list_access_keys_output_t *key_pages[2] = {NULL, NULL};

  succeeded(mikrio_v1_store_list_access_keys(store, &list_keys, &key_pages[0],
                                             &error),
            &error);

  mikrio_v1_list_access_keys_output_view_t key_page =
      mikrio_v1_list_access_keys_output_view(key_pages[0]);

  assert(key_page.count == 1 && key_page.next_continuation_token.data != NULL);
  assert(key_page.next_continuation_token.len > 0);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  store = open_store(path);
  mikrio_v1_list_principals_output_t *rejected_principals = NULL;
  mikrio_v1_list_access_keys_output_t *rejected_keys = NULL;
  list.continuation_token = key_page.next_continuation_token;

  failed(mikrio_v1_store_list_principals(store, &list, &rejected_principals,
                                         &error),
         &error, "InvalidArgument", 0);

  assert(rejected_principals == NULL);
  list_keys.continuation_token = principal_page.next_continuation_token;

  failed(mikrio_v1_store_list_access_keys(store, &list_keys, &rejected_keys,
                                          &error),
         &error, "InvalidArgument", 0);

  assert(rejected_keys == NULL);
  list_keys.continuation_token = key_page.next_continuation_token;
  list_keys.principal_id = identities[1].principal_id;

  failed(mikrio_v1_store_list_access_keys(store, &list_keys, &rejected_keys,
                                          &error),
         &error, "InvalidArgument", 0);

  assert(rejected_keys == NULL);
  list.continuation_token = principal_page.next_continuation_token;
  list_keys.principal_id = identities[0].principal_id;

  succeeded(mikrio_v1_store_list_principals(store, &list, &principal_pages[1],
                                            &error),
            &error);

  succeeded(mikrio_v1_store_list_access_keys(store, &list_keys, &key_pages[1],
                                             &error),
            &error);

  assert(mikrio_v1_list_principals_output_view(principal_pages[1]).count == 1);

  assert(mikrio_v1_list_principals_output_view(principal_pages[1])
             .next_continuation_token.data == NULL);

  assert(mikrio_v1_list_access_keys_output_view(key_pages[1]).count == 1);

  assert(mikrio_v1_list_access_keys_output_view(key_pages[1])
             .next_continuation_token.data == NULL);

  list.continuation_token = text("");
  list_keys.continuation_token = text("");

  failed(mikrio_v1_store_list_principals(store, &list, &rejected_principals,
                                         &error),
         &error, "InvalidArgument", 0);

  failed(mikrio_v1_store_list_access_keys(store, &list_keys, &rejected_keys,
                                          &error),
         &error, "InvalidArgument", 0);

  assert(rejected_principals == NULL && rejected_keys == NULL);
  list.continuation_token = (mikrio_v1_span_t){0};
  list_keys.continuation_token = (mikrio_v1_span_t){0};
  const uint64_t invalid_bounds[] = {0, 1001};

  for (size_t index = 0; index < 2; ++index) {
    list.max_results.value = invalid_bounds[index];
    list_keys.max_results.value = invalid_bounds[index];

    failed(mikrio_v1_store_list_principals(store, &list, &rejected_principals,
                                           &error),
           &error, "InvalidArgument", 0);

    failed(mikrio_v1_store_list_access_keys(store, &list_keys, &rejected_keys,
                                            &error),
           &error, "InvalidArgument", 0);

    assert(rejected_principals == NULL && rejected_keys == NULL);
  }

  mikrio_v1_delete_access_key_input_t revoke = {0};
  revoke.struct_size = sizeof(revoke);
  revoke.principal_id = identities[1].principal_id;
  revoke.access_key_id = credentials[0].access_key_id;

  failed(mikrio_v1_store_delete_access_key(store, &revoke, &error), &error,
         "InvalidArgument", 1);

  list_keys.max_results = (mikrio_v1_optional_u64_t){0};
  mikrio_v1_list_access_keys_output_t *all_keys = NULL;

  succeeded(
      mikrio_v1_store_list_access_keys(store, &list_keys, &all_keys, &error),
      &error);

  assert(mikrio_v1_list_access_keys_output_view(all_keys).count == 2);

  assert(mikrio_v1_list_access_keys_output_view(all_keys)
             .next_continuation_token.data == NULL);

  mikrio_v1_list_access_keys_output_release(all_keys);
  revoke.principal_id = identities[0].principal_id;

  for (size_t index = 0; index < 2; ++index) {
    revoke.access_key_id = credentials[index].access_key_id;

    succeeded(mikrio_v1_store_delete_access_key(store, &revoke, &error),
              &error);
  }

  for (size_t index = 0; index < 2; ++index) {
    mikrio_v1_delete_principal_input_t remove = {0};
    remove.struct_size = sizeof(remove);
    remove.principal_id = identities[index].principal_id;

    succeeded(mikrio_v1_store_delete_principal(store, &remove, &error), &error);
  }

  list.max_results = (mikrio_v1_optional_u64_t){0};
  mikrio_v1_list_principals_output_t *empty = NULL;

  succeeded(mikrio_v1_store_list_principals(store, &list, &empty, &error),
            &error);

  assert(mikrio_v1_list_principals_output_view(empty).count == 0);

  assert(mikrio_v1_list_principals_output_view(empty)
             .next_continuation_token.data == NULL);

  mikrio_v1_list_principals_output_release(empty);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);

  /* Pages retain their exact values after revocation, deletion and close. */
  for (size_t page = 0; page < 2; ++page) {
    size_t principal_index = page == 0 ? first_principal : 1 - first_principal;
    size_t key_index = page == 0 ? first_key : 1 - first_key;

    mikrio_v1_principal_info_view_t identity =
        mikrio_v1_list_principals_output_item(principal_pages[page], 0);

    assert(same_span(identity.principal_id,
                     identities[principal_index].principal_id));

    assert(equals(identity.name, names[principal_index]));
    assert(identity.is_enabled == 1 && identity.reserved == 0);
    assert(identity.created_at_ms == identities[principal_index].created_at_ms);

    mikrio_v1_access_key_info_view_t credential =
        mikrio_v1_list_access_keys_output_item(key_pages[page], 0);

    assert(same_span(credential.access_key_id,
                     credentials[key_index].access_key_id));

    assert(same_span(credential.principal_id, identities[0].principal_id));
    assert(credential.created_at_ms == credentials[key_index].created_at_ms);
    mikrio_v1_list_principals_output_release(principal_pages[page]);
    mikrio_v1_list_access_keys_output_release(key_pages[page]);
  }

  for (size_t index = 0; index < 2; ++index) {
    mikrio_v1_create_access_key_output_release(keys[index]);
    mikrio_v1_principal_info_release(principals[index]);
  }

  puts("PASS administration_pagination_scope_and_detached_fields");
}

static void listener_ownership(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_listen_options_t options = {0};
  options.struct_size = sizeof(options);
  mikrio_v1_server_t *server = NULL;
  options.credentials.access_key_id = text("c-owner");

  failed(mikrio_v1_store_listen(store, &options, &server, &error), &error,
         "InvalidArgument", 0);

  assert(server == NULL);
  options.credentials.secret_access_key = text("synthetic-c-listener-secret");
  options.public_endpoint = text("https://example.invalid/");
  options.struct_size = 0;

  failed(mikrio_v1_store_listen(store, &options, &server, &error), &error,
         "InvalidArgument", 0);

  assert(server == NULL);
  options.struct_size = sizeof(options);

  succeeded(mikrio_v1_store_listen(store, &options, &server, &error), &error);
  mikrio_v1_server_info_t *info = NULL;
  succeeded(mikrio_v1_server_info(server, &info, &error), &error);
  mikrio_v1_server_info_view_t snapshot = mikrio_v1_server_info_view(info);
  assert(equals(snapshot.region, "us-east-1"));
  assert(equals(snapshot.credentials.access_key_id, "c-owner"));

  assert(equals(snapshot.credentials.secret_access_key,
                "synthetic-c-listener-secret"));

  assert(snapshot.endpoint.len > 17 &&
         memcmp(snapshot.endpoint.data, "http://127.0.0.1:", 17) == 0);

  mikrio_v1_server_t *duplicate = NULL;

  failed(mikrio_v1_store_listen(store, &options, &duplicate, &error), &error,
         "ListenerAlreadyRunning", 0);

  assert(duplicate == NULL);
  succeeded(mikrio_v1_server_close(server, &error), &error);
  succeeded(mikrio_v1_server_close(server, &error), &error);
  mikrio_v1_server_release(server);
  succeeded(mikrio_v1_store_listen(store, &options, &server, &error), &error);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  succeeded(mikrio_v1_server_close(server, &error), &error);
  mikrio_v1_server_release(server);

  assert(equals(mikrio_v1_server_info_view(info).credentials.access_key_id,
                "c-owner"));

  mikrio_v1_server_info_release(info);

  store = open_store(path);
  server = NULL;
  options.credentials = (mikrio_v1_credentials_t){0};
  options.host = text("not-an-ip");

  failed(mikrio_v1_store_listen(store, &options, &server, &error), &error,
         "InvalidArgument", 0);

  assert(server == NULL);
  options.host = text("127.0.0.1");
  options.port = (mikrio_v1_optional_u64_t){1, 0, 65536};

  failed(mikrio_v1_store_listen(store, &options, &server, &error), &error,
         "InvalidArgument", 0);

  assert(server == NULL);
  options.port = (mikrio_v1_optional_u64_t){1, 0, 0};
  options.public_endpoint = text("file:///unsupported-origin");

  failed(mikrio_v1_store_listen(store, &options, &server, &error), &error,
         "InvalidArgument", 0);

  assert(server == NULL);
  options.public_endpoint = text("https://example.invalid/");
  succeeded(mikrio_v1_store_listen(store, &options, &server, &error), &error);
  mikrio_v1_server_info_t *explicit_info = NULL;
  succeeded(mikrio_v1_server_info(server, &explicit_info, &error), &error);

  mikrio_v1_span_t explicit_endpoint =
      mikrio_v1_server_info_view(explicit_info).endpoint;

  assert(explicit_endpoint.len > 17 &&
         memcmp(explicit_endpoint.data, "http://127.0.0.1:", 17) == 0);

  assert(!equals(explicit_endpoint, "http://127.0.0.1:0"));
  mikrio_v1_server_info_release(explicit_info);
  mikrio_v1_store_release(store);
  succeeded(mikrio_v1_server_close(server, &error), &error);
  mikrio_v1_server_release(server);
  puts("PASS listener_detached_snapshots_close_and_parent_release");
}

static void access_configuration(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_store_t *peer = open_store(path);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("configuration");
  succeeded(mikrio_v1_store_create_bucket(store, &bucket, &error), &error);
  mikrio_v1_owner_info_t *owner = NULL;
  succeeded(mikrio_v1_store_get_owner_info(store, &owner, &error), &error);
  mikrio_v1_owner_info_view_t identity = mikrio_v1_owner_info_view(owner);

  mikrio_v1_put_bucket_policy_input_t policy_input = {0};
  policy_input.struct_size = sizeof(policy_input);
  policy_input.bucket = bucket.bucket;
  policy_input.expected_bucket_owner = identity.account_id;

  const char *document =
      "{ "
      "\"Version\":\"2012-10-17\",\"Statement\":{\"Effect\":\"Allow\","
      "\"Principal\":\"*\",\"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:"
      "s3:::configuration/*\"}}";

  policy_input.document = text(document);

  failed(mikrio_v1_store_put_bucket_policy(store, &policy_input, &error),
         &error, "AccessDenied", 1);

  mikrio_v1_delete_public_access_block_input_t delete_flags = {0};
  delete_flags.struct_size = sizeof(delete_flags);
  delete_flags.bucket = bucket.bucket;

  delete_flags.struct_size = 0;

  failed(
      mikrio_v1_store_delete_public_access_block(store, &delete_flags, &error),
      &error, "InvalidArgument", 1);

  delete_flags.struct_size = sizeof(delete_flags);

  succeeded(
      mikrio_v1_store_delete_public_access_block(store, &delete_flags, &error),
      &error);

  policy_input.struct_size = 0;

  failed(mikrio_v1_store_put_bucket_policy(store, &policy_input, &error),
         &error, "InvalidArgument", 1);

  policy_input.struct_size = sizeof(policy_input);

  succeeded(mikrio_v1_store_put_bucket_policy(store, &policy_input, &error),
            &error);

  mikrio_v1_get_bucket_policy_input_t get_policy = {0};
  get_policy.struct_size = sizeof(get_policy);
  get_policy.bucket = bucket.bucket;
  mikrio_v1_get_bucket_policy_output_t *policy = NULL;

  get_policy.struct_size = 0;

  failed(mikrio_v1_store_get_bucket_policy(peer, &get_policy, &policy, &error),
         &error, "InvalidArgument", 0);

  assert(policy == NULL);
  get_policy.struct_size = sizeof(get_policy);

  succeeded(
      mikrio_v1_store_get_bucket_policy(peer, &get_policy, &policy, &error),
      &error);

  assert(equals(mikrio_v1_get_bucket_policy_output_view(policy).document,
                document));

  mikrio_v1_get_bucket_policy_status_input_t get_status = {0};
  get_status.struct_size = sizeof(get_status);
  get_status.bucket = bucket.bucket;

  mikrio_v1_put_public_access_block_input_t put_flags = {0};
  put_flags.struct_size = sizeof(put_flags);
  put_flags.bucket = bucket.bucket;
  mikrio_v1_get_public_access_block_input_t get_flags = {0};
  get_flags.struct_size = sizeof(get_flags);
  get_flags.bucket = bucket.bucket;
  mikrio_v1_get_public_access_block_output_t *flags = NULL;

  for (uint32_t bits = 0; bits < 16; ++bits) {
    put_flags.configuration = (mikrio_v1_public_access_block_configuration_t){
        bits & 1, (bits >> 1) & 1, (bits >> 2) & 1, (bits >> 3) & 1};

    put_flags.struct_size = 0;

    failed(mikrio_v1_store_put_public_access_block(store, &put_flags, &error),
           &error, "InvalidArgument", 1);

    put_flags.struct_size = sizeof(put_flags);

    succeeded(
        mikrio_v1_store_put_public_access_block(store, &put_flags, &error),
        &error);

    succeeded(mikrio_v1_store_get_public_access_block(peer, &get_flags, &flags,
                                                      &error),
              &error);

    mikrio_v1_public_access_block_configuration_t value =
        mikrio_v1_get_public_access_block_output_view(flags).configuration;

    assert(value.block_public_acls == (bits & 1));
    assert(value.ignore_public_acls == ((bits >> 1) & 1));
    assert(value.block_public_policy == ((bits >> 2) & 1));
    assert(value.restrict_public_buckets == ((bits >> 3) & 1));
    mikrio_v1_get_public_access_block_output_release(flags);
    flags = NULL;
    mikrio_v1_get_bucket_policy_status_output_t *status = NULL;

    get_status.struct_size = 0;

    failed(mikrio_v1_store_get_bucket_policy_status(peer, &get_status, &status,
                                                    &error),
           &error, "InvalidArgument", 0);

    assert(status == NULL);
    get_status.struct_size = sizeof(get_status);

    succeeded(mikrio_v1_store_get_bucket_policy_status(peer, &get_status,
                                                       &status, &error),
              &error);

    assert(mikrio_v1_get_bucket_policy_status_output_view(status).is_public ==
           1);

    assert(mikrio_v1_get_bucket_policy_status_output_view(status).reserved ==
           0);

    mikrio_v1_get_bucket_policy_status_output_release(status);
  }

  put_flags.configuration.block_public_acls = 2;

  failed(mikrio_v1_store_put_public_access_block(store, &put_flags, &error),
         &error, "InvalidArgument", 1);

  get_flags.struct_size = 0;

  failed(mikrio_v1_store_get_public_access_block(store, &get_flags, &flags,
                                                 &error),
         &error, "InvalidArgument", 0);

  assert(flags == NULL);
  get_flags.struct_size = sizeof(get_flags);

  succeeded(
      mikrio_v1_store_get_public_access_block(peer, &get_flags, &flags, &error),
      &error);

  assert(mikrio_v1_get_public_access_block_output_view(flags)
             .configuration.block_public_acls == 1);

  mikrio_v1_put_bucket_ownership_controls_input_t set_ownership = {0};
  set_ownership.struct_size = sizeof(set_ownership);
  set_ownership.bucket = bucket.bucket;

  failed(mikrio_v1_store_put_bucket_ownership_controls(store, &set_ownership,
                                                       &error),
         &error, "InvalidArgument", 1);

  set_ownership.object_ownership = MIKRIO_V1_BUCKET_OWNER_ENFORCED;

  set_ownership.struct_size = 0;

  failed(mikrio_v1_store_put_bucket_ownership_controls(store, &set_ownership,
                                                       &error),
         &error, "InvalidArgument", 1);

  set_ownership.struct_size = sizeof(set_ownership);

  succeeded(mikrio_v1_store_put_bucket_ownership_controls(store, &set_ownership,
                                                          &error),
            &error);

  set_ownership.reserved = 1;

  failed(mikrio_v1_store_put_bucket_ownership_controls(store, &set_ownership,
                                                       &error),
         &error, "InvalidArgument", 1);

  mikrio_v1_get_bucket_ownership_controls_input_t get_ownership = {0};
  get_ownership.struct_size = sizeof(get_ownership);
  get_ownership.bucket = bucket.bucket;
  mikrio_v1_get_bucket_ownership_controls_output_t *ownership = NULL;

  get_ownership.struct_size = 0;

  failed(mikrio_v1_store_get_bucket_ownership_controls(peer, &get_ownership,
                                                       &ownership, &error),
         &error, "InvalidArgument", 0);

  assert(ownership == NULL);
  get_ownership.struct_size = sizeof(get_ownership);

  succeeded(mikrio_v1_store_get_bucket_ownership_controls(peer, &get_ownership,
                                                          &ownership, &error),
            &error);

  assert(mikrio_v1_get_bucket_ownership_controls_output_view(ownership)
             .object_ownership == MIKRIO_V1_BUCKET_OWNER_ENFORCED);

  assert(
      mikrio_v1_get_bucket_ownership_controls_output_view(ownership).reserved ==
      0);

  mikrio_v1_get_bucket_acl_input_t get_acl = {0};
  get_acl.struct_size = sizeof(get_acl);
  get_acl.bucket = bucket.bucket;
  mikrio_v1_get_bucket_acl_output_t *acl = NULL;

  get_acl.struct_size = 0;

  failed(mikrio_v1_store_get_bucket_acl(peer, &get_acl, &acl, &error), &error,
         "InvalidArgument", 0);

  assert(acl == NULL);
  get_acl.struct_size = sizeof(get_acl);

  succeeded(mikrio_v1_store_get_bucket_acl(peer, &get_acl, &acl, &error),
            &error);

  assert(mikrio_v1_get_bucket_acl_output_view(acl).grant_count == 1);
  mikrio_v1_put_input_t object = put_input("acl-probe");
  object.bucket = bucket.bucket;
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *put = NULL;

  succeeded(mikrio_v1_store_begin_put_object(store, &object, &writer, &error),
            &error);

  succeeded(mikrio_v1_writer_write(writer, text("payload"), &error), &error);
  succeeded(mikrio_v1_writer_finish(writer, &put, &error), &error);
  mikrio_v1_writer_release(writer);
  mikrio_v1_put_output_release(put);
  mikrio_v1_get_object_acl_input_t get_object_acl = {0};
  get_object_acl.struct_size = sizeof(get_object_acl);
  get_object_acl.bucket = bucket.bucket;
  get_object_acl.key = object.key;
  mikrio_v1_get_object_acl_output_t *object_acl = NULL;

  get_object_acl.struct_size = 0;

  failed(mikrio_v1_store_get_object_acl(peer, &get_object_acl, &object_acl,
                                        &error),
         &error, "InvalidArgument", 0);

  assert(object_acl == NULL);
  get_object_acl.struct_size = sizeof(get_object_acl);

  succeeded(mikrio_v1_store_get_object_acl(peer, &get_object_acl, &object_acl,
                                           &error),
            &error);

  assert(mikrio_v1_get_object_acl_output_view(object_acl).grant_count == 1);

  assert(mikrio_v1_get_object_acl_output_grant(object_acl, 0).permission ==
         MIKRIO_V1_ACL_FULL_CONTROL);

  assert(mikrio_v1_get_object_acl_output_grant(object_acl, UINT64_MAX)
             .grantee_canonical_id.data == NULL);

  assert(mikrio_v1_get_bucket_acl_output_grant(acl, 1).permission == 0);

  succeeded(mikrio_v1_store_close(store, &error), &error);
  succeeded(mikrio_v1_store_close(peer, &error), &error);
  mikrio_v1_store_release(store);
  mikrio_v1_store_release(peer);

  assert(equals(mikrio_v1_get_bucket_policy_output_view(policy).document,
                document));

  assert(mikrio_v1_get_public_access_block_output_view(flags)
             .configuration.restrict_public_buckets == 1);

  mikrio_v1_acl_grant_view_t grant =
      mikrio_v1_get_object_acl_output_grant(object_acl, 0);

  assert(grant.reserved == 0);

  mikrio_v1_span_t object_owner =
      mikrio_v1_get_object_acl_output_view(object_acl).owner.canonical_id;

  assert(object_owner.len == identity.canonical_id.len);

  assert(memcmp(object_owner.data, identity.canonical_id.data,
                (size_t)identity.canonical_id.len) == 0);

  assert(grant.grantee_canonical_id.len == identity.canonical_id.len);

  assert(memcmp(grant.grantee_canonical_id.data, identity.canonical_id.data,
                (size_t)identity.canonical_id.len) == 0);

  assert(mikrio_v1_get_bucket_acl_output_view(acl).owner.canonical_id.len ==
         64);

  store = open_store(path);
  mikrio_v1_get_bucket_policy_output_t *reopened_policy = NULL;

  succeeded(mikrio_v1_store_get_bucket_policy(store, &get_policy,
                                              &reopened_policy, &error),
            &error);

  assert(
      equals(mikrio_v1_get_bucket_policy_output_view(reopened_policy).document,
             document));

  mikrio_v1_get_bucket_policy_output_release(reopened_policy);
  mikrio_v1_delete_bucket_policy_input_t delete_policy = {0};
  delete_policy.struct_size = sizeof(delete_policy);
  delete_policy.bucket = bucket.bucket;

  for (int repetition = 0; repetition < 2; ++repetition) {
    delete_policy.struct_size = 0;

    failed(mikrio_v1_store_delete_bucket_policy(store, &delete_policy, &error),
           &error, "InvalidArgument", 1);

    delete_policy.struct_size = sizeof(delete_policy);

    succeeded(
        mikrio_v1_store_delete_bucket_policy(store, &delete_policy, &error),
        &error);

    succeeded(mikrio_v1_store_delete_public_access_block(store, &delete_flags,
                                                         &error),
              &error);
  }

  mikrio_v1_get_public_access_block_output_t *absent_flags = NULL;

  failed(mikrio_v1_store_get_public_access_block(store, &get_flags,
                                                 &absent_flags, &error),
         &error, "NoSuchPublicAccessBlockConfiguration", 0);

  assert(absent_flags == NULL);
  reopened_policy = NULL;

  failed(mikrio_v1_store_get_bucket_policy(store, &get_policy, &reopened_policy,
                                           &error),
         &error, "NoSuchBucketPolicy", 0);

  assert(reopened_policy == NULL);
  succeeded(mikrio_v1_store_close(store, &error), &error);

  failed(mikrio_v1_store_delete_bucket_policy(store, &delete_policy, &error),
         &error, "StoreClosed", 1);

  mikrio_v1_store_release(store);
  mikrio_v1_get_bucket_policy_output_release(policy);
  mikrio_v1_get_public_access_block_output_release(flags);
  mikrio_v1_get_bucket_ownership_controls_output_release(ownership);
  mikrio_v1_get_bucket_acl_output_release(acl);
  mikrio_v1_get_object_acl_output_release(object_acl);
  mikrio_v1_owner_info_release(owner);
  mikrio_v1_get_bucket_acl_output_release(NULL);
  assert(mikrio_v1_get_bucket_acl_output_view(NULL).grant_count == 0);
  puts("PASS access_configuration_and_detached_acl_ownership");
}

static void cors_failures(mikrio_v1_store_t *store,
                          const mikrio_v1_put_bucket_cors_input_t *put,
                          const mikrio_v1_get_bucket_cors_input_t *get,
                          const mikrio_v1_delete_bucket_cors_input_t *remove,
                          const char *code) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_get_bucket_cors_output_t *result = NULL;
  failed(mikrio_v1_store_put_bucket_cors(store, put, &error), &error, code, 1);

  failed(mikrio_v1_store_get_bucket_cors(store, get, &result, &error), &error,
         code, 0);

  assert(result == NULL);

  failed(mikrio_v1_store_delete_bucket_cors(store, remove, &error), &error,
         code, 1);
}

static void cors_configuration(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_owner_info_t *owner = NULL;
  succeeded(mikrio_v1_store_get_owner_info(store, &owner, &error), &error);
  mikrio_v1_span_t account = mikrio_v1_owner_info_view(owner).account_id;
  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("cors");
  succeeded(mikrio_v1_store_create_bucket(store, &bucket, &error), &error);
  mikrio_v1_span_t origins[] = {text("https://app.example")};
  mikrio_v1_span_t methods[] = {text("GET"), text("PUT")};
  mikrio_v1_span_t expose[] = {text("ETag")};
  mikrio_v1_span_t empty = {0};
  mikrio_v1_cors_rule_input_t rules[2] = {0};
  rules[0].struct_size = sizeof(rules[0]);
  rules[0].id = text("browser");
  rules[0].allowed_origins = (mikrio_v1_span_list_t){origins, 1};
  rules[0].allowed_methods = (mikrio_v1_span_list_t){methods, 2};
  rules[0].allowed_headers = (mikrio_v1_span_list_t){&empty, 0};
  rules[0].expose_headers = (mikrio_v1_span_list_t){expose, 1};
  rules[0].max_age_seconds.present = 1;
  rules[1] = rules[0];
  rules[1].id = (mikrio_v1_span_t){0};
  rules[1].allowed_headers = (mikrio_v1_span_list_t){0};
  rules[1].max_age_seconds = (mikrio_v1_optional_i64_t){0};
  mikrio_v1_put_bucket_cors_input_t put = {0};
  put.struct_size = sizeof(put);
  put.bucket = bucket.bucket;
  put.cors_rules = rules;
  put.cors_rule_count = 2;
  mikrio_v1_get_bucket_cors_input_t get = {0};
  get.struct_size = sizeof(get);
  get.bucket = bucket.bucket;
  mikrio_v1_delete_bucket_cors_input_t remove = {0};
  remove.struct_size = sizeof(remove);
  remove.bucket = bucket.bucket;
  put.expected_bucket_owner = account;
  get.expected_bucket_owner = account;
  remove.expected_bucket_owner = account;
  mikrio_v1_get_bucket_cors_output_t *result = NULL;

  failed(mikrio_v1_store_get_bucket_cors(store, &get, &result, &error), &error,
         "NoSuchCORSConfiguration", 0);

  assert(result == NULL);
  put.struct_size = 0;

  failed(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error,
         "InvalidArgument", 1);

  put.struct_size = sizeof(put);

  succeeded(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error);
  origins[0] = text("https://changed.example");

  get.struct_size = 0;

  failed(mikrio_v1_store_get_bucket_cors(store, &get, &result, &error), &error,
         "InvalidArgument", 0);

  assert(result == NULL);
  get.struct_size = sizeof(get);

  succeeded(mikrio_v1_store_get_bucket_cors(store, &get, &result, &error),
            &error);

  assert(mikrio_v1_get_bucket_cors_output_view(result).cors_rule_count == 2);

  mikrio_v1_cors_rule_view_t first =
      mikrio_v1_get_bucket_cors_output_rule(result, 0);

  assert(equals(first.id, "browser"));
  assert(first.allowed_origin_count == 1 && first.allowed_method_count == 2);

  assert(first.allowed_header_count.present == 1 &&
         first.allowed_header_count.value == 0);

  assert(first.expose_header_count.present == 1 &&
         first.expose_header_count.value == 1);

  assert(mikrio_v1_get_bucket_cors_output_rule(result, 1).id.data == NULL);

  assert(first.max_age_seconds.present == 1 &&
         first.max_age_seconds.value == 0);

  assert(mikrio_v1_get_bucket_cors_output_rule(result, 1)
             .allowed_header_count.present == 0);

  assert(mikrio_v1_get_bucket_cors_output_rule(result, 1)
             .max_age_seconds.present == 0);

  assert(equals(mikrio_v1_get_bucket_cors_output_allowed_origin(result, 0, 0),
                "https://app.example"));

  assert(equals(mikrio_v1_get_bucket_cors_output_allowed_method(result, 0, 1),
                "PUT"));

  assert(equals(mikrio_v1_get_bucket_cors_output_expose_header(result, 0, 0),
                "ETag"));

  assert(mikrio_v1_get_bucket_cors_output_allowed_header(result, 0, 0).data ==
         NULL);

  assert(mikrio_v1_get_bucket_cors_output_allowed_origin(result, UINT64_MAX, 0)
             .data == NULL);

  assert(mikrio_v1_get_bucket_cors_output_rule(NULL, 0).allowed_method_count ==
         0);

  const int64_t seconds[] = {INT32_MIN, -1, INT32_MAX};

  for (size_t index = 0; index < sizeof(seconds) / sizeof(seconds[0]);
       ++index) {
    rules[1].max_age_seconds = (mikrio_v1_optional_i64_t){1, 0, seconds[index]};
    succeeded(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error);
    mikrio_v1_get_bucket_cors_output_t *signed_value = NULL;

    succeeded(
        mikrio_v1_store_get_bucket_cors(store, &get, &signed_value, &error),
        &error);

    assert(mikrio_v1_get_bucket_cors_output_rule(signed_value, 1)
               .max_age_seconds.value == seconds[index]);

    mikrio_v1_get_bucket_cors_output_release(signed_value);
  }

  rules[1].max_age_seconds =
      (mikrio_v1_optional_i64_t){1, 0, INT64_C(2147483648)};

  failed(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error,
         "InvalidArgument", 1);

  rules[1].max_age_seconds.value = INT64_C(-2147483649);

  failed(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error,
         "InvalidArgument", 1);

  rules[1].max_age_seconds = (mikrio_v1_optional_i64_t){0};
  origins[0] = text("https://app.example");
  succeeded(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error);

  mikrio_v1_span_t wrong =
      text(equals(account, "000000000000") ? "111111111111" : "000000000000");

  put.expected_bucket_owner = wrong;
  get.expected_bucket_owner = wrong;
  remove.expected_bucket_owner = wrong;
  cors_failures(store, &put, &get, &remove, "AccessDenied");
  put.expected_bucket_owner = text("");
  get.expected_bucket_owner = text("");
  remove.expected_bucket_owner = text("");
  cors_failures(store, &put, &get, &remove, "InvalidArgument");
  put.expected_bucket_owner = (mikrio_v1_span_t){0};
  get.expected_bucket_owner = (mikrio_v1_span_t){0};
  remove.expected_bucket_owner = (mikrio_v1_span_t){0};
  put.bucket = text("missing-cors");
  get.bucket = put.bucket;
  remove.bucket = put.bucket;
  cors_failures(store, &put, &get, &remove, "NoSuchBucket");
  put.bucket = bucket.bucket;
  get.bucket = bucket.bucket;
  remove.bucket = bucket.bucket;

  rules[1].struct_size = 0;

  failed(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error,
         "InvalidArgument", 1);

  rules[1].struct_size = sizeof(rules[1]);
  rules[1].max_age_seconds.present = 2;

  failed(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error,
         "InvalidArgument", 1);

  rules[1].max_age_seconds.present = 0;
  rules[1].allowed_methods.count = 1;
  rules[1].allowed_methods.data = NULL;

  failed(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error,
         "InvalidArgument", 1);

  rules[1].allowed_methods = rules[0].allowed_methods;
  put.cors_rule_count = 101;

  failed(mikrio_v1_store_put_bucket_cors(store, &put, &error), &error,
         "InvalidArgument", 1);

  put.cors_rule_count = 2;
  succeeded(mikrio_v1_store_close(store, &error), &error);
  cors_failures(store, &put, &get, &remove, "StoreClosed");
  mikrio_v1_store_release(store);
  store = open_store(path);
  mikrio_v1_get_bucket_cors_output_t *reopened = NULL;

  succeeded(mikrio_v1_store_get_bucket_cors(store, &get, &reopened, &error),
            &error);

  assert(equals(mikrio_v1_get_bucket_cors_output_allowed_origin(reopened, 0, 0),
                "https://app.example"));

  remove.expected_bucket_owner = account;
  remove.struct_size = 0;

  failed(mikrio_v1_store_delete_bucket_cors(store, &remove, &error), &error,
         "InvalidArgument", 1);

  remove.struct_size = sizeof(remove);

  succeeded(mikrio_v1_store_delete_bucket_cors(store, &remove, &error), &error);
  succeeded(mikrio_v1_store_delete_bucket_cors(store, &remove, &error), &error);
  mikrio_v1_get_bucket_cors_output_t *missing = NULL;

  failed(mikrio_v1_store_get_bucket_cors(store, &get, &missing, &error), &error,
         "NoSuchCORSConfiguration", 0);

  assert(missing == NULL);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  assert(equals(first.id, "browser"));

  assert(equals(mikrio_v1_get_bucket_cors_output_allowed_origin(result, 0, 0),
                "https://app.example"));

  mikrio_v1_get_bucket_cors_output_release(result);
  mikrio_v1_get_bucket_cors_output_release(reopened);
  mikrio_v1_get_bucket_cors_output_release(NULL);
  mikrio_v1_owner_info_release(owner);
  puts("PASS cors_configuration_and_detached_rule_ownership");
}

static void object_tagging(mikrio_v1_store_t *store, const char *path,
                           const char *owner) {
  write_object(store, "tagging", "payload");
  mikrio_v1_error_t *error = NULL;

  mikrio_v1_pair_t tags[] = {{text("name"), text("exact")},
                             {text("Name"), text("")},
                             {text("日本語"), text("𐐀")}};

  mikrio_v1_put_object_tagging_input_t put = {0};
  put.struct_size = sizeof(put);
  put.bucket = text("bucket");
  put.key = text("tagging");
  put.tags = tags;
  put.tag_count = 3;
  succeeded(mikrio_v1_store_put_object_tagging(store, &put, &error), &error);
  tags[0].value = text("caller changed");
  mikrio_v1_get_object_tagging_input_t get = {0};

  get.struct_size =
      offsetof(mikrio_v1_get_object_tagging_input_t, expected_bucket_owner);

  get.bucket = put.bucket;
  get.key = put.key;
  mikrio_v1_get_object_tagging_output_t *result = NULL;
  mikrio_v1_store_t *reader = open_store(path);

  succeeded(mikrio_v1_store_get_object_tagging(reader, &get, &result, &error),
            &error);

  succeeded(mikrio_v1_store_close(reader, &error), &error);
  mikrio_v1_store_release(reader);
  assert(mikrio_v1_get_object_tagging_output_view(result).tag_count == 3);

  assert(
      equals(mikrio_v1_get_object_tagging_output_tag(result, 0).key, "Name"));

  mikrio_v1_pair_t empty = mikrio_v1_get_object_tagging_output_tag(result, 0);
  assert(empty.value.data != NULL && empty.value.len == 0);

  assert(equals(mikrio_v1_get_object_tagging_output_tag(result, 1).value,
                "exact"));

  assert(equals(mikrio_v1_get_object_tagging_output_tag(result, 2).value, "𐐀"));
  assert(mikrio_v1_get_object_tagging_output_tag(result, 3).key.data == NULL);
  assert(mikrio_v1_get_object_tagging_output_view(NULL).tag_count == 0);
  assert(mikrio_v1_get_object_tagging_output_tag(NULL, 0).key.data == NULL);

  mikrio_v1_delete_object_tagging_input_t remove = {0};
  remove.struct_size = sizeof(remove);
  remove.bucket = put.bucket;
  remove.key = put.key;

  mikrio_v1_span_t wrong = text(
      strcmp(owner, "000000000000") == 0 ? "000000000001" : "000000000000");

  get.struct_size = sizeof(get);
  put.expected_bucket_owner = wrong;
  get.expected_bucket_owner = wrong;
  remove.expected_bucket_owner = wrong;
  mikrio_v1_get_object_tagging_output_t *denied = result;

  failed(mikrio_v1_store_get_object_tagging(store, &get, &denied, &error),
         &error, "AccessDenied", 0);

  assert(denied == NULL);

  failed(mikrio_v1_store_put_object_tagging(store, &put, &error), &error,
         "AccessDenied", 1);

  failed(mikrio_v1_store_delete_object_tagging(store, &remove, &error), &error,
         "AccessDenied", 1);

  put.expected_bucket_owner = (mikrio_v1_span_t){0};
  get.expected_bucket_owner = (mikrio_v1_span_t){0};
  remove.expected_bucket_owner = (mikrio_v1_span_t){0};

  put.struct_size = 0;

  failed(mikrio_v1_store_put_object_tagging(store, &put, &error), &error,
         "InvalidArgument", 1);

  put.struct_size = sizeof(put);
  get.struct_size = 0;
  mikrio_v1_get_object_tagging_output_t *invalid = result;

  failed(mikrio_v1_store_get_object_tagging(store, &get, &invalid, &error),
         &error, "InvalidArgument", 0);

  assert(invalid == NULL);
  get.struct_size = sizeof(get);
  put.tags = NULL;

  failed(mikrio_v1_store_put_object_tagging(store, &put, &error), &error,
         "InvalidArgument", 1);

  put.tags = tags;
  put.tag_count = 11;

  failed(mikrio_v1_store_put_object_tagging(store, &put, &error), &error,
         "InvalidArgument", 1);

  put.tag_count = 3;
  put.struct_size = offsetof(mikrio_v1_put_object_tagging_input_t, tags);

  failed(mikrio_v1_store_put_object_tagging(store, &put, &error), &error,
         "InvalidArgument", 1);

  put.struct_size = sizeof(put);
  tags[0].key = text("Name");

  failed(mikrio_v1_store_put_object_tagging(store, &put, &error), &error,
         "InvalidArgument", 1);

  put.tags = NULL;
  put.tag_count = 0;
  succeeded(mikrio_v1_store_put_object_tagging(store, &put, &error), &error);
  remove.struct_size = 0;

  failed(mikrio_v1_store_delete_object_tagging(store, &remove, &error), &error,
         "InvalidArgument", 1);

  remove.struct_size =
      offsetof(mikrio_v1_delete_object_tagging_input_t, expected_bucket_owner);

  succeeded(mikrio_v1_store_delete_object_tagging(store, &remove, &error),
            &error);

  succeeded(mikrio_v1_store_delete_object_tagging(store, &remove, &error),
            &error);

  get.key = text("missing-tagging");
  mikrio_v1_get_object_tagging_output_t *missing = result;

  failed(mikrio_v1_store_get_object_tagging(store, &get, &missing, &error),
         &error, "NoSuchKey", 0);

  assert(missing == NULL);
  put.key = get.key;

  failed(mikrio_v1_store_put_object_tagging(store, &put, &error), &error,
         "NoSuchKey", 1);

  remove.key = get.key;

  failed(mikrio_v1_store_delete_object_tagging(store, &remove, &error), &error,
         "NoSuchKey", 1);

  assert(equals(mikrio_v1_get_object_tagging_output_tag(result, 1).value,
                "exact"));

  mikrio_v1_get_object_tagging_output_release(result);
  mikrio_v1_get_object_tagging_output_release(NULL);
  mikrio_v1_delete_input_t cleanup = {0};
  cleanup.struct_size = sizeof(cleanup);
  cleanup.bucket = put.bucket;
  cleanup.key = text("tagging");
  succeeded(mikrio_v1_store_delete_object(store, &cleanup, &error), &error);
  puts("PASS object_tagging_and_detached_tag_ownership");
}

static void lifecycle_configuration(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("retention");
  succeeded(mikrio_v1_store_create_bucket(store, &bucket, &error), &error);

  mikrio_v1_lifecycle_rule_input_t rules[3] = {0};
  rules[0].struct_size = sizeof(rules[0]);
  rules[0].id = text("retain");
  rules[0].status = MIKRIO_V1_LIFECYCLE_ENABLED;
  rules[0].prefix = text("drafts/");
  rules[0].expiration_days = (mikrio_v1_optional_u64_t){1, 0, 7};
  rules[0].abort_days_after_initiation = (mikrio_v1_optional_u64_t){1, 0, 1};
  rules[1] = rules[0];
  rules[1].id = (mikrio_v1_span_t){0};
  rules[1].status = MIKRIO_V1_LIFECYCLE_DISABLED;
  rules[1].prefix = (mikrio_v1_span_t){0};
  rules[2] = rules[1];
  rules[2].prefix = text("");

  mikrio_v1_put_bucket_lifecycle_configuration_input_t put = {0};
  put.struct_size = sizeof(put);
  put.bucket = bucket.bucket;
  put.lifecycle_rules = rules;
  put.lifecycle_rule_count = 3;

  mikrio_v1_get_bucket_lifecycle_configuration_input_t get = {0};
  get.struct_size = sizeof(get);
  get.bucket = bucket.bucket;

  mikrio_v1_delete_bucket_lifecycle_input_t remove = {0};
  remove.struct_size = sizeof(remove);
  remove.bucket = bucket.bucket;
  mikrio_v1_get_bucket_lifecycle_configuration_output_t *result = NULL;

  failed(mikrio_v1_store_get_bucket_lifecycle_configuration(store, &get,
                                                            &result, &error),
         &error, "NoSuchLifecycleConfiguration", 0);

  assert(result == NULL);

  succeeded(
      mikrio_v1_store_put_bucket_lifecycle_configuration(store, &put, &error),
      &error);

  rules[0].prefix = text("changed");

  succeeded(mikrio_v1_store_get_bucket_lifecycle_configuration(store, &get,
                                                               &result, &error),
            &error);

  assert(mikrio_v1_get_bucket_lifecycle_configuration_output_view(result)
             .lifecycle_rule_count == 3);

  mikrio_v1_lifecycle_rule_view_t first =
      mikrio_v1_get_bucket_lifecycle_configuration_output_rule(result, 0);

  assert(equals(first.prefix, "drafts/"));
  assert(first.status == MIKRIO_V1_LIFECYCLE_ENABLED && first.reserved == 0);

  assert(first.expiration_days.present == 1 &&
         first.expiration_days.value == 7);

  assert(first.abort_days_after_initiation.present == 1 &&
         first.abort_days_after_initiation.value == 1);

  assert(mikrio_v1_get_bucket_lifecycle_configuration_output_rule(result, 1)
             .prefix.data == NULL);

  assert(mikrio_v1_get_bucket_lifecycle_configuration_output_rule(result, 2)
             .prefix.data != NULL);

  assert(mikrio_v1_get_bucket_lifecycle_configuration_output_rule(result, 1)
             .id.len != 0);

  assert(mikrio_v1_get_bucket_lifecycle_configuration_output_rule(NULL, 0)
             .status == 0);

  assert(mikrio_v1_get_bucket_lifecycle_configuration_output_rule(result,
                                                                  UINT64_MAX)
             .status == 0);

  rules[2].expiration_days.value = 0;

  failed(
      mikrio_v1_store_put_bucket_lifecycle_configuration(store, &put, &error),
      &error, "InvalidArgument", 1);

  rules[2].expiration_days.value = 7;
  rules[2].reserved = 1;

  failed(
      mikrio_v1_store_put_bucket_lifecycle_configuration(store, &put, &error),
      &error, "InvalidArgument", 1);

  rules[2].reserved = 0;
  rules[2].struct_size--;

  failed(
      mikrio_v1_store_put_bucket_lifecycle_configuration(store, &put, &error),
      &error, "InvalidArgument", 1);

  rules[2].struct_size++;
  put.lifecycle_rule_count = 1001;

  failed(
      mikrio_v1_store_put_bucket_lifecycle_configuration(store, &put, &error),
      &error, "InvalidArgument", 1);

  put.lifecycle_rule_count = 3;
  remove.expected_bucket_owner = text("");

  failed(mikrio_v1_store_delete_bucket_lifecycle(store, &remove, &error),
         &error, "InvalidArgument", 1);

  remove.expected_bucket_owner = (mikrio_v1_span_t){0};
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  store = open_store(path);
  mikrio_v1_get_bucket_lifecycle_configuration_output_t *reopened = NULL;

  succeeded(mikrio_v1_store_get_bucket_lifecycle_configuration(
                store, &get, &reopened, &error),
            &error);

  assert(equals(
      mikrio_v1_get_bucket_lifecycle_configuration_output_rule(reopened, 0)
          .prefix,
      "drafts/"));

  succeeded(mikrio_v1_store_delete_bucket_lifecycle(store, &remove, &error),
            &error);

  succeeded(mikrio_v1_store_delete_bucket_lifecycle(store, &remove, &error),
            &error);

  mikrio_v1_get_bucket_lifecycle_configuration_output_t *missing = NULL;

  failed(mikrio_v1_store_get_bucket_lifecycle_configuration(store, &get,
                                                            &missing, &error),
         &error, "NoSuchLifecycleConfiguration", 0);

  assert(missing == NULL);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  assert(equals(first.prefix, "drafts/"));
  mikrio_v1_get_bucket_lifecycle_configuration_output_release(result);
  mikrio_v1_get_bucket_lifecycle_configuration_output_release(reopened);
  mikrio_v1_get_bucket_lifecycle_configuration_output_release(NULL);
  puts("PASS lifecycle_configuration_and_detached_rule_ownership");
}

int main(int argc, char **argv) {
  assert(argc == 3 && mikrio_v1_abi_version() == 1);

  if (strcmp(argv[1], "--initialize") == 0) {
    mikrio_v1_store_t *store = open_store(argv[2]);
    mikrio_v1_error_t *error = NULL;
    uint32_t status = mikrio_v1_store_close(store, &error);
    succeeded(status, &error);
    mikrio_v1_store_release(store);
    return 0;
  }

  assert(strlen(argv[2]) == 12);
  administration(argv[1]);
  administration_pagination(argv[1]);
  listener_ownership(argv[1]);
  access_configuration(argv[1]);
  cors_configuration(argv[1]);
  lifecycle_configuration(argv[1]);
  mikrio_v1_store_t *store = open_store(argv[1]);
  mikrio_v1_store_t *peer = open_store(argv[1]);
  mikrio_v1_info_t *info = NULL;
  mikrio_v1_error_t *error = NULL;
  uint32_t status = mikrio_v1_store_info(store, &info, &error);
  succeeded(status, &error);
  options_and_bucket(store, argv[1]);
  object_tagging(store, argv[1], argv[2]);
  input_prefixes_and_guarded_boundaries(store, argv[1]);
  bucket_listing_detached_pages(argv[1]);
  listing_detached_pages(argv[1]);
  batch_deletion_detached_results(argv[1]);
  batch_close_retains_error_progress(argv[1]);
  writer_failure_and_release(store);
  checksum_and_representation_fields(store);
  ranges_and_response_overrides(store);
  terminal_integrity_rejects_replacement(store);
  owner_conditions_and_bucket_retirement(store, argv[2]);
  full_admission_keeps_writer_and_body_progress(store);
  selected_body_and_detached_outputs(store, peer);
  concurrent_close(peer);
  mikrio_v1_store_release(store);
  mikrio_v1_store_release(peer);
  assert(equals(mikrio_v1_info_view(info).region, "us-east-1"));
  mikrio_v1_info_release(info);
  diagnostic_context_drain(argv[1]);
  truncated_body_is_terminal(argv[1]);

  store = open_store(argv[1]);
  mikrio_v1_read_input_t read = read_input("survivor");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;
  status = mikrio_v1_store_get_object(store, &read, &facts, &body, &error);
  succeeded(status, &error);
  assert(mikrio_v1_read_output_view(facts).content_length == 19);
  mikrio_v1_store_release(store);
  /* Parent release must leave valid child allocations through fallback close.
   */
  status = mikrio_v1_body_close(body, &error);
  succeeded(status, &error);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);
  puts("PASS reopen_and_parent_release");
  return 0;
}
