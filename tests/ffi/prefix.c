#define _DEFAULT_SOURCE 1
#include "platform.h"
#include "prefix.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

#ifdef NDEBUG
#error "FFI fixtures require assertions, including calls with side effects"
#endif

_Static_assert(sizeof(mikrio_v1_open_options_t) == 24, "earlier input prefix");
_Static_assert(sizeof(mikrio_v1_info_view_t) == 56, "fixed by-value return");

int main(int argc, char **argv) {
  assert(argc == 2 && mikrio_v1_abi_version() == 1);
  mikrio_v1_open_options_t input = {0};
  input.struct_size = sizeof(input);

  input.directory =
      (mikrio_v1_span_t){(const uint8_t *)argv[1], strlen(argv[1])};

  mikrio_v1_store_t *store = NULL;
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_info_t *info = NULL;
  assert(mikrio_v1_store_open(&input, &store, &error) == 0);
  assert(store != NULL && error == NULL);
  assert(mikrio_v1_store_info(store, &info, &error) == 0);
  assert(info != NULL && error == NULL);
  assert(mikrio_v1_store_close(store, &error) == 0 && error == NULL);
  mikrio_v1_store_release(store);

  struct {
    uint64_t before;
    mikrio_v1_info_view_t view;
    uint64_t after;
  } observed = {.before = UINT64_MAX, .after = UINT64_MAX};

  observed.view = mikrio_v1_info_view(info);
  assert(observed.before == UINT64_MAX && observed.after == UINT64_MAX);
  assert(observed.view.region.len == 9);
  assert(memcmp(observed.view.region.data, "us-east-1", 9) == 0);

  assert(test_directory_matches(observed.view.directory.data,
                                observed.view.directory.len, argv[1]));

  mikrio_v1_info_release(info);
  mikrio_v1_error_release(error);
  puts("PASS separately_compiled_prefix_and_fixed_return");
  return 0;
}
