import assert from "node:assert/strict";
import test from "node:test";
import {
  CreateBucketCommand,
  DeleteBucketCommand,
  DeleteBucketCorsCommand,
  DeleteBucketPolicyCommand,
  DeleteObjectCommand,
  DeleteObjectsCommand,
  DeletePublicAccessBlockCommand,
  GetBucketAclCommand,
  GetBucketCorsCommand,
  GetBucketLocationCommand,
  GetBucketOwnershipControlsCommand,
  GetBucketPolicyCommand,
  GetBucketPolicyStatusCommand,
  GetObjectAclCommand,
  GetObjectCommand,
  GetPublicAccessBlockCommand,
  HeadBucketCommand,
  HeadObjectCommand,
  ListObjectsV2Command,
  ListBucketsCommand,
  paginateListBuckets,
  paginateListObjectsV2,
  PutBucketOwnershipControlsCommand,
  PutBucketCorsCommand,
  PutBucketPolicyCommand,
  PutObjectCommand,
  PutPublicAccessBlockCommand,
  S3ServiceException,
} from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import { client } from "./http-client.mjs";
import "./http-storage.test.mjs";

await test("batch deletion uses SDK digests duplicate conditions and quiet results", async (context) => {
  const first = client();
  const endpoint = process.env["mikrio_fixture_peer"];
  assert.ok(endpoint);
  const peer = client(endpoint);
  const target = { Bucket: "photos", Key: "node-batch" };

  context.after(async () => {
    try {
      await first.send(new DeleteObjectCommand(target));
    } finally {
      first.destroy();
      peer.destroy();
    }
  });

  const put = await first.send(
    new PutObjectCommand({ ...target, Body: "payload" }),
  );

  assert.ok(put.ETag);

  const output = await peer.send(
    new DeleteObjectsCommand({
      Bucket: target.Bucket,
      Delete: {
        Objects: [
          { Key: target.Key, ETag: '"wrong"' },
          { Key: target.Key, ETag: put.ETag },
          { Key: target.Key, ETag: put.ETag },
          { Key: target.Key },
        ],
      },
    }),
  );

  assert.equal(output.$metadata.httpStatusCode, 200);

  assert.deepEqual(
    output.Deleted?.map((object) => object.Key),
    [target.Key, target.Key],
  );

  assert.deepEqual(
    output.Errors?.map((error) => error.Code),
    ["PreconditionFailed", "NoSuchKey"],
  );

  const quiet = await peer.send(
    new DeleteObjectsCommand({
      Bucket: target.Bucket,
      Delete: {
        Quiet: true,
        Objects: [{ Key: target.Key }, { Key: target.Key, ETag: "*" }],
      },
    }),
  );

  assert.equal(quiet.Deleted, undefined);
  assert.equal(quiet.Errors?.[0]?.Code, "NoSuchKey");

  await assert.rejects(
    first.send(new HeadObjectCommand(target)),
    (error: unknown) =>
      error instanceof S3ServiceException &&
      error.$metadata.httpStatusCode === 404,
  );
});

await test("bucket paginator preserves account listing and paginated region fields", async (context) => {
  const first = client();
  const endpoint = process.env["mikrio_fixture_peer"];
  assert.ok(endpoint);
  const peer = client(endpoint);
  const created: string[] = [];

  context.after(async () => {
    const failures: unknown[] = [];

    for (const bucket of created) {
      try {
        await first.send(new DeleteBucketCommand({ Bucket: bucket }));
      } catch (error) {
        failures.push(error);
      }
    }

    first.destroy();
    peer.destroy();

    if (failures.length) {
      throw new AggregateError(failures, "bucket fixture cleanup failed");
    }
  });

  for (const bucket of ["listener-node", "listener-python"]) {
    await first.send(new CreateBucketCommand({ Bucket: bucket }));
    created.push(bucket);
  }

  const names: string[] = [];
  let pageCount = 0;

  for await (const page of paginateListBuckets(
    { client: peer, pageSize: 1 },
    { Prefix: "listener-" },
  )) {
    pageCount += 1;
    assert.ok(pageCount <= 2);
    assert.equal(page.Buckets?.length, 1);
    const bucket = page.Buckets[0];
    assert.ok(bucket?.Name);
    names.push(bucket.Name);
    assert.ok(bucket.CreationDate instanceof Date);
    assert.equal(bucket.BucketRegion, "us-east-1");
    assert.equal(page.Prefix, "listener-");
    assert.equal(page.Owner?.ID?.length, 64);
    assert.equal(page.Owner?.DisplayName, undefined);
    assert.equal(Boolean(page.ContinuationToken), pageCount === 1);
  }

  assert.deepEqual(names, ["listener-node", "listener-python"]);
  const all = await peer.send(new ListBucketsCommand({}));
  assert.equal(all.ContinuationToken, undefined);
  assert.ok(all.Buckets);
  assert.ok(all.Buckets.some((bucket) => bucket.Name === "photos"));
  assert.ok(all.Buckets.every((bucket) => bucket.BucketRegion === undefined));
});

await test("range reads and all response overrides agree across public listeners", async (context) => {
  const first = client();
  const endpoint = process.env["mikrio_fixture_peer"];
  assert.ok(endpoint);
  const peer = client(endpoint);
  const target = { Bucket: "photos", Key: "node-range" };

  context.after(async () => {
    try {
      await first.send(new DeleteObjectCommand(target));
    } finally {
      first.destroy();
      peer.destroy();
    }
  });

  await first.send(
    new PutObjectCommand({
      ...target,
      Body: "testcontent",
      ContentType: "text/plain",
    }),
  );

  const input = {
    ...target,
    Range: "bytes=4-7",
    ChecksumMode: "ENABLED" as const,
    ResponseContentType: "application/pdf",
    ResponseContentEncoding: "identity",
    ResponseContentDisposition: "attachment",
    ResponseContentLanguage: "",
    ResponseCacheControl: "no-store",
    ResponseExpires: new Date("2037-10-21T07:28:00Z"),
  };

  const get = await peer.send(new GetObjectCommand(input));
  assert.equal(await get.Body?.transformToString(), "cont");
  const head = await peer.send(new HeadObjectCommand(input));

  for (const output of [get, head]) {
    assert.equal(output.ContentLength, 4);
    assert.equal(output.ContentRange, "bytes 4-7/11");
    assert.equal(output.ContentType, "application/pdf");
    assert.equal(output.ContentEncoding, "identity");
    assert.equal(output.ContentDisposition, "attachment");
    assert.equal(output.ContentLanguage, "");
    assert.equal(output.CacheControl, "no-store");
    assert.deepEqual(output.Expires, input.ResponseExpires);
    assert.equal(output.ChecksumCRC32, undefined);
    assert.equal(output.ChecksumType, undefined);
  }

  const original = await peer.send(new HeadObjectCommand(target));
  assert.equal(original.ContentType, "text/plain");
  assert.equal(original.ContentLength, 11);
});

await test("listing paginator traverses prefix-only pages and literal keys across listeners", async (context) => {
  const first = client();
  const peerEndpoint = process.env["mikrio_fixture_peer"];
  assert.ok(peerEndpoint);
  const peer = client(peerEndpoint);
  const target = { Bucket: "listener-node" };
  const published: string[] = [];
  let created = false;

  context.after(async () => {
    const failures: unknown[] = [];

    for (const cleanup of [
      ...published.map((key) => async () => {
        await first.send(new DeleteObjectCommand({ ...target, Key: key }));
      }),
      async () => {
        if (created) {
          await first.send(new DeleteBucketCommand(target));
        }
      },
    ]) {
      try {
        await cleanup();
      } catch (error: unknown) {
        failures.push(error);
      }
    }

    first.destroy();
    peer.destroy();

    if (failures.length) {
      throw new AggregateError(failures, "listing SDK cleanup failed");
    }
  });

  await first.send(new CreateBucketCommand(target));
  created = true;

  for (const key of ["a/one", "a/two", "b +雪", "c"]) {
    await first.send(
      new PutObjectCommand({ ...target, Key: key, Body: "abc" }),
    );

    published.push(key);
  }

  const pages = [];

  for await (const page of paginateListObjectsV2(
    { client: peer, pageSize: 1 },
    { ...target, Delimiter: "/" },
  )) {
    pages.push(page);
  }

  assert.equal(pages.length, 3);
  const [prefix, middle, last] = pages;
  assert.ok(prefix && middle && last);
  assert.deepEqual(prefix.Contents ?? [], []);
  assert.deepEqual(prefix.CommonPrefixes, [{ Prefix: "a/" }]);
  assert.equal(prefix.IsTruncated, true);
  assert.equal(middle.Contents?.[0]?.Key, "b +雪");
  assert.equal(middle.Contents?.[0]?.Size, 3);

  assert.equal(
    middle.Contents?.[0]?.ETag,
    '"900150983cd24fb0d6963f7d28e17f72"',
  );

  assert.equal(last.Contents?.[0]?.Key, "c");
  assert.equal(last.IsTruncated, false);
  assert.equal(last.NextContinuationToken, undefined);
  assert.ok(pages.every((page) => page.KeyCount === 1));

  await assert.rejects(
    peer.send(
      new ListObjectsV2Command({
        ...target,
        ContinuationToken: prefix.NextContinuationToken,
      }),
    ),
    (error: unknown) =>
      error instanceof S3ServiceException && error.name === "InvalidArgument",
  );
});

await test("CORS round trip and preflight preserve private presigned access across listeners", async (context) => {
  const first = client();
  const endpoint = process.env["mikrio_fixture_endpoint"];
  const peerEndpoint = process.env["mikrio_fixture_peer"];
  assert.ok(endpoint);
  assert.ok(peerEndpoint);
  const peer = client(peerEndpoint);
  const target = { Bucket: "listener-node" };
  const objectTarget = { ...target, Key: "cors" };
  let created = false;
  let published = false;

  context.after(async () => {
    const failures: unknown[] = [];

    for (const cleanup of [
      async () => {
        if (published) {
          await first.send(new DeleteObjectCommand(objectTarget));
        }
      },
      async () => {
        if (created) {
          await first.send(new DeleteBucketCommand(target));
        }
      },
    ]) {
      try {
        await cleanup();
      } catch (error: unknown) {
        failures.push(error);
      }
    }

    first.destroy();
    peer.destroy();

    if (failures.length) {
      throw new AggregateError(failures, "CORS SDK cleanup failed");
    }
  });

  await first.send(new CreateBucketCommand(target));
  created = true;

  const rules = [
    {
      ID: "first",
      AllowedOrigins: ["https://app.example"],
      AllowedMethods: ["PUT"],
      AllowedHeaders: ["x-first"],
      ExposeHeaders: ["x-first"],
    },
    {
      ID: "browser-😀",
      AllowedOrigins: ["https://app.example"],
      AllowedMethods: ["PUT", "GET", "HEAD"],
      AllowedHeaders: ["content-type"],
      ExposeHeaders: ["ETag"],
      MaxAgeSeconds: -(2 ** 31),
    },
  ];

  await first.send(
    new PutBucketCorsCommand({
      ...target,
      CORSConfiguration: { CORSRules: rules },
    }),
  );

  const configuration = await peer.send(new GetBucketCorsCommand(target));
  assert.deepEqual(configuration.CORSRules, rules);

  await first.send(
    new PutObjectCommand({ ...objectTarget, Body: "private-cors" }),
  );

  published = true;

  for (const base of [endpoint, peerEndpoint]) {
    const url = `${base}/${target.Bucket}/cors`;

    const preflight = await fetch(url, {
      method: "OPTIONS",
      headers: {
        Origin: "https://app.example",
        "Access-Control-Request-Method": "PUT",
        "Access-Control-Request-Headers": "Content-Type",
      },
      signal: AbortSignal.timeout(5000),
    });

    const preflightBody = await preflight.text();
    assert.equal(preflight.status, 200);
    assert.equal(preflightBody, "");

    assert.equal(
      preflight.headers.get("access-control-allow-origin"),
      "https://app.example",
    );

    assert.equal(
      preflight.headers.get("access-control-expose-headers"),
      "ETag",
    );

    assert.equal(
      preflight.headers.get("access-control-max-age"),
      String(-(2 ** 31)),
    );

    const denied = await fetch(url, {
      headers: { Origin: "https://app.example" },
      signal: AbortSignal.timeout(5000),
    });

    const deniedBody = await denied.text();
    assert.equal(denied.status, 403);

    assert.equal(
      denied.headers.get("access-control-allow-origin"),
      "https://app.example",
    );

    assert.match(deniedBody, /<Code>AccessDenied<\/Code>/u);
  }

  const signed = await getSignedUrl(peer, new GetObjectCommand(objectTarget), {
    expiresIn: 60,
  });

  const selected = await fetch(signed, {
    headers: { Origin: "https://app.example" },
    signal: AbortSignal.timeout(5000),
  });

  const selectedBody = await selected.text();
  assert.equal(selected.status, 200);
  assert.equal(selectedBody, "private-cors");
  assert.equal(selected.headers.get("access-control-expose-headers"), "ETag");

  for (let attempt = 0; attempt < 2; attempt++) {
    await first.send(new DeleteBucketCorsCommand(target));
  }

  await assert.rejects(
    peer.send(new GetBucketCorsCommand(target)),
    (error: unknown) =>
      error instanceof S3ServiceException &&
      error.name === "NoSuchCORSConfiguration",
  );

  const after = await fetch(signed, {
    headers: { Origin: "https://app.example" },
    signal: AbortSignal.timeout(5000),
  });

  const afterBody = await after.text();
  assert.equal(after.status, 200);
  assert.equal(afterBody, "private-cors");
  assert.equal(after.headers.get("access-control-allow-origin"), null);
});

await test("configuration and anonymous restriction agree across live listeners", async (context) => {
  const first = client();
  const endpoint = process.env["mikrio_fixture_endpoint"];
  const peerEndpoint = process.env["mikrio_fixture_peer"];
  assert.ok(endpoint);
  assert.ok(peerEndpoint);
  const peer = client(peerEndpoint);
  const target = { Bucket: "listener-node" };
  const objectTarget = { ...target, Key: "public" };
  let created = false;
  let published = false;

  context.after(async () => {
    const failures: unknown[] = [];

    for (const cleanup of [
      async () => {
        if (published) {
          await first.send(new DeleteObjectCommand(objectTarget));
        }
      },
      async () => {
        if (created) {
          await first.send(new DeleteBucketCommand(target));
        }
      },
    ]) {
      try {
        await cleanup();
      } catch (error: unknown) {
        failures.push(error);
      }
    }

    first.destroy();
    peer.destroy();

    if (failures.length) {
      throw new AggregateError(failures, "listener SDK cleanup failed");
    }
  });

  await first.send(new CreateBucketCommand(target));
  created = true;

  const flags = {
    BlockPublicAcls: false,
    IgnorePublicAcls: false,
    BlockPublicPolicy: false,
    RestrictPublicBuckets: false,
  };

  await first.send(
    new PutPublicAccessBlockCommand({
      ...target,
      PublicAccessBlockConfiguration: flags,
    }),
  );

  const block = await peer.send(new GetPublicAccessBlockCommand(target));
  assert.deepEqual(block.PublicAccessBlockConfiguration, flags);

  const document = JSON.stringify({
    Version: "2012-10-17",
    Statement: {
      Effect: "Allow",
      Principal: "*",
      Action: "s3:GetObject",
      Resource: `arn:aws:s3:::${target.Bucket}/*`,
    },
  });

  await first.send(new PutBucketPolicyCommand({ ...target, Policy: document }));

  const policy = await peer.send(new GetBucketPolicyCommand(target));
  assert.equal(policy.Policy, document);

  const ownership = {
    Rules: [{ ObjectOwnership: "BucketOwnerEnforced" as const }],
  };

  await first.send(
    new PutBucketOwnershipControlsCommand({
      ...target,
      OwnershipControls: ownership,
    }),
  );

  const controls = await peer.send(
    new GetBucketOwnershipControlsCommand(target),
  );

  assert.deepEqual(controls.OwnershipControls, ownership);

  await first.send(new PutObjectCommand({ ...objectTarget, Body: "visible" }));

  published = true;
  const bucketAcl = await peer.send(new GetBucketAclCommand(target));
  const objectAcl = await peer.send(new GetObjectAclCommand(objectTarget));
  assert.ok(bucketAcl.Owner?.ID);
  assert.deepEqual(objectAcl.Owner, bucketAcl.Owner);

  assert.deepEqual(objectAcl.Grants, [
    {
      Grantee: { ID: bucketAcl.Owner.ID, Type: "CanonicalUser" },
      Permission: "FULL_CONTROL",
    },
  ]);

  const urls = [endpoint, peerEndpoint].map(
    (base) => `${base}/${target.Bucket}/public`,
  );

  for (const url of urls) {
    const response = await fetch(url, { signal: AbortSignal.timeout(5000) });
    const body = await response.text();
    assert.equal(response.status, 200);
    assert.equal(body, "visible");
  }

  await first.send(
    new PutPublicAccessBlockCommand({
      ...target,
      PublicAccessBlockConfiguration: {
        ...flags,
        RestrictPublicBuckets: true,
      },
    }),
  );

  for (const url of urls) {
    const response = await fetch(url, { signal: AbortSignal.timeout(5000) });
    const body = await response.text();
    assert.equal(response.status, 403);
    assert.match(body, /<Code>AccessDenied<\/Code>/u);
  }

  const classification = await peer.send(
    new GetBucketPolicyStatusCommand(target),
  );

  assert.equal(classification.PolicyStatus?.IsPublic, true);

  for (let attempt = 0; attempt < 2; attempt++) {
    await first.send(new DeleteBucketPolicyCommand(target));
    await first.send(new DeletePublicAccessBlockCommand(target));
  }

  await assert.rejects(
    peer.send(new GetBucketPolicyCommand(target)),
    (error: unknown) =>
      error instanceof S3ServiceException &&
      error.name === "NoSuchBucketPolicy",
  );

  await assert.rejects(
    peer.send(new GetPublicAccessBlockCommand(target)),
    (error: unknown) =>
      error instanceof S3ServiceException &&
      error.name === "NoSuchPublicAccessBlockConfiguration",
  );
});

await test("bucket routes and scoped denials agree across public listeners", async (context) => {
  const first = client();
  const peerEndpoint = process.env["mikrio_fixture_peer"];
  assert.ok(peerEndpoint);
  const peer = client(peerEndpoint);
  const target = { Bucket: "listener-node" };
  let created = false;

  context.after(async () => {
    try {
      if (created) {
        await first.send(new DeleteBucketCommand(target));
      }
    } finally {
      first.destroy();
      peer.destroy();
    }
  });

  await first.send(new CreateBucketCommand(target));
  created = true;
  await peer.send(new HeadBucketCommand(target));
  const location = await peer.send(new GetBucketLocationCommand(target));
  assert.equal(location.LocationConstraint, undefined);

  const object = await peer.send(
    new GetObjectCommand({ Bucket: "photos", Key: "native-seed" }),
  );

  assert.ok(object.Body);
  assert.equal(await object.Body.transformToString(), "native-to-sdk");

  for (const s3 of [first, peer]) {
    await assert.rejects(
      s3.send(
        new PutObjectCommand({
          Bucket: "photos",
          Key: "forbidden",
          Body: "denied",
        }),
      ),
      (error: unknown) =>
        error instanceof S3ServiceException && error.name === "AccessDenied",
    );
  }
});
