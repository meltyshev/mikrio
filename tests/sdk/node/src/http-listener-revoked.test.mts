import assert from "node:assert/strict";
import test from "node:test";
import { GetObjectCommand, S3ServiceException } from "@aws-sdk/client-s3";
import { client } from "./http-client.mjs";

await test("independent process revocation denies both live public listeners", async () => {
  for (const endpoint of [
    process.env["mikrio_fixture_endpoint"],
    process.env["mikrio_fixture_peer"],
  ]) {
    assert.ok(endpoint);
    const s3 = client(endpoint);

    try {
      await assert.rejects(
        s3.send(new GetObjectCommand({ Bucket: "photos", Key: "native-seed" })),
        (error: unknown) =>
          error instanceof S3ServiceException &&
          error.name === "AccessDenied" &&
          error.$metadata.httpStatusCode === 403,
      );
    } finally {
      s3.destroy();
    }
  }
});
