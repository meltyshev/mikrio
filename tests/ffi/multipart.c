#define _DEFAULT_SOURCE 1
#include "mikrio.h"
#include "platform.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

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

static void succeeded_at(uint32_t status, mikrio_v1_error_t **error,
                         unsigned line) {
  if (status != 0) {
    mikrio_v1_error_view_t view = mikrio_v1_error_view(*error);

    fprintf(stderr, "line %u: %.*s: %.*s\n", line, (int)view.code.len,
            view.code.data, (int)view.message.len, view.message.data);

    mikrio_v1_error_release(*error);
    abort();
  }

  assert(*error == NULL);
}

#define ASSERT_SUCCESS(status, error) succeeded_at((status), (error), __LINE__)

static void invalid(uint32_t status, mikrio_v1_error_t **error,
                    uint32_t outcome) {
  assert(status == 1 && *error != NULL);

  assert(equals(mikrio_v1_error_view(*error).code, "InvalidArgument"));
  assert(mikrio_v1_error_view(*error).mutation_outcome == outcome);
  mikrio_v1_error_release(*error);
  *error = NULL;
}

static void failed(uint32_t status, mikrio_v1_error_t **error,
                   const char *code) {
  assert(status == 1 && *error != NULL);
  mikrio_v1_error_view_t view = mikrio_v1_error_view(*error);

  if (!equals(view.code, code)) {
    fprintf(stderr, "expected %s, received %.*s\n", code, (int)view.code.len,
            view.code.data);
  }

  assert(equals(view.code, code));
  assert(view.mutation_outcome == 1);
  mikrio_v1_error_release(*error);
  *error = NULL;
}

static void guarded_attributes(mikrio_v1_store_t *store,
                               mikrio_v1_get_object_attributes_input_t input) {
  size_t page;
  uint8_t *memory = test_guard_pages(&page);
  size_t attributes_page;
  uint8_t *attributes = test_guard_pages(&attributes_page);
  assert(attributes_page == page && page >= sizeof(input));
  assert(input.object_attributes_count <= page / sizeof(mikrio_v1_span_t));

  size_t attribute_bytes =
      input.object_attributes_count * sizeof(mikrio_v1_span_t);

  memcpy(attributes + page - attribute_bytes, input.object_attributes,
         attribute_bytes);

  input.object_attributes =
      (const mikrio_v1_span_t *)(attributes + page - attribute_bytes);

  input.max_parts = (mikrio_v1_optional_u64_t){0};
  const size_t prefixes[] = {72, 88, 104};
  mikrio_v1_error_t *error = NULL;

  for (size_t i = 0; i < sizeof(prefixes) / sizeof(prefixes[0]); ++i) {
    input.struct_size = prefixes[i];
    void *boundary = memory + page - prefixes[i];
    memcpy(boundary, &input, prefixes[i]);
    mikrio_v1_get_object_attributes_output_t *output = NULL;

    ASSERT_SUCCESS(
        mikrio_v1_store_get_object_attributes(store, boundary, &output, &error),
        &error);

    assert(
        mikrio_v1_get_object_attributes_output_view(output).object_size.value ==
        3);

    mikrio_v1_get_object_attributes_output_release(output);
  }

  input.struct_size = sizeof(input);
  input.object_attributes_count = 6;
  input.object_attributes = (const mikrio_v1_span_t *)(memory + page);
  mikrio_v1_get_object_attributes_output_t *output = NULL;

  invalid(mikrio_v1_store_get_object_attributes(store, &input, &output, &error),
          &error, 0);

  assert(output == NULL);

  mikrio_v1_read_input_t read = {0};
  read.struct_size = 248;
  read.bucket = input.bucket;
  read.key = input.key;
  memcpy(memory + page - 248, &read, 248);
  mikrio_v1_read_output_t *head = NULL;

  ASSERT_SUCCESS(mikrio_v1_store_head_object(
                     store, (const void *)(memory + page - 248), &head, &error),
                 &error);

  assert(mikrio_v1_read_output_view(head).parts_count.present == 0);
  mikrio_v1_read_output_release(head);
  test_guard_pages_release(attributes, page);
  test_guard_pages_release(memory, page);
}

static mikrio_v1_get_object_attributes_output_t *
composite_attributes(mikrio_v1_store_t *store) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_create_multipart_upload_input_t create = {0};
  create.struct_size = sizeof(create);
  create.bucket = text("photos");
  create.key = text("composite");
  create.checksum_algorithm = text("SHA256");
  create.checksum_type = text("COMPOSITE");
  mikrio_v1_create_multipart_upload_output_t *upload = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_store_create_multipart_upload(store, &create, &upload, &error),
      &error);

  mikrio_v1_upload_part_input_t part = {0};
  part.struct_size = sizeof(part);
  part.bucket = create.bucket;
  part.key = create.key;

  part.upload_id =
      mikrio_v1_create_multipart_upload_output_view(upload).upload_id;

  part.part_number = 1;
  mikrio_v1_writer_t *writer = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_store_begin_upload_part(store, &part, &writer, &error), &error);

  ASSERT_SUCCESS(mikrio_v1_writer_write(writer, text("abc"), &error), &error);
  mikrio_v1_upload_part_output_t *written = NULL;

  ASSERT_SUCCESS(mikrio_v1_writer_finish_part(writer, &written, &error),
                 &error);

  mikrio_v1_upload_part_output_view_t value =
      mikrio_v1_upload_part_output_view(written);

  mikrio_v1_completed_part_t selection = {0};
  selection.part_number = 1;
  selection.etag = value.etag;
  selection.checksum_sha256 = value.checksum_sha256;
  mikrio_v1_complete_multipart_upload_input_t complete = {0};
  complete.struct_size = sizeof(complete);
  complete.bucket = part.bucket;
  complete.key = part.key;
  complete.upload_id = part.upload_id;
  complete.parts = &selection;
  complete.parts_count = 1;
  mikrio_v1_complete_multipart_upload_output_t *completed = NULL;

  ASSERT_SUCCESS(mikrio_v1_store_complete_multipart_upload(store, &complete,
                                                           &completed, &error),
                 &error);

  mikrio_v1_span_t groups[] = {text("ObjectParts"), text("Checksum")};
  mikrio_v1_get_object_attributes_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = part.bucket;
  input.key = part.key;
  input.object_attributes = groups;
  input.object_attributes_count = 2;
  mikrio_v1_get_object_attributes_output_t *output = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_store_get_object_attributes(store, &input, &output, &error),
      &error);

  mikrio_v1_object_part_view_t selected =
      mikrio_v1_get_object_attributes_output_part(output, 0);

  assert(selected.present == 1 && selected.part_number == 1 &&
         selected.size == 3);

  assert(equals(selected.checksum_sha256,
                "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="));

  assert(mikrio_v1_get_object_attributes_output_part(output, 1).present == 0);

  assert(mikrio_v1_get_object_attributes_output_view(output)
             .object_parts.next_part_number_marker.present == 0);

  mikrio_v1_writer_release(writer);
  mikrio_v1_upload_part_output_release(written);
  mikrio_v1_complete_multipart_upload_output_release(completed);
  mikrio_v1_create_multipart_upload_output_release(upload);
  return output;
}

/* Independent abc vectors from tests/contract/multipart-completion.json.
 * Distinct selected digests expose crossed or dropped fields in C projection.
 */
#define DIGESTS(value)                                                         \
  (value).checksum_crc32, (value).checksum_crc32c, (value).checksum_sha1,      \
      (value).checksum_sha256, (value).checksum_crc64_nvme
#define SET_DIGEST(value, index, digest)                                       \
  do {                                                                         \
    switch (index) {                                                           \
    case 0:                                                                    \
      (value).checksum_crc32 = text(digest);                                   \
      break;                                                                   \
    case 1:                                                                    \
      (value).checksum_crc32c = text(digest);                                  \
      break;                                                                   \
    case 2:                                                                    \
      (value).checksum_sha1 = text(digest);                                    \
      break;                                                                   \
    case 3:                                                                    \
      (value).checksum_sha256 = text(digest);                                  \
      break;                                                                   \
    case 4:                                                                    \
      (value).checksum_crc64_nvme = text(digest);                              \
      break;                                                                   \
    default:                                                                   \
      abort();                                                                 \
    }                                                                          \
  } while (0)

static void check_digest_fields(unsigned selected, const char *expected,
                                mikrio_v1_span_t crc32, mikrio_v1_span_t crc32c,
                                mikrio_v1_span_t sha1, mikrio_v1_span_t sha256,
                                mikrio_v1_span_t crc64) {
  const mikrio_v1_span_t values[] = {crc32, crc32c, sha1, sha256, crc64};

  for (unsigned i = 0; i < 5; ++i) {
    if (i == selected) {
      assert(equals(values[i], expected));
    } else {
      assert(values[i].data == NULL && values[i].len == 0);
    }
  }
}

static void check_representation(mikrio_v1_store_t *store, const char *key,
                                 const char *content_type) {
  mikrio_v1_error_t *error = NULL;

  mikrio_v1_read_input_t read = {0};
  read.struct_size = sizeof(read);
  read.bucket = text("photos");
  read.key = text(key);
  mikrio_v1_read_output_t *head = NULL;

  ASSERT_SUCCESS(mikrio_v1_store_head_object(store, &read, &head, &error),
                 &error);

  mikrio_v1_read_view_t view = mikrio_v1_read_output_view(head);
  assert(view.content_length == 3);
  assert(equals(view.content_type, content_type));
  assert(equals(view.content_encoding, "identity"));
  assert(equals(view.content_disposition, "inline"));
  assert(view.content_language.data != NULL && view.content_language.len == 0);
  assert(equals(view.cache_control, "no-store"));
  assert(view.expires.present == 1 && view.expires.value == -1);
  assert(view.metadata_count == 1 && view.tag_count == 1);
  mikrio_v1_pair_t pair = view.metadata[0];
  assert(equals(pair.key, "mixed") && equals(pair.value, "original"));
  mikrio_v1_read_output_release(head);
  mikrio_v1_get_object_tagging_input_t tags = {0};
  tags.struct_size = sizeof(tags);
  tags.bucket = read.bucket;
  tags.key = read.key;
  mikrio_v1_get_object_tagging_output_t *tagged = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_store_get_object_tagging(store, &tags, &tagged, &error),
      &error);

  assert(mikrio_v1_get_object_tagging_output_view(tagged).tag_count == 1);
  pair = mikrio_v1_get_object_tagging_output_tag(tagged, 0);
  assert(equals(pair.key, "state") && equals(pair.value, "private"));
  mikrio_v1_get_object_tagging_output_release(tagged);
}

static void checksum_fields(mikrio_v1_store_t *store) {
  /* The accepted ranged-copy source is strictly larger than 5 MiB. Only
   * its first three bytes are selected; independent digests below cover abc. */
  mikrio_v1_error_t *source_error = NULL;

  mikrio_v1_put_input_t source = {0};
  source.struct_size = sizeof(source);
  source.bucket = text("photos");
  source.key = text("range-source");
  source.content_length = (mikrio_v1_optional_u64_t){1, 0, 5242881};
  mikrio_v1_writer_t *source_writer = NULL;

  ASSERT_SUCCESS(mikrio_v1_store_begin_put_object(
                     store, &source, &source_writer, &source_error),
                 &source_error);

  ASSERT_SUCCESS(
      mikrio_v1_writer_write(source_writer, text("abc"), &source_error),
      &source_error);

  uint8_t zeros[16384] = {0};

  for (uint64_t remaining = 5242878; remaining > 0;) {
    uint64_t count = remaining < sizeof(zeros) ? remaining : sizeof(zeros);

    ASSERT_SUCCESS(mikrio_v1_writer_write(source_writer,
                                          (mikrio_v1_span_t){zeros, count},
                                          &source_error),
                   &source_error);

    remaining -= count;
  }

  mikrio_v1_put_output_t *source_result = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_writer_finish(source_writer, &source_result, &source_error),
      &source_error);

  mikrio_v1_writer_release(source_writer);

  mikrio_v1_span_t source_etag = mikrio_v1_put_output_view(source_result).etag;

  const struct {
    const char *algorithm;
    const char *checksum_type;
    const char *part_digest;
    const char *completed_digest;
    const char *attribute_digest;
    unsigned checksum_index;
  } vectors[] = {
      {"CRC32", "FULL_OBJECT", "NSRBwg==", "NSRBwg==", "NSRBwg==", 0},
      {"CRC32", "COMPOSITE", "NSRBwg==", "Rf3kaw==-1", "Rf3kaw==", 0},
      {"CRC32C", "FULL_OBJECT", "Nks/tw==", "Nks/tw==", "Nks/tw==", 1},
      {"CRC32C", "COMPOSITE", "Nks/tw==", "ZpGMMA==-1", "ZpGMMA==", 1},
      {"SHA1", "COMPOSITE", "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=",
       "DTztm+wQp3euwjzMNTqMCKYzBF4=-1", "DTztm+wQp3euwjzMNTqMCKYzBF4=", 2},
      {"SHA256", "COMPOSITE", "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=",
       "T4tCwi3TcptRm6b2jS2nzFstYG0F2u1a1RKMwD5sY1g=-1",
       "T4tCwi3TcptRm6b2jS2nzFstYG0F2u1a1RKMwD5sY1g=", 3},
      {"CRC64NVME", "FULL_OBJECT",
       "BeXKuz/B+us=", "BeXKuz/B+us=", "BeXKuz/B+us=", 4},
  };

  for (unsigned i = 0; i < sizeof(vectors) / sizeof(vectors[0]); ++i) {
    char key[32], copied_key[32];
    assert(snprintf(key, sizeof(key), "fields-%u", i) > 0);
    assert(snprintf(copied_key, sizeof(copied_key), "fields-copy-%u", i) > 0);
    mikrio_v1_error_t *error = NULL;
    char metadata_value[] = "original";
    char tag_value[] = "private";
    mikrio_v1_pair_t metadata = {text("MiXeD"), text(metadata_value)};
    mikrio_v1_pair_t tag = {text("state"), text(tag_value)};
    mikrio_v1_create_multipart_upload_input_t create = {0};
    create.struct_size = sizeof(create);
    create.bucket = text("photos");
    create.key = text(key);
    create.content_type = text("text/plain");
    create.content_encoding = text("identity");
    create.content_disposition = text("inline");
    create.content_language = text("");
    create.cache_control = text("no-store");
    create.expires = (mikrio_v1_optional_i64_t){1, 0, -1};
    create.metadata = &metadata;
    create.metadata_count = 1;
    create.tags = &tag;
    create.tags_count = 1;
    create.checksum_algorithm = text(vectors[i].algorithm);
    create.checksum_type = text(vectors[i].checksum_type);
    mikrio_v1_create_multipart_upload_output_t *created = NULL;

    ASSERT_SUCCESS(mikrio_v1_store_create_multipart_upload(store, &create,
                                                           &created, &error),
                   &error);

    memset(metadata_value, 'x', sizeof(metadata_value) - 1);
    memset(tag_value, 'x', sizeof(tag_value) - 1);

    mikrio_v1_create_multipart_upload_output_view_t identity =
        mikrio_v1_create_multipart_upload_output_view(created);

    assert(identity.upload_id.len > 0);
    assert(equals(identity.checksum_algorithm, vectors[i].algorithm));
    assert(equals(identity.checksum_type, vectors[i].checksum_type));
    mikrio_v1_upload_part_input_t part = {0};
    part.struct_size = sizeof(part);
    part.bucket = create.bucket;
    part.key = create.key;
    part.upload_id = identity.upload_id;
    part.part_number = 1;
    part.content_length = (mikrio_v1_optional_u64_t){1, 0, 3};
    part.checksum_algorithm = create.checksum_algorithm;
    part.content_md5 = text("kAFQmDzST7DWlj99KOF/cg==");
    SET_DIGEST(part, vectors[i].checksum_index, vectors[i].part_digest);
    mikrio_v1_writer_t *writer = NULL;
    part.part_number_reserved = 1;

    invalid(mikrio_v1_store_begin_upload_part(store, &part, &writer, &error),
            &error, 1);

    assert(writer == NULL);

    part.part_number_reserved = 0;

    ASSERT_SUCCESS(
        mikrio_v1_store_begin_upload_part(store, &part, &writer, &error),
        &error);

    ASSERT_SUCCESS(mikrio_v1_writer_write(writer, text("abc"), &error), &error);

    mikrio_v1_upload_part_output_t *written = NULL;

    ASSERT_SUCCESS(mikrio_v1_writer_finish_part(writer, &written, &error),
                   &error);

    mikrio_v1_upload_part_output_view_t value =
        mikrio_v1_upload_part_output_view(written);

    assert(equals(value.etag, "\"900150983cd24fb0d6963f7d28e17f72\""));

    check_digest_fields(vectors[i].checksum_index, vectors[i].part_digest,
                        DIGESTS(value));

    mikrio_v1_writer_release(writer);
    mikrio_v1_list_parts_input_t list = {0};
    list.struct_size = sizeof(list);
    list.bucket = create.bucket;
    list.key = create.key;
    list.upload_id = identity.upload_id;
    list.part_number_marker = (mikrio_v1_optional_u64_t){1, 0, 0};
    list.max_parts = (mikrio_v1_optional_u64_t){1, 0, 1};
    mikrio_v1_list_parts_output_t *listed = NULL;

    ASSERT_SUCCESS(mikrio_v1_store_list_parts(store, &list, &listed, &error),
                   &error);

    mikrio_v1_list_parts_output_view_t page =
        mikrio_v1_list_parts_output_view(listed);

    assert(page.parts_count == 1 && page.is_truncated == 0 &&
           page.is_truncated_reserved == 0);

    assert(page.next_part_number_marker.present == 0);
    assert(equals(page.checksum_algorithm, vectors[i].algorithm));
    assert(equals(page.checksum_type, vectors[i].checksum_type));

    mikrio_v1_part_summary_view_t summary =
        mikrio_v1_list_parts_output_item(listed, 0);

    assert(summary.part_number == 1 && summary.part_number_reserved == 0 &&
           summary.size == 3 && summary.last_modified_ms > 0);

    check_digest_fields(vectors[i].checksum_index, vectors[i].part_digest,
                        DIGESTS(summary));

    mikrio_v1_completed_part_t selected = {0};
    selected.part_number = 1;
    selected.etag = value.etag;
    SET_DIGEST(selected, vectors[i].checksum_index, vectors[i].part_digest);
    mikrio_v1_complete_multipart_upload_input_t complete = {0};
    complete.struct_size = sizeof(complete);
    complete.bucket = create.bucket;
    complete.key = create.key;
    complete.upload_id = identity.upload_id;
    complete.parts = &selected;
    complete.parts_count = 1;
    complete.if_none_match = text("*");
    complete.content_length = (mikrio_v1_optional_u64_t){1, 0, 3};
    complete.checksum_type = create.checksum_type;

    SET_DIGEST(complete, vectors[i].checksum_index,
               vectors[i].completed_digest);

    mikrio_v1_complete_multipart_upload_output_t *completed = NULL;
    complete.if_none_match = (mikrio_v1_span_t){0};
    complete.if_match = text("\"missing\"");

    failed(mikrio_v1_store_complete_multipart_upload(store, &complete,
                                                     &completed, &error),
           &error, "NoSuchKey");

    assert(completed == NULL);

    complete.if_match = (mikrio_v1_span_t){0};
    complete.if_none_match = text("*");

    /* A rejected publication leaves a claim for ordinary guarded recovery.
     * Retry only its definite uncommitted conflict, never an unknown outcome.
     */
    for (unsigned attempt = 0;; ++attempt) {
      uint32_t status = mikrio_v1_store_complete_multipart_upload(
          store, &complete, &completed, &error);

      if (status == 0) {
        assert(error == NULL);
        break;
      }

      failed(status, &error, "OperationAborted");
      assert(completed == NULL && attempt < 500);
      test_sleep_ms(20);
    }

    mikrio_v1_complete_multipart_upload_output_view_t committed =
        mikrio_v1_complete_multipart_upload_output_view(completed);

    assert(equals(committed.etag, "\"af5da9f45af7a300e3aded972f8ff687-1\""));

    assert(equals(committed.checksum_type, vectors[i].checksum_type));

    check_digest_fields(vectors[i].checksum_index, vectors[i].completed_digest,
                        DIGESTS(committed));

    check_representation(store, key, "text/plain");

    mikrio_v1_span_t selectors[] = {text("ETag"), text("Checksum"),
                                    text("ObjectSize"), text("ObjectParts"),
                                    text("StorageClass")};

    mikrio_v1_get_object_attributes_input_t facts = {0};
    facts.struct_size = sizeof(facts);
    facts.bucket = create.bucket;
    facts.key = create.key;
    facts.object_attributes = selectors;
    facts.object_attributes_count = 5;
    facts.max_parts = (mikrio_v1_optional_u64_t){1, 0, 1};
    facts.part_number_marker = (mikrio_v1_optional_u64_t){1, 0, 0};
    mikrio_v1_get_object_attributes_output_t *attributes = NULL;

    ASSERT_SUCCESS(mikrio_v1_store_get_object_attributes(store, &facts,
                                                         &attributes, &error),
                   &error);

    mikrio_v1_get_object_attributes_output_view_t view =
        mikrio_v1_get_object_attributes_output_view(attributes);

    assert(equals(view.etag, "\"af5da9f45af7a300e3aded972f8ff687-1\""));

    assert(view.object_size.present == 1 && view.object_size.value == 3);

    assert(view.reserved == 0 && view.reserved2 == 0 &&
           view.last_modified_unix_millis > 0);

    assert(equals(view.storage_class, "STANDARD"));

    assert(view.checksum_present == 1 &&
           equals(view.checksum.checksum_type, vectors[i].checksum_type));

    check_digest_fields(vectors[i].checksum_index, vectors[i].attribute_digest,
                        DIGESTS(view.checksum));

    assert(view.object_parts_present == 1 &&
           view.object_parts.total_parts_count == 1);

    assert(view.object_parts.max_parts == 1 &&
           view.object_parts.part_number_marker == 0);

    assert(view.object_parts.is_truncated == 0 &&
           view.object_parts.reserved == 0 &&
           view.object_parts.next_part_number_marker.present == 0);

    int composite = strcmp(vectors[i].checksum_type, "COMPOSITE") == 0;

    assert(view.object_parts.parts_count == (uint64_t)composite);

    mikrio_v1_object_part_view_t returned =
        mikrio_v1_get_object_attributes_output_part(attributes, 0);

    assert(returned.present == (uint32_t)composite && returned.reserved == 0);

    if (composite) {
      assert(returned.part_number == 1 && returned.size == 3);

      check_digest_fields(vectors[i].checksum_index, vectors[i].part_digest,
                          DIGESTS(returned));
    }

    mikrio_v1_copy_object_input_t copy = {0};
    copy.struct_size = sizeof(copy);
    copy.bucket = create.bucket;
    copy.key = text(copied_key);
    copy.copy_source = (mikrio_v1_object_ref_t){create.bucket, create.key};
    copy.copy_source_if_match = committed.etag;
    copy.copy_source_if_none_match = text("\"different\"");
    copy.copy_source_if_modified_since = (mikrio_v1_optional_i64_t){1, 0, 0};

    copy.copy_source_if_unmodified_since =
        (mikrio_v1_optional_i64_t){1, 0, 4102444800000LL};

    copy.if_none_match = text("*");

    copy.metadata_directive = text("REPLACE");
    copy.tagging_directive = text("REPLACE");
    copy.content_type = text("application/octet-stream");
    copy.content_encoding = create.content_encoding;
    copy.content_disposition = create.content_disposition;
    copy.content_language = create.content_language;
    copy.cache_control = create.cache_control;
    copy.expires = create.expires;
    metadata.value = text("original");
    tag.value = text("private");
    copy.metadata = &metadata;
    copy.metadata_count = 1;
    copy.tags = &tag;
    copy.tags_count = 1;
    copy.checksum_algorithm = create.checksum_algorithm;
    mikrio_v1_copy_object_output_t *copied = NULL;

    ASSERT_SUCCESS(mikrio_v1_store_copy_object(store, &copy, &copied, &error),
                   &error);

    mikrio_v1_copy_object_output_view_t copy_view =
        mikrio_v1_copy_object_output_view(copied);

    assert(equals(copy_view.etag, "\"900150983cd24fb0d6963f7d28e17f72\""));

    assert(copy_view.last_modified_ms > 0);

    check_digest_fields(vectors[i].checksum_index, vectors[i].part_digest,
                        DIGESTS(copy_view));

    check_representation(store, copied_key, "application/octet-stream");
    copy.if_none_match = (mikrio_v1_span_t){0};
    copy.if_match = text("\"different\"");
    mikrio_v1_copy_object_output_t *rejected = (void *)1;

    failed(mikrio_v1_store_copy_object(store, &copy, &rejected, &error), &error,
           "PreconditionFailed");

    assert(rejected == NULL);

    copy.if_match = copy_view.etag;

    ASSERT_SUCCESS(mikrio_v1_store_copy_object(store, &copy, &rejected, &error),
                   &error);

    mikrio_v1_copy_object_output_release(rejected);

    create.key = text("copy-parts");
    mikrio_v1_create_multipart_upload_output_t *copy_upload = NULL;

    ASSERT_SUCCESS(mikrio_v1_store_create_multipart_upload(
                       store, &create, &copy_upload, &error),
                   &error);

    mikrio_v1_upload_part_copy_input_t part_copy = {0};
    part_copy.struct_size = sizeof(part_copy);
    part_copy.bucket = create.bucket;
    part_copy.key = create.key;

    part_copy.upload_id =
        mikrio_v1_create_multipart_upload_output_view(copy_upload).upload_id;

    part_copy.part_number = 1;

    part_copy.copy_source = (mikrio_v1_object_ref_t){source.bucket, source.key};
    part_copy.copy_source_range = text("bytes=0-2");
    mikrio_v1_upload_part_copy_output_t *copied_part = NULL;
    part_copy.part_number_reserved = 1;

    invalid(mikrio_v1_store_upload_part_copy(store, &part_copy, &copied_part,
                                             &error),
            &error, 1);

    assert(copied_part == NULL);

    part_copy.part_number_reserved = 0;
    part_copy.copy_source_if_match = text("\"different\"");

    failed(mikrio_v1_store_upload_part_copy(store, &part_copy, &copied_part,
                                            &error),
           &error, "PreconditionFailed");

    assert(copied_part == NULL);

    part_copy.copy_source_if_match = (mikrio_v1_span_t){0};
    part_copy.copy_source_if_none_match = source_etag;

    failed(mikrio_v1_store_upload_part_copy(store, &part_copy, &copied_part,
                                            &error),
           &error, "PreconditionFailed");

    assert(copied_part == NULL);

    part_copy.copy_source_if_none_match = (mikrio_v1_span_t){0};

    part_copy.copy_source_if_modified_since =
        (mikrio_v1_optional_i64_t){1, 0, 4102444800000LL};

    failed(mikrio_v1_store_upload_part_copy(store, &part_copy, &copied_part,
                                            &error),
           &error, "PreconditionFailed");

    assert(copied_part == NULL);

    part_copy.copy_source_if_modified_since = (mikrio_v1_optional_i64_t){0};

    part_copy.copy_source_if_unmodified_since =
        (mikrio_v1_optional_i64_t){1, 0, 0};

    failed(mikrio_v1_store_upload_part_copy(store, &part_copy, &copied_part,
                                            &error),
           &error, "PreconditionFailed");

    assert(copied_part == NULL);

    part_copy.copy_source_if_match = source_etag;
    part_copy.copy_source_if_none_match = text("\"different\"");

    part_copy.copy_source_if_modified_since =
        (mikrio_v1_optional_i64_t){1, 0, 0};

    part_copy.copy_source_if_unmodified_since =
        (mikrio_v1_optional_i64_t){1, 0, 4102444800000LL};

    ASSERT_SUCCESS(mikrio_v1_store_upload_part_copy(store, &part_copy,
                                                    &copied_part, &error),
                   &error);

    mikrio_v1_upload_part_copy_output_view_t part_view =
        mikrio_v1_upload_part_copy_output_view(copied_part);

    assert(equals(part_view.etag, "\"900150983cd24fb0d6963f7d28e17f72\""));

    assert(part_view.last_modified_ms > 0);

    check_digest_fields(vectors[i].checksum_index, vectors[i].part_digest,
                        DIGESTS(part_view));

    mikrio_v1_abort_multipart_upload_input_t abort_input = {0};
    abort_input.struct_size = sizeof(abort_input);
    abort_input.bucket = part_copy.bucket;
    abort_input.key = part_copy.key;
    abort_input.upload_id = part_copy.upload_id;

    ASSERT_SUCCESS(
        mikrio_v1_store_abort_multipart_upload(store, &abort_input, &error),
        &error);

    mikrio_v1_upload_part_copy_output_release(copied_part);
    mikrio_v1_create_multipart_upload_output_release(copy_upload);
    mikrio_v1_copy_object_output_release(copied);
    mikrio_v1_get_object_attributes_output_release(attributes);
    mikrio_v1_complete_multipart_upload_output_release(completed);
    mikrio_v1_list_parts_output_release(listed);
    mikrio_v1_upload_part_output_release(written);
    mikrio_v1_create_multipart_upload_output_release(created);
  }

  mikrio_v1_put_output_release(source_result);
}

static void upload_pages(mikrio_v1_store_t *store) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_create_multipart_upload_output_t *uploads[3] = {NULL};
  const char *keys[] = {"pages/a", "pages/a", "pages/group/b"};
  mikrio_v1_create_multipart_upload_input_t create = {0};
  create.struct_size = sizeof(create);
  create.bucket = text("photos");

  for (unsigned i = 0; i < 3; ++i) {
    create.key = text(keys[i]);

    ASSERT_SUCCESS(mikrio_v1_store_create_multipart_upload(store, &create,
                                                           &uploads[i], &error),
                   &error);
  }

  mikrio_v1_list_multipart_uploads_input_t input = {0};
  input.struct_size = sizeof(input);
  input.bucket = create.bucket;
  input.prefix = text("pages/");
  input.delimiter = text("/");
  input.max_uploads = (mikrio_v1_optional_u64_t){1, 0, 1};
  mikrio_v1_list_multipart_uploads_output_t *pages[3] = {NULL};

  for (unsigned i = 0; i < 3; ++i) {
    ASSERT_SUCCESS(mikrio_v1_store_list_multipart_uploads(store, &input,
                                                          &pages[i], &error),
                   &error);

    mikrio_v1_list_multipart_uploads_output_view_t page =
        mikrio_v1_list_multipart_uploads_output_view(pages[i]);

    assert(page.is_truncated_reserved == 0);
    assert(page.uploads_count == (i == 2 ? 0u : 1u));
    assert(page.common_prefixes_count == (i == 2 ? 1u : 0u));
    assert(page.is_truncated == (i == 2 ? 0u : 1u));

    if (i == 2) {
      assert(equals(mikrio_v1_list_multipart_uploads_output_prefix(pages[i], 0),
                    "pages/group/"));

      assert(page.next_key_marker.data == NULL &&
             page.next_upload_id_marker.data == NULL);

      assert(mikrio_v1_list_multipart_uploads_output_prefix(pages[i], 1).data ==
             NULL);

      continue;
    }

    mikrio_v1_multipart_upload_summary_view_t value =
        mikrio_v1_list_multipart_uploads_output_item(pages[i], 0);

    assert(equals(value.key, "pages/a"));
    assert(value.upload_id.len > 0 && value.initiated_ms > 0);
    assert(equals(value.checksum_algorithm, "CRC64NVME"));
    assert(equals(value.checksum_type, "FULL_OBJECT"));
    assert(equals(page.next_key_marker, "pages/a"));

    assert(page.next_upload_id_marker.len == value.upload_id.len &&
           memcmp(page.next_upload_id_marker.data, value.upload_id.data,
                  value.upload_id.len) == 0);

    input.key_marker = page.next_key_marker;
    input.upload_id_marker = page.next_upload_id_marker;
  }

  mikrio_v1_span_t first_upload_id =
      mikrio_v1_list_multipart_uploads_output_item(pages[0], 0).upload_id;

  mikrio_v1_span_t second_upload_id =
      mikrio_v1_list_multipart_uploads_output_item(pages[1], 0).upload_id;

  assert(first_upload_id.len == second_upload_id.len &&
         memcmp(first_upload_id.data, second_upload_id.data,
                first_upload_id.len) < 0);

  for (unsigned i = 0; i < 2; ++i) {
    mikrio_v1_span_t upload_id =
        mikrio_v1_create_multipart_upload_output_view(uploads[i]).upload_id;

    assert(upload_id.len == first_upload_id.len &&
           (memcmp(upload_id.data, first_upload_id.data, upload_id.len) == 0 ||
            memcmp(upload_id.data, second_upload_id.data, upload_id.len) == 0));
  }

  for (unsigned i = 0; i < 3; ++i) {
    mikrio_v1_abort_multipart_upload_input_t remove = {0};
    remove.struct_size = sizeof(remove);
    remove.bucket = create.bucket;
    remove.key = text(keys[i]);

    remove.upload_id =
        mikrio_v1_create_multipart_upload_output_view(uploads[i]).upload_id;

    ASSERT_SUCCESS(
        mikrio_v1_store_abort_multipart_upload(store, &remove, &error), &error);

    mikrio_v1_create_multipart_upload_output_release(uploads[i]);
  }

  /* Both marker strings still belong to the pages after upload retirement. */
  assert(equals(
      mikrio_v1_list_multipart_uploads_output_view(pages[0]).next_key_marker,
      "pages/a"));

  for (unsigned i = 0; i < 3; ++i) {
    mikrio_v1_list_multipart_uploads_output_release(pages[i]);
  }
}

int main(int argc, char **argv) {
  assert(argc == 2);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_store_t *store = NULL;
  mikrio_v1_open_options_t options = {0};
  options.struct_size = sizeof(options);
  options.directory = text(argv[1]);
  ASSERT_SUCCESS(mikrio_v1_store_open(&options, &store, &error), &error);
  mikrio_v1_bucket_input_t bucket = {0};
  bucket.struct_size = sizeof(bucket);
  bucket.bucket = text("photos");
  ASSERT_SUCCESS(mikrio_v1_store_create_bucket(store, &bucket, &error), &error);
  checksum_fields(store);
  upload_pages(store);
  mikrio_v1_create_multipart_upload_input_t create = {0};
  create.struct_size = sizeof(create);
  create.bucket = bucket.bucket;
  create.key = text("object");
  mikrio_v1_create_multipart_upload_output_t *created = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_store_create_multipart_upload(store, &create, &created, &error),
      &error);

  mikrio_v1_create_multipart_upload_output_view_t identity =
      mikrio_v1_create_multipart_upload_output_view(created);

  assert(equals(identity.checksum_algorithm, "CRC64NVME"));

  ASSERT_SUCCESS(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
  store = NULL;
  ASSERT_SUCCESS(mikrio_v1_store_open(&options, &store, &error), &error);
  mikrio_v1_upload_part_input_t part = {0};
  part.struct_size = sizeof(part);
  part.bucket = create.bucket;
  part.key = create.key;
  part.upload_id = identity.upload_id;
  part.part_number = 7;
  part.checksum_crc32 = text("NSRBwg==");
  mikrio_v1_writer_t *writer = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_store_begin_upload_part(store, &part, &writer, &error), &error);

  mikrio_v1_put_output_t *wrong = NULL;
  invalid(mikrio_v1_writer_finish(writer, &wrong, &error), &error, 1);
  assert(wrong == NULL);
  ASSERT_SUCCESS(mikrio_v1_writer_write(writer, text("abc"), &error), &error);
  mikrio_v1_upload_part_output_t *written = NULL;

  ASSERT_SUCCESS(mikrio_v1_writer_finish_part(writer, &written, &error),
                 &error);

  invalid(mikrio_v1_writer_finish(writer, &wrong, &error), &error, 2);
  assert(wrong == NULL);
  mikrio_v1_writer_release(writer);
  writer = NULL;
  mikrio_v1_put_input_t put = {0};
  put.struct_size = sizeof(put);
  put.bucket = create.bucket;
  put.key = text("writer-kind");

  ASSERT_SUCCESS(mikrio_v1_store_begin_put_object(store, &put, &writer, &error),
                 &error);

  mikrio_v1_upload_part_output_t *wrong_part = NULL;
  invalid(mikrio_v1_writer_finish_part(writer, &wrong_part, &error), &error, 1);

  assert(wrong_part == NULL);

  ASSERT_SUCCESS(mikrio_v1_writer_write(writer, text("abc"), &error), &error);
  mikrio_v1_put_output_t *put_output = NULL;
  ASSERT_SUCCESS(mikrio_v1_writer_finish(writer, &put_output, &error), &error);
  invalid(mikrio_v1_writer_finish_part(writer, &wrong_part, &error), &error, 2);

  assert(wrong_part == NULL);

  mikrio_v1_writer_release(writer);
  mikrio_v1_put_output_release(put_output);

  mikrio_v1_list_parts_input_t list = {0};
  list.struct_size = sizeof(list);
  list.bucket = create.bucket;
  list.key = create.key;
  list.upload_id = identity.upload_id;
  mikrio_v1_list_parts_output_t *page = NULL;

  ASSERT_SUCCESS(mikrio_v1_store_list_parts(store, &list, &page, &error),
                 &error);

  assert(mikrio_v1_list_parts_output_view(page).parts_count == 1);

  mikrio_v1_part_summary_view_t selected =
      mikrio_v1_list_parts_output_item(page, 0);

  assert(selected.part_number == 7 && selected.size == 3);

  assert(mikrio_v1_list_parts_output_item(page, 1).etag.data == NULL);
  mikrio_v1_completed_part_t selection = {0};
  selection.part_number = 7;
  selection.etag = selected.etag;
  mikrio_v1_complete_multipart_upload_input_t complete = {0};
  complete.struct_size = sizeof(complete);
  complete.bucket = create.bucket;
  complete.key = create.key;
  complete.upload_id = identity.upload_id;
  complete.parts = &selection;
  complete.parts_count = 1;
  mikrio_v1_complete_multipart_upload_output_t *completed = NULL;
  selection.reserved = 1;

  invalid(mikrio_v1_store_complete_multipart_upload(store, &complete,
                                                    &completed, &error),
          &error, 1);

  assert(completed == NULL);

  selection.reserved = 0;

  ASSERT_SUCCESS(mikrio_v1_store_complete_multipart_upload(store, &complete,
                                                           &completed, &error),
                 &error);

  assert(equals(mikrio_v1_complete_multipart_upload_output_view(completed)
                    .checksum_crc64_nvme,
                "BeXKuz/B+us="));

  mikrio_v1_span_t groups[] = {text("ETag"), text("ObjectParts"),
                               text("ObjectSize"), text("Checksum")};

  mikrio_v1_get_object_attributes_input_t attributes = {0};
  attributes.struct_size = sizeof(attributes);
  attributes.bucket = complete.bucket;
  attributes.key = complete.key;
  attributes.object_attributes = groups;
  attributes.object_attributes_count = 4;
  mikrio_v1_get_object_attributes_output_t *facts = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_store_get_object_attributes(store, &attributes, &facts, &error),
      &error);

  mikrio_v1_get_object_attributes_output_view_t view =
      mikrio_v1_get_object_attributes_output_view(facts);

  assert(view.object_size.present == 1 && view.object_size.value == 3);

  assert(view.object_parts_present == 1 &&
         view.object_parts.total_parts_count == 1);

  assert(view.object_parts.parts_count == 0 &&
         view.object_parts.is_truncated == 0);

  assert(view.checksum_present == 1 &&
         equals(view.checksum.checksum_crc64_nvme, "BeXKuz/B+us="));

  assert(mikrio_v1_get_object_attributes_output_part(facts, 0).present == 0);
  guarded_attributes(store, attributes);

  mikrio_v1_get_object_attributes_output_t *composite =
      composite_attributes(store);

  mikrio_v1_get_object_attributes_output_t *rejected = (void *)1;
  attributes.object_attributes_count = 6;

  invalid(mikrio_v1_store_get_object_attributes(store, &attributes, &rejected,
                                                &error),
          &error, 0);

  assert(rejected == NULL);
  attributes.object_attributes_count = 4;
  attributes.max_parts = (mikrio_v1_optional_u64_t){1, 1, 1};

  invalid(mikrio_v1_store_get_object_attributes(store, &attributes, &rejected,
                                                &error),
          &error, 0);

  assert(rejected == NULL);
  mikrio_v1_read_input_t read = {0};
  read.struct_size = sizeof(read);
  read.bucket = complete.bucket;
  read.key = complete.key;
  read.part_number = (mikrio_v1_optional_u64_t){1, 0, 1};
  mikrio_v1_read_output_t *head = NULL;

  ASSERT_SUCCESS(mikrio_v1_store_head_object(store, &read, &head, &error),
                 &error);

  assert(equals(mikrio_v1_read_output_view(head).content_range, "bytes 0-2/3"));
  assert(mikrio_v1_read_output_view(head).parts_count.value == 1);
  mikrio_v1_read_output_release(head);
  mikrio_v1_body_t *body = NULL;

  ASSERT_SUCCESS(mikrio_v1_store_get_object(store, &read, &head, &body, &error),
                 &error);

  assert(mikrio_v1_read_output_view(head).content_length == 3);
  mikrio_v1_read_output_release(head);
  uint8_t bytes[4] = {0};
  uint64_t length = UINT64_MAX;

  ASSERT_SUCCESS(
      mikrio_v1_body_read_chunk(body, bytes, sizeof(bytes), &length, &error),
      &error);

  assert(length == 3 && memcmp(bytes, "abc", 3) == 0);

  ASSERT_SUCCESS(
      mikrio_v1_body_read_chunk(body, bytes, sizeof(bytes), &length, &error),
      &error);

  assert(length == 0);
  ASSERT_SUCCESS(mikrio_v1_body_close(body, &error), &error);
  mikrio_v1_body_release(body);
  /* The entire old 248-byte prefix must still omit the selector. */
  read.struct_size = 248;
  read.part_number = (mikrio_v1_optional_u64_t){1, 0, UINT64_MAX};

  ASSERT_SUCCESS(mikrio_v1_store_head_object(store, &read, &head, &error),
                 &error);

  assert(mikrio_v1_read_output_view(head).parts_count.present == 0);
  mikrio_v1_read_output_release(head);

  mikrio_v1_copy_object_input_t copy = {0};
  copy.struct_size = sizeof(copy);
  copy.bucket = create.bucket;
  copy.key = text("copy");
  copy.copy_source = (mikrio_v1_object_ref_t){create.bucket, create.key};
  mikrio_v1_copy_object_output_t *copied = NULL;

  ASSERT_SUCCESS(mikrio_v1_store_copy_object(store, &copy, &copied, &error),
                 &error);

  mikrio_v1_create_multipart_upload_output_t *abandoned = NULL;
  create.key = text("abandoned");

  ASSERT_SUCCESS(mikrio_v1_store_create_multipart_upload(store, &create,
                                                         &abandoned, &error),
                 &error);

  mikrio_v1_upload_part_copy_input_t copy_part = {0};
  copy_part.struct_size = sizeof(copy_part);
  copy_part.bucket = create.bucket;
  copy_part.key = create.key;

  copy_part.upload_id =
      mikrio_v1_create_multipart_upload_output_view(abandoned).upload_id;

  copy_part.part_number = 1;

  copy_part.copy_source = copy.copy_source;
  mikrio_v1_upload_part_copy_output_t *copied_part = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_store_upload_part_copy(store, &copy_part, &copied_part, &error),
      &error);

  mikrio_v1_list_multipart_uploads_input_t uploads = {0};
  uploads.struct_size = sizeof(uploads);
  uploads.bucket = create.bucket;
  mikrio_v1_list_multipart_uploads_output_t *active = NULL;

  ASSERT_SUCCESS(
      mikrio_v1_store_list_multipart_uploads(store, &uploads, &active, &error),
      &error);

  assert(mikrio_v1_list_multipart_uploads_output_view(active).uploads_count ==
         1);

  mikrio_v1_abort_multipart_upload_input_t abort_input = {0};
  abort_input.struct_size = sizeof(abort_input);
  abort_input.bucket = create.bucket;
  abort_input.key = create.key;
  abort_input.upload_id = copy_part.upload_id;

  ASSERT_SUCCESS(
      mikrio_v1_store_abort_multipart_upload(store, &abort_input, &error),
      &error);

  ASSERT_SUCCESS(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);

  assert(mikrio_v1_get_object_attributes_output_view(facts).object_size.value ==
         3);

  mikrio_v1_get_object_attributes_output_release(facts);
  assert(mikrio_v1_get_object_attributes_output_part(composite, 0).size == 3);
  mikrio_v1_get_object_attributes_output_release(composite);

  assert(equals(mikrio_v1_list_multipart_uploads_output_item(active, 0).key,
                "abandoned"));

  assert(equals(selected.etag, "\"900150983cd24fb0d6963f7d28e17f72\""));

  assert(equals(
      mikrio_v1_upload_part_copy_output_view(copied_part).checksum_crc64_nvme,
      "BeXKuz/B+us="));

  mikrio_v1_create_multipart_upload_output_release(created);
  mikrio_v1_create_multipart_upload_output_release(abandoned);
  mikrio_v1_upload_part_output_release(written);
  mikrio_v1_list_parts_output_release(page);
  mikrio_v1_complete_multipart_upload_output_release(completed);
  mikrio_v1_copy_object_output_release(copied);
  mikrio_v1_upload_part_copy_output_release(copied_part);
  mikrio_v1_list_multipart_uploads_output_release(active);
  puts("PASS multipart_copy_resume_and_owned_views");
  return 0;
}
