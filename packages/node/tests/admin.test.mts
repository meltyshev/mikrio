import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inspect } from "node:util";
import { test } from "node:test";
import { Store, StorageError } from "@mikrio/node";

const POLICY =
  '{ "Version":"2012-10-17", "Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}';

function failure(code: string, outcome?: string): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, code);
    assert.equal(error.mutationOutcome, outcome);
    return true;
  };
}

await test("administration: rotation, immutable inventory and reopen", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-admin-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });

  const owner = await store.getOwnerInfo();
  const principal = await store.createPrincipal({ name: "app" });
  const principalId = principal.principalId;
  assert.ok(principal.isEnabled);
  assert.ok(principal.createdAt instanceof Date);
  assert.ok(Object.isFrozen(principal));
  await store.putPrincipalPolicy({ principalId, document: POLICY });
  const storedPolicy = await store.getPrincipalPolicy({ principalId });
  assert.ok(!inspect(storedPolicy).includes("s3:GetObject"));
  assert.ok(!JSON.stringify(storedPolicy).includes("s3:GetObject"));

  assert.equal(
    (await store.getPrincipalPolicy({ principalId })).document,
    POLICY,
  );

  const key = await store.createAccessKey({ principalId });
  assert.equal(key.secretAccessKey.length, 64);
  assert.ok(!inspect(key).includes(key.secretAccessKey));
  assert.ok(!JSON.stringify(key).includes(key.secretAccessKey));
  const second = await store.createAccessKey({ principalId });
  assert.notEqual(key.accessKey.accessKeyId, second.accessKey.accessKeyId);
  const inventory = await store.listAccessKeys({ principalId });
  assert.equal(inventory.accessKeys.length, 2);
  assert.equal(inventory.nextContinuationToken, undefined);
  assert.ok(Object.isFrozen(inventory.accessKeys));
  assert.ok(inventory.accessKeys.every(Object.isFrozen));
  assert.ok(!JSON.stringify(inventory).includes(key.secretAccessKey));

  await assert.rejects(
    store.deletePrincipal({ principalId }),
    failure("PrincipalHasAccessKeys", "NotCommitted"),
  );

  const disabled = await store.setPrincipalEnabled({
    principalId,
    isEnabled: false,
  });

  assert.equal(disabled.isEnabled, false);
  assert.deepEqual(disabled.createdAt, principal.createdAt);
  await store.close();

  await assert.rejects(
    store.createAccessKey({ principalId }),
    failure("StoreClosed", "NotCommitted"),
  );

  {
    await using reopened = await Store.open({ directory });

    assert.deepEqual(await reopened.getOwnerInfo(), owner);
    assert.deepEqual(await reopened.getPrincipal({ principalId }), disabled);

    for (const accessKey of inventory.accessKeys) {
      await reopened.deleteAccessKey({
        principalId,
        accessKeyId: accessKey.accessKeyId,
      });
    }

    await reopened.deletePrincipalPolicy({ principalId });
    await reopened.deletePrincipalPolicy({ principalId });

    await assert.rejects(
      reopened.getPrincipalPolicy({ principalId }),
      failure("NoSuchPrincipalPolicy"),
    );

    await reopened.deletePrincipal({ principalId });

    await assert.rejects(
      reopened.getPrincipal({ principalId }),
      failure("NoSuchPrincipal"),
    );

    assert.notEqual(
      (await reopened.createPrincipal({ name: "app" })).principalId,
      principalId,
    );
  }
});

await test("administration: bounded scoped pages and canceled mutation", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-admin-pages-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });

  const principals = [];

  for (const name of ["a", "b", "c"]) {
    principals.push(await store.createPrincipal({ name }));
  }

  const first = await store.listPrincipals({ maxResults: 1 });
  assert.ok(first.nextContinuationToken);

  const next = await store.listPrincipals({
    maxResults: 2,
    continuationToken: first.nextContinuationToken,
  });

  assert.equal(next.nextContinuationToken, undefined);

  assert.deepEqual(
    [...first.principals, ...next.principals].map(
      (principal) => principal.principalId,
    ),
    principals.map((principal) => principal.principalId).sort(),
  );

  for (const maxResults of [0, 1001, -1, 1.5, NaN, Infinity, 2 ** 32 + 1]) {
    await assert.rejects(
      store.listPrincipals({ maxResults }),
      failure("InvalidArgument"),
    );
  }

  const controller = new AbortController();
  controller.abort();

  await assert.rejects(
    store.createPrincipal({ name: "canceled" }, { signal: controller.signal }),
    failure("OperationCanceled", "NotCommitted"),
  );

  assert.equal((await store.listPrincipals({})).principals.length, 3);

  await assert.rejects(
    store.createPrincipal({ name: "a" }),
    failure("PrincipalAlreadyExists", "NotCommitted"),
  );

  await assert.rejects(
    store.listPrincipals({ continuationToken: "x" }),
    failure("InvalidArgument"),
  );

  // Deliberately mistyped host input must reject the Promise.
  // @ts-expect-error null is not an accepted optional page bound.
  await assert.rejects(store.listPrincipals({ maxResults: null }), TypeError);
});
