/* C ABI major 1. Package and storage format versions are independent.
 *
 * All calls use the platform C calling convention. A status of 0 is success;
 * 1 is failure. An optional error slot receives a detached allocation, or NULL
 * on success. Output slots must be nonnull, aligned, writable and disjoint from
 * every argument/other slot; they are initialized before work begins. Never
 * pass an occupied slot: release its previous allocation first.
 *
 * A span is borrowed for a call, or from an output until that output's release.
 * Input strings are exact UTF-8, not NUL-terminated. {NULL,0} means absent for
 * optional strings; {nonnull,0} means present-empty. Required empty values are
 * domain-validated. NULL with nonzero length is rejected. Arrays contain count
 * readable aligned entries. No input or caller buffer is retained after return.
 *
 * Zero-initialize options, then set struct_size=sizeof(options). An input may
 * instead end at one of the explicit prefix sizes below. Omitted trailing
 * options have their ordinary absent/zero defaults. Other smaller sizes and
 * sizes greater than 65536 are rejected; larger inputs must have initialized
 * zero extension bytes. Padding in the known layout is not inspected. Even a
 * rejected input must provide an aligned readable struct_size word; an accepted
 * input must provide all declared bytes. Fields are copied without retaining
 * caller memory. No reference to an omitted suffix is formed.
 * Optional integers use present=0/value=0 or present=1/value; reserved is zero.
 * Unknown discriminants, options and nonzero extensions are rejected.
 *
 * Accepted prefix sizes, including the complete layout (bytes, all targets):
 * open_options: 24, 40, 56; listen_options: 8, 24, 40, 56, 88.
 * bucket_input: 24, 40; delete_input: 40, 56, 72.
 * put_input: 40 through 344 in steps of 16. Metadata and tag pointer/count
 * pairs are indivisible; neither pointer alone is an accepted prefix.
 * read_input: 40 through 264 in steps of 16; 248 is the pre-partNumber prefix.
 * get_object_attributes_input: 72, 88, 104.
 * list_buckets_input/list_principals_input: 8 through sizeof in steps of 16.
 * list_objects_v2_input: 24 through 120 in steps of 16.
 * list_access_keys_input: 24, 40, 56; delete_objects_input: 56, 64.
 * get/delete bucket policy, get policy status, get/delete public access block,
 * get ownership controls, get bucket ACL, get/delete bucket CORS,
 * get bucket lifecycle configuration, delete bucket lifecycle: 24, 40.
 * get_object_acl_input, get/delete object tagging: 40, 56.
 * All other sized inputs require their complete layout. Required trailing
 * configuration values and flag/reserved pairs cannot be omitted or split.
 * Nested CORS rules require exactly sizeof(mikrio_v1_cors_rule_input_t).
 *
 * Within an ABI major, accepted input prefixes and their defaults remain valid.
 * New trailing fields require explicit whole-field prefixes and zero/absent
 * defaults. A newer caller may use an older library only with a supported
 * prefix or all-zero unknown bytes; unsupported nonzero fields reject, never
 * disappear. New symbols need a sufficiently new library; abi_version reports
 * the major, not symbol availability. Array strides, spans, nested values and
 * by-value return layouts are fixed: extend them with new types/entry points or
 * accessors, never by appending fields to an existing layout. Unknown output
 * error codes must be handled as errors; unknown outcomes conservatively mean
 * uncertainty.
 *
 * Every allocation uses its matching library release, exactly once. NULL
 * release is harmless. Never free with a foreign allocator, access a released
 * handle, or race release with any use. Arbitrary invalid/dangling/unaligned
 * pointers are caller violations, not recoverable validation errors.
 *
 * Store calls and close may run concurrently. Writer calls are serialized by
 * the caller. Body reads use one cursor (overlap returns BodyInUse); body close
 * may race a read and waits for actual native cleanup. Store close can cancel
 * pending writes/reads, is joinable/retryable and has no implicit timeout.
 * Releasing a parent requests shutdown but leaves child allocations valid;
 * children observe shutdown rather than retaining successful operation rights.
 * Independently opened stores remain independent. Fork after initialization
 * requires exec; inherited handles cannot be used or normally closed.
 *
 * Writer write drains the entire buffer in bounded copies before returning;
 * the caller may then reuse it. Empty writes do not signal EOF. Only finish
 * supplies EOF and can publish; it verifies declared length and checksums.
 * Write acceptance is not a commit acknowledgment. A later write/finish may
 * report failure. Abort waits for unpublished work to settle. Finish/abort
 * terminate the attempt but do not release its wrapper. Release never commits.
 * Only Store close can interrupt an in-flight writer call; do not race abort
 * or release on that writer. Cancellation never implies rollback.
 *
 * A positive-capacity read returns count=0 only at EOF. Invalid capacity does
 * not consume the cursor. EOF is repeatable; explicit close makes reads fail.
 * Detached facts/errors remain inspectable after connection shutdown. Views
 * borrow their output allocation, not the Store/body. NULL view returns zeros.
 * Do not modify views or their referenced memory. Native panics are contained;
 * process abort, allocation abort and foreign undefined behavior are not.
 * The matching library image must remain loaded for the process lifetime after
 * initialization: Store close is not a process-wide runtime quiescence fence.
 * Callback code/context must remain live until successful diagnostic drain.
 */
#ifndef MIKRIO_H
#define MIKRIO_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct mikrio_v1_store mikrio_v1_store_t;
typedef struct mikrio_v1_server mikrio_v1_server_t;
typedef struct mikrio_v1_server_info mikrio_v1_server_info_t;
typedef struct mikrio_v1_writer mikrio_v1_writer_t;
typedef struct mikrio_v1_body mikrio_v1_body_t;
typedef struct mikrio_v1_error mikrio_v1_error_t;
typedef struct mikrio_v1_info mikrio_v1_info_t;
typedef struct mikrio_v1_bucket_output mikrio_v1_bucket_output_t;
typedef struct mikrio_v1_put_output mikrio_v1_put_output_t;
typedef struct mikrio_v1_read_output mikrio_v1_read_output_t;
typedef struct mikrio_v1_delete_objects_output
    mikrio_v1_delete_objects_output_t;
typedef struct mikrio_v1_diagnostics mikrio_v1_diagnostics_t;

typedef struct {
  const uint8_t *data;
  uint64_t len;
} mikrio_v1_span_t;

typedef struct {
  uint32_t present;
  uint32_t reserved;
  uint64_t value;
} mikrio_v1_optional_u64_t;

typedef struct {
  uint32_t present;
  uint32_t reserved;
  int64_t value;
} mikrio_v1_optional_i64_t;

typedef struct {
  mikrio_v1_span_t key;
  mikrio_v1_span_t value;
} mikrio_v1_pair_t;

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t directory;
  mikrio_v1_span_t region;
  /* Absent defaults to true. Present value must be 0 or 1; false requires
   * recognized storage, permitting recovery without creating a new identity. */
  mikrio_v1_optional_u64_t create_if_missing;
} mikrio_v1_open_options_t;

typedef struct {
  mikrio_v1_span_t access_key_id;
  mikrio_v1_span_t secret_access_key;
} mikrio_v1_credentials_t;

/* Absent host/port default to 127.0.0.1/0. Both credential spans absent
 * generate a listener-local identity; supplying only one is invalid.
 * public_endpoint is an optional HTTP(S) origin, not a bind address. No input
 * span is retained. */
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t host;
  mikrio_v1_optional_u64_t port;
  mikrio_v1_span_t public_endpoint;
  mikrio_v1_credentials_t credentials;
} mikrio_v1_listen_options_t;

typedef struct {
  mikrio_v1_span_t endpoint;
  mikrio_v1_span_t region;
  mikrio_v1_credentials_t credentials;
} mikrio_v1_server_info_view_t;

/* One listener per connection. Server close joins HTTP/native settlement while
 * direct Store calls remain usable. Store close/release also stops its server.
 * Snapshot allocations remain readable after either handle has been released;
 * they contain a secret and must not be included in ordinary diagnostics. */
uint32_t mikrio_v1_store_listen(const mikrio_v1_store_t *,
                                const mikrio_v1_listen_options_t *,
                                mikrio_v1_server_t **, mikrio_v1_error_t **);
uint32_t mikrio_v1_server_close(const mikrio_v1_server_t *,
                                mikrio_v1_error_t **);
void mikrio_v1_server_release(mikrio_v1_server_t *);
uint32_t mikrio_v1_server_info(const mikrio_v1_server_t *,
                               mikrio_v1_server_info_t **,
                               mikrio_v1_error_t **);
mikrio_v1_server_info_view_t
mikrio_v1_server_info_view(const mikrio_v1_server_info_t *);
void mikrio_v1_server_info_release(mikrio_v1_server_info_t *);

/* CreateBucket rejects a present expected_bucket_owner. Other bucket methods
 * accept the same account assertion as the native facade. */
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_bucket_input_t;

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_optional_u64_t content_length;
  mikrio_v1_span_t content_type;
  mikrio_v1_span_t content_encoding;
  mikrio_v1_span_t content_disposition;
  mikrio_v1_span_t content_language;
  mikrio_v1_span_t cache_control;
  mikrio_v1_optional_i64_t expires;
  const mikrio_v1_pair_t *metadata;
  uint64_t metadata_count;
  const mikrio_v1_pair_t *tags;
  uint64_t tag_count;
  mikrio_v1_span_t checksum_algorithm;
  mikrio_v1_span_t content_md5;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
  mikrio_v1_span_t if_match;
  mikrio_v1_span_t if_none_match;
} mikrio_v1_put_input_t;

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_span_t checksum_mode;
  mikrio_v1_span_t range;
  mikrio_v1_span_t if_match;
  mikrio_v1_span_t if_none_match;
  mikrio_v1_optional_i64_t if_modified_since;
  mikrio_v1_optional_i64_t if_unmodified_since;
  mikrio_v1_span_t response_content_type;
  mikrio_v1_span_t response_content_encoding;
  mikrio_v1_span_t response_content_disposition;
  mikrio_v1_span_t response_content_language;
  mikrio_v1_span_t response_cache_control;
  mikrio_v1_optional_i64_t response_expires;
  mikrio_v1_optional_u64_t
      part_number; /* 1-based assembly position; absent for old prefixes */
} mikrio_v1_read_input_t;

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_span_t if_match;
} mikrio_v1_delete_input_t;

typedef struct {
  mikrio_v1_span_t store_id;
  uint32_t format_version;
  uint32_t reserved;
  mikrio_v1_span_t directory;
  mikrio_v1_span_t region;
} mikrio_v1_info_view_t;

/* Outcomes: 0 absent (never assume rollback), 1 NotCommitted, 2 Committed,
 * 3 Unknown, 4 Partial (known mixed outcomes in interrupted batch operations).
 */
typedef struct {
  mikrio_v1_span_t code;
  mikrio_v1_span_t message;
  mikrio_v1_span_t cause;
  uint32_t mutation_outcome;
  uint32_t reserved;
} mikrio_v1_error_view_t;

typedef struct {
  mikrio_v1_span_t etag;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_put_view_t;

typedef struct {
  uint64_t content_length;
  mikrio_v1_span_t etag;
  int64_t last_modified_ms;
  const mikrio_v1_pair_t *metadata;
  uint64_t metadata_count;
  uint32_t tag_count;
  uint32_t reserved;
  mikrio_v1_span_t content_type;
  mikrio_v1_span_t content_encoding;
  mikrio_v1_span_t content_disposition;
  mikrio_v1_span_t content_language;
  mikrio_v1_span_t cache_control;
  mikrio_v1_optional_i64_t expires;
  mikrio_v1_span_t content_range;
  mikrio_v1_optional_u64_t parts_count;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
  mikrio_v1_span_t checksum_type;
} mikrio_v1_read_view_t;

uint32_t mikrio_v1_abi_version(void);
/* One detached page owns all item, prefix and token spans until release.
 * NULL pages and out-of-bounds indices return zero views. max_keys defaults to
 * 1000 on absence; 0 returns an empty terminal page; values above 1000 fail.
 * Prefix/delimiter presence must be preserved when continuing. */
/* Listing results own every item/span and remain valid after store close.
 * Release once all borrowed views expire; null/out-of-range views are empty. */
typedef struct mikrio_v1_list_buckets_output mikrio_v1_list_buckets_output_t;
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t prefix;
  mikrio_v1_optional_u64_t max_buckets;
  mikrio_v1_span_t continuation_token;
} mikrio_v1_list_buckets_input_t;
typedef struct {
  uint64_t buckets_count;
  mikrio_v1_span_t continuation_token;
} mikrio_v1_list_buckets_output_view_t;
typedef struct {
  mikrio_v1_span_t name;
  int64_t creation_date_ms;
  mikrio_v1_span_t bucket_region;
} mikrio_v1_bucket_summary_view_t;
uint32_t mikrio_v1_store_list_buckets(const mikrio_v1_store_t *,
                                      const mikrio_v1_list_buckets_input_t *,
                                      mikrio_v1_list_buckets_output_t **,
                                      mikrio_v1_error_t **);
mikrio_v1_list_buckets_output_view_t
mikrio_v1_list_buckets_output_view(const mikrio_v1_list_buckets_output_t *);
mikrio_v1_bucket_summary_view_t
mikrio_v1_list_buckets_output_item(const mikrio_v1_list_buckets_output_t *,
                                   uint64_t index);
void mikrio_v1_list_buckets_output_release(mikrio_v1_list_buckets_output_t *);

typedef struct mikrio_v1_list_objects_v2_output
    mikrio_v1_list_objects_v2_output_t;
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_span_t prefix;
  mikrio_v1_span_t delimiter;
  mikrio_v1_optional_u64_t max_keys;
  mikrio_v1_span_t continuation_token;
  mikrio_v1_span_t start_after;
} mikrio_v1_list_objects_v2_input_t;
typedef struct {
  uint64_t contents_count;
  uint64_t common_prefixes_count;
  uint32_t is_truncated;
  uint32_t reserved;
  mikrio_v1_span_t next_continuation_token;
} mikrio_v1_list_objects_v2_output_view_t;
typedef struct {
  mikrio_v1_span_t key;
  uint64_t size;
  mikrio_v1_span_t etag;
  int64_t last_modified_at_ms;
} mikrio_v1_object_summary_view_t;
uint32_t mikrio_v1_store_list_objects_v2(
    const mikrio_v1_store_t *, const mikrio_v1_list_objects_v2_input_t *,
    mikrio_v1_list_objects_v2_output_t **, mikrio_v1_error_t **);
mikrio_v1_list_objects_v2_output_view_t mikrio_v1_list_objects_v2_output_view(
    const mikrio_v1_list_objects_v2_output_t *);
mikrio_v1_object_summary_view_t mikrio_v1_list_objects_v2_output_item(
    const mikrio_v1_list_objects_v2_output_t *, uint64_t index);
mikrio_v1_span_t mikrio_v1_list_objects_v2_output_prefix(
    const mikrio_v1_list_objects_v2_output_t *, uint64_t index);
void mikrio_v1_list_objects_v2_output_release(
    mikrio_v1_list_objects_v2_output_t *);

uint32_t mikrio_v1_store_open(const mikrio_v1_open_options_t *,
                              mikrio_v1_store_t **, mikrio_v1_error_t **);
uint32_t mikrio_v1_store_close(const mikrio_v1_store_t *, mikrio_v1_error_t **);
void mikrio_v1_store_release(mikrio_v1_store_t *);
uint32_t mikrio_v1_store_info(const mikrio_v1_store_t *, mikrio_v1_info_t **,
                              mikrio_v1_error_t **);
mikrio_v1_info_view_t mikrio_v1_info_view(const mikrio_v1_info_t *);
void mikrio_v1_info_release(mikrio_v1_info_t *);
mikrio_v1_error_view_t mikrio_v1_error_view(const mikrio_v1_error_t *);

/* Batch entries are evaluated in order in independent transactions. Quiet (0/1)
 * suppresses successes only. Duplicate positions remain distinct. Input arrays
 * contain 1-1000 entries; all are copied before native dispatch. */
typedef struct {
  mikrio_v1_span_t key;
  mikrio_v1_span_t etag;
} mikrio_v1_object_identifier_t;
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
  const mikrio_v1_object_identifier_t *objects;
  uint64_t objects_count;
  uint32_t quiet;
  uint32_t reserved;
} mikrio_v1_delete_objects_input_t;
typedef struct {
  uint64_t deleted_count;
  uint64_t errors_count;
} mikrio_v1_delete_objects_output_view_t;
typedef struct {
  mikrio_v1_span_t key;
  mikrio_v1_span_t code;
  mikrio_v1_span_t message;
} mikrio_v1_delete_object_error_view_t;
typedef struct {
  uint64_t index;
  mikrio_v1_span_t key;
  uint32_t mutation_outcome;
  uint32_t reserved;
  mikrio_v1_span_t error_code;
  mikrio_v1_span_t error_message;
} mikrio_v1_delete_object_progress_view_t;
uint32_t mikrio_v1_store_delete_objects(
    const mikrio_v1_store_t *, const mikrio_v1_delete_objects_input_t *,
    mikrio_v1_delete_objects_output_t **, mikrio_v1_error_t **);
mikrio_v1_delete_objects_output_view_t
mikrio_v1_delete_objects_output_view(const mikrio_v1_delete_objects_output_t *);
mikrio_v1_span_t mikrio_v1_delete_objects_output_deleted(
    const mikrio_v1_delete_objects_output_t *, uint64_t);
mikrio_v1_delete_object_error_view_t
mikrio_v1_delete_objects_output_error(const mikrio_v1_delete_objects_output_t *,
                                      uint64_t);
void mikrio_v1_delete_objects_output_release(
    mikrio_v1_delete_objects_output_t *);
/* Interrupted progress borrows the detached error. Count 0 means absent. Every
 * original position is present, independent of quiet; item outcomes use 1/2/3
 * (NotCommitted/Committed/Unknown), never aggregate Partial (4). Missing item
 * errors have NULL spans. NULL/out-of-range views return zero fields. */
uint64_t
mikrio_v1_error_delete_objects_progress_count(const mikrio_v1_error_t *);
mikrio_v1_delete_object_progress_view_t
mikrio_v1_error_delete_objects_progress_item(const mikrio_v1_error_t *,
                                             uint64_t);

void mikrio_v1_error_release(mikrio_v1_error_t *);

/* Successful empty create/delete acknowledgments need no output allocation.
 * GetBucketLocation success returns NULL for the us-east-1 absence convention.
 */
uint32_t mikrio_v1_store_create_bucket(const mikrio_v1_store_t *,
                                       const mikrio_v1_bucket_input_t *,
                                       mikrio_v1_error_t **);
uint32_t mikrio_v1_store_head_bucket(const mikrio_v1_store_t *,
                                     const mikrio_v1_bucket_input_t *,
                                     mikrio_v1_bucket_output_t **,
                                     mikrio_v1_error_t **);
uint32_t mikrio_v1_store_get_bucket_location(const mikrio_v1_store_t *,
                                             const mikrio_v1_bucket_input_t *,
                                             mikrio_v1_bucket_output_t **,
                                             mikrio_v1_error_t **);
uint32_t mikrio_v1_store_delete_bucket(const mikrio_v1_store_t *,
                                       const mikrio_v1_bucket_input_t *,
                                       mikrio_v1_error_t **);
mikrio_v1_span_t
mikrio_v1_bucket_output_view(const mikrio_v1_bucket_output_t *);
void mikrio_v1_bucket_output_release(mikrio_v1_bucket_output_t *);

uint32_t mikrio_v1_store_begin_put_object(const mikrio_v1_store_t *,
                                          const mikrio_v1_put_input_t *,
                                          mikrio_v1_writer_t **,
                                          mikrio_v1_error_t **);
uint32_t mikrio_v1_writer_write(mikrio_v1_writer_t *, mikrio_v1_span_t,
                                mikrio_v1_error_t **);
uint32_t mikrio_v1_writer_finish(mikrio_v1_writer_t *,
                                 mikrio_v1_put_output_t **,
                                 mikrio_v1_error_t **);
uint32_t mikrio_v1_writer_abort(mikrio_v1_writer_t *, mikrio_v1_error_t **);
void mikrio_v1_writer_release(mikrio_v1_writer_t *);
mikrio_v1_put_view_t mikrio_v1_put_output_view(const mikrio_v1_put_output_t *);
void mikrio_v1_put_output_release(mikrio_v1_put_output_t *);

uint32_t mikrio_v1_store_get_object(const mikrio_v1_store_t *,
                                    const mikrio_v1_read_input_t *,
                                    mikrio_v1_read_output_t **,
                                    mikrio_v1_body_t **, mikrio_v1_error_t **);
uint32_t mikrio_v1_store_head_object(const mikrio_v1_store_t *,
                                     const mikrio_v1_read_input_t *,
                                     mikrio_v1_read_output_t **,
                                     mikrio_v1_error_t **);
uint32_t mikrio_v1_store_delete_object(const mikrio_v1_store_t *,
                                       const mikrio_v1_delete_input_t *,
                                       mikrio_v1_error_t **);
mikrio_v1_read_view_t
mikrio_v1_read_output_view(const mikrio_v1_read_output_t *);
void mikrio_v1_read_output_release(mikrio_v1_read_output_t *);
uint32_t mikrio_v1_body_read_chunk(const mikrio_v1_body_t *, uint8_t *,
                                   uint64_t, uint64_t *, mikrio_v1_error_t **);
uint32_t mikrio_v1_body_close(const mikrio_v1_body_t *, mikrio_v1_error_t **);
void mikrio_v1_body_release(mikrio_v1_body_t *);

/* Optional hook: one registration per Store wrapper, one serialized background
 * callback thread. Event text is bounded/redacted and borrowed until return;
 * context is caller-owned and may be NULL. The callback may call Store close
 * or independent storage methods; it must not unwind or throw across C.
 * Store close/release revokes new delivery without waiting for a running
 * callback. Keep context alive until unregister/release succeeds. Unregister
 * disables and drains outside storage locks; it may wait forever for a blocked
 * callback. Calling unregister/release from that same callback returns
 * InvalidArgument (delivery is disabled, allocation/context still live).
 * External unregister is repeatable/concurrent; release must be exclusive.
 * Unlike other release functions, this fallible release drains first and frees
 * only on success. A fresh registration is allowed after the worker exits.
 */
typedef void (*mikrio_v1_diagnostic_callback_t)(void *, mikrio_v1_span_t);
uint32_t mikrio_v1_store_register_diagnostics(const mikrio_v1_store_t *,
                                              mikrio_v1_diagnostic_callback_t,
                                              void *,
                                              mikrio_v1_diagnostics_t **,
                                              mikrio_v1_error_t **);
uint32_t mikrio_v1_diagnostics_unregister(const mikrio_v1_diagnostics_t *,
                                          mikrio_v1_error_t **);
uint32_t mikrio_v1_diagnostics_release(mikrio_v1_diagnostics_t *,
                                       mikrio_v1_error_t **);

/* Local administration is trusted directory authority, never an IAM HTTP API.
 * All results are detached owned allocations; each view borrows until release.
 * List item views return zero fields for NULL/out-of-range access. Pages are
 * live keyset pages, not multi-call snapshots. Tokens are opaque and scoped.
 * CreateAccessKey is the sole secret-returning operation: do not log its view.
 * An unknown creation outcome must not be automatically retried. */
typedef struct mikrio_v1_owner_info mikrio_v1_owner_info_t;
typedef struct {
  mikrio_v1_span_t account_id;
  mikrio_v1_span_t canonical_id;
} mikrio_v1_owner_info_view_t;

mikrio_v1_owner_info_view_t
mikrio_v1_owner_info_view(const mikrio_v1_owner_info_t *value);
void mikrio_v1_owner_info_release(mikrio_v1_owner_info_t *value);

typedef struct mikrio_v1_principal_info mikrio_v1_principal_info_t;
typedef struct {
  mikrio_v1_span_t principal_id;
  mikrio_v1_span_t name;
  uint32_t is_enabled;
  uint32_t reserved;
  int64_t created_at_ms;
} mikrio_v1_principal_info_view_t;

mikrio_v1_principal_info_view_t
mikrio_v1_principal_info_view(const mikrio_v1_principal_info_t *value);
void mikrio_v1_principal_info_release(mikrio_v1_principal_info_t *value);

typedef struct {
  mikrio_v1_span_t access_key_id;
  mikrio_v1_span_t principal_id;
  int64_t created_at_ms;
} mikrio_v1_access_key_info_view_t;

typedef struct mikrio_v1_create_access_key_output
    mikrio_v1_create_access_key_output_t;
typedef struct {
  mikrio_v1_access_key_info_view_t access_key;
  mikrio_v1_span_t secret_access_key;
} mikrio_v1_create_access_key_output_view_t;

mikrio_v1_create_access_key_output_view_t
mikrio_v1_create_access_key_output_view(
    const mikrio_v1_create_access_key_output_t *value);
void mikrio_v1_create_access_key_output_release(
    mikrio_v1_create_access_key_output_t *value);

typedef struct mikrio_v1_list_principals_output
    mikrio_v1_list_principals_output_t;
typedef struct {
  uint64_t count;
  mikrio_v1_span_t next_continuation_token;
} mikrio_v1_list_principals_output_view_t;

mikrio_v1_list_principals_output_view_t mikrio_v1_list_principals_output_view(
    const mikrio_v1_list_principals_output_t *value);
void mikrio_v1_list_principals_output_release(
    mikrio_v1_list_principals_output_t *value);

typedef struct mikrio_v1_list_access_keys_output
    mikrio_v1_list_access_keys_output_t;
typedef struct {
  uint64_t count;
  mikrio_v1_span_t next_continuation_token;
} mikrio_v1_list_access_keys_output_view_t;

mikrio_v1_list_access_keys_output_view_t mikrio_v1_list_access_keys_output_view(
    const mikrio_v1_list_access_keys_output_t *value);
void mikrio_v1_list_access_keys_output_release(
    mikrio_v1_list_access_keys_output_t *value);

typedef struct mikrio_v1_get_principal_policy_output
    mikrio_v1_get_principal_policy_output_t;
typedef struct {
  mikrio_v1_span_t document;
} mikrio_v1_get_principal_policy_output_view_t;

mikrio_v1_get_principal_policy_output_view_t
mikrio_v1_get_principal_policy_output_view(
    const mikrio_v1_get_principal_policy_output_t *value);
void mikrio_v1_get_principal_policy_output_release(
    mikrio_v1_get_principal_policy_output_t *value);

mikrio_v1_principal_info_view_t mikrio_v1_list_principals_output_item(
    const mikrio_v1_list_principals_output_t *value, uint64_t index);
mikrio_v1_access_key_info_view_t mikrio_v1_list_access_keys_output_item(
    const mikrio_v1_list_access_keys_output_t *value, uint64_t index);
uint32_t mikrio_v1_store_get_owner_info(const mikrio_v1_store_t *store,
                                        mikrio_v1_owner_info_t **result,
                                        mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t name;
} mikrio_v1_create_principal_input_t;
uint32_t mikrio_v1_store_create_principal(
    const mikrio_v1_store_t *store,
    const mikrio_v1_create_principal_input_t *input,
    mikrio_v1_principal_info_t **result, mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t principal_id;
} mikrio_v1_get_principal_input_t;
uint32_t
mikrio_v1_store_get_principal(const mikrio_v1_store_t *store,
                              const mikrio_v1_get_principal_input_t *input,
                              mikrio_v1_principal_info_t **result,
                              mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t principal_id;
  uint32_t is_enabled;
  uint32_t reserved;
} mikrio_v1_set_principal_enabled_input_t;
uint32_t mikrio_v1_store_set_principal_enabled(
    const mikrio_v1_store_t *store,
    const mikrio_v1_set_principal_enabled_input_t *input,
    mikrio_v1_principal_info_t **result, mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t principal_id;
} mikrio_v1_delete_principal_input_t;
uint32_t mikrio_v1_store_delete_principal(
    const mikrio_v1_store_t *store,
    const mikrio_v1_delete_principal_input_t *input, mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t principal_id;
  mikrio_v1_span_t document;
} mikrio_v1_put_principal_policy_input_t;
uint32_t mikrio_v1_store_put_principal_policy(
    const mikrio_v1_store_t *store,
    const mikrio_v1_put_principal_policy_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t principal_id;
} mikrio_v1_get_principal_policy_input_t;
uint32_t mikrio_v1_store_get_principal_policy(
    const mikrio_v1_store_t *store,
    const mikrio_v1_get_principal_policy_input_t *input,
    mikrio_v1_get_principal_policy_output_t **result,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t principal_id;
} mikrio_v1_delete_principal_policy_input_t;
uint32_t mikrio_v1_store_delete_principal_policy(
    const mikrio_v1_store_t *store,
    const mikrio_v1_delete_principal_policy_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t principal_id;
} mikrio_v1_create_access_key_input_t;
uint32_t mikrio_v1_store_create_access_key(
    const mikrio_v1_store_t *store,
    const mikrio_v1_create_access_key_input_t *input,
    mikrio_v1_create_access_key_output_t **result, mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t principal_id;
  mikrio_v1_span_t access_key_id;
} mikrio_v1_delete_access_key_input_t;
uint32_t mikrio_v1_store_delete_access_key(
    const mikrio_v1_store_t *store,
    const mikrio_v1_delete_access_key_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_optional_u64_t max_results;
  mikrio_v1_span_t continuation_token;
} mikrio_v1_list_principals_input_t;
uint32_t
mikrio_v1_store_list_principals(const mikrio_v1_store_t *store,
                                const mikrio_v1_list_principals_input_t *input,
                                mikrio_v1_list_principals_output_t **result,
                                mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t principal_id;
  mikrio_v1_optional_u64_t max_results;
  mikrio_v1_span_t continuation_token;
} mikrio_v1_list_access_keys_input_t;
uint32_t mikrio_v1_store_list_access_keys(
    const mikrio_v1_store_t *store,
    const mikrio_v1_list_access_keys_input_t *input,
    mikrio_v1_list_access_keys_output_t **result, mikrio_v1_error_t **error);

/* Access configuration is replaced atomically. All four flags below are
 * required 0/1 fields. A present all-zero configuration differs from deletion.
 * Ownership and ACL discriminants use the fixed uint32_t constants below.
 * Returned allocations are detached; their spans/items borrow that allocation
 * until its matching release, including after Store close/release. */
#define MIKRIO_V1_BUCKET_OWNER_ENFORCED UINT32_C(1)
#define MIKRIO_V1_ACL_FULL_CONTROL UINT32_C(1)

typedef struct {
  uint32_t block_public_acls;
  uint32_t ignore_public_acls;
  uint32_t block_public_policy;
  uint32_t restrict_public_buckets;
} mikrio_v1_public_access_block_configuration_t;

typedef struct {
  mikrio_v1_span_t canonical_id;
} mikrio_v1_acl_owner_view_t;
typedef struct {
  mikrio_v1_span_t grantee_canonical_id;
  uint32_t permission;
  uint32_t reserved;
} mikrio_v1_acl_grant_view_t;

typedef struct mikrio_v1_get_bucket_policy_output
    mikrio_v1_get_bucket_policy_output_t;
typedef struct {
  mikrio_v1_span_t document;
} mikrio_v1_get_bucket_policy_output_view_t;

mikrio_v1_get_bucket_policy_output_view_t
mikrio_v1_get_bucket_policy_output_view(
    const mikrio_v1_get_bucket_policy_output_t *value);
void mikrio_v1_get_bucket_policy_output_release(
    mikrio_v1_get_bucket_policy_output_t *value);

typedef struct mikrio_v1_get_bucket_policy_status_output
    mikrio_v1_get_bucket_policy_status_output_t;
typedef struct {
  uint32_t is_public;
  uint32_t reserved;
} mikrio_v1_get_bucket_policy_status_output_view_t;

mikrio_v1_get_bucket_policy_status_output_view_t
mikrio_v1_get_bucket_policy_status_output_view(
    const mikrio_v1_get_bucket_policy_status_output_t *value);
void mikrio_v1_get_bucket_policy_status_output_release(
    mikrio_v1_get_bucket_policy_status_output_t *value);

typedef struct mikrio_v1_get_public_access_block_output
    mikrio_v1_get_public_access_block_output_t;
typedef struct {
  mikrio_v1_public_access_block_configuration_t configuration;
} mikrio_v1_get_public_access_block_output_view_t;

mikrio_v1_get_public_access_block_output_view_t
mikrio_v1_get_public_access_block_output_view(
    const mikrio_v1_get_public_access_block_output_t *value);
void mikrio_v1_get_public_access_block_output_release(
    mikrio_v1_get_public_access_block_output_t *value);

typedef struct mikrio_v1_get_bucket_ownership_controls_output
    mikrio_v1_get_bucket_ownership_controls_output_t;
typedef struct {
  uint32_t object_ownership;
  uint32_t reserved;
} mikrio_v1_get_bucket_ownership_controls_output_view_t;

mikrio_v1_get_bucket_ownership_controls_output_view_t
mikrio_v1_get_bucket_ownership_controls_output_view(
    const mikrio_v1_get_bucket_ownership_controls_output_t *value);
void mikrio_v1_get_bucket_ownership_controls_output_release(
    mikrio_v1_get_bucket_ownership_controls_output_t *value);

typedef struct mikrio_v1_get_bucket_acl_output
    mikrio_v1_get_bucket_acl_output_t;
typedef struct {
  mikrio_v1_acl_owner_view_t owner;
  uint64_t grant_count;
} mikrio_v1_get_bucket_acl_output_view_t;

mikrio_v1_get_bucket_acl_output_view_t mikrio_v1_get_bucket_acl_output_view(
    const mikrio_v1_get_bucket_acl_output_t *value);
void mikrio_v1_get_bucket_acl_output_release(
    mikrio_v1_get_bucket_acl_output_t *value);
/* NULL result or out-of-range index returns an all-zero view. */
mikrio_v1_acl_grant_view_t mikrio_v1_get_bucket_acl_output_grant(
    const mikrio_v1_get_bucket_acl_output_t *value, uint64_t index);

typedef struct mikrio_v1_get_object_acl_output
    mikrio_v1_get_object_acl_output_t;
typedef struct {
  mikrio_v1_acl_owner_view_t owner;
  uint64_t grant_count;
} mikrio_v1_get_object_acl_output_view_t;

mikrio_v1_get_object_acl_output_view_t mikrio_v1_get_object_acl_output_view(
    const mikrio_v1_get_object_acl_output_t *value);
void mikrio_v1_get_object_acl_output_release(
    mikrio_v1_get_object_acl_output_t *value);
/* NULL result or out-of-range index returns an all-zero view. */
mikrio_v1_acl_grant_view_t mikrio_v1_get_object_acl_output_grant(
    const mikrio_v1_get_object_acl_output_t *value, uint64_t index);

/* Lifecycle inputs borrow all arrays/spans only during the call. Rules use
 * exactly sizeof(mikrio_v1_lifecycle_rule_input_t), a fixed canonical stride.
 * All rules use a modern Filter: NULL/0 prefix is absent, nonnull/0 is empty.
 * At least one action is required. Days are 1..INT32_MAX; optional
 * discriminants must be 0/1 and reserved words zero. PUT atomically
 * replaces 1..1000 rules. GET/DELETE accept the input prefix before
 * expected_bucket_owner. GET returns NoSuchLifecycleConfiguration for absence.
 * Returned rules/spans borrow the detached output until its release;
 * NULL/out-of-range views are all zero. Mutation acknowledgments allocate
 * nothing. Release never publishes. */
#define MIKRIO_V1_LIFECYCLE_ENABLED 1u
#define MIKRIO_V1_LIFECYCLE_DISABLED 2u
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t id;
  uint32_t status;
  uint32_t reserved;
  mikrio_v1_span_t prefix;
  mikrio_v1_optional_u64_t expiration_days;
  mikrio_v1_optional_u64_t abort_days_after_initiation;
} mikrio_v1_lifecycle_rule_input_t;
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
  const mikrio_v1_lifecycle_rule_input_t *lifecycle_rules;
  uint64_t lifecycle_rule_count;
} mikrio_v1_put_bucket_lifecycle_configuration_input_t;
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_get_bucket_lifecycle_configuration_input_t;
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_delete_bucket_lifecycle_input_t;
typedef struct mikrio_v1_get_bucket_lifecycle_configuration_output
    mikrio_v1_get_bucket_lifecycle_configuration_output_t;
typedef struct {
  uint64_t lifecycle_rule_count;
} mikrio_v1_get_bucket_lifecycle_configuration_output_view_t;
typedef struct {
  mikrio_v1_span_t id;
  uint32_t status;
  uint32_t reserved;
  mikrio_v1_span_t prefix;
  mikrio_v1_optional_u64_t expiration_days;
  mikrio_v1_optional_u64_t abort_days_after_initiation;
} mikrio_v1_lifecycle_rule_view_t;
uint32_t mikrio_v1_store_put_bucket_lifecycle_configuration(
    const mikrio_v1_store_t *,
    const mikrio_v1_put_bucket_lifecycle_configuration_input_t *,
    mikrio_v1_error_t **);
uint32_t mikrio_v1_store_get_bucket_lifecycle_configuration(
    const mikrio_v1_store_t *,
    const mikrio_v1_get_bucket_lifecycle_configuration_input_t *,
    mikrio_v1_get_bucket_lifecycle_configuration_output_t **,
    mikrio_v1_error_t **);
uint32_t mikrio_v1_store_delete_bucket_lifecycle(
    const mikrio_v1_store_t *,
    const mikrio_v1_delete_bucket_lifecycle_input_t *, mikrio_v1_error_t **);
mikrio_v1_get_bucket_lifecycle_configuration_output_view_t
mikrio_v1_get_bucket_lifecycle_configuration_output_view(
    const mikrio_v1_get_bucket_lifecycle_configuration_output_t *);
mikrio_v1_lifecycle_rule_view_t
mikrio_v1_get_bucket_lifecycle_configuration_output_rule(
    const mikrio_v1_get_bucket_lifecycle_configuration_output_t *,
    uint64_t index);
void mikrio_v1_get_bucket_lifecycle_configuration_output_release(
    mikrio_v1_get_bucket_lifecycle_configuration_output_t *);

/* CORS string arrays borrow caller memory only during the synchronous call.
 * Null/zero optional arrays are absent; nonnull/zero arrays are explicitly
 * empty. Nested rule elements use exactly sizeof(mikrio_v1_cors_rule_input_t),
 * with no extension bytes, because the rule array has a fixed canonical stride.
 */
typedef struct {
  const mikrio_v1_span_t *data;
  uint64_t count;
} mikrio_v1_span_list_t;

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t id;
  mikrio_v1_span_list_t allowed_origins;
  mikrio_v1_span_list_t allowed_methods;
  mikrio_v1_span_list_t allowed_headers;
  mikrio_v1_span_list_t expose_headers;
  mikrio_v1_optional_i64_t max_age_seconds;
} mikrio_v1_cors_rule_input_t;

/* Attributes are detached owned results. Views and all nested spans borrow the
 * output allocation; release it with the matching library after all view use.
 * Selectors are a fixed-stride array of 1-5 UTF-8 spans, with exact S3
 * spellings. Count-only parts have zero parts_count; total_parts_count remains
 * available. Null results and out-of-range indexes return zero views.
 * max_parts defaults to 1,000 (valid 1-1,000); part_number_marker defaults to
 * zero (valid 0-10,000, exclusive original part number). Count-only results
 * echo these inputs with false truncation and absent continuation. */
typedef struct mikrio_v1_get_object_attributes_output
    mikrio_v1_get_object_attributes_output_t;
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t expected_bucket_owner;
  const mikrio_v1_span_t *object_attributes;
  uint64_t object_attributes_count;
  mikrio_v1_optional_u64_t max_parts;
  mikrio_v1_optional_u64_t part_number_marker;
} mikrio_v1_get_object_attributes_input_t;
typedef struct {
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
  mikrio_v1_span_t checksum_type;
} mikrio_v1_checksum_view_t;
typedef struct {
  uint64_t total_parts_count;
  uint64_t max_parts;
  uint64_t part_number_marker;
  uint64_t parts_count;
  uint32_t is_truncated;
  uint32_t reserved;
  mikrio_v1_optional_u64_t next_part_number_marker;
} mikrio_v1_object_parts_view_t;
typedef struct {
  mikrio_v1_optional_u64_t object_size;
  mikrio_v1_span_t etag;
  uint32_t checksum_present;
  uint32_t reserved;
  mikrio_v1_checksum_view_t checksum;
  mikrio_v1_span_t storage_class;
  uint32_t object_parts_present;
  uint32_t reserved2;
  mikrio_v1_object_parts_view_t object_parts;
  int64_t last_modified_unix_millis;
} mikrio_v1_get_object_attributes_output_view_t;
typedef struct {
  uint32_t present;
  uint32_t reserved;
  uint64_t part_number;
  uint64_t size;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_object_part_view_t;
uint32_t mikrio_v1_store_get_object_attributes(
    const mikrio_v1_store_t *, const mikrio_v1_get_object_attributes_input_t *,
    mikrio_v1_get_object_attributes_output_t **, mikrio_v1_error_t **);
mikrio_v1_get_object_attributes_output_view_t
mikrio_v1_get_object_attributes_output_view(
    const mikrio_v1_get_object_attributes_output_t *);
mikrio_v1_object_part_view_t mikrio_v1_get_object_attributes_output_part(
    const mikrio_v1_get_object_attributes_output_t *, uint64_t index);
void mikrio_v1_get_object_attributes_output_release(
    mikrio_v1_get_object_attributes_output_t *);

/* Whole-set object tags use the existing typed key/value pair layout.
 * GET owns its result independently of Store; indexed pair spans borrow that
 * result until release. Null results/out-of-range indexes return zero views.
 * PUT requires the full input, including tags/tag_count; NULL/0 clears the set.
 * GET/DELETE accept the prefix ending before expected_bucket_owner.
 * Mutation acknowledgments allocate nothing. */
typedef struct mikrio_v1_get_object_tagging_output
    mikrio_v1_get_object_tagging_output_t;
typedef struct {
  uint64_t tag_count;
} mikrio_v1_get_object_tagging_output_view_t;
mikrio_v1_get_object_tagging_output_view_t
mikrio_v1_get_object_tagging_output_view(
    const mikrio_v1_get_object_tagging_output_t *value);
mikrio_v1_pair_t mikrio_v1_get_object_tagging_output_tag(
    const mikrio_v1_get_object_tagging_output_t *value, uint64_t index);
void mikrio_v1_get_object_tagging_output_release(
    mikrio_v1_get_object_tagging_output_t *value);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_get_object_tagging_input_t;
uint32_t mikrio_v1_store_get_object_tagging(
    const mikrio_v1_store_t *store,
    const mikrio_v1_get_object_tagging_input_t *input,
    mikrio_v1_get_object_tagging_output_t **result, mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t expected_bucket_owner;
  const mikrio_v1_pair_t *tags;
  uint64_t tag_count;
} mikrio_v1_put_object_tagging_input_t;
uint32_t mikrio_v1_store_put_object_tagging(
    const mikrio_v1_store_t *store,
    const mikrio_v1_put_object_tagging_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_delete_object_tagging_input_t;
uint32_t mikrio_v1_store_delete_object_tagging(
    const mikrio_v1_store_t *store,
    const mikrio_v1_delete_object_tagging_input_t *input,
    mikrio_v1_error_t **error);

typedef struct mikrio_v1_get_bucket_cors_output
    mikrio_v1_get_bucket_cors_output_t;
typedef struct {
  uint64_t cors_rule_count;
} mikrio_v1_get_bucket_cors_output_view_t;
typedef struct {
  mikrio_v1_span_t id;
  uint64_t allowed_origin_count;
  uint64_t allowed_method_count;
  mikrio_v1_optional_u64_t allowed_header_count;
  mikrio_v1_optional_u64_t expose_header_count;
  mikrio_v1_optional_i64_t max_age_seconds;
} mikrio_v1_cors_rule_view_t;

/* All views borrow the detached output allocation until its release. Null
 * outputs or out-of-range rule/string indexes return zero views/spans. */
mikrio_v1_get_bucket_cors_output_view_t mikrio_v1_get_bucket_cors_output_view(
    const mikrio_v1_get_bucket_cors_output_t *value);
mikrio_v1_cors_rule_view_t mikrio_v1_get_bucket_cors_output_rule(
    const mikrio_v1_get_bucket_cors_output_t *value, uint64_t index);
void mikrio_v1_get_bucket_cors_output_release(
    mikrio_v1_get_bucket_cors_output_t *value);
mikrio_v1_span_t mikrio_v1_get_bucket_cors_output_allowed_origin(
    const mikrio_v1_get_bucket_cors_output_t *value, uint64_t rule_index,
    uint64_t index);
mikrio_v1_span_t mikrio_v1_get_bucket_cors_output_allowed_method(
    const mikrio_v1_get_bucket_cors_output_t *value, uint64_t rule_index,
    uint64_t index);
mikrio_v1_span_t mikrio_v1_get_bucket_cors_output_allowed_header(
    const mikrio_v1_get_bucket_cors_output_t *value, uint64_t rule_index,
    uint64_t index);
mikrio_v1_span_t mikrio_v1_get_bucket_cors_output_expose_header(
    const mikrio_v1_get_bucket_cors_output_t *value, uint64_t rule_index,
    uint64_t index);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
  const mikrio_v1_cors_rule_input_t *cors_rules;
  uint64_t cors_rule_count;
} mikrio_v1_put_bucket_cors_input_t;
uint32_t
mikrio_v1_store_put_bucket_cors(const mikrio_v1_store_t *store,
                                const mikrio_v1_put_bucket_cors_input_t *input,
                                mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_get_bucket_cors_input_t;
uint32_t
mikrio_v1_store_get_bucket_cors(const mikrio_v1_store_t *store,
                                const mikrio_v1_get_bucket_cors_input_t *input,
                                mikrio_v1_get_bucket_cors_output_t **result,
                                mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_delete_bucket_cors_input_t;
uint32_t mikrio_v1_store_delete_bucket_cors(
    const mikrio_v1_store_t *store,
    const mikrio_v1_delete_bucket_cors_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_span_t document;
} mikrio_v1_put_bucket_policy_input_t;
uint32_t mikrio_v1_store_put_bucket_policy(
    const mikrio_v1_store_t *store,
    const mikrio_v1_put_bucket_policy_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_get_bucket_policy_input_t;
uint32_t mikrio_v1_store_get_bucket_policy(
    const mikrio_v1_store_t *store,
    const mikrio_v1_get_bucket_policy_input_t *input,
    mikrio_v1_get_bucket_policy_output_t **result, mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_delete_bucket_policy_input_t;
uint32_t mikrio_v1_store_delete_bucket_policy(
    const mikrio_v1_store_t *store,
    const mikrio_v1_delete_bucket_policy_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_get_bucket_policy_status_input_t;
uint32_t mikrio_v1_store_get_bucket_policy_status(
    const mikrio_v1_store_t *store,
    const mikrio_v1_get_bucket_policy_status_input_t *input,
    mikrio_v1_get_bucket_policy_status_output_t **result,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_public_access_block_configuration_t configuration;
} mikrio_v1_put_public_access_block_input_t;
uint32_t mikrio_v1_store_put_public_access_block(
    const mikrio_v1_store_t *store,
    const mikrio_v1_put_public_access_block_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_get_public_access_block_input_t;
uint32_t mikrio_v1_store_get_public_access_block(
    const mikrio_v1_store_t *store,
    const mikrio_v1_get_public_access_block_input_t *input,
    mikrio_v1_get_public_access_block_output_t **result,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_delete_public_access_block_input_t;
uint32_t mikrio_v1_store_delete_public_access_block(
    const mikrio_v1_store_t *store,
    const mikrio_v1_delete_public_access_block_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
  uint32_t object_ownership;
  uint32_t reserved;
} mikrio_v1_put_bucket_ownership_controls_input_t;
uint32_t mikrio_v1_store_put_bucket_ownership_controls(
    const mikrio_v1_store_t *store,
    const mikrio_v1_put_bucket_ownership_controls_input_t *input,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_get_bucket_ownership_controls_input_t;
uint32_t mikrio_v1_store_get_bucket_ownership_controls(
    const mikrio_v1_store_t *store,
    const mikrio_v1_get_bucket_ownership_controls_input_t *input,
    mikrio_v1_get_bucket_ownership_controls_output_t **result,
    mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_get_bucket_acl_input_t;
uint32_t
mikrio_v1_store_get_bucket_acl(const mikrio_v1_store_t *store,
                               const mikrio_v1_get_bucket_acl_input_t *input,
                               mikrio_v1_get_bucket_acl_output_t **result,
                               mikrio_v1_error_t **error);

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_get_object_acl_input_t;
uint32_t
mikrio_v1_store_get_object_acl(const mikrio_v1_store_t *store,
                               const mikrio_v1_get_object_acl_input_t *input,
                               mikrio_v1_get_object_acl_output_t **result,
                               mikrio_v1_error_t **error);

/* COPY/multipart inputs require their complete sized layout. Nested object
 * references and completed-part arrays have fixed v1 strides. Part reserved
 * fields must be zero. Copy references contain literal UTF-8 bucket/key spans;
 * only copy_source_range uses the closed bytes=first-last syntax. Checksum,
 * checksum-type and directive spans use their exact uppercase S3 spellings.
 * Every result view borrows its detached allocation until matching release.
 * Null results and out-of-range indices yield zero views. */
typedef struct {
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
} mikrio_v1_object_ref_t;
typedef struct {
  uint32_t part_number;
  uint32_t reserved;
  mikrio_v1_span_t etag;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_completed_part_t;
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_object_ref_t copy_source;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_span_t expected_source_bucket_owner;
  mikrio_v1_span_t copy_source_if_match;
  mikrio_v1_span_t copy_source_if_none_match;
  mikrio_v1_optional_i64_t copy_source_if_modified_since;
  mikrio_v1_optional_i64_t copy_source_if_unmodified_since;
  mikrio_v1_span_t metadata_directive;
  mikrio_v1_span_t content_type;
  mikrio_v1_span_t content_encoding;
  mikrio_v1_span_t content_disposition;
  mikrio_v1_span_t content_language;
  mikrio_v1_span_t cache_control;
  mikrio_v1_optional_i64_t expires;
  const mikrio_v1_pair_t *metadata;
  uint64_t metadata_count;
  const mikrio_v1_pair_t *tags;
  uint64_t tags_count;
  mikrio_v1_span_t tagging_directive;
  mikrio_v1_span_t checksum_algorithm;
  mikrio_v1_span_t if_match;
  mikrio_v1_span_t if_none_match;
} mikrio_v1_copy_object_input_t;
typedef struct mikrio_v1_copy_object_output mikrio_v1_copy_object_output_t;
uint32_t mikrio_v1_store_copy_object(const mikrio_v1_store_t *,
                                     const mikrio_v1_copy_object_input_t *,
                                     mikrio_v1_copy_object_output_t **,
                                     mikrio_v1_error_t **);
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_span_t content_type;
  mikrio_v1_span_t content_encoding;
  mikrio_v1_span_t content_disposition;
  mikrio_v1_span_t content_language;
  mikrio_v1_span_t cache_control;
  mikrio_v1_optional_i64_t expires;
  const mikrio_v1_pair_t *metadata;
  uint64_t metadata_count;
  const mikrio_v1_pair_t *tags;
  uint64_t tags_count;
  mikrio_v1_span_t checksum_algorithm;
  mikrio_v1_span_t checksum_type;
} mikrio_v1_create_multipart_upload_input_t;
typedef struct mikrio_v1_create_multipart_upload_output
    mikrio_v1_create_multipart_upload_output_t;
uint32_t mikrio_v1_store_create_multipart_upload(
    const mikrio_v1_store_t *,
    const mikrio_v1_create_multipart_upload_input_t *,
    mikrio_v1_create_multipart_upload_output_t **, mikrio_v1_error_t **);
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t upload_id;
  uint32_t part_number;
  uint32_t part_number_reserved;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_optional_u64_t content_length;
  mikrio_v1_span_t checksum_algorithm;
  mikrio_v1_span_t content_md5;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_upload_part_input_t;
uint32_t
mikrio_v1_store_begin_upload_part(const mikrio_v1_store_t *,
                                  const mikrio_v1_upload_part_input_t *,
                                  mikrio_v1_writer_t **, mikrio_v1_error_t **);
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t upload_id;
  uint32_t part_number;
  uint32_t part_number_reserved;
  mikrio_v1_object_ref_t copy_source;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_span_t expected_source_bucket_owner;
  mikrio_v1_span_t copy_source_if_match;
  mikrio_v1_span_t copy_source_if_none_match;
  mikrio_v1_optional_i64_t copy_source_if_modified_since;
  mikrio_v1_optional_i64_t copy_source_if_unmodified_since;
  mikrio_v1_span_t copy_source_range;
} mikrio_v1_upload_part_copy_input_t;
typedef struct mikrio_v1_upload_part_copy_output
    mikrio_v1_upload_part_copy_output_t;
uint32_t mikrio_v1_store_upload_part_copy(
    const mikrio_v1_store_t *, const mikrio_v1_upload_part_copy_input_t *,
    mikrio_v1_upload_part_copy_output_t **, mikrio_v1_error_t **);
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t upload_id;
  const mikrio_v1_completed_part_t *parts;
  uint64_t parts_count;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_span_t if_match;
  mikrio_v1_span_t if_none_match;
  mikrio_v1_optional_u64_t content_length;
  mikrio_v1_span_t checksum_type;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_complete_multipart_upload_input_t;
typedef struct mikrio_v1_complete_multipart_upload_output
    mikrio_v1_complete_multipart_upload_output_t;
uint32_t mikrio_v1_store_complete_multipart_upload(
    const mikrio_v1_store_t *,
    const mikrio_v1_complete_multipart_upload_input_t *,
    mikrio_v1_complete_multipart_upload_output_t **, mikrio_v1_error_t **);
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t upload_id;
  mikrio_v1_span_t expected_bucket_owner;
} mikrio_v1_abort_multipart_upload_input_t;
uint32_t mikrio_v1_store_abort_multipart_upload(
    const mikrio_v1_store_t *, const mikrio_v1_abort_multipart_upload_input_t *,
    mikrio_v1_error_t **);
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t key;
  mikrio_v1_span_t upload_id;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_optional_u64_t part_number_marker;
  mikrio_v1_optional_u64_t max_parts;
} mikrio_v1_list_parts_input_t;
typedef struct mikrio_v1_list_parts_output mikrio_v1_list_parts_output_t;
uint32_t mikrio_v1_store_list_parts(const mikrio_v1_store_t *,
                                    const mikrio_v1_list_parts_input_t *,
                                    mikrio_v1_list_parts_output_t **,
                                    mikrio_v1_error_t **);
typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t bucket;
  mikrio_v1_span_t expected_bucket_owner;
  mikrio_v1_span_t prefix;
  mikrio_v1_span_t delimiter;
  mikrio_v1_span_t key_marker;
  mikrio_v1_span_t upload_id_marker;
  mikrio_v1_optional_u64_t max_uploads;
} mikrio_v1_list_multipart_uploads_input_t;
typedef struct mikrio_v1_list_multipart_uploads_output
    mikrio_v1_list_multipart_uploads_output_t;
uint32_t mikrio_v1_store_list_multipart_uploads(
    const mikrio_v1_store_t *, const mikrio_v1_list_multipart_uploads_input_t *,
    mikrio_v1_list_multipart_uploads_output_t **, mikrio_v1_error_t **);
typedef struct {
  mikrio_v1_span_t etag;
  int64_t last_modified_ms;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_copy_object_output_view_t;
mikrio_v1_copy_object_output_view_t
mikrio_v1_copy_object_output_view(const mikrio_v1_copy_object_output_t *);
void mikrio_v1_copy_object_output_release(mikrio_v1_copy_object_output_t *);
typedef struct {
  mikrio_v1_span_t upload_id;
  mikrio_v1_span_t checksum_algorithm;
  mikrio_v1_span_t checksum_type;
} mikrio_v1_create_multipart_upload_output_view_t;
mikrio_v1_create_multipart_upload_output_view_t
mikrio_v1_create_multipart_upload_output_view(
    const mikrio_v1_create_multipart_upload_output_t *);
void mikrio_v1_create_multipart_upload_output_release(
    mikrio_v1_create_multipart_upload_output_t *);
typedef struct mikrio_v1_upload_part_output mikrio_v1_upload_part_output_t;
typedef struct {
  mikrio_v1_span_t etag;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_upload_part_output_view_t;
mikrio_v1_upload_part_output_view_t
mikrio_v1_upload_part_output_view(const mikrio_v1_upload_part_output_t *);
void mikrio_v1_upload_part_output_release(mikrio_v1_upload_part_output_t *);
typedef struct {
  mikrio_v1_span_t etag;
  int64_t last_modified_ms;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_upload_part_copy_output_view_t;
mikrio_v1_upload_part_copy_output_view_t mikrio_v1_upload_part_copy_output_view(
    const mikrio_v1_upload_part_copy_output_t *);
void mikrio_v1_upload_part_copy_output_release(
    mikrio_v1_upload_part_copy_output_t *);
typedef struct {
  mikrio_v1_span_t etag;
  mikrio_v1_span_t checksum_type;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_complete_multipart_upload_output_view_t;
mikrio_v1_complete_multipart_upload_output_view_t
mikrio_v1_complete_multipart_upload_output_view(
    const mikrio_v1_complete_multipart_upload_output_t *);
void mikrio_v1_complete_multipart_upload_output_release(
    mikrio_v1_complete_multipart_upload_output_t *);
typedef struct {
  uint32_t part_number;
  uint32_t part_number_reserved;
  uint64_t size;
  mikrio_v1_span_t etag;
  int64_t last_modified_ms;
  mikrio_v1_span_t checksum_crc32;
  mikrio_v1_span_t checksum_crc32c;
  mikrio_v1_span_t checksum_sha1;
  mikrio_v1_span_t checksum_sha256;
  mikrio_v1_span_t checksum_crc64_nvme;
} mikrio_v1_part_summary_view_t;
mikrio_v1_part_summary_view_t
mikrio_v1_list_parts_output_item(const mikrio_v1_list_parts_output_t *,
                                 uint64_t);
typedef struct {
  mikrio_v1_span_t key;
  mikrio_v1_span_t upload_id;
  int64_t initiated_ms;
  mikrio_v1_span_t checksum_algorithm;
  mikrio_v1_span_t checksum_type;
} mikrio_v1_multipart_upload_summary_view_t;
mikrio_v1_multipart_upload_summary_view_t
mikrio_v1_list_multipart_uploads_output_item(
    const mikrio_v1_list_multipart_uploads_output_t *, uint64_t);
typedef struct {
  uint64_t parts_count;
  uint32_t is_truncated;
  uint32_t is_truncated_reserved;
  mikrio_v1_optional_u64_t next_part_number_marker;
  mikrio_v1_span_t checksum_algorithm;
  mikrio_v1_span_t checksum_type;
} mikrio_v1_list_parts_output_view_t;
mikrio_v1_list_parts_output_view_t
mikrio_v1_list_parts_output_view(const mikrio_v1_list_parts_output_t *);
void mikrio_v1_list_parts_output_release(mikrio_v1_list_parts_output_t *);
typedef struct {
  uint64_t uploads_count;
  uint64_t common_prefixes_count;
  uint32_t is_truncated;
  uint32_t is_truncated_reserved;
  mikrio_v1_span_t next_key_marker;
  mikrio_v1_span_t next_upload_id_marker;
} mikrio_v1_list_multipart_uploads_output_view_t;
mikrio_v1_list_multipart_uploads_output_view_t
mikrio_v1_list_multipart_uploads_output_view(
    const mikrio_v1_list_multipart_uploads_output_t *);
void mikrio_v1_list_multipart_uploads_output_release(
    mikrio_v1_list_multipart_uploads_output_t *);
mikrio_v1_span_t mikrio_v1_list_multipart_uploads_output_prefix(
    const mikrio_v1_list_multipart_uploads_output_t *, uint64_t);
/* The part writer uses ordinary write/abort/release, and this typed finish.
 * Calling either finish on the wrong writer kind fails without delivering EOF
 * or consuming the writer. Existing writer_finish retains its PUT result. */
uint32_t mikrio_v1_writer_finish_part(mikrio_v1_writer_t *,
                                      mikrio_v1_upload_part_output_t **,
                                      mikrio_v1_error_t **);

#ifdef __cplusplus
}
#endif
#endif
