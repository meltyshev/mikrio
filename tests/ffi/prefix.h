/* Synthetic earlier consumer contract, not a released mikrio header.
 * This fixture deliberately does not include the current canonical header.
 * It knows only the directory prefix and the established by-value info layout.
 * Keep it unchanged when adding optional fields to the production input. */
#ifndef MIKRIO_TEST_PREFIX_H
#define MIKRIO_TEST_PREFIX_H

#include <stdint.h>

typedef struct mikrio_v1_store mikrio_v1_store_t;
typedef struct mikrio_v1_info mikrio_v1_info_t;
typedef struct mikrio_v1_error mikrio_v1_error_t;

typedef struct {
  const uint8_t *data;
  uint64_t len;
} mikrio_v1_span_t;

typedef struct {
  uint64_t struct_size;
  mikrio_v1_span_t directory;
} mikrio_v1_open_options_t;

typedef struct {
  mikrio_v1_span_t store_id;
  uint32_t format_version;
  uint32_t reserved;
  mikrio_v1_span_t directory;
  mikrio_v1_span_t region;
} mikrio_v1_info_view_t;

uint32_t mikrio_v1_abi_version(void);
uint32_t mikrio_v1_store_open(const mikrio_v1_open_options_t *,
                              mikrio_v1_store_t **, mikrio_v1_error_t **);
uint32_t mikrio_v1_store_close(const mikrio_v1_store_t *, mikrio_v1_error_t **);
void mikrio_v1_store_release(mikrio_v1_store_t *);
uint32_t mikrio_v1_store_info(const mikrio_v1_store_t *, mikrio_v1_info_t **,
                              mikrio_v1_error_t **);
mikrio_v1_info_view_t mikrio_v1_info_view(const mikrio_v1_info_t *);
void mikrio_v1_info_release(mikrio_v1_info_t *);
void mikrio_v1_error_release(mikrio_v1_error_t *);

#endif
