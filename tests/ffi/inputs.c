#define _DEFAULT_SOURCE 1
#include "mikrio.h"
#include "platform.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef NDEBUG
#error "FFI fixtures require assertions, including calls with side effects"
#endif

static mikrio_v1_span_t text(const char *value) {
  return (mikrio_v1_span_t){(const uint8_t *)value, strlen(value)};
}

static int equals(mikrio_v1_span_t value, const char *expected) {
  return value.len == strlen(expected) &&
         (value.len == 0 || memcmp(value.data, expected, value.len) == 0);
}

static void succeeded(uint32_t status, mikrio_v1_error_t **error) {
  if (status != 0) {
    mikrio_v1_error_view_t view = mikrio_v1_error_view(*error);

    fprintf(stderr, "%.*s: %.*s\n", (int)view.code.len, view.code.data,
            (int)view.message.len, view.message.data);

    mikrio_v1_error_release(*error);
    abort();
  }

  assert(*error == NULL);
}

static mikrio_v1_store_t *open_store(const char *path) {
  mikrio_v1_open_options_t options = {0};
  options.struct_size = sizeof(options);
  options.directory = text(path);
  options.region = text("eu-west-1");
  mikrio_v1_store_t *store = NULL;
  mikrio_v1_error_t *error = NULL;
  succeeded(mikrio_v1_store_open(&options, &store, &error), &error);
  assert(store != NULL);
  return store;
}

static mikrio_v1_put_input_t put_input(void) {
  mikrio_v1_put_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = text("checked-inputs");
  input.key = text("literal-key");
  return input;
}

static void reject_put(mikrio_v1_store_t *store,
                       const mikrio_v1_put_input_t *input,
                       mikrio_v1_error_t **error) {
  mikrio_v1_writer_t *writer = NULL;
  assert(mikrio_v1_store_begin_put_object(store, input, &writer, error) == 1);
  assert(writer == NULL && *error != NULL);
  mikrio_v1_error_view_t view = mikrio_v1_error_view(*error);
  assert(equals(view.code, "InvalidArgument"));
  assert(view.mutation_outcome == 1 && view.reserved == 0);
  assert(view.message.data != NULL && view.message.len > 0);
  assert(view.message.len <= 4096);
  assert(view.cause.data == NULL && view.cause.len == 0);
}

static void borrowed_pairs_are_copied(mikrio_v1_store_t *store) {
  char key[] = "literal-key-unused-suffix";
  char first_key[] = "MiXeD";
  char first_value[] = "one";
  char second_key[] = "Zed";
  char second_value[] = "two";

  mikrio_v1_pair_t metadata[] = {{text(first_key), text(first_value)},
                                 {text(second_key), text(second_value)}};

  mikrio_v1_pair_t tags[] = {{text("First"), text("")},
                             {text("Second"), text("tag")}};

  mikrio_v1_put_input_t input = put_input();
  input.key = (mikrio_v1_span_t){(const uint8_t *)key, 11};
  input.content_length = (mikrio_v1_optional_u64_t){1, 0, 3};
  input.expires = (mikrio_v1_optional_i64_t){1, 0, -1};
  input.metadata = metadata;
  input.metadata_count = 2;
  input.tags = tags;
  input.tag_count = 2;
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *written = NULL;
  mikrio_v1_error_t *error = NULL;

  succeeded(mikrio_v1_store_begin_put_object(store, &input, &writer, &error),
            &error);

  /* Successful begin owns every input byte, including both fixed-stride pairs.
   * Reusing live caller allocations must not alter the queued worker's values.
   */
  memset(key, 'x', sizeof(key));
  memset(first_key, 'x', sizeof(first_key));
  memset(first_value, 'x', sizeof(first_value));
  memset(second_key, 'x', sizeof(second_key));
  memset(second_value, 'x', sizeof(second_value));
  memset(metadata, 0, sizeof(metadata));
  memset(tags, 0, sizeof(tags));
  memset(&input, 0, sizeof(input));
  succeeded(mikrio_v1_writer_write(writer, text("abc"), &error), &error);
  succeeded(mikrio_v1_writer_finish(writer, &written, &error), &error);

  assert(equals(mikrio_v1_put_output_view(written).etag,
                "\"900150983cd24fb0d6963f7d28e17f72\""));

  mikrio_v1_put_output_release(written);
  mikrio_v1_writer_release(writer);
  puts("PASS borrowed_spans_and_two_element_pairs_are_copied");
}

static void bounded_pairs(mikrio_v1_store_t *store) {
  char keys[64][4];
  char values[64][31];
  mikrio_v1_pair_t metadata[65] = {0};

  for (size_t index = 0; index < 64; ++index) {
    assert(snprintf(keys[index], sizeof(keys[index]), "k%02u",
                    (unsigned)index) == 3);

    memset(values[index], 'a', 30);
    values[index][30] = '\0';

    metadata[index] = (mikrio_v1_pair_t){text(keys[index]),
                                         {(const uint8_t *)values[index], 29}};
  }

  /* Ten distinct 128-unit keys and 256-unit values use three UTF-8 bytes per
   * non-ASCII UTF-16 unit. The allocation ceiling must preserve valid tags. */
  char tag_keys[10][383];
  char tag_value[769];
  mikrio_v1_pair_t tags[10];

  for (size_t unit = 0; unit < 256; ++unit) {
    memcpy(tag_value + unit * 3, "\xe9\x9b\xaa", 3);
  }

  tag_value[768] = '\0';

  for (size_t index = 0; index < 10; ++index) {
    tag_keys[index][0] = (char)('a' + index);

    for (size_t unit = 0; unit < 127; ++unit) {
      memcpy(tag_keys[index] + 1 + unit * 3, "\xe9\x9b\xaa", 3);
    }

    tag_keys[index][382] = '\0';
    tags[index] = (mikrio_v1_pair_t){text(tag_keys[index]), text(tag_value)};
  }

  mikrio_v1_put_input_t input = put_input();
  input.key = text("pair-boundaries");
  input.metadata = metadata;
  input.metadata_count = 64;
  input.tags = tags;
  input.tag_count = 10;
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *written = NULL;
  mikrio_v1_error_t *error = NULL;

  succeeded(mikrio_v1_store_begin_put_object(store, &input, &writer, &error),
            &error);

  succeeded(mikrio_v1_writer_write(writer, text("abc"), &error), &error);
  succeeded(mikrio_v1_writer_finish(writer, &written, &error), &error);
  mikrio_v1_put_output_release(written);
  mikrio_v1_writer_release(writer);

  input.metadata_count = 65;
  reject_put(store, &input, &error);
  mikrio_v1_error_release(error);
  error = NULL;
  input.metadata_count = 64;
  metadata[0].value.len = 30;
  reject_put(store, &input, &error);
  mikrio_v1_error_release(error);
  error = NULL;
  metadata[0].value.len = 29;
  char oversized[16 * 1024];
  memset(oversized, 'x', sizeof(oversized));

  tags[0].value =
      (mikrio_v1_span_t){(const uint8_t *)oversized, sizeof(oversized)};

  reject_put(store, &input, &error);
  mikrio_v1_error_release(error);
  error = NULL;

  mikrio_v1_read_input_t selected = {0};
  selected.struct_size = sizeof(selected);
  selected.bucket = input.bucket;
  selected.key = input.key;
  mikrio_v1_read_output_t *facts = NULL;

  succeeded(mikrio_v1_store_head_object(store, &selected, &facts, &error),
            &error);

  mikrio_v1_read_view_t view = mikrio_v1_read_output_view(facts);
  assert(view.metadata_count == 64 && view.tag_count == 10);
  assert(view.content_length == 3);
  assert(equals(view.etag, "\"900150983cd24fb0d6963f7d28e17f72\""));

  for (size_t index = 0; index < 64; ++index) {
    assert(equals(view.metadata[index].key, keys[index]));
    assert(view.metadata[index].value.len == 29);
    assert(memcmp(view.metadata[index].value.data, values[index], 29) == 0);
  }

  mikrio_v1_read_output_release(facts);
  puts("PASS pair_budgets_preserve_maximum_metadata_and_unicode_tags");
}

static void invalid_inputs(mikrio_v1_store_t *store, mikrio_v1_error_t **errors,
                           size_t *count) {
  const mikrio_v1_optional_u64_t unsigned_values[] = {
      {2, 0, 0}, {1, 1, 3}, {0, 0, 3}};

  const mikrio_v1_optional_i64_t signed_values[] = {
      {2, 0, 0}, {1, 1, -1}, {0, 0, -1}};

  for (size_t index = 0; index < 3; ++index) {
    mikrio_v1_put_input_t input = put_input();
    input.content_length = unsigned_values[index];
    reject_put(store, &input, &errors[(*count)++]);
    input = put_input();
    input.expires = signed_values[index];
    reject_put(store, &input, &errors[(*count)++]);
  }

  const uint8_t invalid_utf8[] = {0xff};

  const mikrio_v1_span_t invalid_spans[] = {
      {NULL, 1}, {invalid_utf8, sizeof(invalid_utf8)}};

  for (size_t index = 0; index < 2; ++index) {
    mikrio_v1_put_input_t input = put_input();
    input.key = invalid_spans[index];
    reject_put(store, &input, &errors[(*count)++]);
    input = put_input();
    input.content_type = invalid_spans[index];
    reject_put(store, &input, &errors[(*count)++]);
  }

  mikrio_v1_pair_t pairs[] = {{text("first"), text("valid")},
                              {text("second"), invalid_spans[1]}};

  mikrio_v1_put_input_t input = put_input();
  input.metadata = pairs;
  input.metadata_count = 2;
  reject_put(store, &input, &errors[(*count)++]);
  pairs[1] = (mikrio_v1_pair_t){invalid_spans[1], text("valid")};
  reject_put(store, &input, &errors[(*count)++]);
  input.metadata = NULL;
  input.metadata_count = 1;
  reject_put(store, &input, &errors[(*count)++]);

  /* Even rejected counts have real backing allocations. No dangling or random
   * pointer is part of this checked-input test. */
  mikrio_v1_pair_t too_many[2049] = {0};
  input.metadata = too_many;
  input.metadata_count = 2049;
  reject_put(store, &input, &errors[(*count)++]);
  input = put_input();
  input.tags = too_many;
  input.tag_count = 11;
  reject_put(store, &input, &errors[(*count)++]);

  mikrio_v1_writer_t *writer = NULL;
  input = put_input();
  input.content_length = unsigned_values[0];
  assert(mikrio_v1_store_begin_put_object(store, &input, &writer, NULL) == 1);
  assert(writer == NULL);
  assert(*count == 15);
}

static void unchanged_readback(mikrio_v1_store_t *store) {
  mikrio_v1_read_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = text("checked-inputs");
  input.key = text("literal-key");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;
  mikrio_v1_error_t *error = NULL;

  succeeded(mikrio_v1_store_get_object(store, &input, &facts, &body, &error),
            &error);

  mikrio_v1_read_view_t view = mikrio_v1_read_output_view(facts);
  assert(view.metadata_count == 2 && view.tag_count == 2);
  assert(equals(view.metadata[0].key, "mixed"));
  assert(equals(view.metadata[0].value, "one"));
  assert(equals(view.metadata[1].key, "zed"));
  assert(equals(view.metadata[1].value, "two"));
  assert(view.expires.present == 1 && view.expires.reserved == 0);
  assert(view.expires.value == -1);
  assert(view.parts_count.present == 0 && view.parts_count.reserved == 0);
  assert(view.parts_count.value == 0 && view.content_length == 3);
  uint8_t buffer[4] = {0};
  uint64_t count = 0;

  succeeded(
      mikrio_v1_body_read_chunk(body, buffer, sizeof(buffer), &count, &error),
      &error);

  assert(count == 3 && memcmp(buffer, "abc", 3) == 0);
  succeeded(mikrio_v1_body_close(body, &error), &error);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);
}

static void errorless_writer_failure(mikrio_v1_store_t *store) {
  mikrio_v1_put_input_t input = put_input();
  input.content_length = (mikrio_v1_optional_u64_t){1, 0, 4};
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_put_output_t *written = NULL;

  assert(mikrio_v1_store_begin_put_object(store, &input, &writer, NULL) == 0);
  assert(writer != NULL);
  assert(mikrio_v1_writer_write(writer, text("x"), NULL) == 0);
  assert(mikrio_v1_writer_finish(writer, &written, NULL) == 1);
  assert(written == NULL);
  assert(mikrio_v1_writer_write(writer, text("abc"), NULL) == 1);
  assert(mikrio_v1_writer_finish(writer, &written, NULL) == 1);
  assert(written == NULL);
  mikrio_v1_writer_release(writer);
  unchanged_readback(store);

  /* A rejected begin never transfers a writer, even without an error slot. */
  writer = NULL;
  input.struct_size = 0;
  assert(mikrio_v1_store_begin_put_object(store, &input, &writer, NULL) == 1);
  assert(writer == NULL);
  puts("PASS errorless_writer_failure_preserves_predecessor");
}

static void read_failures_preserve_cursor(mikrio_v1_store_t *store) {
  mikrio_v1_read_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = text("checked-inputs");
  input.key = text("literal-key");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;
  assert(mikrio_v1_store_get_object(store, &input, &facts, &body, NULL) == 0);
  assert(facts != NULL && body != NULL);
  const uint8_t expected[] = {'a', 'b', 'c'};

  for (size_t index = 0; index < sizeof(expected); ++index) {
    uint8_t buffer[] = {0x41, 0x42, 0x43};
    const uint8_t untouched[] = {0x41, 0x42, 0x43};
    uint64_t count = UINT64_MAX;

    /* Reject before consuming at each position, not only before the first read.
     * The one-byte destination has live, independently checked guard bytes. */
    assert(mikrio_v1_body_read_chunk(body, &buffer[1], 0, &count, NULL) == 1);
    assert(count == 0 && memcmp(buffer, untouched, sizeof(buffer)) == 0);
    count = UINT64_MAX;
    assert(mikrio_v1_body_read_chunk(body, &buffer[1], 1, &count, NULL) == 0);
    assert(count == 1 && buffer[1] == expected[index]);
    assert(buffer[0] == untouched[0] && buffer[2] == untouched[2]);
  }

  uint8_t buffer[] = {0x41, 0x42, 0x43};
  const uint8_t untouched[] = {0x41, 0x42, 0x43};

  for (size_t iteration = 0; iteration < 2; ++iteration) {
    uint64_t count = UINT64_MAX;
    assert(mikrio_v1_body_read_chunk(body, &buffer[1], 1, &count, NULL) == 0);
    assert(count == 0 && memcmp(buffer, untouched, sizeof(buffer)) == 0);
  }

  assert(mikrio_v1_body_close(body, NULL) == 0);
  assert(mikrio_v1_body_close(body, NULL) == 0);
  uint64_t count = UINT64_MAX;
  assert(mikrio_v1_body_read_chunk(body, &buffer[1], 1, &count, NULL) == 1);
  assert(count == 0 && memcmp(buffer, untouched, sizeof(buffer)) == 0);
  mikrio_v1_error_t *error = NULL;
  count = UINT64_MAX;
  assert(mikrio_v1_body_read_chunk(body, &buffer[1], 1, &count, &error) == 1);
  assert(count == 0 && memcmp(buffer, untouched, sizeof(buffer)) == 0);
  mikrio_v1_body_release(body);
  mikrio_v1_read_output_release(facts);
  mikrio_v1_error_view_t view = mikrio_v1_error_view(error);
  assert(equals(view.code, "BodyClosed") && view.mutation_outcome == 0);
  mikrio_v1_error_release(error);
  puts("PASS errorless_read_failures_preserve_cursor_and_buffer");
}

static mikrio_v1_error_t *closed_outputs(mikrio_v1_store_t *store) {
  mikrio_v1_read_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = text("checked-inputs");
  input.key = text("literal-key");
  mikrio_v1_read_output_t *facts = NULL;
  mikrio_v1_body_t *body = NULL;
  mikrio_v1_error_t *error = NULL;
  assert(mikrio_v1_store_get_object(store, &input, &facts, &body, &error) == 1);
  assert(facts == NULL && body == NULL && error != NULL);
  assert(mikrio_v1_store_get_object(store, &input, &facts, &body, NULL) == 1);
  assert(facts == NULL && body == NULL);
  assert(mikrio_v1_store_head_object(store, &input, &facts, NULL) == 1);
  assert(facts == NULL);
  mikrio_v1_put_input_t put = put_input();
  mikrio_v1_writer_t *writer = NULL;
  mikrio_v1_error_t *writer_error = NULL;

  uint32_t status =
      mikrio_v1_store_begin_put_object(store, &put, &writer, &writer_error);

  /* Begin only transfers an unpublished producer. A worker's admission failure
   * may arrive at finish; neither phase can acknowledge a closed-store PUT. */
  if (status == 0) {
    assert(writer != NULL && writer_error == NULL);
    mikrio_v1_put_output_t *written = NULL;
    assert(mikrio_v1_writer_finish(writer, &written, NULL) == 1);
    assert(written == NULL);
    assert(mikrio_v1_writer_finish(writer, &written, &writer_error) == 1);
    assert(written == NULL);
    mikrio_v1_writer_release(writer);
  } else {
    assert(status == 1 && writer == NULL);
  }

  assert(writer_error != NULL);
  mikrio_v1_error_view_t writer_view = mikrio_v1_error_view(writer_error);
  assert(equals(writer_view.code, "StoreClosed"));
  assert(writer_view.mutation_outcome == 1 && writer_view.reserved == 0);
  mikrio_v1_error_release(writer_error);
  return error;
}

int main(int argc, char **argv) {
  assert(argc == 2);
  mikrio_v1_store_t *store = open_store(argv[1]);
  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("checked-inputs");
  mikrio_v1_error_t *error = NULL;
  succeeded(mikrio_v1_store_create_bucket(store, &bucket, &error), &error);
  borrowed_pairs_are_copied(store);
  bounded_pairs(store);
  mikrio_v1_error_t *errors[15] = {0};
  size_t count = 0;
  invalid_inputs(store, errors, &count);
  unchanged_readback(store);
  errorless_writer_failure(store);
  read_failures_preserve_cursor(store);
  mikrio_v1_info_t *info = NULL;
  succeeded(mikrio_v1_store_info(store, &info, &error), &error);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_error_t *closed = closed_outputs(store);
  mikrio_v1_store_release(store);
  mikrio_v1_error_view_t closed_view = mikrio_v1_error_view(closed);
  assert(equals(closed_view.code, "StoreClosed"));
  assert(closed_view.mutation_outcome == 0 && closed_view.reserved == 0);
  mikrio_v1_error_release(closed);
  puts("PASS closed_store_transfers_no_successful_read_or_put_results");

  for (size_t index = 0; index < count; ++index) {
    mikrio_v1_error_view_t view = mikrio_v1_error_view(errors[index]);
    assert(equals(view.code, "InvalidArgument"));
    assert(view.mutation_outcome == 1 && view.reserved == 0);
    assert(view.message.data != NULL && view.message.len > 0);
    assert(view.cause.data == NULL && view.cause.len == 0);
    mikrio_v1_error_release(errors[index]);
  }

  store = open_store(argv[1]);
  unchanged_readback(store);
  mikrio_v1_info_t *reopened = NULL;
  succeeded(mikrio_v1_store_info(store, &reopened, &error), &error);
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  mikrio_v1_info_view_t original = mikrio_v1_info_view(info);
  mikrio_v1_info_view_t current = mikrio_v1_info_view(reopened);

  assert(original.store_id.len > 0 &&
         original.store_id.len == current.store_id.len);

  assert(memcmp(original.store_id.data, current.store_id.data,
                original.store_id.len) == 0);

  assert(original.format_version == current.format_version &&
         original.format_version == 1);

  assert(original.reserved == 0 && current.reserved == 0);

  assert(test_directory_matches(original.directory.data, original.directory.len,
                                argv[1]));

  assert(test_directory_matches(current.directory.data, current.directory.len,
                                argv[1]));

  assert(original.directory.len == current.directory.len);

  assert(memcmp(original.directory.data, current.directory.data,
                original.directory.len) == 0);

  assert(equals(original.region, "eu-west-1") &&
         equals(current.region, "eu-west-1"));

  mikrio_v1_info_release(info);
  mikrio_v1_info_release(reopened);
  mikrio_v1_error_release(NULL);
  mikrio_v1_info_release(NULL);
  puts("PASS checked_inputs_leave_predecessor_and_detached_errors_intact");
  puts("PASS detached_store_info_survives_reopen");
  return 0;
}
