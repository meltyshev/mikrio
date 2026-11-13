import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Store, StorageError, type CallOptions } from "@mikrio/node";

const POLICY =
  '{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}';

function failure(
  code: string,
  outcome: string | undefined,
): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, code);
    assert.equal(error.mutationOutcome, outcome);
    return true;
  };
}

function operations(
  store: Store,
  principalId: string,
  accessKeyId: string,
  options?: CallOptions,
): [string, () => Promise<unknown>, string | undefined][] {
  return [
    ["owner", () => store.getOwnerInfo(options), undefined],
    ["principals", () => store.listPrincipals({}, options), undefined],
    [
      "createPrincipal",
      () => store.createPrincipal({ name: "candidate" }, options),
      "NotCommitted",
    ],
    [
      "principal",
      () => store.getPrincipal({ principalId }, options),
      undefined,
    ],
    [
      "enable",
      () =>
        store.setPrincipalEnabled({ principalId, isEnabled: false }, options),
      "NotCommitted",
    ],
    [
      "deletePrincipal",
      () => store.deletePrincipal({ principalId }, options),
      "NotCommitted",
    ],
    [
      "putPolicy",
      () =>
        store.putPrincipalPolicy({ principalId, document: POLICY }, options),
      "NotCommitted",
    ],
    [
      "getPolicy",
      () => store.getPrincipalPolicy({ principalId }, options),
      undefined,
    ],
    [
      "deletePolicy",
      () => store.deletePrincipalPolicy({ principalId }, options),
      "NotCommitted",
    ],
    [
      "createKey",
      () => store.createAccessKey({ principalId }, options),
      "NotCommitted",
    ],
    ["keys", () => store.listAccessKeys({ principalId }, options), undefined],
    [
      "deleteKey",
      () => store.deleteAccessKey({ principalId, accessKeyId }, options),
      "NotCommitted",
    ],
  ];
}

await test("administration: every missing, closed and canceled operation preserves its typed outcome", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-admin-errors-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained administration failure: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });
  const principal = await store.createPrincipal({ name: "seed" });
  const principalId = principal.principalId;
  await store.putPrincipalPolicy({ principalId, document: POLICY });
  const key = await store.createAccessKey({ principalId });
  const accessKeyId = key.accessKey.accessKeyId;
  const controller = new AbortController();
  controller.abort();

  const canceled = operations(store, principalId, accessKeyId, {
    signal: controller.signal,
  });

  assert.equal(canceled.length, 12);

  for (const [name, operation, outcome] of canceled) {
    await assert.rejects(
      operation(),
      failure("OperationCanceled", outcome),
      name,
    );
  }

  assert.deepEqual(await store.getPrincipal({ principalId }), principal);

  assert.equal(
    (await store.getPrincipalPolicy({ principalId })).document,
    POLICY,
  );

  assert.deepEqual((await store.listAccessKeys({ principalId })).accessKeys, [
    key.accessKey,
  ]);

  assert.equal((await store.listPrincipals({})).principals.length, 1);
  await store.deleteAccessKey({ principalId, accessKeyId });
  await store.deletePrincipal({ principalId });
  const calls = operations(store, principalId, accessKeyId);

  for (const [name, operation, outcome] of calls.slice(3)) {
    await assert.rejects(
      operation(),
      failure("NoSuchPrincipal", outcome),
      name,
    );
  }

  await store.close();

  for (const [name, operation, outcome] of calls) {
    await assert.rejects(operation(), failure("StoreClosed", outcome), name);
  }

  await using reopened = await Store.open({ directory });
  assert.deepEqual((await reopened.listPrincipals({})).principals, []);
});

await test("administration: UTF-8 name bounds preserve case and normalization distinctions", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-admin-names-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained administration failure: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });
  const names = ["é".repeat(64), "App", "app", "é", "e\u0301"];
  const principals = [];

  for (const name of names) {
    principals.push(await store.createPrincipal({ name }));
  }

  assert.equal(
    new Set(principals.map((principal) => principal.principalId)).size,
    names.length,
  );

  assert.deepEqual(
    principals.map((principal) => principal.name),
    names,
  );

  for (const name of ["", "é".repeat(65), "a\x00b", "a\nb", "a\x7fb"]) {
    await assert.rejects(
      store.createPrincipal({ name }),
      failure("InvalidArgument", "NotCommitted"),
    );
  }

  const principal = principals[0];
  assert.ok(principal);

  await assert.rejects(
    store.setPrincipalEnabled({
      principalId: principal.principalId,
      // @ts-expect-error Numeric booleans are rejected at the host boundary.
      isEnabled: 1,
    }),
    TypeError,
  );

  await store.close();
  await using reopened = await Store.open({ directory });
  const inventory = await reopened.listPrincipals({});

  assert.deepEqual(
    new Set(inventory.principals.map((principal) => principal.name)),
    new Set(names),
  );

  assert.ok(inventory.principals.every((principal) => principal.isEnabled));
});
