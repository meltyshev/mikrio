#include "mikrio.h"
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

static int same(mikrio_v1_span_t left, mikrio_v1_span_t right) {
  return left.len == right.len &&
         (left.len == 0 || memcmp(left.data, right.data, left.len) == 0);
}

static void succeeded(uint32_t status, mikrio_v1_error_t **error) {
  if (status != 0) {
    mikrio_v1_error_view_t view = mikrio_v1_error_view(*error);

    fprintf(stderr, "native status %u: %.*s\n", status, (int)view.code.len,
            view.code.data);

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
  assert(view.mutation_outcome == outcome && view.reserved == 0);
  mikrio_v1_error_release(*error);
  *error = NULL;
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

static void close_store(mikrio_v1_store_t *store) {
  mikrio_v1_error_t *error = NULL;
  succeeded(mikrio_v1_store_close(store, &error), &error);
  mikrio_v1_store_release(store);
}

static void await_step(const char *expected) {
  char command[64];
  assert(fgets(command, sizeof(command), stdin) != NULL);
  size_t length = strlen(command);
  assert(length > 0 && command[length - 1] == '\n');
  command[length - 1] = '\0';
  assert(strcmp(command, expected) == 0);
}

static void completed(const char *step) {
  printf("DONE %s\n", step);
  assert(fflush(stdout) == 0);
}

static void serve_listener(const char *path, int is_second_listener) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_listen_options_t options = {0};
  options.struct_size = sizeof(options);

  options.credentials.access_key_id =
      text(is_second_listener ? "ffi-peer-second" : "ffi-peer-first");

  options.credentials.secret_access_key = text("synthetic-ffi-peer-secret");
  mikrio_v1_server_t *server = NULL;
  succeeded(mikrio_v1_store_listen(store, &options, &server, &error), &error);
  mikrio_v1_server_info_t *info = NULL;
  succeeded(mikrio_v1_server_info(server, &info, &error), &error);
  mikrio_v1_server_info_view_t view = mikrio_v1_server_info_view(info);
  assert(equals(view.region, "us-east-1"));
  printf("ENDPOINT %.*s\n", (int)view.endpoint.len, view.endpoint.data);
  assert(fflush(stdout) == 0);
  await_step("stop");
  succeeded(mikrio_v1_server_close(server, &error), &error);
  mikrio_v1_server_info_release(info);
  mikrio_v1_server_release(server);
  close_store(store);
  completed("stop");
}

static void put_policy_and_check_document(mikrio_v1_store_t *store,
                                          mikrio_v1_span_t principal_id,
                                          const char *document) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_put_principal_policy_input_t put = {0};
  put.struct_size = sizeof(put);
  put.principal_id = principal_id;
  put.document = text(document);
  succeeded(mikrio_v1_store_put_principal_policy(store, &put, &error), &error);
  mikrio_v1_get_principal_policy_input_t get = {0};
  get.struct_size = sizeof(get);
  get.principal_id = principal_id;
  mikrio_v1_get_principal_policy_output_t *output = NULL;

  succeeded(mikrio_v1_store_get_principal_policy(store, &get, &output, &error),
            &error);

  assert(equals(mikrio_v1_get_principal_policy_output_view(output).document,
                document));

  mikrio_v1_get_principal_policy_output_release(output);
}

static void set_enabled_and_check_principal(mikrio_v1_store_t *store,
                                            mikrio_v1_span_t principal_id,
                                            uint32_t value) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_set_principal_enabled_input_t input = {0};
  input.struct_size = sizeof(input);
  input.principal_id = principal_id;
  input.is_enabled = value;
  mikrio_v1_principal_info_t *output = NULL;

  succeeded(
      mikrio_v1_store_set_principal_enabled(store, &input, &output, &error),
      &error);

  assert(mikrio_v1_principal_info_view(output).is_enabled == value);

  assert(
      same(mikrio_v1_principal_info_view(output).principal_id, principal_id));

  mikrio_v1_principal_info_release(output);
}

static mikrio_v1_create_access_key_output_t *
issue_key(mikrio_v1_store_t *store, mikrio_v1_span_t principal_id) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_create_access_key_input_t input = {0};
  input.struct_size = sizeof(input);
  input.principal_id = principal_id;
  mikrio_v1_create_access_key_output_t *output = NULL;

  succeeded(mikrio_v1_store_create_access_key(store, &input, &output, &error),
            &error);

  mikrio_v1_create_access_key_output_view_t view =
      mikrio_v1_create_access_key_output_view(output);

  assert(same(view.access_key.principal_id, principal_id));
  assert(view.access_key.access_key_id.len == 40);
  assert(view.secret_access_key.len == 64);

  /* The controller consumes this one-time secret only in memory. */
  printf("KEY %.*s %.*s\n", (int)view.access_key.access_key_id.len,
         view.access_key.access_key_id.data, (int)view.secret_access_key.len,
         view.secret_access_key.data);

  assert(fflush(stdout) == 0);
  return output;
}

static void list_keys(mikrio_v1_store_t *store, mikrio_v1_span_t principal_id,
                      uint64_t count) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_list_access_keys_input_t input = {0};
  input.struct_size = sizeof(input);
  input.principal_id = principal_id;
  input.max_results = (mikrio_v1_optional_u64_t){1, 0, 2};
  mikrio_v1_list_access_keys_output_t *output = NULL;

  succeeded(mikrio_v1_store_list_access_keys(store, &input, &output, &error),
            &error);

  mikrio_v1_list_access_keys_output_view_t page =
      mikrio_v1_list_access_keys_output_view(output);

  assert(page.count == count && page.next_continuation_token.data == NULL);

  for (uint64_t index = 0; index < count; ++index) {
    mikrio_v1_access_key_info_view_t item =
        mikrio_v1_list_access_keys_output_item(output, index);

    assert(same(item.principal_id, principal_id));
    assert(item.access_key_id.len == 40 && item.created_at_ms > 0);
  }

  mikrio_v1_list_access_keys_output_release(output);
}

static void revoke_key(mikrio_v1_store_t *store, mikrio_v1_span_t principal_id,
                       mikrio_v1_create_access_key_output_t *key) {
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_delete_access_key_input_t input = {0};
  input.struct_size = sizeof(input);
  input.principal_id = principal_id;

  input.access_key_id =
      mikrio_v1_create_access_key_output_view(key).access_key.access_key_id;

  succeeded(mikrio_v1_store_delete_access_key(store, &input, &error), &error);
  succeeded(mikrio_v1_store_delete_access_key(store, &input, &error), &error);
}

static void administer(const char *path) {
  mikrio_v1_store_t *store = open_store(path);
  mikrio_v1_error_t *error = NULL;
  mikrio_v1_owner_info_t *owner = NULL;
  succeeded(mikrio_v1_store_get_owner_info(store, &owner, &error), &error);
  assert(mikrio_v1_owner_info_view(owner).account_id.len == 12);
  assert(mikrio_v1_owner_info_view(owner).canonical_id.len == 64);
  mikrio_v1_create_principal_input_t create = {0};
  create.struct_size = sizeof(create);
  create.name = text("c-live-peer-α");
  mikrio_v1_principal_info_t *principal = NULL;

  succeeded(
      mikrio_v1_store_create_principal(store, &create, &principal, &error),
      &error);

  mikrio_v1_principal_info_view_t identity =
      mikrio_v1_principal_info_view(principal);

  assert(equals(identity.name, "c-live-peer-α"));
  assert(identity.principal_id.len == 32 && identity.is_enabled == 1);
  mikrio_v1_list_principals_input_t list = {0};
  list.struct_size = sizeof(list);
  list.max_results = (mikrio_v1_optional_u64_t){1, 0, 1};
  mikrio_v1_list_principals_output_t *page = NULL;

  succeeded(mikrio_v1_store_list_principals(store, &list, &page, &error),
            &error);

  assert(mikrio_v1_list_principals_output_view(page).count == 1);

  assert(mikrio_v1_list_principals_output_view(page)
             .next_continuation_token.data == NULL);

  assert(same(mikrio_v1_list_principals_output_item(page, 0).principal_id,
              identity.principal_id));

  mikrio_v1_list_principals_output_release(page);

  mikrio_v1_create_access_key_output_t *first_key =
      issue_key(store, identity.principal_id);

  list_keys(store, identity.principal_id, 1);
  mikrio_v1_delete_principal_input_t remove = {0};
  remove.struct_size = sizeof(remove);
  remove.principal_id = identity.principal_id;

  failed(mikrio_v1_store_delete_principal(store, &remove, &error), &error,
         "PrincipalHasAccessKeys", 1);

  completed("created");

  const char *allow_get =
      "{\"Version\":\"2012-10-17\",\"Statement\":{\"Effect\":\"Allow\","
      "\"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::c-peer/*\"}}";

  const char *allow_put =
      "{\"Version\":\"2012-10-17\",\"Statement\":{\"Effect\":\"Allow\","
      "\"Action\":\"s3:PutObject\",\"Resource\":\"arn:aws:s3:::c-peer/*\"}}";

  await_step("allow-get");
  put_policy_and_check_document(store, identity.principal_id, allow_get);
  close_store(store);
  store = open_store(path);
  mikrio_v1_owner_info_t *reopened = NULL;
  succeeded(mikrio_v1_store_get_owner_info(store, &reopened, &error), &error);

  assert(same(mikrio_v1_owner_info_view(owner).account_id,
              mikrio_v1_owner_info_view(reopened).account_id));

  assert(same(mikrio_v1_owner_info_view(owner).canonical_id,
              mikrio_v1_owner_info_view(reopened).canonical_id));

  mikrio_v1_owner_info_release(reopened);
  mikrio_v1_get_principal_input_t get = {0};
  get.struct_size = sizeof(get);
  get.principal_id = identity.principal_id;
  mikrio_v1_principal_info_t *current = NULL;

  succeeded(mikrio_v1_store_get_principal(store, &get, &current, &error),
            &error);

  assert(same(mikrio_v1_principal_info_view(current).name, identity.name));

  assert(mikrio_v1_principal_info_view(current).created_at_ms ==
         identity.created_at_ms);

  mikrio_v1_principal_info_release(current);
  current = NULL;
  list_keys(store, identity.principal_id, 1);
  completed("allow-get");

  await_step("allow-put");
  put_policy_and_check_document(store, identity.principal_id, allow_put);
  completed("allow-put");
  await_step("delete-policy");
  mikrio_v1_delete_principal_policy_input_t clear = {0};
  clear.struct_size = sizeof(clear);
  clear.principal_id = identity.principal_id;

  succeeded(mikrio_v1_store_delete_principal_policy(store, &clear, &error),
            &error);

  succeeded(mikrio_v1_store_delete_principal_policy(store, &clear, &error),
            &error);

  mikrio_v1_get_principal_policy_input_t get_policy = {0};
  get_policy.struct_size = sizeof(get_policy);
  get_policy.principal_id = identity.principal_id;
  mikrio_v1_get_principal_policy_output_t *missing = NULL;

  failed(mikrio_v1_store_get_principal_policy(store, &get_policy, &missing,
                                              &error),
         &error, "NoSuchPrincipalPolicy", 0);

  assert(missing == NULL);
  completed("delete-policy");

  await_step("disable");
  put_policy_and_check_document(store, identity.principal_id, allow_get);
  set_enabled_and_check_principal(store, identity.principal_id, 0);
  completed("disable");
  await_step("issue-disabled-key");

  mikrio_v1_create_access_key_output_t *second_key =
      issue_key(store, identity.principal_id);

  list_keys(store, identity.principal_id, 2);
  completed("issue-disabled-key");
  await_step("enable");
  set_enabled_and_check_principal(store, identity.principal_id, 1);
  completed("enable");
  await_step("revoke-first");
  revoke_key(store, identity.principal_id, first_key);
  list_keys(store, identity.principal_id, 1);
  completed("revoke-first");
  await_step("disable-enable");
  set_enabled_and_check_principal(store, identity.principal_id, 0);
  set_enabled_and_check_principal(store, identity.principal_id, 1);
  completed("disable-enable");

  await_step("delete");

  failed(mikrio_v1_store_delete_principal(store, &remove, &error), &error,
         "PrincipalHasAccessKeys", 1);

  revoke_key(store, identity.principal_id, second_key);
  list_keys(store, identity.principal_id, 0);
  succeeded(mikrio_v1_store_delete_principal(store, &remove, &error), &error);
  close_store(store);
  store = open_store(path);

  failed(mikrio_v1_store_get_principal(store, &get, &current, &error), &error,
         "NoSuchPrincipal", 0);

  assert(current == NULL);
  completed("delete");
  await_step("recreate");

  succeeded(mikrio_v1_store_create_principal(store, &create, &current, &error),
            &error);

  assert(!same(mikrio_v1_principal_info_view(current).principal_id,
               identity.principal_id));

  mikrio_v1_span_t new_principal_id =
      mikrio_v1_principal_info_view(current).principal_id;

  mikrio_v1_create_access_key_output_t *third_key =
      issue_key(store, new_principal_id);

  completed("recreate");
  await_step("stop");
  revoke_key(store, new_principal_id, third_key);
  remove.principal_id = new_principal_id;
  succeeded(mikrio_v1_store_delete_principal(store, &remove, &error), &error);
  close_store(store);
  mikrio_v1_create_access_key_output_release(first_key);
  mikrio_v1_create_access_key_output_release(second_key);
  mikrio_v1_create_access_key_output_release(third_key);
  mikrio_v1_principal_info_release(principal);
  mikrio_v1_principal_info_release(current);
  mikrio_v1_owner_info_release(owner);
  completed("stop");
}

int main(int argc, char **argv) {
  assert(argc == 3);

  if (strcmp(argv[1], "admin") == 0) {
    administer(argv[2]);
    return 0;
  }

  assert(strcmp(argv[1], "listener-first") == 0 ||
         strcmp(argv[1], "listener-second") == 0);

  serve_listener(argv[2], strcmp(argv[1], "listener-second") == 0);
  return 0;
}
