import assert from "node:assert/strict";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { Store } from "mikrio";
import {
  DeleteBucketLifecycleCommand,
  GetBucketLifecycleConfigurationCommand,
  PutBucketLifecycleConfigurationCommand,
  S3Client,
  S3ServiceException,
} from "@aws-sdk/client-s3";
import { PostDemo, type PostForm } from "../src/post-demo.mjs";
import { S3Storage } from "../src/storage.mjs";
import { OWNER, OTHER, createFixture } from "./support.mjs";

async function upload(
  form: PostForm,
  value: string,
  origin: string,
): Promise<Response> {
  const data = new FormData();

  for (const [name, field] of Object.entries(form.fields)) {
    data.append(name, field);
  }

  data.append(
    "file",
    new Blob([value], { type: "ignored/file-type" }),
    "ignored.txt",
  );

  return fetch(form.url, {
    method: "POST",
    headers: { origin },
    body: data,
    redirect: "manual",
  });
}

await test("SDK POST forms publish reuse read back and clean late writes independently of attachments", async (t) => {
  const { options, resources } = await createFixture(t);
  const path = join(dirname(options.path), "store");
  const store = await Store.open({ directory: path });
  resources.defer(() => store.close());
  const bucket = "post-demonstration";
  const origin = "https://app.example";
  await store.createBucket({ bucket });

  await store.putBucketCors({
    bucket,
    corsRules: [
      {
        allowedOrigins: [origin],
        allowedMethods: ["POST", "GET"],
        allowedHeaders: ["content-type"],
        exposeHeaders: ["ETag", "Location"],
      },
    ],
  });

  const principal = await store.createPrincipal({ name: "post-backend" });

  await store.putPrincipalPolicy({
    principalId: principal.principalId,
    document: JSON.stringify({
      Version: "2012-10-17",
      Statement: {
        Effect: "Allow",
        Action: "s3:*",
        Resource: `arn:aws:s3:::${bucket}*`,
      },
    }),
  });

  const key = await store.createAccessKey({
    principalId: principal.principalId,
  });

  const listener = await store.listen({ host: "127.0.0.1", port: 0 });
  resources.defer(() => listener.close());

  const credentials = {
    accessKeyId: key.accessKey.accessKeyId,
    secretAccessKey: key.secretAccessKey,
  };

  const storage = new S3Storage({
    endpoint: listener.endpoint,
    region: listener.region,
    backend: credentials,
    uploader: credentials,
  });

  resources.defer(() => storage.close());
  const demo = new PostDemo(storage, bucket, "ephemeral/", origin, true);
  let lastKey = "";

  for (const status of ["200", "201", "204", "303"]) {
    const issued = await demo.issue(OWNER, { status });
    lastKey = issued.form.fields.key ?? "";
    assert.ok(lastKey);

    for (const payload of ["", "first", "replacement"]) {
      const response = await upload(issued.form, payload, origin);
      const body = await response.text();
      assert.equal(response.status, Number(status));
      assert.equal(response.headers.get("access-control-allow-origin"), origin);

      if (status === "201") {
        assert.match(body, /<PostResponse/);
        assert.match(body, /<ETag>/);
        assert.ok(body.includes(lastKey));
      } else {
        assert.equal(body, "");
      }

      if (status === "303") {
        const destination = new URL(response.headers.get("location") ?? "");

        assert.equal(
          destination.origin + destination.pathname,
          origin + "/post.html",
        );

        assert.equal(destination.searchParams.get("key"), lastKey);
      }

      const fetched = await fetch(await demo.download(OWNER, issued.id));
      assert.equal(fetched.status, 200);
      assert.equal(await fetched.text(), payload);
    }

    const other = await fetch(await demo.download(OTHER, issued.id));
    assert.equal(other.status, 404);
    await other.arrayBuffer();

    const tooLarge = await upload(
      issued.form,
      "x".repeat(issued.maxFileBytes + 1),
      origin,
    );

    assert.equal(tooLarge.status, 400);
    assert.match(await tooLarge.text(), /EntityTooLarge/);

    assert.equal(
      await (await fetch(await demo.download(OWNER, issued.id))).text(),
      "replacement",
    );

    await demo.delete(OWNER, issued.id);
    const late = await upload(issued.form, "late", origin);
    assert.equal(late.status, Number(status));
    await late.arrayBuffer();

    assert.deepEqual(await demo.cleanup(issued.cleanupAt), {
      deleted: 1,
      failed: 0,
    });
  }

  const survivor = await demo.issue(OWNER, {});
  const response = await upload(survivor.form, "durable", origin);
  assert.equal(response.status, 201);
  await response.arrayBuffer();
  await listener.close();
  await store.close();
  const reopened = await Store.open({ directory: path });
  resources.defer(() => reopened.close());

  const selected = await reopened.getObject({
    bucket,
    key: survivor.form.fields.key ?? "",
  });

  try {
    assert.equal(
      Buffer.from(await selected.body.readAll({ maxBytes: 100 })).toString(),
      "durable",
    );
  } finally {
    await selected.body.close();
  }

  assert.notEqual(lastKey, survivor.form.fields.key);
});

await test("SDK lifecycle configuration uses normal checksums and round trips both actions through the installed listener", async (t) => {
  const { options, resources } = await createFixture(t);

  const store = await Store.open({
    directory: join(dirname(options.path), "lifecycle-store"),
  });

  resources.defer(() => store.close());
  const bucket = "lifecycle-sdk";
  await store.createBucket({ bucket });
  const principal = await store.createPrincipal({ name: "lifecycle-backend" });

  await store.putPrincipalPolicy({
    principalId: principal.principalId,
    document: JSON.stringify({
      Version: "2012-10-17",
      Statement: {
        Effect: "Allow",
        Action: [
          "s3:PutLifecycleConfiguration",
          "s3:GetLifecycleConfiguration",
        ],
        Resource: `arn:aws:s3:::${bucket}`,
      },
    }),
  });

  const key = await store.createAccessKey({
    principalId: principal.principalId,
  });

  const listener = await store.listen({ host: "127.0.0.1", port: 0 });
  resources.defer(() => listener.close());

  const client = new S3Client({
    endpoint: listener.endpoint,
    region: listener.region,
    forcePathStyle: true,
    credentials: {
      accessKeyId: key.accessKey.accessKeyId,
      secretAccessKey: key.secretAccessKey,
    },
    maxAttempts: 1,
  });

  resources.defer(() => client.destroy());

  const rules = [
    {
      ID: "retention-😀",
      Status: "Enabled" as const,
      Filter: { Prefix: "ephemeral/" },
      Expiration: { Days: 3 },
      AbortIncompleteMultipartUpload: { DaysAfterInitiation: 1 },
    },
    {
      ID: "disabled",
      Status: "Disabled" as const,
      Filter: {},
      Expiration: { Days: 7 },
    },
  ];

  await client.send(
    new PutBucketLifecycleConfigurationCommand({
      Bucket: bucket,
      LifecycleConfiguration: { Rules: rules },
    }),
  );

  assert.deepEqual(
    (
      await client.send(
        new GetBucketLifecycleConfigurationCommand({ Bucket: bucket }),
      )
    ).Rules,
    rules,
  );

  const detached = await store.getBucketLifecycleConfiguration({
    bucket,
  });

  assert.equal(detached.lifecycleRules.length, 2);

  for (let i = 0; i < 2; i += 1) {
    await client.send(new DeleteBucketLifecycleCommand({ Bucket: bucket }));
  }

  await assert.rejects(
    client.send(new GetBucketLifecycleConfigurationCommand({ Bucket: bucket })),
    (error: unknown) =>
      error instanceof S3ServiceException &&
      error.name === "NoSuchLifecycleConfiguration",
  );

  assert.equal(
    detached.lifecycleRules[0]?.abortIncompleteMultipartUpload
      ?.daysAfterInitiation,
    1,
  );
});
