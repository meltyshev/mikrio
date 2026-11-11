// Controlled transport observations are client facts, not service qualification.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { crc32 } from "node:zlib";
import test from "node:test";
import {
  CompleteMultipartUploadCommand,
  CopyObjectCommand,
  CreateMultipartUploadCommand,
  PutObjectCommand,
  PutObjectTaggingCommand,
  S3Client,
  UploadPartCommand,
  UploadPartCopyCommand,
} from "@aws-sdk/client-s3";
import { Upload } from "@aws-sdk/lib-storage";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";

const HEADER_NAMES = new Set([
  "content-md5",
  "content-type",
  "x-amz-checksum-algorithm",
  "x-amz-checksum-type",
  "x-amz-sdk-checksum-algorithm",
  "x-amz-checksum-crc32",
  "x-amz-checksum-crc32c",
  "x-amz-checksum-crc64nvme",
  "x-amz-checksum-sha1",
  "x-amz-checksum-sha256",
  "x-amz-copy-source",
  "x-amz-copy-source-range",
  "x-amz-metadata-directive",
  "x-amz-tagging-directive",
]);

interface Exchange {
  operation: string;
  workflow: string;
  method: string;
  headers: Record<string, string>;
  bodyBytes: number;
  bodySha256: string;
}

await test("presigning hoists exact inline tags and XML integrity assertions", async () => {
  const client = new S3Client({
    endpoint: "http://127.0.0.1:1",
    region: "us-east-1",
    forcePathStyle: true,
    credentials: {
      accessKeyId: "synthetic-key",
      secretAccessKey: "synthetic-secret",
    },
  });

  try {
    const inline = new URL(
      await getSignedUrl(
        client,
        new PutObjectCommand({
          Bucket: "photos",
          Key: "object",
          Body: "123456789",
          Tagging: "Name=a%2Bb+space",
          ExpectedBucketOwner: "123456789012",
        }),
      ),
    );

    assert.equal(inline.searchParams.get("x-amz-tagging"), "Name=a%2Bb+space");
    assert.equal(inline.searchParams.get("x-amz-checksum-crc32"), "y/Q5Jg==");

    assert.equal(
      inline.searchParams.get("x-amz-sdk-checksum-algorithm"),
      "CRC32",
    );

    assert.equal(
      inline.searchParams.get("x-amz-expected-bucket-owner"),
      "123456789012",
    );

    let xml: string | undefined;

    client.middlewareStack.add(
      (next) => async (args) => {
        const request = args.request;

        assert.ok(
          typeof request === "object" && request !== null && "body" in request,
        );

        assert.ok(typeof request.body === "string");
        xml = request.body;
        return next(args);
      },
      { step: "build", name: "captureTaggingBody" },
    );

    const dedicated = new URL(
      await getSignedUrl(
        client,
        new PutObjectTaggingCommand({
          Bucket: "photos",
          Key: "object",
          Tagging: { TagSet: [] },
          ChecksumAlgorithm: "CRC32",
        }),
      ),
    );

    assert.equal(dedicated.searchParams.get("tagging"), "");
    assert.ok(xml);
    const checksum = Buffer.alloc(4);
    checksum.writeUInt32BE(crc32(xml));

    assert.equal(
      dedicated.searchParams.get("x-amz-checksum-crc32"),
      checksum.toString("base64"),
    );

    assert.equal(
      dedicated.searchParams.get("x-amz-sdk-checksum-algorithm"),
      "CRC32",
    );
  } finally {
    client.destroy();
  }
});

await test("default and explicit checksum middleware reaches the owned transport", async () => {
  const exchanges: Exchange[] = [];
  let operation = "";
  let workflow = "low-level";

  const responses: Record<string, string> = {
    CreateMultipartUpload:
      "<InitiateMultipartUploadResult><Bucket>bucket</Bucket><Key>key</Key><UploadId>owned-upload</UploadId></InitiateMultipartUploadResult>",
    CopyObject:
      '<CopyObjectResult><ETag>"fixture"</ETag><LastModified>2026-10-01T00:00:00Z</LastModified></CopyObjectResult>',
    UploadPartCopy:
      '<CopyPartResult><ETag>"fixture"</ETag><LastModified>2026-10-01T00:00:00Z</LastModified></CopyPartResult>',
    CompleteMultipartUpload:
      '<CompleteMultipartUploadResult><Bucket>bucket</Bucket><Key>key</Key><ETag>"fixture-1"</ETag></CompleteMultipartUploadResult>',
  };

  const client = new S3Client({
    endpoint: "http://127.0.0.1:1",
    region: "us-east-1",
    forcePathStyle: true,
    maxAttempts: 1,
    credentials: {
      accessKeyId: "synthetic-key",
      secretAccessKey: "synthetic-secret",
    },
    requestHandler: {
      handle(request: {
        protocol: string;
        hostname: string;
        port?: number;
        method: string;
        headers: Record<string, string>;
        body?: unknown;
      }) {
        assert.equal(request.protocol, "http:");
        assert.equal(request.hostname, "127.0.0.1");
        assert.equal(request.port, 1);
        const input = request.body ?? "";
        assert.ok(typeof input === "string" || input instanceof Uint8Array);
        const body = Buffer.from(input);
        assert.ok(body.length <= 6 * 1024 * 1024);

        const isManagedCompletion =
          operation === "CompleteMultipartUpload" &&
          workflow === "managed-upload";

        if (isManagedCompletion) {
          assert.ok(body.includes("<ChecksumCRC32>yTuzdQ==</ChecksumCRC32>"));
          assert.ok(body.includes("<ChecksumCRC32>y/Q5Jg==</ChecksumCRC32>"));
        }

        exchanges.push({
          operation,
          workflow,
          method: request.method,
          headers: Object.fromEntries(
            Object.entries(request.headers).filter(([name]) =>
              HEADER_NAMES.has(name.toLowerCase()),
            ),
          ),
          bodyBytes: body.length,
          bodySha256: createHash("sha256").update(body).digest("hex"),
        });

        const responseHeaders: Record<string, string> = {
          "content-type": "application/xml",
          etag: '"fixture"',
        };

        const checksum = request.headers["x-amz-checksum-crc32"];

        if (operation === "UploadPart" && checksum) {
          responseHeaders["x-amz-checksum-crc32"] = checksum;
        }

        return Promise.resolve({
          response: {
            statusCode: 200,
            headers: responseHeaders,
            body: Buffer.from(responses[operation] ?? ""),
          },
        });
      },
    },
  });

  // Managed upload is deliberately serial so this fixture's operation label
  // cannot race between middleware dispatch and the transport observation.
  client.middlewareStack.add(
    (next, context) => (arguments_) => {
      assert.ok(context.commandName);
      operation = context.commandName.replace(/Command$/, "");
      return next(arguments_);
    },
    { step: "initialize", name: "ownedOperationLabel" },
  );

  try {
    const identity = { Bucket: "bucket", Key: "key" };
    const part = { ...identity, UploadId: "owned-upload", PartNumber: 1 };

    await client.send(
      new PutObjectCommand({ ...identity, Body: Buffer.from("123456789") }),
    );

    await client.send(new CreateMultipartUploadCommand(identity));

    await client.send(
      new UploadPartCommand({ ...part, Body: Buffer.from("123456789") }),
    );

    await client.send(
      new CompleteMultipartUploadCommand({
        ...identity,
        UploadId: "owned-upload",
        MultipartUpload: { Parts: [{ PartNumber: 1, ETag: '"fixture"' }] },
      }),
    );

    await client.send(
      new CopyObjectCommand({
        ...identity,
        CopySource: "source/literal%20/%2B%C3%A9",
      }),
    );

    await client.send(
      new UploadPartCopyCommand({
        ...part,
        CopySource: "source/literal%20/%2B%C3%A9",
      }),
    );

    await client.send(
      new CreateMultipartUploadCommand({
        ...identity,
        ChecksumAlgorithm: "SHA256",
        ChecksumType: "COMPOSITE",
      }),
    );

    assert.equal(exchanges.length, 7);
    workflow = "managed-upload";

    await new Upload({
      client,
      params: {
        ...identity,
        Key: "managed",
        Body: Buffer.concat([
          Buffer.alloc(5 * 1024 * 1024),
          Buffer.from("123456789"),
        ]),
      },
      queueSize: 1,
      partSize: 5 * 1024 * 1024,
    }).done();

    assert.deepEqual(
      exchanges.slice(7).map((entry) => entry.operation),
      [
        "CreateMultipartUpload",
        "UploadPart",
        "UploadPart",
        "CompleteMultipartUpload",
      ],
    );

    assert.equal(exchanges[7]?.headers["x-amz-checksum-algorithm"], "CRC32");
    assert.equal(exchanges[8]?.headers["x-amz-checksum-crc32"], "yTuzdQ==");
    assert.equal(exchanges[9]?.headers["x-amz-checksum-crc32"], "y/Q5Jg==");

    const destination = process.env["mikrio_sdk_defaults_record"];

    if (destination) {
      await writeFile(
        destination,
        JSON.stringify(
          {
            origin:
              "JavaScript v3 controlled transport; authored responses; no service execution",
            sourceForm: "Buffer over explicit HTTP endpoint",
            exchanges,
          },
          null,
          2,
        ) + "\n",
      );
    }

    assert.equal(exchanges.length, 11);

    for (const index of [1, 3, 4, 5]) {
      const exchange = exchanges[index];
      assert.ok(exchange);

      assert.equal(
        Object.keys(exchange.headers).some(
          (name) => name.includes("checksum") || name.endsWith("directive"),
        ),
        false,
      );
    }

    for (const index of [4, 5]) {
      assert.equal(
        exchanges[index]?.headers["x-amz-copy-source"],
        "source/literal%20/%2B%C3%A9",
      );
    }

    assert.equal(exchanges[0]?.headers["x-amz-checksum-crc32"], "y/Q5Jg==");
    assert.equal(exchanges[2]?.headers["x-amz-checksum-crc32"], "y/Q5Jg==");
    assert.equal(exchanges[6]?.headers["x-amz-checksum-algorithm"], "SHA256");
    assert.equal(exchanges[6]?.headers["x-amz-checksum-type"], "COMPOSITE");
  } finally {
    client.destroy();
  }
});
