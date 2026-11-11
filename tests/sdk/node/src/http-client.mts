import assert from "node:assert/strict";
import { S3Client } from "@aws-sdk/client-s3";

export function client(
  endpoint = process.env["mikrio_fixture_endpoint"],
): S3Client {
  assert.ok(endpoint);

  return new S3Client({
    endpoint,
    region: "us-east-1",
    forcePathStyle: true,
    maxAttempts: 1,
    credentials: {
      accessKeyId:
        process.env["mikrio_fixture_key"] ??
        "mikrio_a00112233445566778899AABBCCDDEEFF",
      secretAccessKey:
        process.env["mikrio_fixture_secret"] ?? "synthetic-http-probe-secret",
    },
  });
}
