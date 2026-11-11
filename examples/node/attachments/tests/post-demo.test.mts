import assert from "node:assert/strict";
import { test } from "node:test";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { request } from "node:http";
import { Auth } from "../src/auth.mjs";
import { Attachments } from "../src/attachments.mjs";
import { createApplicationServer } from "../src/http.mjs";
import {
  DEMO_RETENTION_MS,
  PostDemo,
  type PostForm,
} from "../src/post-demo.mjs";
import type { ObjectLocation } from "../src/storage.mjs";
import {
  MemoryObjects,
  OWNER,
  OTHER,
  expectAppError,
  createFixture,
} from "./support.mjs";

class DemoObjects extends MemoryObjects {
  readonly forms: ObjectLocation[] = [];

  createPostForm(object: ObjectLocation): Promise<PostForm> {
    this.forms.push(object);

    return Promise.resolve({
      url: "https://owned.invalid",
      fields: { key: object.key },
    });
  }
}

await test("POST demo HTTP issuance requires session and CSRF and account-scoped readback cannot target another user's key", async (t) => {
  const { state, options, resources } = await createFixture(t);
  const storage = new DemoObjects();
  const origin = "http://app.owned.invalid";

  const demo = new PostDemo(
    storage,
    "post-demonstration",
    "ephemeral/",
    origin,
    true,
  );

  const cookies = new Map<string, string>();

  for (const [accountId, token] of [
    [OWNER, "a".repeat(64)],
    [OTHER, "b".repeat(64)],
  ]) {
    assert.ok(accountId && token);

    await state.call("addSession", {
      accountId,
      tokenHash: createHash("sha256").update(token).digest("hex"),
      now: Date.now(),
    });

    cookies.set(accountId, `attachment-session=${token}`);
  }

  const app = await createApplicationServer(
    new Auth(state),
    new Attachments(state, storage, options.bucket, options.prefix, true),
    origin,
    "http://storage.owned.invalid",
    undefined,
    demo,
  );

  resources.defer(() => app.close());
  app.server.listen(0, "127.0.0.1");
  await once(app.server, "listening");
  const address = app.server.address();
  assert.ok(address && typeof address !== "string");

  const send = (
    path: string,
    account?: string,
    extra: Record<string, string> = {},
  ) =>
    new Promise<{ status: number; body: string }>((resolve, reject) => {
      const req = request(
        {
          hostname: "127.0.0.1",
          port: address.port,
          path,
          method: "POST",
          headers: {
            host: "app.owned.invalid",
            origin,
            "x-attachment-request": "1",
            "content-type": "application/json",
            "content-length": "2",
            ...(account ? { cookie: cookies.get(account) ?? "" } : {}),
            ...extra,
          },
        },
        (response) => {
          let body = "";
          response.setEncoding("utf8");

          response.on("data", (chunk: string) => {
            body += chunk;
          });

          response.once("end", () =>
            resolve({ status: response.statusCode ?? 0, body }),
          );

          response.once("error", reject);
        },
      );

      req.once("error", reject);
      req.end("{}");
    });

  const path = "/api/post-demo/forms";
  assert.equal((await send(path)).status, 401);

  assert.equal(
    (await send(path, OWNER, { origin: "https://untrusted.invalid" })).status,
    401,
  );

  assert.equal(
    (await send(path, OWNER, { "x-attachment-request": "0" })).status,
    401,
  );

  assert.equal(storage.forms.length, 0);
  const response = await send(path, OWNER);
  assert.equal(response.status, 201);
  const issued = JSON.parse(response.body) as { id: string; form: PostForm };
  const key = issued.form.fields.key;
  assert.ok(key);
  storage.objects.set(key, Buffer.from("owner"));
  const otherRead = await send(`/api/post-demo/${issued.id}/download`, OTHER);
  assert.equal(otherRead.status, 200);
  const url = (JSON.parse(otherRead.body) as { url: string }).url;
  assert.ok(url.includes("/" + OTHER + "/"));
  assert.ok(!url.includes("/" + OWNER + "/"));

  assert.equal(
    (await send(`/api/post-demo/${issued.id}/delete`, OTHER)).status,
    200,
  );

  assert.equal(storage.objects.has(key), true);

  assert.equal(
    (await send(`/api/post-demo/${issued.id}/delete`, OWNER)).status,
    200,
  );

  assert.equal(storage.objects.has(key), false);
  assert.deepEqual(await state.call("attachments", { accountId: OWNER }), []);
});

await test("POST demo owns separate account keys and recurring bounded cleanup catches replay after deletion and restart", async () => {
  const storage = new DemoObjects();

  const demo = new PostDemo(
    storage,
    "post-demonstration",
    "ephemeral/",
    "https://app.invalid",
    true,
  );

  const issued = await demo.issue(OWNER, {});
  const key = issued.form.fields.key;
  assert.ok(key);
  assert.ok(key.startsWith("ephemeral/" + OWNER + "/"));

  assert.notEqual(
    await demo.download(OTHER, issued.id),
    await demo.download(OWNER, issued.id),
  );

  storage.objects.set(key, Buffer.from("first"));

  assert.deepEqual(await demo.cleanup(issued.cleanupAt - 1), {
    deleted: 0,
    failed: 0,
  });

  await demo.delete(OWNER, issued.id);
  assert.equal(storage.objects.has(key), false);
  storage.objects.set(key, Buffer.from("late replay"));

  assert.deepEqual(await demo.cleanup(issued.cleanupAt), {
    deleted: 1,
    failed: 0,
  });

  storage.objects.set(key, Buffer.from("late admitted transfer"));

  const restarted = new PostDemo(
    storage,
    "post-demonstration",
    "ephemeral/",
    "https://app.invalid",
    true,
  );

  assert.deepEqual(
    await restarted.cleanup(issued.cleanupAt + DEMO_RETENTION_MS),
    { deleted: 1, failed: 0 },
  );

  storage.objects.set("private/attachment", Buffer.from("independent owner"));

  for (let i = 0; i < 17; i += 1) {
    storage.objects.set(
      "ephemeral/unclaimed-" + i.toString().padStart(2, "0"),
      Buffer.from("abandoned"),
    );
  }

  assert.deepEqual(await restarted.cleanup(issued.cleanupAt), {
    deleted: 16,
    failed: 0,
  });

  assert.deepEqual(await restarted.cleanup(issued.cleanupAt), {
    deleted: 1,
    failed: 0,
  });

  assert.equal(storage.objects.has("private/attachment"), true);
});

await test("POST demo refuses unaccepted cleanup invalid issuance and traversal while retaining failed work", async () => {
  const storage = new DemoObjects();

  const paused = new PostDemo(
    storage,
    "post-demonstration",
    "ephemeral/",
    "https://app.invalid",
    false,
  );

  await assert.rejects(paused.issue(OWNER, {}), expectAppError("unavailable"));
  await assert.rejects(paused.cleanup(), expectAppError("unavailable"));
  assert.equal(storage.forms.length, 0);

  const demo = new PostDemo(
    storage,
    "post-demonstration",
    "ephemeral/",
    "https://app.invalid",
    true,
  );

  for (const input of [
    { key: "chosen" },
    { status: 201 },
    { status: "302" },
    { redirect: "https://untrusted.invalid" },
  ]) {
    await assert.rejects(demo.issue(OWNER, input), expectAppError("invalid"));
  }

  assert.throws(
    () => demo.download(OWNER, "../private/key"),
    expectAppError("invalid"),
  );

  const issued = await demo.issue(OWNER, { status: "201" });
  const key = issued.form.fields.key;
  assert.ok(key);
  storage.objects.set(key, Buffer.from("value"));
  storage.loseDeleteResponse = true;

  assert.deepEqual(await demo.cleanup(issued.cleanupAt), {
    deleted: 0,
    failed: 1,
  });

  storage.objects.set(key, Buffer.from("late value"));
  storage.loseDeleteResponse = false;

  assert.deepEqual(await demo.cleanup(issued.cleanupAt), {
    deleted: 1,
    failed: 0,
  });
});
