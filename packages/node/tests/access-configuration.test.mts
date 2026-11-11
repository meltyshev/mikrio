import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inspect } from "node:util";
import { test } from "node:test";
import {
  Store,
  StorageError,
  type PublicAccessBlockConfiguration,
} from "mikrio";

const PUBLIC =
  '{ "Version":"2012-10-17", "Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}';

function flags(bits: number): PublicAccessBlockConfiguration {
  return {
    blockPublicAcls: (bits & 1) !== 0,
    ignorePublicAcls: (bits & 2) !== 0,
    blockPublicPolicy: (bits & 4) !== 0,
    restrictPublicBuckets: (bits & 8) !== 0,
  };
}

function failure(code: string, outcome?: string): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, code);
    assert.equal(error.mutationOutcome, outcome);
    return true;
  };
}

await test("configuration: complete replacements, immutable ACLs and reopen", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-configuration-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });
  await using peer = await Store.open({ directory });

  await store.createBucket({ bucket: "photos" });
  const owner = await store.getOwnerInfo();

  assert.deepEqual(
    (await peer.getPublicAccessBlock({ bucket: "photos" })).configuration,
    flags(15),
  );

  await assert.rejects(
    store.putBucketPolicy({ bucket: "photos", document: PUBLIC }),
    failure("AccessDenied", "NotCommitted"),
  );

  await store.deletePublicAccessBlock({ bucket: "photos" });

  await store.putBucketPolicy({
    bucket: "photos",
    document: PUBLIC,
    expectedBucketOwner: owner.accountId,
  });

  const policy = await peer.getBucketPolicy({ bucket: "photos" });
  assert.equal(policy.document, PUBLIC);
  assert.ok(!inspect(policy).includes("s3:GetObject"));
  assert.ok(!JSON.stringify(policy).includes("s3:GetObject"));

  for (let bits = 0; bits < 16; bits++) {
    await store.putPublicAccessBlock({
      bucket: "photos",
      configuration: flags(bits),
    });

    const output = await peer.getPublicAccessBlock({ bucket: "photos" });
    assert.deepEqual(output.configuration, flags(bits));
    assert.ok(Object.isFrozen(output));
    assert.ok(Object.isFrozen(output.configuration));

    assert.ok(
      (await peer.getBucketPolicyStatus({ bucket: "photos" })).isPublic,
    );
  }

  await store.putBucketOwnershipControls({
    bucket: "photos",
    objectOwnership: "BucketOwnerEnforced",
  });

  assert.equal(
    (await peer.getBucketOwnershipControls({ bucket: "photos" }))
      .objectOwnership,
    "BucketOwnerEnforced",
  );

  await store.putObject({
    bucket: "photos",
    key: "object",
    body: new Uint8Array([1, 2]),
  });

  const acl = await peer.getObjectAcl({ bucket: "photos", key: "object" });
  assert.deepEqual(acl, await peer.getBucketAcl({ bucket: "photos" }));

  assert.deepEqual(acl, {
    owner: { canonicalId: owner.canonicalId },
    grants: [
      { granteeCanonicalId: owner.canonicalId, permission: "FULL_CONTROL" },
    ],
  });

  assert.ok(Object.isFrozen(acl));
  assert.ok(Object.isFrozen(acl.owner));
  assert.ok(Object.isFrozen(acl.grants));
  assert.ok(acl.grants.every(Object.isFrozen));
  await store.close();
  await peer.close();

  {
    await using reopened = await Store.open({ directory });

    assert.equal(
      (await reopened.getBucketPolicy({ bucket: "photos" })).document,
      policy.document,
    );

    assert.deepEqual(
      await reopened.getObjectAcl({ bucket: "photos", key: "object" }),
      acl,
    );

    assert.deepEqual(
      (await reopened.getPublicAccessBlock({ bucket: "photos" })).configuration,
      flags(15),
    );

    for (let repetition = 0; repetition < 2; repetition++) {
      await reopened.deleteBucketPolicy({ bucket: "photos" });
      await reopened.deletePublicAccessBlock({ bucket: "photos" });
    }

    await assert.rejects(
      reopened.getBucketPolicyStatus({ bucket: "photos" }),
      failure("NoSuchBucketPolicy"),
    );

    await assert.rejects(
      reopened.getPublicAccessBlock({ bucket: "photos" }),
      failure("NoSuchPublicAccessBlockConfiguration"),
    );

    await reopened.putPublicAccessBlock({
      bucket: "photos",
      configuration: flags(0),
    });

    assert.deepEqual(
      (await reopened.getPublicAccessBlock({ bucket: "photos" })).configuration,
      flags(0),
    );

    await reopened.deleteObject({ bucket: "photos", key: "object" });

    await assert.rejects(
      reopened.getObjectAcl({ bucket: "photos", key: "object" }),
      failure("NoSuchKey"),
    );
  }
});

await test("configuration: required booleans, input snapshots and canceled writes", async (context) => {
  const directory = await mkdtemp(
    join(tmpdir(), "mikrio-configuration-input-"),
  );

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });

  await store.createBucket({ bucket: "photos" });

  await assert.rejects(
    store.putPublicAccessBlock({
      bucket: "photos",
      // @ts-expect-error Every boolean is required by the public replacement contract.
      configuration: { blockPublicAcls: false },
    }),
    TypeError,
  );

  await assert.rejects(
    store.putPublicAccessBlock({
      bucket: "photos",
      // @ts-expect-error Native booleans cannot be coerced from numeric values.
      configuration: { ...flags(0), blockPublicAcls: 0 },
    }),
    TypeError,
  );

  await assert.rejects(
    store.putBucketPolicy({
      bucket: "photos",
      document: '{"Statement":[],"Statement":[]}',
    }),
    failure("InvalidArgument", "NotCommitted"),
  );

  await assert.rejects(
    store.putBucketOwnershipControls({
      bucket: "photos",
      // @ts-expect-error ACL-enabling ownership is outside the public model.
      objectOwnership: "ObjectWriter",
    }),
    failure("InvalidArgument", "NotCommitted"),
  );

  const configuration = { ...flags(0) };

  const pending = store.putPublicAccessBlock({
    bucket: "photos",
    configuration,
  });

  configuration.blockPublicPolicy = true;
  await pending;

  assert.deepEqual(
    (await store.getPublicAccessBlock({ bucket: "photos" })).configuration,
    flags(0),
  );

  const controller = new AbortController();
  controller.abort();

  await assert.rejects(
    store.putPublicAccessBlock(
      { bucket: "photos", configuration: flags(15) },
      { signal: controller.signal },
    ),
    failure("OperationCanceled", "NotCommitted"),
  );

  assert.deepEqual(
    (await store.getPublicAccessBlock({ bucket: "photos" })).configuration,
    flags(0),
  );

  await store.close();

  await assert.rejects(
    store.putPublicAccessBlock({
      bucket: "photos",
      configuration: flags(15),
    }),
    failure("StoreClosed", "NotCommitted"),
  );
});
