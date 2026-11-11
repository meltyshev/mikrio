/* A synthetic future input, compiled against the current header. The original
 * library must reject its nonzero extension; the isolated future library must
 * accept it and apply the new default region. Neither is a released ABI. */
#include "mikrio.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

#ifdef NDEBUG
#error "FFI fixtures require assertions, including calls with side effects"
#endif

int main(int argc, char **argv) {
  assert(argc == 3);
  int future = strcmp(argv[2], "future") == 0;
  assert(future || strcmp(argv[2], "current") == 0);

  struct {
    mikrio_v1_open_options_t known;
    uint64_t future_region;
  } input = {{0}, 1};

  _Static_assert(sizeof(input) == 64, "synthetic future layout");
  input.known.struct_size = sizeof(input);

  input.known.directory =
      (mikrio_v1_span_t){(const uint8_t *)argv[1], strlen(argv[1])};

  mikrio_v1_store_t *store = NULL;
  mikrio_v1_error_t *error = NULL;
  uint32_t status = mikrio_v1_store_open(&input.known, &store, &error);

  if (!future) {
    assert(status == 1 && store == NULL && error != NULL);
    mikrio_v1_error_view_t failure = mikrio_v1_error_view(error);
    assert(failure.code.len == 15);
    assert(memcmp(failure.code.data, "InvalidArgument", 15) == 0);
    mikrio_v1_error_release(error);
    puts("PASS newer_nonzero_option_rejected_by_current_library");
    return 0;
  }

  assert(status == 0 && store != NULL && error == NULL);
  mikrio_v1_info_t *info = NULL;
  assert(mikrio_v1_store_info(store, &info, &error) == 0 && error == NULL);
  assert(mikrio_v1_store_close(store, &error) == 0 && error == NULL);
  mikrio_v1_store_release(store);
  mikrio_v1_info_view_t view = mikrio_v1_info_view(info);
  assert(view.region.len == 9);
  assert(memcmp(view.region.data, "eu-west-1", 9) == 0);
  mikrio_v1_info_release(info);
  puts("PASS newer_option_applied_by_future_library");
  return 0;
}
